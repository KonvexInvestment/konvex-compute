//! Asynchroner Datenabruf von Polymarket (Gamma + CLOB API) und Ornn (OCPI).

use crate::math::{analyze_expiry_strikes, RawStrikeInput};
use crate::models::{
    BackfillCurveDay, ClobHistoryPoint, ExpiryCurvePoint, FetchedGpuBundle, FetchedMarketBundle,
    GpuConfig, GpuDashboard, SpotPoint, SpotQuote,
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use reqwest::Client;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::Semaphore;

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const CLOB_BASE: &str = "https://clob.polymarket.com";
const ORNN_BASE: &str = "https://api.ornnai.com";
const GPU_PRICES_TAG_ID: &str = "106603";
const USER_AGENT: &str = "KonvexCompute/1.0 (+local-personal-analytics)";

pub const EXTRA_ORNN_GPUS: [&str; 3] = ["A100 SXM4", "RTX 5090", "RTX PRO 6000 WS"];

pub fn gpu_catalog() -> Vec<GpuConfig> {
    vec![
        GpuConfig {
            key: "H100".into(),
            label: "H100".into(),
            full_name: "NVIDIA H100 SXM".into(),
            series_slug: "h100-average-rental-price".into(),
            ornn_name: "H100 SXM".into(),
            color: "#38b6ff".into(),
            band_rgb: "56, 182, 255".into(),
        },
        GpuConfig {
            key: "H200".into(),
            label: "H200".into(),
            full_name: "NVIDIA H200".into(),
            series_slug: "h200-average-rental-price".into(),
            ornn_name: "H200".into(),
            color: "#a78bfa".into(),
            band_rgb: "167, 139, 250".into(),
        },
        GpuConfig {
            key: "B200".into(),
            label: "B200".into(),
            full_name: "NVIDIA B200".into(),
            series_slug: "b200-average-rental-price".into(),
            ornn_name: "B200".into(),
            color: "#f59e0b".into(),
            band_rgb: "245, 158, 11".into(),
        },
        GpuConfig {
            key: "B300".into(),
            label: "B300".into(),
            full_name: "NVIDIA B300".into(),
            series_slug: "b300-average-rental-price".into(),
            ornn_name: "B300".into(),
            color: "#30d158".into(),
            band_rgb: "48, 209, 88".into(),
        },
    ]
}

pub fn build_http_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(USER_AGENT)
        .gzip(true)
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .map_err(|e| format!("HTTP-Client konnte nicht initialisiert werden: {e}"))
}

fn month_short_de(month: u32) -> &'static str {
    match month {
        1 => "Jan.",
        2 => "Feb.",
        3 => "März",
        4 => "Apr.",
        5 => "Mai",
        6 => "Juni",
        7 => "Juli",
        8 => "Aug.",
        9 => "Sept.",
        10 => "Okt.",
        11 => "Nov.",
        12 => "Dez.",
        _ => "?",
    }
}

fn month_long_de(month: u32) -> &'static str {
    match month {
        1 => "Januar",
        2 => "Februar",
        3 => "März",
        4 => "April",
        5 => "Mai",
        6 => "Juni",
        7 => "Juli",
        8 => "August",
        9 => "September",
        10 => "Oktober",
        11 => "November",
        12 => "Dezember",
        _ => "?",
    }
}

fn month_en_to_num(s: &str) -> Option<u32> {
    match s {
        "january" => Some(1),
        "february" => Some(2),
        "march" => Some(3),
        "april" => Some(4),
        "may" => Some(5),
        "june" => Some(6),
        "july" => Some(7),
        "august" => Some(8),
        "september" => Some(9),
        "october" => Some(10),
        "november" => Some(11),
        "december" => Some(12),
        _ => None,
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    next.and_then(|d| d.pred_opt()).map(|d| d.day()).unwrap_or(28)
}

/// Parst RFC3339 oder YYYY-MM-DD UTF-8-sicher zu YYYY-MM-DD.
fn parse_iso_date_str(raw: &str) -> Option<String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.date_naive().format("%Y-%m-%d").to_string());
    }
    let prefix: String = raw.chars().take(10).collect();
    NaiveDate::parse_from_str(&prefix, "%Y-%m-%d")
        .ok()
        .map(|d| d.format("%Y-%m-%d").to_string())
}

