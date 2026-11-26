pub mod fetcher;
pub mod math;
pub mod models;
pub mod store;

use crate::fetcher::{
    build_http_client, collect_full_market_bundle, fetch_clob_price_history,
    reconstruct_daily_backfill,
};
use crate::models::{
    BackfillSummary, ClobHistoryPoint, DashboardState, ExportResult, RefreshSummary,
    SnapshotDetail, StrikeHistoryResponse,
};
use crate::store::ComputeStore;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{watch, Mutex};

const AUTO_REFRESH_MIN: u64 = 15;

pub struct AppState {
    pub store: ComputeStore,
    pub client: Client,
    pub refreshing: AtomicBool,
    pub backfilling: AtomicBool,
    pub refresh_lock: Mutex<()>,
    pub backfill_lock: Mutex<()>,
    pub strike_cache: Mutex<HashMap<(String, u32, u32), (Instant, Vec<ClobHistoryPoint>)>>,
    pub strike_inflight: Mutex<HashMap<(String, u32, u32), watch::Sender<bool>>>,
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
        })
    }

    fn current_state(&self) -> Result<DashboardState, String> {
        self.store.get_dashboard_state(
            self.refreshing.load(Ordering::Relaxed),
            self.backfilling.load(Ordering::Relaxed),
            AUTO_REFRESH_MIN,
        )
    }

    async fn run_refresh(&self) -> Result<RefreshSummary, String> {
        let Ok(_guard) = self.refresh_lock.try_lock() else {
            return Err("Aktualisierung läuft bereits.".into());
        };
        self.refreshing.store(true, Ordering::Relaxed);
        let res = async {
            let bundle = collect_full_market_bundle(&self.client).await;
            let (saved_curves, saved_daily) = self.store.save_bundle(&bundle)?;
            let state = self.store.get_dashboard_state(
                false,
                self.backfilling.load(Ordering::Relaxed),
                AUTO_REFRESH_MIN,
            )?;
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
        res
    }

    async fn run_backfill(&self, days: u32) -> Result<BackfillSummary, String> {
        let Ok(_guard) = self.backfill_lock.try_lock() else {
            return Err("Historien-Backfill läuft bereits.".into());
        };
        self.backfilling.store(true, Ordering::Relaxed);
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
            let saved_days = self.store.save_backfill_days(&days_out)?;
            let state = self.store.get_dashboard_state(
                self.refreshing.load(Ordering::Relaxed),
                false,
                AUTO_REFRESH_MIN,
            )?;
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
            let handle = app.handle().clone();
            let state = Arc::clone(&bg_state);
            tauri::async_runtime::spawn(async move {
                let initial = state.current_state().ok();
                // Prüft explizit Kurvenabdeckung pro GPU (nicht nur Spot-Daten nach Legacy-Import)
                let need_live = initial
                    .as_ref()
                    .map(|s| !s.has_data || s.gpus.values().any(|g| g.expiries.is_empty()))
                    .unwrap_or(true);

                if need_live {
                    if let Ok(summary) = state.run_refresh().await {
                        let _ = handle.emit("compute:updated", &summary.state);
                    }
                }

                let after_live = state.current_state().ok();
                let need_backfill = after_live
                    .as_ref()
                    .map(|s| {
                        s.has_data
                            && (s.last_backfill_at.is_none()
                                || s.gpus
                                    .values()
                                    .any(|g| !g.expiries.is_empty() && g.backfill_snapshot_count == 0))
                    })
                    .unwrap_or(false);

                if need_backfill {
                    if let Ok(summary) = state.run_backfill(14).await {
                        let _ = handle.emit("compute:updated", &summary.state);
                    }
                }

                let mut ticker = tokio::time::interval(Duration::from_secs(AUTO_REFRESH_MIN * 60));
                ticker.tick().await;
                loop {
                    ticker.tick().await;
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
