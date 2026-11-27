//! Tests des Regime-Modells auf synthetischen Daten (keine Marktdaten im Repository).

use super::*;

fn d0() -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()
}

/// Deterministisches Rauschen in [-1, 1].
fn noise(i: i64, salt: u64) -> f64 {
    let mut x = (i as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(salt.wrapping_mul(1442695040888963407));
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    (x % 20001) as f64 / 10000.0 - 1.0
}

fn add_months(d: NaiveDate, m: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + m;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap()
}

#[test]
fn rating_strings_map_to_buckets() {
    assert_eq!(RatingBucket::from_rating("BBB-"), Some(RatingBucket::Bbb));
    assert_eq!(RatingBucket::from_rating("Baa2"), Some(RatingBucket::Bbb));
    assert_eq!(RatingBucket::from_rating("B+"), Some(RatingBucket::B));
    assert_eq!(RatingBucket::from_rating("Ba3"), Some(RatingBucket::Bb));
    assert_eq!(RatingBucket::from_rating("CCC+"), Some(RatingBucket::Ccc));
    assert_eq!(RatingBucket::from_rating("Caa1"), Some(RatingBucket::Ccc));
    assert_eq!(RatingBucket::from_rating("AA+"), Some(RatingBucket::Aa));
    assert_eq!(RatingBucket::from_rating("NR"), None);
}

#[test]
fn robust_z_ignores_outliers_and_clips() {
    let mut w: Vec<f64> = (0..100).map(|i| 3.0 + 0.1 * noise(i, 1)).collect();
    w.push(50.0);
    let z = robust_z(&w, 3.0).unwrap();
    assert!(z.abs() < 0.5, "{z}");
    assert_eq!(robust_z(&w, 50.0), Some(4.0));
    assert_eq!(robust_z(&[1.0, 1.0, 1.0], 2.0), None);
}

#[test]
fn constant_maturity_interpolates_between_months() {
    let day = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
    let node = |y, m, dd, med| {
        CurveNode::from_delivery(NaiveDate::from_ymd_opt(y, m, dd).unwrap(), Some(med), None)
    };
    let nodes = vec![
        node(2026, 11, 30, 9.0), // laufender Abrechnungsmonat: fällt heraus
        node(2027, 1, 31, 3.0),
        node(2027, 3, 31, 2.0),
    ];
    let jan = (nodes[1].expiry - day).num_days() as f64;
    let mar = (nodes[2].expiry - day).num_days() as f64;
    let v = constant_maturity(day, &nodes, 90, |n| n.median).unwrap();
    let expect = 3.0 + (2.0 - 3.0) * (90.0 - jan) / (mar - jan);
    assert!((v - expect).abs() < 1e-9, "{v} vs {expect}");
    // Bis 45 Tage über den letzten Verfall hinaus flach, danach keine Aussage
    assert_eq!(constant_maturity(day, &nodes, 140, |n| n.median), Some(2.0));
    assert_eq!(constant_maturity(day, &nodes, 365, |n| n.median), None);
}

#[test]
fn implied_position_interpolates_logarithmically() {
    let grid = [
        Some(40.0),
        Some(60.0),
        Some(80.0),
        Some(110.0),
        Some(190.0),
        Some(320.0),
        Some(800.0),
    ];
    assert_eq!(implied_position(30.0, &grid), Some(0.0));
    assert!((implied_position(110.0, &grid).unwrap() - 3.0).abs() < 1e-9);
    let between = implied_position((110.0_f64 * 190.0).sqrt(), &grid).unwrap();
    assert!((between - 3.5).abs() < 1e-9);
    let beyond = implied_position(5000.0, &grid).unwrap();
    assert!(beyond > 6.0 && beyond <= 7.0);
    assert_eq!(implied_position(100.0, &[None; 7]), None);
}

#[test]
fn bocpd_flags_a_mean_shift_quickly_and_stays_quiet_before() {
    let mut xs: Vec<f64> = (0..200).map(|i| noise(i, 7)).collect();
    xs.extend((200..260).map(|i| 4.0 + noise(i, 7)));
    let p = bocpd_recent_break(&xs, 100.0, 7, 400);
    let quiet_max = p[30..195].iter().copied().fold(0.0, f64::max);
    assert!(quiet_max < 0.5, "Fehlalarm vor dem Bruch: {quiet_max}");
    let after_max = p[200..207].iter().copied().fold(0.0, f64::max);
    assert!(after_max > 0.8, "Bruch nicht erkannt: {after_max}");
}

#[test]
fn bocpd_warmup_is_masked() {
    let xs = vec![0.1, f64::NAN, -0.2, 0.3, 5.0];
    let mut p = bocpd_recent_break(&xs, 100.0, 7, 400);
    mask_warmup(&mut p, &xs, 3);
    assert!(p[0].is_nan() && p[1].is_nan() && p[2].is_nan());
    assert!(p[3].is_finite() && p[4].is_finite());
}

#[test]
fn short_curve_history_reports_unknown_but_shows_credit_stress() {
    let (mut inputs, today) = build(&Scenario { stress: late_stress, price: flat, spot: flat });
    inputs.curve.retain(|c| c.date > today - Duration::days(5));
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Unbekannt);
    assert!(r.credit_stress);
    assert_eq!(r.curve_days, 5);
    assert!(r.price_break_prob.is_none());
    assert!(r.warnings.iter().all(|w| !w.contains("Bruchpunkt")), "{:?}", r.warnings);
}