struct ExpiryDateMeta {
    delivery_date: String,
    short_label: String,
    long_label: String,
    month_label: String,
}

fn parse_expiry_meta(slug: &str, end_date_str: Option<&str>) -> Option<ExpiryDateMeta> {
    let lower = slug.to_lowercase();
    if let Some(pos) = lower.find("-in-") {
        let rest = &lower[pos + 4..];
        let parts: Vec<&str> = rest.split('-').collect();
        if parts.len() >= 2 {
            if let (Some(m), Ok(y)) = (month_en_to_num(parts[0]), parts[1].parse::<i32>()) {
                if (2024..=2040).contains(&y) {
                    let last_day = days_in_month(y, m);
                    let delivery = format!("{y:04}-{m:02}-{last_day:02}");
                    let yr_short = format!("{:02}", y % 100);
                    return Some(ExpiryDateMeta {
                        delivery_date: delivery,
                        short_label: format!("{} {}", month_short_de(m), yr_short),
                        long_label: format!("{last_day}. {} {y}", month_long_de(m)),
                        month_label: format!("{} {y}", month_long_de(m)),
                    });
                }
            }
        }
    }

    if let Some(ed) = end_date_str {
        if let Some(iso) = parse_iso_date_str(ed) {
            if let Ok(d) = NaiveDate::parse_from_str(&iso, "%Y-%m-%d") {
                let prev = d - Duration::days(1);
                let y = prev.year();
                let m = prev.month();
                let day = prev.day();
                return Some(ExpiryDateMeta {
                    delivery_date: prev.format("%Y-%m-%d").to_string(),
                    short_label: format!("{} {:02}", month_short_de(m), y % 100),
                    long_label: format!("{day}. {} {y}", month_long_de(m)),
                    month_label: format!("{} {y}", month_long_de(m)),
                });
            }
        }
    }

    None
}

/// Parst die Dollar-Schwelle inkl. möglicher Tausenderkommas (z. B. "Above $1,200.50").
fn parse_strike_dollar(text: &str) -> Option<f64> {
    // Position nur im selben String verwenden, in dem gesucht wurde: `to_lowercase`
    // kann Byte-Längen ändern (z. B. "İ"), ein Slice in `text` könnte sonst panicen.
    let lower = text.to_lowercase();
    let search_slice = match lower.find("above") {
        Some(pos) => &lower[pos..],
        None => lower.as_str(),
    };
    let idx = search_slice.find('$')?;
    let after = &search_slice[idx + 1..];
    let raw: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    let cleaned = raw.replace(',', "");
    let val = cleaned.parse::<f64>().ok()?;
    if val.is_finite() && val > 0.0 {
        Some(val)
    } else {
        None
    }
}

fn json_as_finite_f64(val: Option<&Value>) -> Option<f64> {
    let v = val?;
    let f = if let Some(n) = v.as_f64() {
        n
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<f64>().ok()?
    } else {
        return None;
    };
    if f.is_finite() {
        Some(f)
    } else {
        None
    }
}

/// Ermittelt den Index des "Yes"-Outcomes aus `outcomes` (Fallback 0 nur falls Standard-Ja/Nein).
fn yes_outcome_index(market: &Value) -> Option<usize> {
    let outcomes = market.get("outcomes")?;
    let arr_val = if let Some(s) = outcomes.as_str() {
        serde_json::from_str::<Value>(s).ok()?
    } else {
        outcomes.clone()
    };
    let arr = arr_val.as_array()?;
    arr.iter().position(|v| {
        v.as_str()
            .map(|s| s.trim().eq_ignore_ascii_case("yes"))
            .unwrap_or(false)
    })
}

