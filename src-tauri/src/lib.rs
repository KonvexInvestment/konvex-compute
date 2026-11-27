pub mod fetcher;
pub mod math;
pub mod models;
pub mod store;

use crate::fetcher::{
    build_http_client, collect_full_market_bundle, fetch_clob_price_history,
    reconstruct_daily_backfill,
};
use crate::models::{
    BackfillSummary, BusyState, ClobHistoryPoint, DashboardState, ExportResult, RefreshSummary,
    SnapshotDetail, StrikeHistoryResponse,
};
use crate::store::ComputeStore;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{watch, Mutex};

const AUTO_REFRESH_MIN: u64 = 15;
/// Automatischer Backfill beim Start höchstens so oft (füllt auch Lücken nach Tagen ohne App).
const AUTO_BACKFILL_MIN_AGE_HOURS: i64 = 20;
/// Takt, in dem der Hintergrund-Poller die Fälligkeit gegen die Uhrzeit prüft.
const SCHEDULER_TICK_SECS: u64 = 30;

/// True, wenn der RFC3339-Zeitstempel fehlt, unlesbar oder älter als `max_age` ist.
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

pub struct AppState {
    pub store: ComputeStore,
    pub client: Client,
    pub refreshing: AtomicBool,
    pub backfilling: AtomicBool,
    pub refresh_lock: Mutex<()>,
    pub backfill_lock: Mutex<()>,
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
}

impl AppState {
    fn new() -> Result<Self, String> {
        let store = ComputeStore::open_default()?;
        let client = build_http_client()?;
        Ok(Self {
            store,
            client,
            refreshing: AtomicBool::new(false),
            backfilling: AtomicBool::new(false),
            refresh_lock: Mutex::new(()),
            backfill_lock: Mutex::new(()),
            strike_cache: Mutex::new(HashMap::new()),
            strike_inflight: Mutex::new(HashMap::new()),
            next_auto_refresh_ts: AtomicI64::new(0),
            app_handle: OnceLock::new(),
            busy_emit_lock: std::sync::Mutex::new(()),
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