#[test]
fn ln_gamma_matches_known_values() {
    assert!(ln_gamma(1.0).abs() < 1e-12);
    assert!((ln_gamma(5.0) - 24.0_f64.ln()).abs() < 1e-10);
    assert!((ln_gamma(0.5) - std::f64::consts::PI.sqrt().ln()).abs() < 1e-10);
}

struct Scenario {
    /// Aufschlag auf die Hochzins-OAS in Prozentpunkten je Tag (andere Stufen proportional)
    stress: fn(i64) -> f64,
    /// Faktor auf das Kurvenniveau je Tag
    price: fn(i64) -> f64,
    /// Faktor auf den Spot relativ zur Kurve je Tag
    spot: fn(i64) -> f64,
}

const DAYS: i64 = 1000;

fn build(s: &Scenario) -> (ModelInputs, NaiveDate) {
    let start = d0();
    let today = start + Duration::days(DAYS - 1);
    let base = [
        (OAS_HY, 3.5),
        ("BAMLH0A3HYC", 9.0),
        ("BAMLC0A4CBBB", 1.1),
        ("BAMLC0A1CAAA", 0.4),
        ("BAMLC0A2CAA", 0.6),
        ("BAMLC0A3CA", 0.8),
        ("BAMLH0A1HYBB", 2.0),
        ("BAMLH0A2HYB", 3.2),
    ];
    let mut oas = BTreeMap::new();
    for (k, (id, level)) in base.iter().enumerate() {
        let mut series = Series::new();
        for i in 0..DAYS {
            let day = start + Duration::days(i);
            if matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                continue;
            }
            let bump = (s.stress)(i) * level / 3.5;
            series.insert(day, level + bump + 0.03 * level * noise(i, k as u64 + 11));
        }
        oas.insert(id.to_string(), series);
    }
    let mut curve = Vec::new();
    for i in (DAYS - 200)..DAYS {
        let day = start + Duration::days(i);
        let level = 2.0 * (s.price)(i) * (1.0 + 0.004 * noise(i, 3));
        let first = day.with_day(1).unwrap();
        let nodes = (1..=14)
            .map(|m| {
                let delivery = add_months(first, m + 1) - Duration::days(1);
                let med = level * (1.0 - 0.01 * m as f64);
                CurveNode::from_delivery(delivery, Some(med), Some(med * 0.9))
            })
            .collect();
        curve.push(CurveDay {
            date: day,
            spot: Some(level * (s.spot)(i)),
            nodes,
        });
    }
    (
        ModelInputs {
            gpu: "TEST".into(),
            oas,
            issuers: Vec::new(),
            curve,
        },
        today,
    )
}

fn calm(_: i64) -> f64 {
    0.0
}
fn flat(_: i64) -> f64 {
    1.0
}
fn late_stress(i: i64) -> f64 {
    if i >= DAYS - 60 {
        2.5
    } else {
        0.0
    }
}
fn late_fall(i: i64) -> f64 {
    let t0 = DAYS - 40;
    if i < t0 {
        1.0
    } else {
        1.0 - 0.01 * (i - t0).min(25) as f64
    }
}
fn late_spot_fall(i: i64) -> f64 {
    let t0 = DAYS - 40;
    if i < t0 {
        1.0
    } else {
        1.0 - 0.004 * (i - t0).min(25) as f64
    }
}

#[test]
fn calm_markets_are_calm() {
    let (inputs, today) = build(&Scenario { stress: calm, price: flat, spot: flat });
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Ruhig, "{:?}", r.transitions);
    assert!(r.transitions.is_empty(), "{:?}", r.transitions);
    assert!(r.fsi.unwrap().abs() < 1.0);
    assert!(!r.history.is_empty());
}