fn parse_prob_yes(market: &Value) -> Option<f64> {
    let yes_idx = yes_outcome_index(market).unwrap_or(0);
    if let Some(op) = market.get("outcomePrices") {
        let arr_val = if let Some(s) = op.as_str() {
            serde_json::from_str::<Value>(s).ok()
        } else {
            Some(op.clone())
        };
        if let Some(Value::Array(arr)) = arr_val {
            if let Some(v) = arr.get(yes_idx) {
                if let Some(p) = json_as_finite_f64(Some(v)) {
                    if (0.0..=1.0).contains(&p) {
                        return Some(p);
                    }
                }
            }
        }
    }
    if yes_idx == 0 {
        if let (Some(bid), Some(ask)) = (
            json_as_finite_f64(market.get("bestBid")),
            json_as_finite_f64(market.get("bestAsk")),
        ) {
            let mid = (bid + ask) * 0.5;
            if (0.0..=1.0).contains(&mid) {
                return Some(mid);
            }
        }
        if let Some(last) = json_as_finite_f64(market.get("lastTradePrice")) {
            if (0.0..=1.0).contains(&last) {
                return Some(last);
            }
        }
    }
    None
}

fn parse_yes_token_id(market: &Value) -> Option<String> {
    let yes_idx = yes_outcome_index(market).unwrap_or(0);
    let tokens = market.get("clobTokenIds")?;
    let arr_val = if let Some(s) = tokens.as_str() {
        serde_json::from_str::<Value>(s).ok()
    } else {
        Some(tokens.clone())
    };
    if let Some(Value::Array(arr)) = arr_val {
        if let Some(v) = arr.get(yes_idx) {
            return v.as_str().map(|s| s.to_string());
        }
    }
    None
}

fn encode_uri_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Führt einen GET-Request mit begrenztem Backoff bei 429/5xx aus.
async fn get_json_with_retry(client: &Client, url: &str) -> Result<Value, String> {
    let mut last_err = String::new();
    for attempt in 0..3u32 {
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp
                        .json::<Value>()
                        .await
                        .map_err(|e| format!("JSON-Fehler bei {url}: {e}"));
                }
                last_err = format!("HTTP {status} bei {url}");
                if status.as_u16() == 429 || status.is_server_error() {
                    tokio::time::sleep(std::time::Duration::from_millis(300 * (1u64 << attempt)))
                        .await;
                    continue;
                }
                return Err(last_err);
            }
            Err(e) => {
                last_err = format!("Netzwerkfehler bei {url}: {e}");
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(300 * (1u64 << attempt)))
                        .await;
                }
            }
        }
    }
    Err(last_err)
}

pub async fn fetch_polymarket_gpu_events(client: &Client) -> Result<Vec<Value>, String> {
    let mut all = Vec::new();
    let limit = 100usize;
    let mut offset = 0usize;

    loop {
        let url = format!(
            "{GAMMA_BASE}/events?tag_id={GPU_PRICES_TAG_ID}&active=true&closed=false&limit={limit}&offset={offset}"
        );
        let val = get_json_with_retry(client, &url).await?;
        let batch = val
            .as_array()
            .cloned()
            .ok_or_else(|| "Polymarket Gamma lieferte kein Event-Array.".to_string())?;
        let count = batch.len();
        all.extend(batch);
        if count < limit || offset >= 1000 {
            break;
        }
        offset += limit;
    }
    Ok(all)
}

