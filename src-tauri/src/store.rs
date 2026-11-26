//! Lokaler SQLite Data Store (rusqlite, bundled) mit atomaren Transaktionen,
//! getrennter Tages-/Intraday-Spot-Haltung und nicht-überlappenden 1T/1W/1M-Vergleichen.

use crate::fetcher::gpu_catalog;
use crate::models::{
    BackfillCurveDay, ComparisonSlot, DashboardState, DbStats, ExpiryCurvePoint, ExtraSpot,
    FetchedMarketBundle, GpuDashboard, SnapshotDetail, SnapshotSummary, SpotPoint,
};
use chrono::{Duration, NaiveDate, Utc};
use directories::{ProjectDirs, UserDirs};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS spot_daily (
    gpu TEXT NOT NULL,
    date_iso TEXT NOT NULL,
    index_value REAL NOT NULL,
    source TEXT NOT NULL DEFAULT 'ornn',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (gpu, date_iso)
);

CREATE TABLE IF NOT EXISTS spot_intraday (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    gpu TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    index_value REAL NOT NULL,
    ornn_updated_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_spot_intraday_gpu_time
    ON spot_intraday (gpu, captured_at DESC);

CREATE TABLE IF NOT EXISTS curve_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    captured_at TEXT NOT NULL,
    snapshot_date TEXT NOT NULL,
    snapshot_kind TEXT NOT NULL DEFAULT 'live' CHECK (snapshot_kind IN ('live', 'daily_backfill')),
    gpu TEXT NOT NULL,
    spot_price REAL,
    total_liquidity REAL,
    total_volume_24h REAL,
    total_volume_total REAL,
    expiry_count INTEGER NOT NULL DEFAULT 0,
    expiries_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_curve_snapshots_gpu_time
    ON curve_snapshots (gpu, captured_at DESC);
CREATE INDEX IF NOT EXISTS idx_curve_snapshots_gpu_date_kind
    ON curve_snapshots (gpu, snapshot_date, snapshot_kind);

CREATE TABLE IF NOT EXISTS extra_spots (
    gpu_name TEXT PRIMARY KEY,
    index_value REAL NOT NULL,
    last_updated TEXT,
    captured_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS meta_kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Live-Snapshots bleiben so lange vollständig erhalten (alle 15 Min.), danach nur der letzte je Tag.
const LIVE_KEEP_ALL_HOURS: i64 = 48;

pub struct ComputeStore {
    pub db_path: PathBuf,
}

impl ComputeStore {
    pub fn default_db_path() -> PathBuf {
        if let Some(dirs) = ProjectDirs::from("de", "konvexinvestment", "compute") {
            return dirs.data_dir().join("compute.db");
        }
        PathBuf::from("compute.db")
    }

    pub fn open_default() -> Result<Self, String> {
        Self::open(Self::default_db_path())
    }

    pub fn open(db_path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("DB-Verzeichnis konnte nicht erstellt werden: {e}"))?;
        }
        let store = Self { db_path };
        store.init_schema()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("SQLite-Verbindung fehlgeschlagen: {e}"))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
        Ok(conn)
    }

    fn init_schema(&self) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("SQLite-Schema-Fehler: {e}"))?;
        Ok(())
    }

    pub fn save_bundle(&self, bundle: &FetchedMarketBundle) -> Result<(usize, usize), String> {
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("SQLite Transaktion fehlgeschlagen: {e}"))?;

        let captured_at = &bundle.captured_at;
        let snap_date = captured_at.get(..10).unwrap_or("1970-01-01");
        let mut saved_curves = 0usize;
        let mut saved_daily = 0usize;
        let mut saved_intraday = 0usize;

        for (gpu_key, gpu_data) in &bundle.gpus {
            // 1. Offizielle Ornn-Tagesabschlüsse sauber in spot_daily (nie mit Stundenwerten mischen)
            for row in &gpu_data.ornn_history {
                if !row.index_value.is_finite() || row.index_value <= 0.0 {
                    continue;
                }
                tx.execute(
                    "INSERT INTO spot_daily (gpu, date_iso, index_value, source, updated_at)
                     VALUES (?1, ?2, ?3, 'ornn', ?4)
                     ON CONFLICT(gpu, date_iso) DO UPDATE SET
                         index_value = excluded.index_value,
                         source = 'ornn',
                         updated_at = excluded.updated_at",
                    params![gpu_key, row.date, row.index_value, captured_at],
                )
                .map_err(|e| format!("spot_daily Upsert-Fehler: {e}"))?;
                saved_daily += 1;
            }

            // 2. Echte Live-Stundenwerte ausschließlich in spot_intraday (nie aus Vortags-Fallback!)
            if let Some(spot) = &gpu_data.spot {
                if spot.index_value.is_finite() && spot.index_value > 0.0 {
                    tx.execute(
                        "INSERT INTO spot_intraday (gpu, captured_at, index_value, ornn_updated_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![gpu_key, captured_at, spot.index_value, spot.last_updated],
                    )
                    .map_err(|e| format!("spot_intraday Insert-Fehler: {e}"))?;
                    saved_intraday += 1;
                }
            }

            // 3. Forward-Kurven-Snapshot speichern
            if !gpu_data.expiries.is_empty() {
                let json = serde_json::to_string(&gpu_data.expiries)
                    .map_err(|e| format!("JSON-Serialisierung fehlgeschlagen: {e}"))?;
                tx.execute(
                    "INSERT INTO curve_snapshots (
                        captured_at, snapshot_date, snapshot_kind, gpu, spot_price,
                        total_liquidity, total_volume_24h, total_volume_total, expiry_count, expiries_json
                    ) VALUES (?1, ?2, 'live', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        captured_at,
                        snap_date,
                        gpu_key,
                        gpu_data.reference_spot_price,
                        gpu_data.total_liquidity,
                        gpu_data.total_volume_24h,
                        gpu_data.total_volume_total,
                        gpu_data.expiries.len() as i64,
                        json
                    ],
                )
                .map_err(|e| format!("curve_snapshots Insert-Fehler: {e}"))?;
                saved_curves += 1;
            }
        }

        for sp in &bundle.extra_spots {
            if !sp.index_value.is_finite() || sp.index_value <= 0.0 {
                continue;
            }
            tx.execute(
                "INSERT INTO extra_spots (gpu_name, index_value, last_updated, captured_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(gpu_name) DO UPDATE SET
                     index_value = excluded.index_value,
                     last_updated = excluded.last_updated,
                     captured_at = excluded.captured_at",
                params![sp.gpu_name, sp.index_value, sp.last_updated, captured_at],
            )
            .map_err(|e| format!("extra_spots Upsert-Fehler: {e}"))?;
        }

        // Letzten Versuch immer festhalten, aber last_captured_at nur bei echten neuen Daten fortschreiben
        tx.execute(
            "INSERT INTO meta_kv (key, value) VALUES ('last_attempt_at', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![captured_at],
        )
        .map_err(|e| format!("meta_kv last_attempt_at Fehler: {e}"))?;

        if saved_curves > 0 || saved_daily > 0 || saved_intraday > 0 {
            tx.execute(
                "INSERT INTO meta_kv (key, value) VALUES ('last_captured_at', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![captured_at],
            )
            .map_err(|e| format!("meta_kv last_captured_at Fehler: {e}"))?;
        }

        let err_json = serde_json::to_string(&bundle.errors).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT INTO meta_kv (key, value) VALUES ('last_errors', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![err_json],
        )
        .map_err(|e| format!("meta_kv last_errors Fehler: {e}"))?;

        // Ohne Ausdünnen wächst die DB um ~28 MB pro Tag (4 GPUs x 96 Stände x ~75 KB).
        let reference = chrono::DateTime::parse_from_rfc3339(captured_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let cutoff = (reference - Duration::hours(LIVE_KEEP_ALL_HOURS))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        Self::prune_live_snapshots(&tx, &cutoff)?;

        tx.commit()
            .map_err(|e| format!("SQLite Commit fehlgeschlagen: {e}"))?;
        Ok((saved_curves, saved_daily))
    }

    /// Dünnt Live-Snapshots, die vor `cutoff` erfasst wurden, auf den letzten Stand je GPU
    /// und Kalendertag aus. Genau diesen Stand nutzen Tagesvergleiche und Snapshot-Auswahl,
    /// sichtbar geht also nichts verloren. Backfill-Tage bleiben unberührt.
    fn prune_live_snapshots(conn: &Connection, cutoff: &str) -> Result<usize, String> {
        conn.execute(
            "DELETE FROM curve_snapshots
             WHERE snapshot_kind = 'live'
               AND captured_at < ?1
               AND id NOT IN (
                   SELECT id FROM (
                       SELECT id, ROW_NUMBER() OVER (
                           PARTITION BY gpu, snapshot_date
                           ORDER BY captured_at DESC, id DESC
                       ) AS rn
                       FROM curve_snapshots
                       WHERE snapshot_kind = 'live'
                   )
                   WHERE rn = 1
               )",
            params![cutoff],
        )
        .map_err(|e| format!("Ausdünnen alter Live-Snapshots fehlgeschlagen: {e}"))
    }

    /// Speichert Backfill-Tage mit BEGIN IMMEDIATE und erhält bestehende Verfälle bei Teilabrufen.
    pub fn save_backfill_days(&self, days: &[BackfillCurveDay]) -> Result<usize, String> {
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("SQLite Backfill-Transaktion fehlgeschlagen: {e}"))?;
        let mut saved = 0usize;

        for day in days {
            // Live-Snapshots desselben Tages bleiben unberührt
            let has_live: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM curve_snapshots WHERE gpu = ?1 AND snapshot_date = ?2 AND snapshot_kind = 'live'",
                    params![day.gpu, day.snapshot_date],
                    |r| r.get(0),
                )
                .map_err(|e| format!("Backfill Live-Check Fehler: {e}"))?;
            if has_live > 0 {
                continue;
            }

            // Vorhandenen Backfill-Stand laden und pro Verfall-Slug zusammenführen,
            // sodass Teilfehler die bestehende Abdeckung niemals verschlechtern
            let existing: Option<(i64, Option<f64>, String)> = tx
                .query_row(
                    "SELECT id, spot_price, expiries_json FROM curve_snapshots
                     WHERE gpu = ?1 AND snapshot_date = ?2 AND snapshot_kind = 'daily_backfill'
                     ORDER BY id DESC LIMIT 1",
                    params![day.gpu, day.snapshot_date],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| format!("Backfill Bestand lesen fehlgeschlagen: {e}"))?;

            let mut merged_by_slug: HashMap<String, ExpiryCurvePoint> = HashMap::new();
            let mut merged_spot = day.spot_price;

            if let Some((_, old_spot, old_json)) = &existing {
                if merged_spot.is_none() {
                    merged_spot = *old_spot;
                }
                if let Ok(old_exps) = serde_json::from_str::<Vec<ExpiryCurvePoint>>(old_json) {
                    for e in old_exps {
                        merged_by_slug.insert(e.slug.clone(), e);
                    }
                }
            }

            for e in &day.expiries {
                let replace = match merged_by_slug.get(&e.slug) {
                    Some(prev) => e.strike_count >= prev.strike_count,
                    None => true,
                };
                if replace {
                    merged_by_slug.insert(e.slug.clone(), e.clone());
                }
            }

            let mut merged_list: Vec<ExpiryCurvePoint> = merged_by_slug.into_values().collect();
            merged_list.sort_by(|a, b| a.delivery_date.cmp(&b.delivery_date));
            if merged_list.is_empty() {
                continue;
            }

            let json = serde_json::to_string(&merged_list)
                .map_err(|e| format!("Backfill JSON-Fehler: {e}"))?;

            if let Some((old_id, _, _)) = existing {
                tx.execute(
                    "UPDATE curve_snapshots
                     SET captured_at = ?1, spot_price = ?2, expiry_count = ?3, expiries_json = ?4
                     WHERE id = ?5",
                    params![
                        day.captured_at,
                        merged_spot,
                        merged_list.len() as i64,
                        json,
                        old_id
                    ],
                )
                .map_err(|e| format!("Backfill Update-Fehler: {e}"))?;
            } else {
                tx.execute(
                    "INSERT INTO curve_snapshots (
                        captured_at, snapshot_date, snapshot_kind, gpu, spot_price,
                        total_liquidity, total_volume_24h, total_volume_total, expiry_count, expiries_json
                    ) VALUES (?1, ?2, 'daily_backfill', ?3, ?4, 0, 0, 0, ?5, ?6)",
                    params![
                        day.captured_at,
                        day.snapshot_date,
                        day.gpu,
                        merged_spot,
                        merged_list.len() as i64,
                        json
                    ],
                )
                .map_err(|e| format!("Backfill Insert-Fehler: {e}"))?;
            }
            saved += 1;
        }

        let now_str = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        tx.execute(
            "INSERT INTO meta_kv (key, value) VALUES ('last_backfill_at', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![now_str],
        )
        .map_err(|e| format!("meta_kv last_backfill_at Fehler: {e}"))?;

        tx.commit()
            .map_err(|e| format!("Backfill Commit fehlgeschlagen: {e}"))?;
        Ok(saved)
    }

    /// Berechnet die Spot-Veränderung exakt über `days_back` Tage (ohne trügerischen Fallback auf älteste/jüngste Zeile).
    fn compute_period_change(
        daily: &[SpotPoint],
        current_spot: Option<f64>,
        anchor_date: NaiveDate,
        days_back: i64,
        max_stale_extra_days: i64,
    ) -> Option<f64> {
        let spot = current_spot?;
        if daily.is_empty() || spot <= 0.0 {
            return None;
        }
        let target_date = anchor_date - Duration::days(days_back);
        let min_date = target_date - Duration::days(max_stale_extra_days);
        let mut best: Option<&SpotPoint> = None;
        for row in daily {
            if let Ok(d) = NaiveDate::parse_from_str(&row.date, "%Y-%m-%d") {
                if d <= target_date && d >= min_date {
                    best = Some(row);
                } else if d > target_date {
                    break;
                }
            }
        }
        let base_row = best?;
        if base_row.index_value <= 0.0 {
            return None;
        }
        let pct = (spot / base_row.index_value - 1.0) * 100.0;
        Some((pct * 100.0).round() / 100.0)
    }

    /// Sucht einen Vergleichs-Snapshot im strikt nicht-überlappenden Horizont-Fenster.
    fn find_comparison_slot(
        conn: &Connection,
        gpu: &str,
        latest_date: NaiveDate,
        latest_id: Option<i64>,
        target_days: i64,
        min_days: i64,
        max_days: i64,
        label: &str,
    ) -> Option<ComparisonSlot> {
        let min_date = (latest_date - Duration::days(max_days))
            .format("%Y-%m-%d")
            .to_string();
        let max_date = (latest_date - Duration::days(min_days))
            .format("%Y-%m-%d")
            .to_string();

        // Bewusst ohne expiries_json: bei 15-Minuten-Snapshots liegen schnell Hunderte
        // Kandidaten im Fenster, das JSON wird nur für den Gewinner geladen.
        let mut stmt = conn
            .prepare(
                "SELECT id, snapshot_date, snapshot_kind
                 FROM curve_snapshots
                 WHERE gpu = ?1 AND snapshot_date >= ?2 AND snapshot_date <= ?3
                 ORDER BY snapshot_date DESC, CASE WHEN snapshot_kind = 'live' THEN 0 ELSE 1 END ASC, id DESC",
            )
            .ok()?;

        let rows = stmt
            .query_map(params![gpu, min_date, max_date], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .ok()?;

        let target_date = latest_date - Duration::days(target_days);
        // (Distanz zum Zieltag, Live-Vorrang, Snapshot-ID)
        let mut best: Option<(i64, i64, i64)> = None;

        for (id, snapshot_date, snapshot_kind) in rows.flatten() {
            if Some(id) == latest_id {
                continue;
            }
            let Ok(d) = NaiveDate::parse_from_str(&snapshot_date, "%Y-%m-%d") else {
                continue;
            };
            let dist = (d - target_date).num_days().abs();
            let kind_penalty = if snapshot_kind == "live" { 0 } else { 1 };
            match &best {
                Some(prev) if (prev.0, prev.1) <= (dist, kind_penalty) => {}
                _ => best = Some((dist, kind_penalty, id)),
            }
        }

        let (_, _, best_id) = best?;
        let (id, captured_at, snapshot_date, snapshot_kind, spot_price, json_str) = conn
            .query_row(
                "SELECT id, captured_at, snapshot_date, snapshot_kind, spot_price, expiries_json
                 FROM curve_snapshots WHERE id = ?1",
                params![best_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<f64>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .ok()?;
        let d = NaiveDate::parse_from_str(&snapshot_date, "%Y-%m-%d").ok()?;
        let days_ago = (latest_date - d).num_days();
        let expiries: Vec<ExpiryCurvePoint> = serde_json::from_str(&json_str).unwrap_or_default();

        Some(ComparisonSlot {
            label: label.to_string(),
            snapshot_id: Some(id),
            snapshot_date: Some(snapshot_date),
            captured_at: Some(captured_at),
            snapshot_kind: Some(snapshot_kind),
            days_ago: Some(days_ago),
            spot_price,
            expiries,
        })
    }

    /// Liefert eine kombinierte Liste aus 1 repräsentativem Snapshot je Tag (letzte 60 Tage)
    /// plus den letzten 16 Intraday-Snapshots, damit Intraday-Polling keine älteren Tage verdrängt.
    fn list_selectable_snapshots(conn: &Connection, gpu: &str) -> Vec<SnapshotSummary> {
        let mut by_id: HashMap<i64, SnapshotSummary> = HashMap::new();

        // 1. Neueste 16 Intraday-Stände
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, captured_at, snapshot_date, snapshot_kind, spot_price
             FROM curve_snapshots WHERE gpu = ?1
             ORDER BY captured_at DESC, id DESC LIMIT 16",
        ) {
            if let Ok(rows) = stmt.query_map(params![gpu], |r| {
                Ok(SnapshotSummary {
                    id: r.get(0)?,
                    captured_at: r.get(1)?,
                    snapshot_date: r.get(2)?,
                    snapshot_kind: r.get(3)?,
                    spot_price: r.get(4)?,
                })
            }) {
                for s in rows.flatten() {
                    by_id.insert(s.id, s);
                }
            }
        }

        // 2. Je Kalendertag ein repräsentativer Snapshot (Live bevorzugt, sonst Backfill)
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, captured_at, snapshot_date, snapshot_kind, spot_price
             FROM (
                 SELECT id, captured_at, snapshot_date, snapshot_kind, spot_price,
                        ROW_NUMBER() OVER (
                            PARTITION BY snapshot_date
                            ORDER BY CASE WHEN snapshot_kind = 'live' THEN 0 ELSE 1 END ASC, captured_at DESC, id DESC
                        ) AS rn
                 FROM curve_snapshots
                 WHERE gpu = ?1
             )
             WHERE rn = 1
             ORDER BY snapshot_date DESC
             LIMIT 60",
        ) {
            if let Ok(rows) = stmt.query_map(params![gpu], |r| {
                Ok(SnapshotSummary {
                    id: r.get(0)?,
                    captured_at: r.get(1)?,
                    snapshot_date: r.get(2)?,
                    snapshot_kind: r.get(3)?,
                    spot_price: r.get(4)?,
                })
            }) {
                for s in rows.flatten() {
                    by_id.insert(s.id, s);
                }
            }
        }

        let mut list: Vec<SnapshotSummary> = by_id.into_values().collect();
        list.sort_by(|a, b| {
            b.snapshot_date
                .cmp(&a.snapshot_date)
                .then_with(|| b.captured_at.cmp(&a.captured_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        list
    }

    pub fn get_dashboard_state(
        &self,
        refreshing: bool,
        backfilling: bool,
        interval_min: u64,
    ) -> Result<DashboardState, String> {
        let mut conn = self.connect()?;
        // Gemeinsame Lesetransaktion für konsistenten Zustand über alle Tabellen
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|e| format!("Lesetransaktion fehlgeschlagen: {e}"))?;

        let mut kv: HashMap<String, String> = HashMap::new();
        if let Ok(mut stmt) = tx.prepare("SELECT key, value FROM meta_kv") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for (k, v) in rows.flatten() {
                    kv.insert(k, v);
                }
            }
        }

        let last_captured_at = kv.get("last_captured_at").cloned();
        let last_attempt_at = kv.get("last_attempt_at").cloned();
        let last_backfill_at = kv.get("last_backfill_at").cloned();
        let last_errors: Vec<String> = kv
            .get("last_errors")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        let catalog = gpu_catalog();
        let gpu_order: Vec<String> = catalog.iter().map(|c| c.key.clone()).collect();
        let mut gpus_out = BTreeMap::new();
        let mut total_snapshots = 0i64;
        let mut live_snapshots = 0i64;
        let mut backfill_snapshots = 0i64;
        let mut total_spot_days = 0i64;
        let today_utc_date = Utc::now().date_naive();

        for cfg in catalog {
            let gpu_key = cfg.key.clone();

            let mut spot_history = Vec::new();
            if let Ok(mut stmt) = tx.prepare(
                "SELECT date_iso, index_value, source FROM spot_daily WHERE gpu = ?1 ORDER BY date_iso ASC",
            ) {
                if let Ok(rows) = stmt.query_map(params![&gpu_key], |r| {
                    Ok(SpotPoint {
                        date: r.get(0)?,
                        index_value: ((r.get::<_, f64>(1)? * 10_000.0).round()) / 10_000.0,
                        source: r.get(2)?,
                    })
                }) {
                    spot_history.extend(rows.flatten());
                }
            }
            total_spot_days += spot_history.len() as i64;

            let latest_snap = tx
                .query_row(
                    "SELECT id, captured_at, snapshot_date, spot_price, total_liquidity, total_volume_24h, total_volume_total, expiries_json
                     FROM curve_snapshots
                     WHERE gpu = ?1
                     ORDER BY CASE WHEN snapshot_kind = 'live' THEN 0 ELSE 1 END ASC, captured_at DESC, id DESC
                     LIMIT 1",
                    params![&gpu_key],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<f64>>(3)?,
                            r.get::<_, Option<f64>>(4)?,
                            r.get::<_, Option<f64>>(5)?,
                            r.get::<_, Option<f64>>(6)?,
                            r.get::<_, String>(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("Snapshot-Lesefehler: {e}"))?;

            let available_snapshots = Self::list_selectable_snapshots(&tx, &gpu_key);

            let snap_count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM curve_snapshots WHERE gpu = ?1",
                    params![&gpu_key],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let live_count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM curve_snapshots WHERE gpu = ?1 AND snapshot_kind = 'live'",
                    params![&gpu_key],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let bf_count = (snap_count - live_count).max(0);

            total_snapshots += snap_count;
            live_snapshots += live_count;
            backfill_snapshots += bf_count;

            let latest_intraday = tx
                .query_row(
                    "SELECT index_value, ornn_updated_at, captured_at FROM spot_intraday WHERE gpu = ?1 ORDER BY id DESC LIMIT 1",
                    params![&gpu_key],
                    |r| {
                        Ok((
                            r.get::<_, f64>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .unwrap_or(None);

            let (snap_id, snap_captured_at, snap_date_str, snap_spot, total_liq, total_vol24, total_vol, mut expiries) =
                match latest_snap {
                    Some((id, cap, sdate, sp, liq, v24, vtot, json_str)) => {
                        let exps: Vec<ExpiryCurvePoint> =
                            serde_json::from_str(&json_str).unwrap_or_default();
                        (
                            Some(id),
                            Some(cap),
                            Some(sdate),
                            sp,
                            liq.unwrap_or(0.0),
                            v24.unwrap_or(0.0),
                            vtot.unwrap_or(0.0),
                            exps,
                        )
                    }
                    None => (None, None, None, None, 0.0, 0.0, 0.0, Vec::new()),
                };

            // Neuere Quelle gewinnt: Intraday nur bevorzugen, wenn sein Datum >= letztem Tagesabschluss ist
            let intraday_candidate = latest_intraday.map(|(val, upd, cap)| {
                let ts = upd.unwrap_or(cap);
                let d = ts
                    .get(..10)
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    .unwrap_or(today_utc_date);
                (val, ts, d)
            });
            let daily_candidate = spot_history.last().map(|last_daily| {
                let d = NaiveDate::parse_from_str(&last_daily.date, "%Y-%m-%d")
                    .unwrap_or(today_utc_date);
                (last_daily.index_value, last_daily.date.clone(), d)
            });

            let (spot_price, spot_updated_at, spot_source, spot_anchor_date) =
                match (intraday_candidate, daily_candidate) {
                    (Some((iv, its, id)), Some((dv, dts, dd))) => {
                        if id >= dd {
                            (Some((iv * 1000.0).round() / 1000.0), Some(its), "ornn-live".into(), id)
                        } else {
                            (Some((dv * 1000.0).round() / 1000.0), Some(dts), "ornn-daily".into(), dd)
                        }
                    }
                    (Some((iv, its, id)), None) => {
                        (Some((iv * 1000.0).round() / 1000.0), Some(its), "ornn-live".into(), id)
                    }
                    (None, Some((dv, dts, dd))) => {
                        (Some((dv * 1000.0).round() / 1000.0), Some(dts), "ornn-daily".into(), dd)
                    }
                    (None, None) => (
                        snap_spot.map(|v| (v * 1000.0).round() / 1000.0),
                        None,
                        "none".into(),
                        today_utc_date,
                    ),
                };

            let latest_date = snap_date_str
                .as_deref()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                .unwrap_or(today_utc_date);

            // Strikt nicht-überlappende Vergleichshorizonte:
            // 1T: 1..2 Tage, 1W: 5..10 Tage, 1M: 24..38 Tage
            let compare_1d = Self::find_comparison_slot(
                &tx, &gpu_key, latest_date, snap_id, 1, 1, 2, "Gestern (1T)",
            );
            let compare_7d = Self::find_comparison_slot(
                &tx, &gpu_key, latest_date, snap_id, 7, 5, 10, "Vor 7 Tagen (1W)",
            );
            let compare_30d = Self::find_comparison_slot(
                &tx, &gpu_key, latest_date, snap_id, 30, 24, 38, "Vor 30 Tagen (1M)",
            );

            let map_1d: HashMap<&str, f64> = compare_1d
                .as_ref()
                .map(|c| {
                    c.expiries
                        .iter()
                        .filter_map(|e| e.median.map(|m| (e.slug.as_str(), m)))
                        .collect()
                })
                .unwrap_or_default();

            let map_7d: HashMap<&str, f64> = compare_7d
                .as_ref()
                .map(|c| {
                    c.expiries
                        .iter()
                        .filter_map(|e| e.median.map(|m| (e.slug.as_str(), m)))
                        .collect()
                })
                .unwrap_or_default();

            for exp in &mut expiries {
                if let Some(med) = exp.median {
                    if let Some(&prev1) = map_1d.get(exp.slug.as_str()) {
                        exp.prev_1d_median = Some(prev1);
                        exp.delta_1d_usd = Some(((med - prev1) * 1000.0).round() / 1000.0);
                        if prev1 > 0.0 {
                            exp.delta_1d_pct =
                                Some((((med / prev1 - 1.0) * 100.0) * 100.0).round() / 100.0);
                        }
                    }
                    if let Some(&prev7) = map_7d.get(exp.slug.as_str()) {
                        exp.prev_7d_median = Some(prev7);
                        exp.delta_7d_usd = Some(((med - prev7) * 1000.0).round() / 1000.0);
                        if prev7 > 0.0 {
                            exp.delta_7d_pct =
                                Some((((med / prev7 - 1.0) * 100.0) * 100.0).round() / 100.0);
                        }
                    }
                }
            }

            let change_1d_pct = Self::compute_period_change(&spot_history, spot_price, spot_anchor_date, 1, 2);
            let change_7d_pct = Self::compute_period_change(&spot_history, spot_price, spot_anchor_date, 7, 3);
            let change_30d_pct = Self::compute_period_change(&spot_history, spot_price, spot_anchor_date, 30, 5);
            let change_90d_pct = Self::compute_period_change(&spot_history, spot_price, spot_anchor_date, 90, 7);

            let front_median = expiries.first().and_then(|e| e.median);
            let back_median = expiries.last().and_then(|e| e.median);
            let slope_to_back_pct = match (back_median, spot_price) {
                (Some(back), Some(sp)) if sp > 0.0 => {
                    Some((((back / sp - 1.0) * 100.0) * 100.0).round() / 100.0)
                }
                _ => None,
            };

            gpus_out.insert(
                gpu_key,
                GpuDashboard {
                    meta: cfg,
                    spot_price,
                    spot_updated_at,
                    spot_source,
                    change_1d_pct,
                    change_7d_pct,
                    change_30d_pct,
                    change_90d_pct,
                    slope_to_back_pct,
                    front_median,
                    back_median,
                    spot_history,
                    expiries,
                    total_liquidity: total_liq,
                    total_volume_24h: total_vol24,
                    total_volume_total: total_vol,
                    snapshot_id: snap_id,
                    snapshot_captured_at: snap_captured_at,
                    snapshot_count: snap_count,
                    live_snapshot_count: live_count,
                    backfill_snapshot_count: bf_count,
                    available_snapshots,
                    compare_1d,
                    compare_7d,
                    compare_30d,
                },
            );
        }

        let mut extra_spots = Vec::new();
        if let Ok(mut stmt) = tx.prepare(
            "SELECT gpu_name, index_value, last_updated, captured_at FROM extra_spots ORDER BY gpu_name ASC",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok(ExtraSpot {
                    gpu_name: r.get(0)?,
                    index_value: ((r.get::<_, f64>(1)? * 1000.0).round()) / 1000.0,
                    last_updated: r.get(2)?,
                    captured_at: r.get(3)?,
                })
            }) {
                extra_spots.extend(rows.flatten());
            }
        }

        let earliest_spot_date: Option<String> = tx
            .query_row("SELECT MIN(date_iso) FROM spot_daily", [], |r| r.get(0))
            .optional()
            .unwrap_or(None)
            .flatten();
        let earliest_curve_date: Option<String> = tx
            .query_row("SELECT MIN(snapshot_date) FROM curve_snapshots", [], |r| r.get(0))
            .optional()
            .unwrap_or(None)
            .flatten();

        let _ = tx.finish();

        let size_kb = fs::metadata(&self.db_path)
            .map(|m| ((m.len() as f64 / 1024.0) * 10.0).round() / 10.0)
            .unwrap_or(0.0);

        let has_data = gpus_out.values().any(|g| !g.expiries.is_empty());

        Ok(DashboardState {
            version: env!("CARGO_PKG_VERSION").to_string(),
            today_utc: today_utc_date.format("%Y-%m-%d").to_string(),
            last_captured_at,
            last_attempt_at,
            last_backfill_at,
            last_errors,
            has_data,
            refreshing,
            backfilling,
            interval_min,
            next_refresh_at: None,
            gpu_order,
            gpus: gpus_out,
            extra_spots,
            db_stats: DbStats {
                path: self.db_path.to_string_lossy().to_string(),
                size_kb,
                total_snapshots,
                live_snapshots,
                backfill_snapshots,
                total_spot_days,
                earliest_spot_date,
                earliest_curve_date,
            },
        })
    }

    pub fn get_snapshot(&self, snapshot_id: i64) -> Result<Option<SnapshotDetail>, String> {
        let conn = self.connect()?;
        let row = conn
            .query_row(
                "SELECT id, captured_at, snapshot_date, snapshot_kind, gpu, spot_price, total_liquidity, total_volume_24h, expiries_json
                 FROM curve_snapshots WHERE id = ?1",
                params![snapshot_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<f64>>(5)?,
                        r.get::<_, Option<f64>>(6)?,
                        r.get::<_, Option<f64>>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("Snapshot-Lesefehler: {e}"))?;

        Ok(row.map(|(id, cap, sdate, skind, gpu, sp, liq, v24, json_str)| SnapshotDetail {
            id,
            captured_at: cap,
            snapshot_date: sdate,
            snapshot_kind: skind,
            gpu,
            spot_price: sp,
            total_liquidity: liq.unwrap_or(0.0),
            total_volume_24h: v24.unwrap_or(0.0),
            expiries: serde_json::from_str(&json_str).unwrap_or_default(),
        }))
    }

    fn csv_escape(field: &str) -> String {
        if field.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", field.replace('"', "\"\""))
        } else {
            field.to_string()
        }
    }

    pub fn export_csv_to_downloads(&self, gpu_key: &str) -> Result<(String, usize), String> {
        if !matches!(gpu_key, "H100" | "H200" | "B200" | "B300") {
            return Err(format!("Unbekannte GPU: {gpu_key}"));
        }
        let state = self.get_dashboard_state(false, false, 15)?;
        let gpu = state
            .gpus
            .get(gpu_key)
            .ok_or_else(|| format!("Keine Daten für {gpu_key}"))?;

        let mut lines = Vec::new();
        lines.push(
            "section,gpu,date,label,median_usd,mean_trimmed_usd,mean_raw_usd,p10_usd,p25_usd,p75_usd,p90_usd,vs_spot_pct,delta_1d_usd,delta_7d_usd,liquidity_usd,volume_24h_usd".to_string(),
        );

        for row in &gpu.spot_history {
            let fields = [
                "spot_history".to_string(),
                gpu_key.to_string(),
                row.date.clone(),
                "Ornn Spot".to_string(),
                format!("{}", row.index_value),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ];
            lines.push(fields.iter().map(|f| Self::csv_escape(f)).collect::<Vec<_>>().join(","));
        }

        let fmt_opt = |v: Option<f64>| v.map(|x| format!("{x}")).unwrap_or_default();
        for e in &gpu.expiries {
            let fields = [
                "forward_curve".to_string(),
                gpu_key.to_string(),
                e.delivery_date.clone(),
                e.short_label.clone(),
                fmt_opt(e.median),
                fmt_opt(e.mean_trimmed),
                fmt_opt(e.mean_raw),
                fmt_opt(e.p10),
                fmt_opt(e.p25),
                fmt_opt(e.p75),
                fmt_opt(e.p90),
                fmt_opt(e.vs_spot_pct),
                fmt_opt(e.delta_1d_usd),
                fmt_opt(e.delta_7d_usd),
                format!("{}", e.liquidity),
                format!("{}", e.volume_24h),
            ];
            lines.push(fields.iter().map(|f| Self::csv_escape(f)).collect::<Vec<_>>().join(","));
        }

        let dir = UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| {
                self.db_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            });
        fs::create_dir_all(&dir).map_err(|e| format!("Export-Ordner nicht erstellbar: {e}"))?;

        let base = format!(
            "konvex-compute-{}-{}",
            gpu_key.to_lowercase(),
            Utc::now().format("%Y%m%d-%H%M%S")
        );
        let row_count = lines.len().saturating_sub(1);
        let payload = lines.join("\n");

        // Exklusives Erstellen (create_new) verhindert Überschreiben und Symlink-Verfolgung
        for suffix in 0..100u32 {
            let fname = if suffix == 0 {
                format!("{base}.csv")
            } else {
                format!("{base}-{suffix}.csv")
            };
            let candidate = dir.join(fname);
            match OpenOptions::new().write(true).create_new(true).open(&candidate) {
                Ok(mut fh) => {
                    fh.write_all(payload.as_bytes())
                        .map_err(|e| format!("CSV konnte nicht geschrieben werden: {e}"))?;
                    return Ok((candidate.to_string_lossy().to_string(), row_count));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("CSV-Datei konnte nicht angelegt werden: {e}")),
            }
        }
        Err("Kein freier Dateiname für CSV-Export verfügbar.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{FetchedGpuBundle, SpotQuote};

    fn make_exp(slug: &str, med: f64, strikes: usize) -> ExpiryCurvePoint {
        ExpiryCurvePoint {
            slug: slug.into(),
            title: slug.into(),
            url: format!("https://polymarket.com/event/{slug}"),
            end_date: Some("2027-07-01T04:59:00Z".into()),
            delivery_date: "2027-06-30".into(),
            short_label: "Juni 27".into(),
            long_label: "30. Juni 2027".into(),
            month_label: "Juni 2027".into(),
            liquidity: 500_000.0,
            volume_24h: 80_000.0,
            volume_total: 200_000.0,
            open_interest: 50_000.0,
            strike_count: strikes,
            arbitrage_violations: 0,
            median: Some(med),
            mean_trimmed: Some(med + 0.05),
            trimmed_band_label: Some("P10 bis P90".into()),
            mean_raw: Some(med + 0.30),
            p10: Some(1.5),
            p25: Some(2.2),
            p75: Some(3.8),
            p90: Some(5.0),
            iqr: Some(1.6),
            skew_spread: Some(0.3),
            skew_ratio: Some(1.5),
            vs_spot_abs: Some(med - 3.27),
            vs_spot_pct: Some((med / 3.27 - 1.0) * 100.0),
            prev_1d_median: None,
            delta_1d_usd: None,
            delta_1d_pct: None,
            prev_7d_median: None,
            delta_7d_usd: None,
            delta_7d_pct: None,
            strikes: Vec::new(),
            buckets: Vec::new(),
        }
    }

    #[test]
    fn non_overlapping_horizons_and_backfill_merge() {
        let tmp = std::env::temp_dir().join(format!(
            "konvex-compute-test-r2-{}.db",
            std::process::id()
        ));
        let _ = fs::remove_file(&tmp);
        let store = ComputeStore::open(tmp.clone()).unwrap();

        let cfg = gpu_catalog().into_iter().next().unwrap();

        // Ein 3 Tage alter Snapshot darf weder 1T (1..2T) noch 1W (5..10T) belegen!
        store
            .save_backfill_days(&[
                BackfillCurveDay {
                    gpu: "H100".into(),
                    snapshot_date: "2026-11-23".into(),
                    captured_at: "2026-11-23T23:59:59Z".into(),
                    spot_price: Some(3.00),
                    expiries: vec![make_exp("h100-june-2027", 2.65, 10)],
                },
                BackfillCurveDay {
                    gpu: "H100".into(),
                    snapshot_date: "2026-11-25".into(),
                    captured_at: "2026-11-25T23:59:59Z".into(),
                    spot_price: Some(3.10),
                    expiries: vec![
                        make_exp("h100-june-2027", 2.70, 10),
                        make_exp("h100-dec-2027", 2.60, 10),
                    ],
                },
                BackfillCurveDay {
                    gpu: "H100".into(),
                    snapshot_date: "2026-11-19".into(),
                    captured_at: "2026-11-19T23:59:59Z".into(),
                    spot_price: Some(2.78),
                    expiries: vec![make_exp("h100-june-2027", 2.50, 10)],
                },
            ])
            .unwrap();

        // Teil-Backfill mit nur 1 Verfall darf den zweiten Verfall am 25.11. NICHT löschen
        store
            .save_backfill_days(&[BackfillCurveDay {
                gpu: "H100".into(),
                snapshot_date: "2026-11-25".into(),
                captured_at: "2026-11-25T23:59:59Z".into(),
                spot_price: None,
                expiries: vec![make_exp("h100-june-2027", 2.72, 12)],
            }])
            .unwrap();

        let mut gpus = BTreeMap::new();
        gpus.insert(
            "H100".into(),
            FetchedGpuBundle {
                meta: cfg,
                spot: Some(SpotQuote {
                    gpu_name: "H100 SXM".into(),
                    index_value: 3.27,
                    last_updated: Some("2026-11-26T11:00:00Z".into()),
                }),
                reference_spot_price: Some(3.27),
                ornn_history: vec![
                    SpotPoint { date: "2026-11-19".into(), index_value: 2.78, source: "ornn".into() },
                    SpotPoint { date: "2026-11-25".into(), index_value: 3.10, source: "ornn".into() },
                ],
                expiries: vec![make_exp("h100-june-2027", 2.85, 12)],
                total_liquidity: 500_000.0,
                total_volume_24h: 80_000.0,
                total_volume_total: 200_000.0,
            },
        );
        store
            .save_bundle(&FetchedMarketBundle {
                captured_at: "2026-11-26T12:00:00Z".into(),
                errors: Vec::new(),
                gpus,
                extra_spots: Vec::new(),
            })
            .unwrap();

        let state = store.get_dashboard_state(false, false, 15).unwrap();
        let h100 = state.gpus.get("H100").unwrap();
        // spot_daily enthält NUR offizielle Tagesabschlüsse (2 Tage, kein überschriebener Live-Tag)
        assert_eq!(h100.spot_history.len(), 2);
        assert_eq!(h100.spot_source, "ornn-live");
        assert_eq!(h100.compare_1d.as_ref().unwrap().snapshot_date.as_deref(), Some("2026-11-25"));
        assert_eq!(h100.compare_1d.as_ref().unwrap().expiries.len(), 2);
        assert_eq!(h100.compare_7d.as_ref().unwrap().snapshot_date.as_deref(), Some("2026-11-19"));
        assert!((h100.expiries[0].delta_1d_usd.unwrap() - 0.13).abs() < 1e-3);
        assert!((h100.expiries[0].delta_7d_usd.unwrap() - 0.35).abs() < 1e-3);
        // 30T-Änderung ist None (keine 30 Tage Historie vorhanden, kein falscher Fallback)
        assert_eq!(h100.change_30d_pct, None);

        let _ = fs::remove_file(&tmp);
    }

    fn live_bundle(captured_at: &str, med: f64) -> FetchedMarketBundle {
        let mut gpus = BTreeMap::new();
        gpus.insert(
            "H100".into(),
            FetchedGpuBundle {
                meta: gpu_catalog().into_iter().next().unwrap(),
                spot: None,
                reference_spot_price: Some(3.0),
                ornn_history: Vec::new(),
                expiries: vec![make_exp("h100-june-2027", med, 10)],
                total_liquidity: 1.0,
                total_volume_24h: 1.0,
                total_volume_total: 1.0,
            },
        );
        FetchedMarketBundle {
            captured_at: captured_at.into(),
            errors: Vec::new(),
            gpus,
            extra_spots: Vec::new(),
        }
    }

    #[test]
    fn old_live_snapshots_are_thinned_to_last_of_day() {
        let tmp = std::env::temp_dir().join(format!(
            "konvex-compute-test-prune-{}.db",
            std::process::id()
        ));
        let _ = fs::remove_file(&tmp);
        let store = ComputeStore::open(tmp.clone()).unwrap();

        // Alter Tag mit drei Ständen, jüngerer Tag (innerhalb 48h) mit zwei Ständen
        for (ts, med) in [
            ("2026-11-20T08:00:00Z", 2.60),
            ("2026-11-20T12:00:00Z", 2.61),
            ("2026-11-20T23:45:00Z", 2.62),
            ("2026-11-25T09:00:00Z", 2.70),
            ("2026-11-25T09:15:00Z", 2.71),
        ] {
            store.save_bundle(&live_bundle(ts, med)).unwrap();
        }
        // Ein Backfill-Tag darf vom Ausdünnen nie betroffen sein
        store
            .save_backfill_days(&[BackfillCurveDay {
                gpu: "H100".into(),
                snapshot_date: "2026-11-19".into(),
                captured_at: "2026-11-19T23:59:59Z".into(),
                spot_price: Some(2.9),
                expiries: vec![make_exp("h100-june-2027", 2.55, 10)],
            }])
            .unwrap();
        // Auslöser: Abruf am 26.11. -> Grenze 24.11. 12:00
        store.save_bundle(&live_bundle("2026-11-26T12:00:00Z", 2.80)).unwrap();

        let conn = store.connect().unwrap();
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT captured_at, snapshot_kind FROM curve_snapshots ORDER BY captured_at")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        let stamps: Vec<&str> = rows.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            stamps,
            vec![
                "2026-11-19T23:59:59Z",
                "2026-11-20T23:45:00Z",
                "2026-11-25T09:00:00Z",
                "2026-11-25T09:15:00Z",
                "2026-11-26T12:00:00Z",
            ]
        );

        // Der behaltene Tagesstand ist genau der, den der Vergleich ohnehin gewählt hätte
        let state = store.get_dashboard_state(false, false, 15).unwrap();
        let h100 = state.gpus.get("H100").unwrap();
        assert_eq!(h100.compare_1d.as_ref().unwrap().captured_at.as_deref(), Some("2026-11-25T09:15:00Z"));

        drop(conn);
        let _ = fs::remove_file(&tmp);
    }
}
