pub mod credit;
pub mod credit_fetch;
pub mod credit_store;
pub mod fetcher;
pub mod math;
pub mod models;
pub mod regime;
pub mod store;

use crate::fetcher::{
    build_http_client, collect_full_market_bundle, fetch_clob_price_history,
    reconstruct_daily_backfill,
};
use crate::models::{
    BackfillSummary, BusyState, ClobHistoryPoint, DashboardState, ExportResult, RefreshSummary,
    SnapshotDetail, StrikeHistoryResponse,
};
use crate::credit::SelectedBond;
use crate::credit_fetch::{
    build_fred_client, connect_ib_session, fetch_fred_series, fetch_ib_bonds, fetch_ib_histories,
    fetch_treasury_curve, fred_series_ids, history_targets, CreditSettings, IbFetchOutcome, IbSession,
    FRED_HISTORY_DAYS, FRED_OVERLAP_DAYS, TREASURY_ANCHOR_SERIES,
};
use crate::credit_store::IssuerBondInfo;
use crate::regime::{compute_regime, RegimeReport};
use crate::store::ComputeStore;
use reqwest::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{watch, Mutex, OwnedMutexGuard};

const AUTO_REFRESH_MIN: u64 = 15;
/// Automatischer Backfill beim Start höchstens so oft (füllt auch Lücken nach Tagen ohne App).
const AUTO_BACKFILL_MIN_AGE_HOURS: i64 = 20;
/// Takt, in dem der Hintergrund-Poller die Fälligkeit gegen die Uhrzeit prüft.
const SCHEDULER_TICK_SECS: u64 = 30;
/// Kredit- und Anleihedaten ändern sich täglich: Abruf höchstens alle sechs Stunden.
const CREDIT_REFRESH_HOURS: i64 = 6;
/// Abstand automatischer IB-Versuche nach einem Lauf ohne neue Kurse (Stunden).
const IB_RETRY_HOURS: i64 = 2;
/// Ältester Tag der Zinskurve, der noch als aktueller Abruf gilt (Feiertage, lange Wochenenden).
const TREASURY_MAX_AGE_DAYS: i64 = 7;
/// Nach einem Fehlschlag frühestens nach dieser Zeit erneut versuchen.
const CREDIT_RETRY_MIN: i64 = 30;
/// Prüftakt des Kredit-Pollers.
const CREDIT_TICK_SECS: u64 = 300;
const GPU_KEYS_ALLOWED: [&str; 4] = ["H100", "H200", "B200", "B300"];

/// True, wenn der RFC3339-Zeitstempel fehlt, unlesbar oder älter als `max_age` ist.
/// Eine Zinskurve zählt als Erfolg, wenn ihr letzter Tag höchstens TREASURY_MAX_AGE_DAYS
/// alt ist und an diesem Tag mindestens drei Laufzeiten Werte haben (so viele braucht die
/// Spreadrechnung zum Interpolieren).
fn treasury_curve_check(
    curve: &std::collections::BTreeMap<String, Vec<(chrono::NaiveDate, f64)>>,
    today: chrono::NaiveDate,
) -> Result<(), String> {
    let Some(last) = curve.get(TREASURY_ANCHOR_SERIES).and_then(|v| v.iter().map(|p| p.0).max()) else {
        return Err("Treasury.gov: keine Renditen im Abrufzeitraum".into());
    };
    if (today - last).num_days() > TREASURY_MAX_AGE_DAYS {
        return Err(format!("Treasury.gov: letzte Zinskurve vom {}, keine aktuelle", last.format("%d.%m.%Y")));
    }
    let tenors = curve.values().filter(|v| v.iter().any(|p| p.0 == last)).count();
    if tenors < 3 {
        return Err(format!("Treasury.gov: am {} nur {tenors} Laufzeiten", last.format("%d.%m.%Y")));
    }
    Ok(())
}

fn is_older_than(ts: Option<&str>, max_age: chrono::Duration) -> bool {
    match ts.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        Some(dt) => chrono::Utc::now().signed_duration_since(dt) > max_age,
        None => true,
    }
}