pub async fn fetch_ornn_spot(client: &Client, gpu_name: &str) -> Result<SpotQuote, String> {
    let url = format!("{ORNN_BASE}/api/gpu/{}", encode_uri_segment(gpu_name));
    let val = get_json_with_retry(client, &url).await?;
    let data = val
        .get("data")
        .ok_or_else(|| format!("Ornn Spot ({gpu_name}): Feld data fehlt."))?;
    let index_value = json_as_finite_f64(data.get("index_value"))
        .filter(|v| *v > 0.0)
        .ok_or_else(|| format!("Ornn Spot ({gpu_name}): ungültiger index_value."))?;
    Ok(SpotQuote {
        gpu_name: data
            .get("gpu_name")
            .and_then(|v| v.as_str())
            .unwrap_or(gpu_name)
            .to_string(),
        index_value,
        last_updated: data
            .get("last_updated")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

pub async fn fetch_ornn_history(
    client: &Client,
    gpu_name: &str,
    days: i64,
) -> Result<Vec<SpotPoint>, String> {
    let end_d = Utc::now().date_naive();
    let start_d = end_d - Duration::days(days);
    let url = format!(
        "{ORNN_BASE}/api/gpu/{}/index-history?startDate={}&endDate={}",
        encode_uri_segment(gpu_name),
        start_d.format("%Y-%m-%d"),
        end_d.format("%Y-%m-%d")
    );
    let val = get_json_with_retry(client, &url).await?;
    let rows = val
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| format!("Ornn Historie ({gpu_name}): Feld data fehlt."))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let (Some(ts), Some(iv)) = (
            row.get("timestamp").and_then(|v| v.as_str()),
            json_as_finite_f64(row.get("index_value")),
        ) {
            if iv > 0.0 {
                if let Some(date_str) = parse_iso_date_str(ts) {
                    out.push(SpotPoint {
                        date: date_str,
                        index_value: iv,
                        source: "ornn".into(),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(out)
}

/// Die CLOB-API lehnt `startTs`/`endTs`-Fenster ab etwa 15 Tagen mit
/// "interval is too long" ab (unabhängig von `fidelity`). Längere Zeiträume
/// werden deshalb in Abschnitte dieser Länge zerlegt.
const CLOB_MAX_WINDOW_SECS: i64 = 14 * 86_400;

/// Zerlegt [start_ts, end_ts] in aufeinanderfolgende Fenster von höchstens `max_len` Sekunden.
fn split_time_windows(start_ts: i64, end_ts: i64, max_len: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if end_ts <= start_ts || max_len <= 0 {
        return out;
    }
    let mut cur = start_ts;
    while cur < end_ts {
        let next = (cur + max_len).min(end_ts);
        out.push((cur, next));
        cur = next;
    }
    out
}

async fn fetch_clob_price_history_window(
    client: &Client,
    token_id: &str,
    start_ts: i64,
    end_ts: i64,
    fidelity: u32,
) -> Result<Vec<ClobHistoryPoint>, String> {
    let url = format!(
        "{CLOB_BASE}/prices-history?market={}&startTs={start_ts}&endTs={end_ts}&fidelity={fidelity}",
        encode_uri_segment(token_id)
    );
    let val = get_json_with_retry(client, &url).await?;
    let arr = val
        .get("history")
        .and_then(|h| h.as_array())
        .ok_or_else(|| format!("CLOB Antwort ohne history-Array für Token {token_id}"))?;
    let mut points = Vec::with_capacity(arr.len());
    for item in arr {
        if let (Some(t), Some(p)) = (
            item.get("t").and_then(|v| v.as_i64()),
            json_as_finite_f64(item.get("p")),
        ) {
            if (0.0..=1.0).contains(&p) {
                points.push(ClobHistoryPoint { t, p });
            }
        }
    }
    Ok(points)
}

pub async fn fetch_clob_price_history_range(
    client: &Client,
    token_id: &str,
    start_ts: i64,
    end_ts: i64,
    fidelity: u32,
) -> Result<Vec<ClobHistoryPoint>, String> {
    let mut points = Vec::new();
    for (win_start, win_end) in split_time_windows(start_ts, end_ts, CLOB_MAX_WINDOW_SECS) {
        points.extend(
            fetch_clob_price_history_window(client, token_id, win_start, win_end, fidelity).await?,
        );
    }
    points.sort_by_key(|pt| pt.t);
    points.dedup_by_key(|pt| pt.t);
    Ok(points)
}

pub async fn fetch_clob_price_history(
    client: &Client,
    token_id: &str,
    days: u32,
    fidelity: u32,
) -> Result<Vec<ClobHistoryPoint>, String> {
    let end_ts = Utc::now().timestamp();
    let start_ts = end_ts - (days.max(1) as i64) * 86_400;
    fetch_clob_price_history_range(client, token_id, start_ts, end_ts, fidelity).await
}

pub async fn collect_full_market_bundle(client: &Client) -> FetchedMarketBundle {
    let captured_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut errors = Vec::new();

    let events = match fetch_polymarket_gpu_events(client).await {
        Ok(evs) => evs,
        Err(err) => {
            errors.push(err);
            Vec::new()
        }
    };

    let mut events_by_series: HashMap<String, Vec<Value>> = HashMap::new();
    for ev in events {
        let slug = ev
            .get("seriesSlug")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                ev.get("series")
                    .and_then(|s| s.as_array())
                    .and_then(|a| a.first())
                    .and_then(|first| first.get("slug"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        if let Some(s) = slug {
            events_by_series.entry(s).or_default().push(ev);
        }
    }

    let catalog = gpu_catalog();
    let mut gpus_out = BTreeMap::new();
    let today_iso = Utc::now().date_naive().format("%Y-%m-%d").to_string();

    for cfg in catalog {
        let (spot_res, hist_res) = tokio::join!(
            fetch_ornn_spot(client, &cfg.ornn_name),
            fetch_ornn_history(client, &cfg.ornn_name, 95)
        );
        let spot = match spot_res {
            Ok(s) => Some(s),
            Err(e) => {
                errors.push(e);
                None
            }
        };
        let history = match hist_res {
            Ok(h) => h,
            Err(e) => {
                errors.push(e);
                Vec::new()
            }
        };
        let reference_spot_price = spot
            .as_ref()
            .map(|s| s.index_value)
            .or_else(|| history.last().map(|h| h.index_value));

        let series_events = events_by_series.remove(&cfg.series_slug).unwrap_or_default();
        let mut expiries = Vec::with_capacity(series_events.len());
        let mut total_liq = 0.0;
        let mut total_vol24 = 0.0;
        let mut total_vol = 0.0;

        for ev in series_events {
            let slug = ev
                .get("slug")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let end_date = ev
                .get("endDate")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let Some(meta) = parse_expiry_meta(&slug, end_date.as_deref()) else {
                continue;
            };
            // Abgelaufene Liefermonate, deren Event noch auf Auflösung wartet, sind keine
            // Forward-Punkte mehr und würden sonst links von "Heute" im Chart landen.
            if meta.delivery_date < today_iso {
                continue;
            }

            let mut raw_strikes = Vec::new();
            if let Some(markets) = ev.get("markets").and_then(|m| m.as_array()) {
                for m in markets {
                    if m.get("closed").and_then(|v| v.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    let title = m
                        .get("groupItemTitle")
                        .and_then(|v| v.as_str())
                        .or_else(|| m.get("question").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let Some(strike) = parse_strike_dollar(title) else {
                        continue;
                    };
                    let Some(prob_yes) = parse_prob_yes(m) else {
                        continue;
                    };
                    raw_strikes.push(RawStrikeInput {
                        strike,
                        title: m
                            .get("groupItemTitle")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("Above ${strike:.2}")),
                        question: m
                            .get("question")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        slug: m
                            .get("slug")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        prob_yes,
                        best_bid: json_as_finite_f64(m.get("bestBid")),
                        best_ask: json_as_finite_f64(m.get("bestAsk")),
                        last_trade: json_as_finite_f64(m.get("lastTradePrice")),
                        spread: json_as_finite_f64(m.get("spread")),
                        one_day_change: json_as_finite_f64(m.get("oneDayPriceChange")),
                        liquidity: json_as_finite_f64(m.get("liquidityNum"))
                            .or_else(|| json_as_finite_f64(m.get("liquidity")))
                            .unwrap_or(0.0)
                            .max(0.0),
                        volume_24h: json_as_finite_f64(m.get("volume24hr"))
                            .unwrap_or(0.0)
                            .max(0.0),
                        volume_total: json_as_finite_f64(m.get("volumeNum"))
                            .or_else(|| json_as_finite_f64(m.get("volume")))
                            .unwrap_or(0.0)
                            .max(0.0),
                        token_id: parse_yes_token_id(m),
                    });
                }
            }

            let stats = analyze_expiry_strikes(raw_strikes, reference_spot_price);
            let ev_liq = json_as_finite_f64(ev.get("liquidity")).unwrap_or(0.0).max(0.0);
            let ev_vol24 = json_as_finite_f64(ev.get("volume24hr")).unwrap_or(0.0).max(0.0);
            let ev_vol = json_as_finite_f64(ev.get("volume")).unwrap_or(0.0).max(0.0);
            let ev_oi = json_as_finite_f64(ev.get("openInterest")).unwrap_or(0.0).max(0.0);
            total_liq += ev_liq;
            total_vol24 += ev_vol24;
            total_vol += ev_vol;

            expiries.push(ExpiryCurvePoint {
                url: format!("https://polymarket.com/event/{slug}"),
                title: ev
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&slug)
                    .to_string(),
                slug,
                end_date,
                delivery_date: meta.delivery_date,
                short_label: meta.short_label,
                long_label: meta.long_label,
                month_label: meta.month_label,
                liquidity: (ev_liq * 100.0).round() / 100.0,
                volume_24h: (ev_vol24 * 100.0).round() / 100.0,
                volume_total: (ev_vol * 100.0).round() / 100.0,
                open_interest: (ev_oi * 100.0).round() / 100.0,
                strike_count: stats.strike_count,
                arbitrage_violations: stats.arbitrage_violations,
                median: stats.median,
                mean_trimmed: stats.mean_trimmed,
                trimmed_band_label: stats.trimmed_band_label,
                mean_raw: stats.mean_raw,
                p10: stats.p10,
                p25: stats.p25,
                p75: stats.p75,
                p90: stats.p90,
                iqr: stats.iqr,
                skew_spread: stats.skew_spread,
                skew_ratio: stats.skew_ratio,
                vs_spot_abs: stats.vs_spot_abs,
                vs_spot_pct: stats.vs_spot_pct,
                prev_1d_median: None,
                delta_1d_usd: None,
                delta_1d_pct: None,
                prev_7d_median: None,
                delta_7d_usd: None,
                delta_7d_pct: None,
                strikes: stats.strikes,
                buckets: stats.buckets,
            });
        }

        expiries.sort_by(|a, b| a.delivery_date.cmp(&b.delivery_date));

        gpus_out.insert(
            cfg.key.clone(),
            FetchedGpuBundle {
                meta: cfg,
                spot,
                reference_spot_price,
                ornn_history: history,
                expiries,
                total_liquidity: (total_liq * 100.0).round() / 100.0,
                total_volume_24h: (total_vol24 * 100.0).round() / 100.0,
                total_volume_total: (total_vol * 100.0).round() / 100.0,
            },
        );
    }

    let mut extra_spots = Vec::new();
    for name in EXTRA_ORNN_GPUS {
        if let Ok(sp) = fetch_ornn_spot(client, name).await {
            extra_spots.push(sp);
        }
    }

    FetchedMarketBundle {
        captured_at,
        errors,
        gpus: gpus_out,
        extra_spots,
    }
}

/// Rekonstruiert historische Tages-Forward-Kurven der letzten `days` abgeschlossenen UTC-Tage
/// mit eingefrorenem Stichtag, vollem Anfragefenster, neutralen PAVA-Gewichten und 24h-Staleness-Grenze.
pub async fn reconstruct_daily_backfill(
    client: &Client,
    gpus: &BTreeMap<String, GpuDashboard>,
    days: u32,
) -> (Vec<BackfillCurveDay>, usize, Vec<String>) {
    // Stichtag einmal für den gesamten Lauf einfrieren (stabil über UTC-Mitternacht)
    let anchor_today = Utc::now().date_naive();
    let earliest_date = anchor_today - Duration::days(days as i64);
    let req_start_ts = earliest_date
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    let req_end_ts = anchor_today
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or_else(|| Utc::now().timestamp());

    let mut token_ids: Vec<String> = Vec::new();
    for g in gpus.values() {
        for exp in &g.expiries {
            for s in &exp.strikes {
                if let Some(tid) = &s.token_id {
                    token_ids.push(tid.clone());
                }
            }
        }
    }
    token_ids.sort();
    token_ids.dedup();

    // Begrenzte Parallelität + sanftes Pacing (8 gleichzeitig)
    let sem = Arc::new(Semaphore::new(8));
    let mut handles = Vec::with_capacity(token_ids.len());
    for tid in token_ids {
        let client_c = client.clone();
        let sem_c = Arc::clone(&sem);
        handles.push(tokio::spawn(async move {
            let _permit = sem_c.acquire().await.ok()?;
            let res = fetch_clob_price_history_range(&client_c, &tid, req_start_ts, req_end_ts, 120).await;
            Some((tid, res))
        }));
    }

    let mut hist_by_token: HashMap<String, Vec<ClobHistoryPoint>> = HashMap::new();
    let mut fetched_ok = 0usize;
    let mut errors = Vec::new();

    for h in handles {
        if let Ok(Some((tid, res))) = h.await {
            match res {
                Ok(pts) => {
                    fetched_ok += 1;
                    if !pts.is_empty() {
                        hist_by_token.insert(tid, pts);
                    }
                }
                Err(e) => {
                    if errors.len() < 5 {
                        errors.push(e);
                    }
                }
            }
        }
    }

    let mut out_days = Vec::new();

    for (gpu_key, gpu) in gpus {
        let spot_by_date: HashMap<&str, f64> = gpu
            .spot_history
            .iter()
            .map(|p| (p.date.as_str(), p.index_value))
            .collect();

        for offset in 1..=(days as i64) {
            let target_date = anchor_today - Duration::days(offset);
            let date_str = target_date.format("%Y-%m-%d").to_string();
            let day_start_ts = target_date
                .and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc().timestamp())
                .unwrap_or(0);
            let day_end_ts = target_date
                .and_hms_opt(23, 59, 59)
                .map(|dt| dt.and_utc().timestamp())
                .unwrap_or(0);

            let spot_val = spot_by_date.get(date_str.as_str()).copied();
            let mut day_expiries = Vec::new();

            for exp in &gpu.expiries {
                let mut raw_strikes = Vec::new();
                for s in &exp.strikes {
                    let Some(tid) = &s.token_id else { continue };
                    let Some(pts) = hist_by_token.get(tid) else { continue };
                    // Nur Quotes vom selben UTC-Tag (max. 24h alt vor Tagesende)
                    let chosen = pts
                        .iter()
                        .rev()
                        .find(|pt| pt.t <= day_end_ts && pt.t >= day_start_ts);
                    if let Some(pt) = chosen {
                        raw_strikes.push(RawStrikeInput {
                            strike: s.strike,
                            title: s.title.clone(),
                            question: s.question.clone(),
                            slug: s.slug.clone(),
                            prob_yes: pt.p,
                            best_bid: None,
                            best_ask: None,
                            last_trade: Some(pt.p),
                            spread: None,
                            one_day_change: None,
                            // Neutrale PAVA-Gewichte für historische Rekonstruktion
                            liquidity: 0.0,
                            volume_24h: 0.0,
                            volume_total: 0.0,
                            token_id: Some(tid.clone()),
                        });
                    }
                }

                // Mindestabdeckung: mindestens 4 Strikes und mindestens 50 % der heutigen Strike-Leiter
                let min_needed = 4usize.max(exp.strikes.len().div_ceil(2));
                if raw_strikes.len() >= min_needed {
                    let stats = analyze_expiry_strikes(raw_strikes, spot_val);
                    if stats.median.is_some() {
                        day_expiries.push(ExpiryCurvePoint {
                            slug: exp.slug.clone(),
                            title: exp.title.clone(),
                            url: exp.url.clone(),
                            end_date: exp.end_date.clone(),
                            delivery_date: exp.delivery_date.clone(),
                            short_label: exp.short_label.clone(),
                            long_label: exp.long_label.clone(),
                            month_label: exp.month_label.clone(),
                            liquidity: 0.0,
                            volume_24h: 0.0,
                            volume_total: 0.0,
                            open_interest: 0.0,
                            strike_count: stats.strike_count,
                            arbitrage_violations: stats.arbitrage_violations,
                            median: stats.median,
                            mean_trimmed: stats.mean_trimmed,
                            trimmed_band_label: stats.trimmed_band_label,
                            mean_raw: stats.mean_raw,
                            p10: stats.p10,
                            p25: stats.p25,
                            p75: stats.p75,
                            p90: stats.p90,
                            iqr: stats.iqr,
                            skew_spread: stats.skew_spread,
                            skew_ratio: stats.skew_ratio,
                            vs_spot_abs: stats.vs_spot_abs,
                            vs_spot_pct: stats.vs_spot_pct,
                            prev_1d_median: None,
                            delta_1d_usd: None,
                            delta_1d_pct: None,
                            prev_7d_median: None,
                            delta_7d_usd: None,
                            delta_7d_pct: None,
                            strikes: Vec::new(),
                            buckets: Vec::new(),
                        });
                    }
                }
            }

            if !day_expiries.is_empty() {
                day_expiries.sort_by(|a, b| a.delivery_date.cmp(&b.delivery_date));
                out_days.push(BackfillCurveDay {
                    gpu: gpu_key.clone(),
                    snapshot_date: date_str.clone(),
                    captured_at: format!("{date_str}T23:59:59Z"),
                    spot_price: spot_val,
                    expiries: day_expiries,
                });
            }
        }
    }

    (out_days, fetched_ok, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strike_parser_handles_commas_and_above() {
        assert_eq!(parse_strike_dollar("Above $3.80"), Some(3.8));
        assert_eq!(parse_strike_dollar("Above $1,200.50"), Some(1200.5));
        assert_eq!(parse_strike_dollar("Will price be above $12.25?"), Some(12.25));
        // Zeichen, deren Kleinschreibung die Byte-Länge ändert, dürfen nicht panicen
        // (Kelvin-Zeichen: 3 Byte -> "k" mit 1 Byte; alter Code slicte mitten ins Zeichen)
        assert_eq!(parse_strike_dollar("€\u{212A} above $2.50"), Some(2.5));
        assert_eq!(parse_strike_dollar("İİİİİİİİİİİİ above $2.50"), Some(2.5));
    }

    #[test]
    fn clob_windows_stay_below_api_limit() {
        let day = 86_400;
        // 14 Tage passen in ein Fenster, 30 Tage werden lückenlos in 3 Fenster zerlegt
        assert_eq!(split_time_windows(0, 14 * day, CLOB_MAX_WINDOW_SECS), vec![(0, 14 * day)]);
        let w = split_time_windows(0, 30 * day, CLOB_MAX_WINDOW_SECS);
        assert_eq!(w.len(), 3);
        assert_eq!(w.first().unwrap().0, 0);
        assert_eq!(w.last().unwrap().1, 30 * day);
        for pair in w.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
        assert!(w.iter().all(|(a, b)| b - a <= CLOB_MAX_WINDOW_SECS));
        assert!(split_time_windows(10, 10, CLOB_MAX_WINDOW_SECS).is_empty());
    }

    #[test]
    fn yes_outcome_index_maps_non_default_order() {
        let m = json!({
            "outcomes": "[\"No\", \"Yes\"]",
            "outcomePrices": "[\"0.30\", \"0.70\"]",
            "clobTokenIds": "[\"tok-no\", \"tok-yes\"]"
        });
        assert_eq!(parse_prob_yes(&m), Some(0.70));
        assert_eq!(parse_yes_token_id(&m).as_deref(), Some("tok-yes"));
    }
}
