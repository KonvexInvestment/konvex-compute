//! Kurven- und Verteilungsmathematik für binäre Polymarket-Strike-Leitern.
//!
//! Eine gemeinsame stückweise lineare Überlebensfunktion S(x) = P(X > x) dient
//! konsistent als Grundlage für Quantile, getrimmten Kern-Erwartungswert und PDF-Buckets.

use crate::models::{DistributionBucket, StrikeQuote};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct RawStrikeInput {
    pub strike: f64,
    pub title: String,
    pub question: String,
    pub slug: String,
    pub prob_yes: f64,
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

#[derive(Debug, Clone)]
pub struct ExpiryAnalytics {
    pub strike_count: usize,
    pub arbitrage_violations: usize,
    pub median: Option<f64>,
    pub mean_trimmed: Option<f64>,
    pub trimmed_band_label: Option<String>,
    pub mean_raw: Option<f64>,
    pub p10: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub p90: Option<f64>,
    pub iqr: Option<f64>,
    pub skew_spread: Option<f64>,
    pub skew_ratio: Option<f64>,
    pub vs_spot_abs: Option<f64>,
    pub vs_spot_pct: Option<f64>,
    pub strikes: Vec<StrikeQuote>,
    pub buckets: Vec<DistributionBucket>,
}

fn round_digits(v: f64, digits: i32) -> f64 {
    let f = 10_f64.powi(digits);
    (v * f).round() / f
}

/// Pool-Adjacent-Violators-Algorithmus für monoton fallende Werte in [0, 1].
pub fn pava_non_increasing(values: &[f64], weights: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    struct Block {
        sum_wy: f64,
        sum_w: f64,
        start: usize,
        end: usize,
    }

    let mut blocks: Vec<Block> = Vec::with_capacity(n);
    for i in 0..n {
        let v = values[i].clamp(0.0, 1.0);
        let w = weights.get(i).copied().unwrap_or(1.0).max(1e-6);
        blocks.push(Block {
            sum_wy: v * w,
            sum_w: w,
            start: i,
            end: i,
        });
        while blocks.len() >= 2 {
            let len = blocks.len();
            let prev_mean = blocks[len - 2].sum_wy / blocks[len - 2].sum_w;
            let curr_mean = blocks[len - 1].sum_wy / blocks[len - 1].sum_w;
            if prev_mean < curr_mean {
                let curr = blocks.pop().expect("len >= 2");
                let prev = blocks.last_mut().expect("len >= 1");
                prev.sum_wy += curr.sum_wy;
                prev.sum_w += curr.sum_w;
                prev.end = curr.end;
            } else {
                break;
            }
        }
    }

    let mut out = vec![0.0_f64; n];
    for b in blocks {
        let mean = (b.sum_wy / b.sum_w).clamp(0.0, 1.0);
        for item in out.iter_mut().take(b.end + 1).skip(b.start) {
            *item = mean;
        }
    }
    out
}

/// Unterer Stützpunkt k_floor für die linke Fortsetzung von S(x) auf [0, K_0].
fn lower_anchor(strikes: &[f64]) -> f64 {
    let k0 = strikes[0];
    let step = if strikes.len() >= 2 {
        (strikes[1] - strikes[0]).max(k0 * 0.15).max(0.05)
    } else {
        (k0 * 0.25).max(0.1)
    };
    (k0 - step).max(0.0)
}

/// Gemeinsame stückweise lineare Überlebensfunktion S(x) auf [0, K_{n-1}].
fn eval_survival(strikes: &[f64], survival: &[f64], x: f64) -> f64 {
    if strikes.is_empty() {
        return 0.0;
    }
    if x <= 0.0 {
        return 1.0;
    }
    let k0 = strikes[0];
    let s0 = survival[0];
    if x <= k0 {
        let k_floor = lower_anchor(strikes);
        if x <= k_floor || (k0 - k_floor).abs() < 1e-12 {
            return 1.0;
        }
        let frac = (x - k_floor) / (k0 - k_floor);
        return (1.0 + frac * (s0 - 1.0)).clamp(0.0, 1.0);
    }

    let n = strikes.len();
    if x >= strikes[n - 1] {
        return survival[n - 1];
    }

    for i in 0..n - 1 {
        let k1 = strikes[i];
        let k2 = strikes[i + 1];
        if x >= k1 && x <= k2 {
            let dk = (k2 - k1).max(1e-12);
            let frac = (x - k1) / dk;
            return (survival[i] + frac * (survival[i + 1] - survival[i])).clamp(0.0, 1.0);
        }
    }
    0.0
}

/// Generalisierte Inverse der gemeinsamen Überlebensfunktion S(x) ohne rechte Fantasie-Extrapolation.
///
/// Behandelt exakte Treffer (auch bei einem einzelnen Strike) sowie Plateaus sauber über
/// die Mitte des gesamten Plateaus. Unerreichbare rechte Quantile liefern None.
pub fn interpolate_quantile(strikes: &[f64], survival: &[f64], quantile: f64) -> Option<f64> {
    if strikes.is_empty() || strikes.len() != survival.len() {
        return None;
    }
    let target_s = (1.0 - quantile).clamp(0.0, 1.0);
    const EPS: f64 = 1e-9;

    // 1. Exakte Treffer bzw. Plateaus auf dem gesamten zusammenhängenden Bereich
    let mut exact_first: Option<usize> = None;
    let mut exact_last: Option<usize> = None;
    for (i, &s) in survival.iter().enumerate() {
        if (s - target_s).abs() <= EPS {
            if exact_first.is_none() {
                exact_first = Some(i);
            }
            exact_last = Some(i);
        }
    }
    if let (Some(a), Some(b)) = (exact_first, exact_last) {
        return Some((strikes[a] + strikes[b]) * 0.5);
    }

    // 2. Strenges Kreuzungsintervall zwischen beobachteten Strikes
    for i in 0..strikes.len().saturating_sub(1) {
        let s1 = survival[i];
        let s2 = survival[i + 1];
        if s1 > target_s && target_s > s2 {
            let frac = (s1 - target_s) / (s1 - s2);
            return Some(strikes[i] + frac * (strikes[i + 1] - strikes[i]));
        }
    }

    // 3. Linker Rand zwischen k_floor (S=1) und K_0 (S=s_0)
    if survival[0] < target_s && strikes[0] > 0.0 {
        let k_low = lower_anchor(strikes);
        let denom = 1.0 - survival[0];
        if denom > EPS {
            let frac = (1.0 - target_s) / denom;
            return Some((k_low + frac * (strikes[0] - k_low)).max(0.0));
        }
    }

    // 4. Ziel liegt strikt jenseits des höchsten beobachteten Strikes (target_s < s_last):
    // bewusst None statt ungesicherter Extrapolation.
    None
}

/// Voller roher Integral-Erwartungswert E[X] = Integral_0^inf S(x) dx.
/// Nutzt auf [0, K_{n-1}] dieselbe Überlebensfunktion und schätzt den oberen Rest-Tail
/// jenseits K_{n-1} exponentiell.
pub fn compute_expected_value_raw(strikes: &[f64], survival: &[f64]) -> Option<f64> {
    if strikes.is_empty() || strikes.len() != survival.len() {
        return None;
    }
    let k0 = strikes[0];
    let s0 = survival[0];
    let k_floor = lower_anchor(strikes);
    let mut total = k_floor + 0.5 * (1.0 + s0) * (k0 - k_floor);

    for i in 0..strikes.len().saturating_sub(1) {
        let dk = strikes[i + 1] - strikes[i];
        if dk > 0.0 {
            total += 0.5 * (survival[i] + survival[i + 1]) * dk;
        }
    }

    let n = strikes.len();
    let s_last = survival[n - 1];
    if s_last > 1e-4 {
        let tail_scale = if n >= 2 {
            let last_step = (strikes[n - 1] - strikes[n - 2]).max(0.1);
            let s_prev = survival[n - 2];
            if s_prev > s_last + 1e-4 {
                let decay = (s_prev / s_last).ln() / last_step;
                (1.0 / decay.max(1e-4)).min(last_step * 2.0)
            } else {
                last_step * 0.75
            }
        } else {
            (strikes[n - 1] * 0.15).max(0.2)
        };
        total += s_last * tail_scale;
    }
    Some(total)
}

fn fmt_pct_label(alpha: f64) -> String {
    let p_lo = alpha * 100.0;
    let p_hi = (1.0 - alpha) * 100.0;
    let fmt = |v: f64| {
        if (v - v.round()).abs() < 0.05 {
            format!("P{:.0}", v.round())
        } else {
            format!("P{:.1}", v).replace('.', ",")
        }
    };
    format!("{} bis {}", fmt(p_lo), fmt(p_hi))
}

/// Symmetrisch getrimmter Kern-Erwartungswert strikt auf Basis beobachteter Strikes.
///
/// Wählt eff_alpha = max(base_alpha, s_min_obs, 1 - s_max_obs) (ab 0.40 Rückfall auf den Median),
/// damit beide Quantilgrenzen exakt auf der gemeinsamen Überlebensfunktion liegen
/// und liefert das effektive Band-Label transparent mit zurück.
pub fn compute_expected_value_trimmed(
    strikes: &[f64],
    survival: &[f64],
    base_alpha: f64,
) -> Option<(f64, String)> {
    if strikes.is_empty() || strikes.len() != survival.len() {
        return None;
    }
    let s_min_obs = survival[survival.len() - 1];
    let s_max_obs = survival[0];

    let obs_alpha = s_min_obs.max(1.0 - s_max_obs);
    let eff_alpha = base_alpha.clamp(0.01, 0.25).max(obs_alpha);
    if eff_alpha >= 0.40 {
        let med = interpolate_quantile(strikes, survival, 0.50)?;
        return Some((med, "Median (P50)".into()));
    }

    let s_low_bound = eff_alpha;
    let s_high_bound = 1.0 - eff_alpha;
    let x_lo = interpolate_quantile(strikes, survival, eff_alpha)?;
    let x_hi = interpolate_quantile(strikes, survival, 1.0 - eff_alpha)?;
    if x_hi <= x_lo {
        return Some((x_lo, fmt_pct_label(eff_alpha)));
    }

    let mut xs: Vec<f64> = Vec::with_capacity(strikes.len() + 2);
    xs.push(x_lo);
    for &k in strikes {
        if k > x_lo && k < x_hi {
            xs.push(k);
        }
    }
    xs.push(x_hi);

    let mass = s_high_bound - s_low_bound;
    let mut area = 0.0;
    for i in 0..xs.len() - 1 {
        let a = xs[i];
        let b = xs[i + 1];
        let sa = (eval_survival(strikes, survival, a) - s_low_bound).clamp(0.0, mass);
        let sb = (eval_survival(strikes, survival, b) - s_low_bound).clamp(0.0, mass);
        area += 0.5 * (sa + sb) * (b - a);
    }
    Some((x_lo + area / mass, fmt_pct_label(eff_alpha)))
}

/// Erzeugt vollständige Wahrscheinlichkeits-Buckets ohne Masseverlust (Summe = 1.0).
pub fn build_distribution_buckets(strikes: &[f64], survival: &[f64]) -> Vec<DistributionBucket> {
    if strikes.is_empty() || strikes.len() != survival.len() {
        return Vec::new();
    }
    let mut buckets = Vec::with_capacity(strikes.len() + 2);

    let left_prob = (1.0 - survival[0]).max(0.0);
    if left_prob > 1e-6 {
        let k_floor = lower_anchor(strikes);
        buckets.push(DistributionBucket {
            label: format!("< {:.2} $", strikes[0]),
            low: Some(k_floor),
            high: Some(strikes[0]),
            mid: round_digits((k_floor + strikes[0]) * 0.5, 4),
            prob: round_digits(left_prob, 6),
            density: None,
            is_tail: true,
        });
    }

    for i in 0..strikes.len().saturating_sub(1) {
        let k1 = strikes[i];
        let k2 = strikes[i + 1];
        let prob = (survival[i] - survival[i + 1]).max(0.0);
        let width = (k2 - k1).max(1e-6);
        buckets.push(DistributionBucket {
            label: format!("{:.2} $ bis {:.2} $", k1, k2),
            low: Some(k1),
            high: Some(k2),
            mid: round_digits((k1 + k2) * 0.5, 4),
            prob: round_digits(prob, 6),
            density: Some(round_digits(prob / width, 6)),
            is_tail: false,
        });
    }

    let n = strikes.len();
    let right_prob = survival[n - 1].max(0.0);
    if right_prob > 1e-6 {
        let step = if n > 1 {
            strikes[n - 1] - strikes[n - 2]
        } else {
            strikes[n - 1] * 0.2
        };
        buckets.push(DistributionBucket {
            label: format!("> {:.2} $", strikes[n - 1]),
            low: Some(strikes[n - 1]),
            high: None,
            mid: round_digits(strikes[n - 1] + step * 0.5, 4),
            prob: round_digits(right_prob, 6),
            density: None,
            is_tail: true,
        });
    }

    buckets
}

pub fn analyze_expiry_strikes(
    raw_strikes: Vec<RawStrikeInput>,
    spot_price: Option<f64>,
) -> ExpiryAnalytics {
    let mut dedup: BTreeMap<i64, RawStrikeInput> = BTreeMap::new();
    for item in raw_strikes {
        if !item.strike.is_finite()
            || item.strike <= 0.0
            || !item.prob_yes.is_finite()
            || !(0.0..=1.0).contains(&item.prob_yes)
        {
            continue;
        }
        let key = (item.strike * 10_000.0).round() as i64;
        match dedup.get(&key) {
            Some(prev) if prev.liquidity >= item.liquidity => {}
            _ => {
                dedup.insert(key, item);
            }
        }
    }

    let ordered: Vec<RawStrikeInput> = dedup.into_values().collect();
    if ordered.is_empty() {
        return ExpiryAnalytics {
            strike_count: 0,
            arbitrage_violations: 0,
            median: None,
            mean_trimmed: None,
            trimmed_band_label: None,
            mean_raw: None,
            p10: None,
            p25: None,
            p75: None,
            p90: None,
            iqr: None,
            skew_spread: None,
            skew_ratio: None,
            vs_spot_abs: None,
            vs_spot_pct: None,
            strikes: Vec::new(),
            buckets: Vec::new(),
        };
    }

    let strikes: Vec<f64> = ordered.iter().map(|s| s.strike).collect();
    let raw_probs: Vec<f64> = ordered.iter().map(|s| s.prob_yes.clamp(0.0, 1.0)).collect();
    let weights: Vec<f64> = ordered
        .iter()
        .map(|s| (1.0 + s.liquidity.max(0.0)).log10().max(1.0))
        .collect();

    let violations = raw_probs
        .windows(2)
        .filter(|w| w[1] > w[0] + 0.005)
        .count();

    let iso_probs = pava_non_increasing(&raw_probs, &weights);

    let enriched: Vec<StrikeQuote> = ordered
        .into_iter()
        .zip(iso_probs.iter().copied())
        .map(|(item, p_iso)| StrikeQuote {
            strike: item.strike,
            title: item.title,
            question: item.question,
            slug: item.slug,
            prob_yes: round_digits(item.prob_yes.clamp(0.0, 1.0), 5),
            prob_pava: round_digits(p_iso, 5),
            pava_diff: round_digits(p_iso - item.prob_yes.clamp(0.0, 1.0), 5),
            best_bid: item.best_bid,
            best_ask: item.best_ask,
            last_trade: item.last_trade,
            spread: item.spread,
            one_day_change: item.one_day_change,
            liquidity: round_digits(item.liquidity, 2),
            volume_24h: round_digits(item.volume_24h, 2),
            volume_total: round_digits(item.volume_total, 2),
            token_id: item.token_id,
        })
        .collect();

    let p10 = interpolate_quantile(&strikes, &iso_probs, 0.10);
    let p25 = interpolate_quantile(&strikes, &iso_probs, 0.25);
    let p50 = interpolate_quantile(&strikes, &iso_probs, 0.50);
    let p75 = interpolate_quantile(&strikes, &iso_probs, 0.75);
    let p90 = interpolate_quantile(&strikes, &iso_probs, 0.90);
    let mean_raw = compute_expected_value_raw(&strikes, &iso_probs);
    let trimmed = compute_expected_value_trimmed(&strikes, &iso_probs, 0.05);
    let (mean_trimmed, trimmed_band_label) = match trimmed {
        Some((v, lbl)) => (Some(v), Some(lbl)),
        None => (None, None),
    };

    let iqr = match (p25, p75) {
        (Some(a), Some(b)) => Some(b - a),
        _ => None,
    };
    let skew_spread = match (mean_raw, p50) {
        (Some(m), Some(med)) => Some(m - med),
        _ => None,
    };
    let skew_ratio = match (p25, p50, p75) {
        (Some(a), Some(m), Some(b)) if (m - a) > 1e-4 => Some((b - m) / (m - a)),
        _ => None,
    };

    let (vs_spot_abs, vs_spot_pct) = match (p50, spot_price) {
        (Some(med), Some(sp)) if sp.is_finite() && sp > 0.0 => {
            (Some(med - sp), Some((med / sp - 1.0) * 100.0))
        }
        _ => (None, None),
    };

    let r3 = |v: Option<f64>| v.map(|x| round_digits(x, 3));
    let r2 = |v: Option<f64>| v.map(|x| round_digits(x, 2));

    let buckets = build_distribution_buckets(&strikes, &iso_probs);

    ExpiryAnalytics {
        strike_count: enriched.len(),
        arbitrage_violations: violations,
        median: r3(p50),
        mean_trimmed: r3(mean_trimmed),
        trimmed_band_label,
        mean_raw: r3(mean_raw),
        p10: r3(p10),
        p25: r3(p25),
        p75: r3(p75),
        p90: r3(p90),
        iqr: r3(iqr),
        skew_spread: r3(skew_spread),
        skew_ratio: r2(skew_ratio),
        vs_spot_abs: r3(vs_spot_abs),
        vs_spot_pct: r2(vs_spot_pct),
        strikes: enriched,
        buckets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(k: f64, p: f64) -> RawStrikeInput {
        RawStrikeInput {
            strike: k,
            title: format!("Above ${k:.2}"),
            question: String::new(),
            slug: String::new(),
            prob_yes: p,
            best_bid: None,
            best_ask: None,
            last_trade: None,
            spread: None,
            one_day_change: None,
            liquidity: 20_000.0,
            volume_24h: 1_000.0,
            volume_total: 5_000.0,
            token_id: None,
        }
    }

    #[test]
    fn pava_enforces_monotonicity() {
        let iso = pava_non_increasing(&[0.95, 0.80, 0.86, 0.50, 0.30, 0.10], &[1.0; 6]);
        for w in iso.windows(2) {
            assert!(w[0] >= w[1] - 1e-12);
        }
        assert!((iso[1] - 0.83).abs() < 1e-9);
        assert!((iso[2] - 0.83).abs() < 1e-9);
    }

    #[test]
    fn symmetric_quantiles_and_means_match() {
        let res = analyze_expiry_strikes(
            vec![
                mk(2.0, 0.90),
                mk(2.5, 0.75),
                mk(3.0, 0.50),
                mk(3.5, 0.25),
                mk(4.0, 0.10),
            ],
            Some(3.20),
        );
        assert!((res.median.unwrap() - 3.0).abs() < 1e-3);
        assert!((res.p25.unwrap() - 2.5).abs() < 1e-3);
        assert!((res.p75.unwrap() - 3.5).abs() < 1e-3);
        assert!((res.mean_trimmed.unwrap() - 3.0).abs() < 1e-3);
        assert_eq!(res.trimmed_band_label.as_deref(), Some("P10 bis P90"));
        let b_sum: f64 = res.buckets.iter().map(|b| b.prob).sum();
        assert!((b_sum - 1.0).abs() < 1e-4);
    }

    #[test]
    fn single_strike_and_plateau_quantiles() {
        // Einzelner Strike exakt bei 50 % liefert seinen Preis als Median
        let single = analyze_expiry_strikes(vec![mk(3.5, 0.50)], Some(3.27));
        assert_eq!(single.median, Some(3.5));
        assert_eq!(single.p90, None); // rechts unerreichbar -> None statt Fantasie-Extrapolation

        // Plateau über 3 Strikes liefert die Mitte des gesamten Plateaus
        let plateau = interpolate_quantile(&[2.0, 3.0, 4.0], &[0.50, 0.50, 0.50], 0.50);
        assert_eq!(plateau, Some(3.0));
    }

    #[test]
    fn small_tails_preserve_full_probability_mass() {
        let res = analyze_expiry_strikes(vec![mk(2.0, 0.999), mk(3.0, 0.50), mk(4.0, 0.001)], None);
        let b_sum: f64 = res.buckets.iter().map(|b| b.prob).sum();
        assert!((b_sum - 1.0).abs() < 1e-4);
    }

    #[test]
    fn trimmed_mean_tames_two_year_longshot_tail() {
        let res = analyze_expiry_strikes(
            vec![
                mk(0.25, 0.875),
                mk(0.50, 0.805),
                mk(1.00, 0.775),
                mk(1.50, 0.615),
                mk(2.00, 0.465),
                mk(3.00, 0.315),
                mk(5.00, 0.165),
                mk(7.00, 0.135),
                mk(10.00, 0.105),
                mk(13.00, 0.085),
            ],
            Some(3.27),
        );
        let med = res.median.unwrap();
        let trimmed = res.mean_trimmed.unwrap();
        let raw = res.mean_raw.unwrap();
        assert!(med > 1.8 && med < 2.0);
        assert!(trimmed < raw);
        assert_eq!(res.trimmed_band_label.as_deref(), Some("P12,5 bis P87,5"));
    }
}