/// Gescheiterter Backfill: kein einziger Tag rekonstruiert und Abruffehler vorhanden
/// (offline nach dem Aufwachen, Abbruch nach wenigen Strikes, Rate-Limit). Dann darf
/// last_backfill_at nicht fortgeschrieben werden, sonst sperrt die 20-Stunden-Regel
/// den nächsten automatischen Versuch, obwohl nichts geladen wurde.
fn backfill_failed_completely(days_reconstructed: usize, errors: &[String]) -> bool {
    days_reconstructed == 0 && !errors.is_empty()
}

fn unix_to_rfc3339(ts: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Stand der Kredit- und Anleiheabrufe für den Reiter Finanzierung.
#[derive(Debug, Clone, Serialize)]
pub struct CreditStatus {
    pub running: bool,
    /// Stufe 2 des IB-Abrufs: Der Korb steht, der längere Kursverlauf wird nachgeladen.
    pub history_running: bool,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub ib_last_success_at: Option<String>,
    pub ib_status: Option<String>,
    pub errors: Vec<String>,
}

/// Was Stufe 2 des IB-Abrufs aus Stufe 1 übernimmt: Verbindung, Korb, Kontrakte und Status.
struct HistoryJob {
    session: IbSession,
    targets: Vec<SelectedBond>,
    outcome: IbFetchOutcome,
    status: String,
    today: chrono::NaiveDate,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinanceView {
    pub report: RegimeReport,
    pub bonds: Vec<IssuerBondInfo>,
    pub status: CreditStatus,
    pub settings: CreditSettings,
}

pub struct AppState {
    pub store: ComputeStore,
    pub client: Client,
    /// Eigener Client für FRED (Kennung mit Kontaktadresse, sonst weist FRED ab).
    pub fred_client: Client,
    pub refreshing: AtomicBool,
    pub backfilling: AtomicBool,
    pub crediting: AtomicBool,
    pub history_loading: AtomicBool,
    pub refresh_lock: Mutex<()>,
    pub backfill_lock: Mutex<()>,
    /// Hält ein Kreditabruf samt Stufe 2 des IB-Abrufs, die im Hintergrund weiterläuft.
    pub credit_lock: Arc<Mutex<()>>,
    pub strike_cache: Mutex<HashMap<(String, u32, u32), (Instant, Vec<ClobHistoryPoint>)>>,
    pub strike_inflight: Mutex<HashMap<(String, u32, u32), watch::Sender<bool>>>,
    /// Fälligkeit der nächsten automatischen Aktualisierung (Unix-Sekunden, 0 = sofort).
    /// Wanduhr statt Tokio-Timer: Der monotone Takt pausiert im macOS-Ruhezustand.
    pub next_auto_refresh_ts: AtomicI64,
    /// Für Busy-Events an das Frontend; wird im Tauri-Setup gesetzt.
    pub app_handle: OnceLock<AppHandle>,
    /// Serialisiert Lesen+Senden der Busy-Flags: Da jede Flag-Änderung ein Event auslöst,
    /// trägt so das zuletzt gesendete Event immer den aktuellen Stand.
    busy_emit_lock: std::sync::Mutex<()>,
    /// Serialisiert Lesen und Senden des Kreditstatus, damit ein älteres Ende-Event nie nach
    /// dem Start-Event eines neuen Laufs ankommt.
    credit_emit_lock: std::sync::Mutex<()>,
}

impl AppState {
    fn new() -> Result<Self, String> {
        let store = ComputeStore::open_default()?;
        let client = build_http_client()?;
        let fred_client = build_fred_client()?;
        Ok(Self {
            store,
            client,
            fred_client,
            refreshing: AtomicBool::new(false),
            backfilling: AtomicBool::new(false),
            crediting: AtomicBool::new(false),
            history_loading: AtomicBool::new(false),
            refresh_lock: Mutex::new(()),
            backfill_lock: Mutex::new(()),
            credit_lock: Arc::new(Mutex::new(())),
            strike_cache: Mutex::new(HashMap::new()),
            strike_inflight: Mutex::new(HashMap::new()),
            next_auto_refresh_ts: AtomicI64::new(0),
            app_handle: OnceLock::new(),
            busy_emit_lock: std::sync::Mutex::new(()),
            credit_emit_lock: std::sync::Mutex::new(()),
        })
    }

    fn next_refresh_at(&self) -> Option<String> {
        match self.next_auto_refresh_ts.load(Ordering::Relaxed) {
            0 => None,
            ts => unix_to_rfc3339(ts),
        }
    }

    fn schedule_next_refresh_from(&self, from_unix: i64) {
        self.next_auto_refresh_ts
            .store(from_unix + (AUTO_REFRESH_MIN * 60) as i64, Ordering::Relaxed);
    }

    fn build_state(&self, refreshing: bool, backfilling: bool) -> Result<DashboardState, String> {
        let mut s = self
            .store
            .get_dashboard_state(refreshing, backfilling, AUTO_REFRESH_MIN)?;
        s.next_refresh_at = self.next_refresh_at();
        Ok(s)
    }

    fn current_state(&self) -> Result<DashboardState, String> {
        self.build_state(
            self.refreshing.load(Ordering::Relaxed),
            self.backfilling.load(Ordering::Relaxed),
        )
    }

    /// Meldet dem Frontend sofort, dass ein Abruf/Backfill startet oder endet,
    /// damit Buttons gesperrt werden statt "läuft bereits"-Fehler zu erzeugen.
    fn emit_busy(&self) {
        let _serial = self.busy_emit_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = self.app_handle.get() {
            let _ = handle.emit(
                "compute:busy",
                BusyState {
                    refreshing: self.refreshing.load(Ordering::Relaxed),
                    backfilling: self.backfilling.load(Ordering::Relaxed),
                    next_refresh_at: self.next_refresh_at(),
                },
            );
        }
    }

    async fn run_refresh(&self) -> Result<RefreshSummary, String> {
        let Ok(_guard) = self.refresh_lock.try_lock() else {
            return Err("Aktualisierung läuft bereits.".into());
        };
        // Jeder Abruf (auch manuell) verschiebt den nächsten automatischen um ein volles Intervall
        self.schedule_next_refresh_from(chrono::Utc::now().timestamp());
        self.refreshing.store(true, Ordering::Relaxed);
        self.emit_busy();
        let res = async {
            let bundle = collect_full_market_bundle(&self.client).await;
            let (saved_curves, saved_daily) = self.store.save_bundle(&bundle)?;
            let state = self.build_state(false, self.backfilling.load(Ordering::Relaxed))?;
            Ok(RefreshSummary {
                ok: true,
                captured_at: bundle.captured_at,
                saved_curves,
                saved_daily,
                errors: bundle.errors,
                state,
            })
        }
        .await;
        self.refreshing.store(false, Ordering::Relaxed);
        self.emit_busy();
        res
    }

    fn credit_status(&self) -> CreditStatus {
        let meta = |k: &str| self.store.get_meta(k).ok().flatten();
        CreditStatus {
            running: self.crediting.load(Ordering::Relaxed),
            history_running: self.history_loading.load(Ordering::Relaxed),
            last_attempt_at: meta("credit_last_attempt_at"),
            last_success_at: meta("credit_last_success_at"),
            ib_last_success_at: meta("ib_last_success_at"),
            ib_status: meta("ib_last_status"),
            errors: meta("credit_last_errors")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
        }
    }

    /// Fällig, wenn der letzte Erfolg älter als sechs Stunden ist und der letzte Versuch
    /// nicht gerade erst scheiterte.
    fn credit_due(&self) -> bool {
        let st = self.credit_status();
        is_older_than(
            st.last_success_at.as_deref(),
            chrono::Duration::hours(CREDIT_REFRESH_HOURS),
        ) && is_older_than(
            st.last_attempt_at.as_deref(),
            chrono::Duration::minutes(CREDIT_RETRY_MIN),
        )
    }

    /// Lädt die Treasury-Kurve (Treasury.gov), die OAS-Reihen (FRED) und, falls erreichbar,
    /// Anleihekurse über IB. Automatische Läufe fragen IB nur ab, wenn der letzte IB-Erfolg
    /// älter als CREDIT_REFRESH_HOURS ist: Ein Teilfehler bei FRED wiederholt sonst alle
    /// CREDIT_RETRY_MIN Minuten den minutenlangen Anleiheabruf.
    ///
    /// Der IB-Abruf hat zwei Stufen: Stufe 1 stellt Korb und Monatskurse zusammen und kehrt
    /// zurück; Stufe 2 lädt danach im Hintergrund den längeren Verlauf nach und hält die
    /// Sperre bis zu ihrem Ende.
    async fn run_credit_refresh(self: &Arc<Self>, manual: bool) -> Result<CreditStatus, String> {
        let Ok(guard) = Arc::clone(&self.credit_lock).try_lock_owned() else {
            return Err("Kreditabruf läuft bereits.".into());
        };
        self.crediting.store(true, Ordering::Relaxed);
        self.emit_credit();
        let now = chrono::Utc::now();
        let now_s = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let today = now.date_naive();
        let mut errors = Vec::new();
        let from_for = |id: &str| match self.store.latest_credit_date(id) {
            Ok(Some(last)) => last - chrono::Duration::days(FRED_OVERLAP_DAYS),
            _ => today - chrono::Duration::days(FRED_HISTORY_DAYS),
        };

        let mut treasury_ok = false;
        match fetch_treasury_curve(&self.client, from_for(TREASURY_ANCHOR_SERIES), today).await {
            Ok(curve) => {
                match treasury_curve_check(&curve, today) {
                    Ok(()) => treasury_ok = true,
                    Err(e) => errors.push(e),
                }
                for (id, points) in &curve {
                    if let Err(e) = self.store.save_credit_series(id, points) {
                        errors.push(e);
                        treasury_ok = false;
                    }
                }
            }
            Err(e) => errors.push(e),
        }

        // Eine Reihe zählt nur mit mindestens einem Wert: Eine Antwort mit passendem Kopf, aber
        // ohne lesbare Werte ist ein Teilfehler und führt zum kürzeren Wiederholungsabstand.
        let fred_ids = fred_series_ids();
        let mut fred_ok = 0usize;
        for id in &fred_ids {
            match fetch_fred_series(&self.fred_client, id, from_for(id)).await {
                Ok(points) if points.is_empty() => errors.push(format!("FRED {id}: keine Werte im Abrufzeitraum")),
                Ok(points) => match self.store.save_credit_series(id, &points) {
                    Ok(_) => fred_ok += 1,
                    Err(e) => errors.push(e),
                },
                Err(e) => errors.push(e),
            }
        }

        // Zinskurve und Aufschläge sind gespeichert: Erfolg schon jetzt vermerken und melden,
        // damit der Datenstand während des IB-Abrufs stimmt. Erfolg nur, wenn Zinskurve und
        // alle OAS-Reihen da sind; sonst neuer Versuch nach CREDIT_RETRY_MIN.
        if fred_ok == fred_ids.len() && treasury_ok {
            let _ = self.store.set_meta("credit_last_success_at", &now_s);
            self.emit_credit();
        }

        let settings = self.store.load_credit_settings();
        let meta = |k: &str| self.store.get_meta(k).ok().flatten();
        // Automatisch höchstens alle CREDIT_REFRESH_HOURS nach einem Erfolg und alle
        // IB_RETRY_HOURS nach einem Versuch; ein Klick fragt immer ab.
        let ib_due = manual
            || (is_older_than(meta("ib_last_success_at").as_deref(), chrono::Duration::hours(CREDIT_REFRESH_HOURS))
                && is_older_than(meta("ib_last_attempt_at").as_deref(), chrono::Duration::hours(IB_RETRY_HOURS)));
        let mut stage_two: Option<HistoryJob> = None;
        if settings.ib_enabled && ib_due {
            let _ = self.store.set_meta("ib_last_attempt_at", &now_s);
            match connect_ib_session(&settings).await {
                Err(e) => {
                    let _ = self.store.set_meta("ib_last_status", "Keine Verbindung zu TWS oder IB Gateway");
                    errors.push(format!("IB: {e}"));
                }
                Ok(mut session) => {
                    let previous = self.store.load_bond_selection().unwrap_or_default();
                    let stored = self.store.load_stored_bonds().unwrap_or_default();
                    let outcome = fetch_ib_bonds(&mut session, &settings, &previous, &stored, today).await;
                    let bonds: usize = outcome.selections.values().map(Vec::len).sum();
                    let status = format!(
                        "Verbunden über Port {}: {bonds} Anleihen im Korb, {} Kursreihen geladen, {} übersprungen",
                        session.port,
                        outcome.histories.len(),
                        outcome.skipped.len()
                    );
                    let _ = self.store.set_meta("ib_last_status", &status);
                    let mut saved = false;
                    if !outcome.selections.is_empty() {
                        match self.store.save_ib_outcome(&outcome, today) {
                            Ok(_) => {
                                saved = true;
                                // Erfolg nur mit mindestens einer neu geladenen Kursreihe
                                if !outcome.histories.is_empty() {
                                    let _ = self.store.set_meta("ib_last_success_at", &now_s);
                                }
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    errors.extend(outcome.errors.iter().cloned());
                    if saved {
                        let stored = self.store.load_stored_bonds().unwrap_or_default();
                        let targets = history_targets(&outcome.selections, &stored, &self.store.load_history_done());
                        if !targets.is_empty() {
                            stage_two = Some(HistoryJob { session, targets, outcome, status, today });
                        }
                    }
                }
            }
        } else if settings.ib_enabled {
            // Anleihekurse sind aktuell genug; der Status des letzten IB-Laufs bleibt stehen
        } else {
            let _ = self.store.set_meta("ib_last_status", "IB-Abruf ausgeschaltet");
        }

        // Versuch mit dem Endzeitpunkt: Nach einem langen Lauf beginnt der Wiederholungsabstand erst jetzt
        let end_s = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let _ = self.store.set_meta("credit_last_attempt_at", &end_s);
        let _ = self.store.set_meta(
            "credit_last_errors",
            &serde_json::to_string(&errors).unwrap_or_else(|_| "[]".into()),
        );
        if stage_two.is_some() {
            self.history_loading.store(true, Ordering::Relaxed);
        }
        self.crediting.store(false, Ordering::Relaxed);
        // Rückgabe noch unter der Sperre: Sie beschreibt diesen Lauf, nicht einen danach gestarteten
        let result = self.credit_status();
        match stage_two {
            Some(job) => {
                // Erst melden, dann Stufe 2 starten: Ihr Ende-Event kommt so immer danach
                self.emit_credit();
                let me = Arc::clone(self);
                tauri::async_runtime::spawn(async move { me.load_bond_histories(guard, job).await });
            }
            None => {
                // Erst die Sperre freigeben, dann das Ende melden: Ein Klick nach dem Ende-Event
                // bekommt so nie mehr „läuft bereits“.
                drop(guard);
                self.emit_credit();
            }
        }
        if fred_ok == 0 && !treasury_ok {
            return Err(format!(
                "Weder Zinskurve noch Kreditaufschläge abrufbar. {}",
                errors.first().map(String::as_str).unwrap_or("")
            ));
        }
        Ok(result)
    }

    /// Stufe 2 des IB-Abrufs: lädt den längeren Kursverlauf der Korbanleihen nach. Der Korb
    /// bleibt unverändert; Emittenten, die inzwischen aus den Einstellungen entfernt wurden,
    /// schreibt das Speichern nicht zurück.
    async fn load_bond_histories(self: Arc<Self>, guard: OwnedMutexGuard<()>, job: HistoryJob) {
        // Setzt die Anzeige „Verlauf lädt“ auch dann zurück, wenn diese Stufe unerwartet abbricht
        struct ResetFlag<'a>(&'a AtomicBool);
        impl Drop for ResetFlag<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Relaxed);
            }
        }
        let _reset = ResetFlag(&self.history_loading);
        let HistoryJob { mut session, targets, outcome, status, today } = job;
        let fetched = fetch_ib_histories(&mut session, &targets, &outcome.contracts).await;
        drop(session);
        let loaded = fetched.histories.len();
        let save = IbFetchOutcome {
            port: outcome.port,
            selections: outcome.selections,
            histories: fetched.histories,
            ..Default::default()
        };
        let mut errors: Vec<String> = self
            .store
            .get_meta("credit_last_errors")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        match self.store.save_ib_outcome(&save, today) {
            Ok(_) => {
                if let Err(e) = self.store.add_history_done(&fetched.done) {
                    errors.push(e);
                }
            }
            Err(e) => errors.push(e),
        }
        if !fetched.pending.is_empty() {
            errors.push(format!(
                "IB: Kursverlauf noch nicht geladen für {} (IB antwortet nicht rechtzeitig), nächster Versuch beim nächsten Abruf",
                fetched.pending.join(", ")
            ));
        }
        let _ = self.store.set_meta(
            "ib_last_status",
            &format!("{status}; Verlauf für {loaded} von {} Anleihen nachgeladen", targets.len()),
        );
        let _ = self.store.set_meta(
            "credit_last_errors",
            &serde_json::to_string(&errors).unwrap_or_else(|_| "[]".into()),
        );
        self.history_loading.store(false, Ordering::Relaxed);
        drop(guard);
        self.emit_credit();
    }

    fn emit_credit(&self) {
        let _serial = self.credit_emit_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = self.app_handle.get() {
            let _ = handle.emit("compute:credit", self.credit_status());
        }
    }

    fn finance_view(&self, gpu: &str) -> Result<FinanceView, String> {
        let settings = self.store.load_credit_settings();
        let today = chrono::Utc::now().date_naive();
        let (inputs, bonds) = self.store.build_model_inputs(gpu, &settings)?;
        Ok(FinanceView {
            report: compute_regime(&inputs, today),
            bonds,
            status: self.credit_status(),
            settings,
        })
    }

    async fn run_backfill(&self, days: u32) -> Result<BackfillSummary, String> {
        let Ok(_guard) = self.backfill_lock.try_lock() else {
            return Err("Historien-Backfill läuft bereits.".into());
        };
        self.backfilling.store(true, Ordering::Relaxed);
        self.emit_busy();
        let res = async {
            let mut cur = self.current_state()?;
            if !cur.has_data {
                // Sauber über run_refresh laufen (mit refresh_lock statt ungeschütztem Parallel-Write)
                let _ = self.run_refresh().await;
                cur = self.current_state()?;
            }
            if !cur.has_data {
                return Err("Noch keine Polymarket-Verfälle für Backfill vorhanden.".into());
            }
            let clamped_days = days.clamp(3, 30);
            let (days_out, strikes_fetched, errors) =
                reconstruct_daily_backfill(&self.client, &cur.gpus, clamped_days).await;
            if backfill_failed_completely(days_out.len(), &errors) {
                return Err(format!(
                    "Kein Tag rekonstruierbar ({strikes_fetched} Kursverläufe geladen), Backfill wird beim nächsten Start erneut versucht. {}",
                    errors.first().map(String::as_str).unwrap_or("")
                ));
            }
            let saved_days = self.store.save_backfill_days(&days_out)?;
            let state = self.build_state(self.refreshing.load(Ordering::Relaxed), false)?;
            Ok(BackfillSummary {
                ok: true,
                saved_days,
                gpus_processed: cur.gpus.len(),
                strikes_fetched,
                errors,
                state,
            })
        }
        .await;
        self.backfilling.store(false, Ordering::Relaxed);
        self.emit_busy();
        res
    }
}

#[tauri::command]
async fn get_dashboard_state(state: State<'_, Arc<AppState>>) -> Result<DashboardState, String> {
    state.current_state()
}

#[tauri::command]
async fn refresh_now(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<RefreshSummary, String> {
    let summary = state.run_refresh().await?;
    let _ = app.emit("compute:updated", &summary.state);
    Ok(summary)
}

#[tauri::command]
async fn backfill_history(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<BackfillSummary, String> {
    let summary = state.run_backfill(days.unwrap_or(14)).await?;
    let _ = app.emit("compute:updated", &summary.state);
    Ok(summary)
}

#[tauri::command]
async fn get_snapshot(
    state: State<'_, Arc<AppState>>,
    snapshot_id: i64,
) -> Result<Option<SnapshotDetail>, String> {
    if snapshot_id <= 0 {
        return Err("Ungültige Snapshot-ID.".into());
    }
    state.store.get_snapshot(snapshot_id)
}

#[tauri::command]
async fn get_strike_history(
    state: State<'_, Arc<AppState>>,
    token_id: String,
    days: Option<u32>,
    fidelity: Option<u32>,
) -> Result<StrikeHistoryResponse, String> {
    let clean_id = token_id.trim().to_string();
    // Polymarket CLOB Token-IDs sind dezimale Integer-Strings (typ. 60..80 Ziffern)
    if clean_id.is_empty()
        || clean_id.len() > 128
        || !clean_id.chars().all(|c| c.is_ascii_digit())
    {
        return Err("Ungültige CLOB-Token-ID.".into());
    }
    let d = days.unwrap_or(30).clamp(1, 180);
    let f = fidelity.unwrap_or(60).clamp(1, 1440);
    let key = (clean_id.clone(), d, f);

    // Cache-Hit oder Single-Flight-Deduplizierung via watch-Channel (kein Lost-Wakeup möglich)
    for _ in 0..3 {
        {
            let cache = state.strike_cache.lock().await;
            if let Some((ts, pts)) = cache.get(&key) {
                if ts.elapsed() < Duration::from_secs(180) {
                    return Ok(StrikeHistoryResponse {
                        token_id: clean_id,
                        days: d,
                        fidelity: f,
                        history: pts.clone(),
                    });
                }
            }
        }

        let maybe_rx = {
            let mut inflight = state.strike_inflight.lock().await;
            if let Some(tx) = inflight.get(&key) {
                Some(tx.subscribe())
            } else {
                let (tx, _rx) = watch::channel(false);
                inflight.insert(key.clone(), tx);
                None
            }
        };

        if let Some(mut rx) = maybe_rx {
            if !*rx.borrow() {
                let _ = tokio::time::timeout(Duration::from_secs(30), rx.changed()).await;
            }
            continue;
        }

        let fetch_res = fetch_clob_price_history(&state.client, &clean_id, d, f).await;
        if let Ok(ref pts) = fetch_res {
            let mut cache = state.strike_cache.lock().await;
            if cache.len() > 128 {
                cache.clear();
            }
            cache.insert(key.clone(), (Instant::now(), pts.clone()));
        }
        if let Some(tx) = state.strike_inflight.lock().await.remove(&key) {
            let _ = tx.send(true);
        }

        let pts = fetch_res?;
        return Ok(StrikeHistoryResponse {
            token_id: clean_id,
            days: d,
            fidelity: f,
            history: pts,
        });
    }

    Err("CLOB-Verlaufsabruf hat das Zeitlimit überschritten.".into())
}

#[tauri::command]
async fn export_csv(state: State<'_, Arc<AppState>>, gpu: String) -> Result<ExportResult, String> {
    let key = gpu.trim().to_uppercase();
    let (path, rows) = state.store.export_csv_to_downloads(&key)?;
    Ok(ExportResult { ok: true, path, rows })
}

#[tauri::command]
async fn get_finance_view(
    state: State<'_, Arc<AppState>>,
    gpu: String,
) -> Result<FinanceView, String> {
    let key = gpu.trim().to_uppercase();
    if !GPU_KEYS_ALLOWED.contains(&key.as_str()) {
        return Err("Unbekannte GPU.".into());
    }
    let st = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || st.finance_view(&key))
        .await
        .map_err(|e| format!("Berechnung abgebrochen: {e}"))?
}

#[tauri::command]
async fn refresh_credit_now(state: State<'_, Arc<AppState>>) -> Result<CreditStatus, String> {
    state.run_credit_refresh(true).await
}

#[tauri::command]
async fn save_credit_settings(
    state: State<'_, Arc<AppState>>,
    settings: CreditSettings,
) -> Result<CreditSettings, String> {
    let clean = settings.validated()?;
    state.store.save_credit_settings(&clean, chrono::Utc::now().date_naive())?;
    Ok(clean)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = Arc::new(AppState::new().expect("AppState konnte nicht erstellt werden"));
    let bg_state = Arc::clone(&app_state);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .manage(app_state)
        .setup(move |app| {
            // Fallback: Das Fenster startet unsichtbar und wird vom Frontend nach setTheme gezeigt.
            // Scheitert das (z. B. Skriptfehler), erscheint es hier spätestens nach 3 Sekunden.
            let show_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if let Some(win) = show_handle.get_webview_window("main") {
                    if !win.is_visible().unwrap_or(true) {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            });

            let handle = app.handle().clone();
            let state = Arc::clone(&bg_state);
            let _ = state.app_handle.set(handle.clone());

            // Kredit-Poller getrennt vom 15-Minuten-Takt: Ein langsamer IB-Abruf hält die Kurven nicht auf
            let credit_state = Arc::clone(&bg_state);
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(20)).await;
                loop {
                    if credit_state.credit_due() {
                        let _ = credit_state.run_credit_refresh(false).await;
                    }
                    tokio::time::sleep(Duration::from_secs(CREDIT_TICK_SECS)).await;
                }
            });
            tauri::async_runtime::spawn(async move {
                let initial = state.current_state().ok();
                // Fälligkeit aus dem letzten Datenstand ableiten (ein Start-Abruf setzt sie neu)
                if let Some(last) = initial
                    .as_ref()
                    .and_then(|s| s.last_captured_at.as_deref())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                {
                    state.schedule_next_refresh_from(last.timestamp());
                }
                // Prüft explizit Kurvenabdeckung pro GPU (nicht nur Spot-Daten nach Legacy-Import)
                // und den Datenstand: nach Tagen ohne App sofort frisch laden statt 15 Min. Altdaten.
                let need_live = initial
                    .as_ref()
                    .map(|s| {
                        !s.has_data
                            || s.gpus.values().any(|g| g.expiries.is_empty())
                            || is_older_than(
                                s.last_captured_at.as_deref(),
                                chrono::Duration::minutes(AUTO_REFRESH_MIN as i64),
                            )
                    })
                    .unwrap_or(true);

                if need_live {
                    if let Ok(summary) = state.run_refresh().await {
                        let _ = handle.emit("compute:updated", &summary.state);
                    }
                }

                // Nicht mehr "solange eine GPU 0 Backfill-Tage hat": Für junge Märkte (z. B. H200
                // ohne CLOB-Vortage) lief so bei jedem Start ein voller Backfill mit Hunderten Abrufen.
                let after_live = state.current_state().ok();
                let need_backfill = after_live
                    .as_ref()
                    .map(|s| {
                        s.has_data
                            && is_older_than(
                                s.last_backfill_at.as_deref(),
                                chrono::Duration::hours(AUTO_BACKFILL_MIN_AGE_HOURS),
                            )
                    })
                    .unwrap_or(false);

                if need_backfill {
                    if let Ok(summary) = state.run_backfill(14).await {
                        let _ = handle.emit("compute:updated", &summary.state);
                    }
                }

                // Fälligkeit gegen die Wanduhr prüfen: Manuelle Abrufe verschieben den Termin,
                // und nach dem Aufwachen aus dem Ruhezustand wird binnen 30 s aktualisiert.
                loop {
                    tokio::time::sleep(Duration::from_secs(SCHEDULER_TICK_SECS)).await;
                    let due = state.next_auto_refresh_ts.load(Ordering::Relaxed);
                    if chrono::Utc::now().timestamp() < due {
                        continue;
                    }
                    if let Ok(summary) = state.run_refresh().await {
                        let _ = handle.emit("compute:updated", &summary.state);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard_state,
            refresh_now,
            backfill_history,
            get_snapshot,
            get_strike_history,
            export_csv,
            get_finance_view,
            refresh_credit_now,
            save_credit_settings,
        ])
        .run(tauri::generate_context!())
        .expect("Fehler beim Starten von Konvex Compute");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staleness_check_handles_missing_and_old_timestamps() {
        let fifteen = chrono::Duration::minutes(15);
        assert!(is_older_than(None, fifteen));
        assert!(is_older_than(Some("kein Datum"), fifteen));
        assert!(is_older_than(Some("2020-01-01T00:00:00Z"), fifteen));
        let fresh = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(!is_older_than(Some(&fresh), fifteen));
    }

    #[test]
    fn total_backfill_outage_is_not_recorded_as_done() {
        let err = vec!["Netzwerkfehler bei https://clob.polymarket.com/prices-history".to_string()];
        // Offline oder Abbruch nach wenigen Strikes: kein Tag rekonstruiert, Fehler vorhanden
        // -> Fehler, Zeitstempel bleibt
        assert!(backfill_failed_completely(0, &err));
        // Teilausfall mit rekonstruierten Tagen: Rest wird gespeichert
        assert!(!backfill_failed_completely(12, &err));
        // Junger Markt ohne Vortage bzw. ohne Token: kein Fehler, gilt als erledigt
        assert!(!backfill_failed_completely(0, &[]));
        assert!(!backfill_failed_completely(40, &[]));
    }
}