#[test]
fn credit_stress_with_firm_curve_is_decoupled() {
    let (inputs, today) = build(&Scenario { stress: late_stress, price: flat, spot: flat });
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Entkoppelt, "{:?}", r.transitions);
    assert!(r.fsi.unwrap() >= STRESS_ENTER);
    assert_eq!(r.transitions.last().unwrap().to, Regime::Entkoppelt);
    assert!(!r.early_warning, "{:?}", r.warnings);
}

#[test]
fn falling_rents_without_stress_are_price_pressure() {
    let (inputs, today) = build(&Scenario { stress: calm, price: late_fall, spot: late_spot_fall });
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Preisdruck, "{:?}", r.transitions);
}

#[test]
fn stress_plus_joint_fall_is_transmission() {
    let (inputs, today) = build(&Scenario { stress: late_stress, price: late_fall, spot: late_spot_fall });
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Uebertragung, "{:?}", r.transitions);
    let kinds: Vec<Regime> = r.transitions.iter().map(|t| t.to).collect();
    assert!(kinds.contains(&Regime::Entkoppelt), "{kinds:?}");
    assert!(r.price.near_change_30d_pct.unwrap() < -8.0);
}

#[test]
fn stress_hysteresis_holds_inside_the_band() {
    // Pendeln zwischen Austritts- und Eintrittsschwelle ändert den Zustand nicht
    let mut high = false;
    for f in [0.9, 0.6, 0.95, 0.7] {
        high = next_stress_state(high, Some(f));
        assert!(!high, "{f}");
    }
    high = next_stress_state(high, Some(1.0));
    assert!(high);
    for f in [0.6, 0.9, 0.55, 0.8] {
        high = next_stress_state(high, Some(f));
        assert!(high, "{f}");
    }
    assert!(next_stress_state(high, None));
    assert!(!next_stress_state(high, Some(0.49)));
}

#[test]
fn falling_needs_median_quartile_and_spot_and_ends_late() {
    let both = FallSignal { dn14: Some(-0.06), dp14: Some(-0.07), dn30: None, dp30: None, dbasis: Some(-0.01) };
    assert!(next_falling_state(false, &both));
    // Nur der Median fällt, das untere Quartil hält: kein gemeinsamer Rückgang
    let median_only = FallSignal { dp14: Some(-0.01), ..both };
    assert!(!next_falling_state(false, &median_only));
    // Spot hält, während die Kurve fällt (Basis steigt): nur Erwartung, noch kein Rückgang
    let spot_firm = FallSignal { dbasis: Some(0.03), ..both };
    assert!(!next_falling_state(false, &spot_firm));
    // Abklingen: Median fällt über 14 Tage nur noch 3 %, das reicht zum Halten
    let fading = FallSignal { dn14: Some(-0.03), dp14: Some(0.0), dn30: Some(-0.05), dp30: Some(-0.02), dbasis: None };
    assert!(next_falling_state(true, &fading));
    let over = FallSignal { dn14: Some(0.0), dp14: Some(0.0), dn30: Some(-0.01), dp30: Some(0.0), dbasis: None };
    assert!(!next_falling_state(true, &over));
}

fn pulse(i: i64) -> f64 {
    if (DAYS - 150..DAYS - 80).contains(&i) {
        2.5
    } else {
        0.0
    }
}

#[test]
fn stress_pulse_enters_and_leaves_once() {
    let (inputs, today) = build(&Scenario { stress: pulse, price: flat, spot: flat });
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Ruhig, "{:?}", r.transitions);
    let kinds: Vec<Regime> = r.transitions.iter().map(|t| t.to).collect();
    assert_eq!(kinds, vec![Regime::Entkoppelt, Regime::Ruhig], "{:?}", r.transitions);
}

#[test]
fn missing_data_reports_unknown() {
    let r = compute_regime(&ModelInputs::default(), d0());
    assert_eq!(r.regime, Regime::Unbekannt);
    assert!(!r.notes.is_empty());
}

#[test]
fn issuer_gap_feeds_the_ai_block() {
    let (mut inputs, today) = build(&Scenario { stress: calm, price: flat, spot: flat });
    // Agenturrating BBB, am Anleihemarkt aber zwischen BB- und B-Niveau
    let mut spreads = Series::new();
    for i in 0..90 {
        spreads.insert(today - Duration::days(i), 300.0);
    }
    inputs.issuers.push(IssuerSeries {
        key: "X".into(),
        label: "X".into(),
        agency_rating: "BBB".into(),
        spreads_bp: spreads,
        bonds: vec![],
    });
    let r = compute_regime(&inputs, today);
    let iss = &r.issuers[0];
    assert_eq!(iss.implied_bucket.as_deref(), Some("B"), "{iss:?}");
    assert!(iss.gap_buckets.unwrap() > 1.5, "{iss:?}");
    assert!(r.ai_block.unwrap() > 1.5);
}
