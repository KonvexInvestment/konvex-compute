use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuConfig {
    pub key: String,
    pub label: String,
    pub full_name: String,
    pub series_slug: String,
    pub ornn_name: String,
    pub color: String,
    pub band_rgb: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotPoint {
    pub date: String,
    pub index_value: f64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotQuote {
    pub gpu_name: String,
    pub index_value: f64,
    pub last_updated: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrikeQuote {
    pub strike: f64,
    pub title: String,
    pub question: String,
    pub slug: String,
    pub prob_yes: f64,
    pub prob_pava: f64,
    pub pava_diff: f64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub last_trade: Option<f64>,
    pub spread: Option<f64>,
    pub one_day_change: Option<f64>,
    pub liquidity: f64,
    pub volume_24h: f64,
    pub volume_total: f64,
    pub token_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionBucket {
    pub label: String,
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub mid: f64,
    pub prob: f64,
    pub density: Option<f64>,
    pub is_tail: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiryCurvePoint {
    pub slug: String,
    pub title: String,
    pub url: String,
    pub end_date: Option<String>,
    pub delivery_date: String,
    pub short_label: String,
    pub long_label: String,
    pub month_label: String,
    pub liquidity: f64,
    pub volume_24h: f64,
    pub volume_total: f64,
    pub open_interest: f64,
    pub strike_count: usize,
    pub arbitrage_violations: usize,
    pub median: Option<f64>,
    /// Symmetrisch getrimmter Kern-Erwartungswert innerhalb beobachteter Strikes.
    #[serde(default)]
    pub mean_trimmed: Option<f64>,
    /// Effektives Quantilband des Kern-Erwartungswerts, z. B. "P10 bis P90".
    #[serde(default)]
    pub trimmed_band_label: Option<String>,
    /// Voller Integral-Erwartungswert E[X] inkl. oberem Tail.
    #[serde(alias = "mean")]
    pub mean_raw: Option<f64>,
    pub p10: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub p90: Option<f64>,
    pub iqr: Option<f64>,
    #[serde(default)]
    pub skew_spread: Option<f64>,
    #[serde(default)]
    pub skew_ratio: Option<f64>,
    pub vs_spot_abs: Option<f64>,
    pub vs_spot_pct: Option<f64>,
    #[serde(default)]
    pub prev_1d_median: Option<f64>,
    #[serde(default)]
    pub delta_1d_usd: Option<f64>,
    #[serde(default)]
    pub delta_1d_pct: Option<f64>,
    #[serde(default)]
    pub prev_7d_median: Option<f64>,
    #[serde(default)]
    pub delta_7d_usd: Option<f64>,
    #[serde(default)]
    pub delta_7d_pct: Option<f64>,
    #[serde(default)]
    pub strikes: Vec<StrikeQuote>,
    #[serde(default)]
    pub buckets: Vec<DistributionBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub id: i64,
    pub captured_at: String,
    pub snapshot_date: String,
    pub snapshot_kind: String,
    pub spot_price: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDetail {
    pub id: i64,
    pub captured_at: String,
    pub snapshot_date: String,
    pub snapshot_kind: String,
    pub gpu: String,
    pub spot_price: Option<f64>,
    pub total_liquidity: f64,
    pub total_volume_24h: f64,
    pub expiries: Vec<ExpiryCurvePoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonSlot {
    pub label: String,
    pub snapshot_id: Option<i64>,
    pub snapshot_date: Option<String>,
    pub captured_at: Option<String>,
    pub snapshot_kind: Option<String>,
    pub days_ago: Option<i64>,
    pub spot_price: Option<f64>,
    pub expiries: Vec<ExpiryCurvePoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuDashboard {
    pub meta: GpuConfig,
    pub spot_price: Option<f64>,
    pub spot_updated_at: Option<String>,
    pub spot_source: String,
    pub change_1d_pct: Option<f64>,
    pub change_7d_pct: Option<f64>,
    pub change_30d_pct: Option<f64>,
    pub change_90d_pct: Option<f64>,
    pub slope_to_back_pct: Option<f64>,
    pub front_median: Option<f64>,
    pub back_median: Option<f64>,
    pub spot_history: Vec<SpotPoint>,
    pub expiries: Vec<ExpiryCurvePoint>,
    pub total_liquidity: f64,
    pub total_volume_24h: f64,
    pub total_volume_total: f64,
    pub snapshot_id: Option<i64>,
    pub snapshot_captured_at: Option<String>,
    pub snapshot_count: i64,
    pub live_snapshot_count: i64,
    pub backfill_snapshot_count: i64,
    pub available_snapshots: Vec<SnapshotSummary>,
    pub compare_1d: Option<ComparisonSlot>,
    pub compare_7d: Option<ComparisonSlot>,
    pub compare_30d: Option<ComparisonSlot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraSpot {
    pub gpu_name: String,
    pub index_value: f64,
    pub last_updated: Option<String>,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStats {
    pub path: String,
    pub size_kb: f64,
    pub total_snapshots: i64,
    pub live_snapshots: i64,
    pub backfill_snapshots: i64,
    pub total_spot_days: i64,
    pub earliest_spot_date: Option<String>,
    pub earliest_curve_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardState {
    pub version: String,
    pub today_utc: String,
    pub last_captured_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_backfill_at: Option<String>,
    pub last_errors: Vec<String>,
    pub has_data: bool,
    pub refreshing: bool,
    pub backfilling: bool,
    pub interval_min: u64,
    pub gpu_order: Vec<String>,
    pub gpus: BTreeMap<String, GpuDashboard>,
    pub extra_spots: Vec<ExtraSpot>,
    pub db_stats: DbStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClobHistoryPoint {
    pub t: i64,
    pub p: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrikeHistoryResponse {
    pub token_id: String,
    pub days: u32,
    pub fidelity: u32,
    pub history: Vec<ClobHistoryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshSummary {
    pub ok: bool,
    pub captured_at: String,
    pub saved_curves: usize,
    pub saved_daily: usize,
    pub errors: Vec<String>,
    pub state: DashboardState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillSummary {
    pub ok: bool,
    pub saved_days: usize,
    pub gpus_processed: usize,
    pub strikes_fetched: usize,
    pub errors: Vec<String>,
    pub state: DashboardState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub ok: bool,
    pub path: String,
    pub rows: usize,
}

#[derive(Debug, Clone)]
pub struct FetchedGpuBundle {
    pub meta: GpuConfig,
    /// Echtes Live-Stundenquote von Ornn (None bei Fehler; niemals mit Vortageskurs belegt).
    pub spot: Option<SpotQuote>,
    /// Bester Referenzkurs für vs_spot-Berechnung (Live-Quote, sonst letzter Tagesabschluss).
    pub reference_spot_price: Option<f64>,
    pub ornn_history: Vec<SpotPoint>,
    pub expiries: Vec<ExpiryCurvePoint>,
    pub total_liquidity: f64,
    pub total_volume_24h: f64,
    pub total_volume_total: f64,
}

#[derive(Debug, Clone)]
pub struct FetchedMarketBundle {
    pub captured_at: String,
    pub errors: Vec<String>,
    pub gpus: BTreeMap<String, FetchedGpuBundle>,
    pub extra_spots: Vec<SpotQuote>,
}

#[derive(Debug, Clone)]
pub struct BackfillCurveDay {
    pub gpu: String,
    pub snapshot_date: String,
    pub captured_at: String,
    pub spot_price: Option<f64>,
    pub expiries: Vec<ExpiryCurvePoint>,
}
