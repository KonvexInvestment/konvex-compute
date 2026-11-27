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
fn bocpd_needs_a_scale_and_masks_the_warmup() {
    let mut xs: Vec<f64> = (0..16).map(|i| noise(i, 3)).collect();
    xs[1] = f64::NAN;
    let raw = bocpd_recent_break(&xs, 100.0, 7, 400);
    // Ohne zehn vorherige gültige Beobachtungen gibt es keine Normierung
    assert!(raw[..11].iter().all(|p| p.is_nan()), "{raw:?}");
    assert!(raw[11..].iter().all(|p| p.is_finite()), "{raw:?}");
    let mut p = raw.clone();
    // Das dritte echte Update steht an Index 13 (zehn Werte für die Skala, eine Lücke)
    mask_warmup(&mut p, 3);
    assert!(p[..13].iter().all(|v| v.is_nan()));
    assert!(p[13..].iter().all(|v| v.is_finite()));
}

#[test]
fn bocpd_ignores_later_observations() {
    let mut xs: Vec<f64> = (0..120).map(|i| noise(i, 11)).collect();
    xs.extend((120..180).map(|i| 6.0 + 3.0 * noise(i, 11)));
    let short = bocpd_recent_break(&xs[..100], 100.0, 7, 400);
    let long = bocpd_recent_break(&xs, 100.0, 7, 400);
    for (a, b) in short.iter().zip(&long) {
        assert!((a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-12, "{a} != {b}");
    }
}

#[test]
fn regime_history_does_not_change_with_later_data() {
    let (inputs, today) = build(&Scenario { stress: late_stress, price: late_fall, spot: late_fall });
    let cut = today - Duration::days(40);
    let mut early = inputs.clone();
    early.curve.retain(|c| c.date <= cut);
    for s in early.oas.values_mut() {
        s.retain(|d, _| *d <= cut);
    }
    let r_early = compute_regime(&early, cut);
    let r_full = compute_regime(&inputs, today);
    let full: BTreeMap<NaiveDate, &RegimeDay> = r_full.history.iter().map(|h| (h.date, h)).collect();
    let mut compared = 0;
    for h in &r_early.history {
        let Some(f) = full.get(&h.date) else { continue };
        assert_eq!(h.regime, f.regime, "{}", h.date);
        assert_eq!(h.raw, f.raw, "{}", h.date);
        assert_eq!(h.fsi.map(|v| (v * 1e9).round()), f.fsi.map(|v| (v * 1e9).round()), "{}", h.date);
        let same = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-9,
            (None, None) => true,
            _ => false,
        };
        assert!(same(h.price_break_prob, f.price_break_prob), "{}", h.date);
        assert!(same(h.credit_break_prob, f.credit_break_prob), "{}", h.date);
        compared += 1;
    }
    assert!(compared > 300, "{compared}");
}

#[test]
fn a_data_gap_restarts_the_confirmation_count() {
    // Kurvenlücke mitten in einem Wechsel: Der Kandidat muss danach neu zählen
    let (mut inputs, today) = build(&Scenario { stress: late_stress, price: flat, spot: flat });
    let r_full = compute_regime(&inputs, today);
    let first_entk = r_full
        .history
        .iter()
        .find(|h| h.raw == Regime::Entkoppelt)
        .map(|h| h.date)
        .expect("Szenario erzeugt Kreditstress");
    let gap = first_entk + Duration::days(2);
    inputs.curve.retain(|c| c.date != gap);
    let r = compute_regime(&inputs, today);
    let day = |d: NaiveDate| r.history.iter().find(|h| h.date == d).unwrap();
    assert_eq!(day(gap).raw, Regime::Unbekannt);
    // Zwei Tage vor der Lücke reichen zusammen mit einem Tag danach nicht für die Bestätigung
    assert_ne!(day(gap + Duration::days(1)).regime, Regime::Entkoppelt);
    assert_eq!(day(gap + Duration::days(3)).regime, Regime::Entkoppelt);
}

#[test]
fn a_long_outage_does_not_show_an_old_regime_as_current() {
    let (mut inputs, today) = build(&Scenario { stress: calm, price: flat, spot: flat });
    inputs.curve.retain(|c| c.date <= today - Duration::days(10));
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Unbekannt);
    assert_eq!(r.classified_until, Some(today - Duration::days(10)));
    assert!(r.notes.iter().any(|n| n.contains("Zuletzt bestätigt: Ruhig")), "{:?}", r.notes);
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
        ratings: vec![],
        members: BTreeMap::new(),
        linked_bp: spreads.clone(),
        spreads_bp: spreads,
        bonds: vec![],
    });
    let r = compute_regime(&inputs, today);
    let iss = &r.issuers[0];
    assert_eq!(iss.implied_bucket.as_deref(), Some("B"), "{iss:?}");
    assert!(iss.gap_buckets.unwrap() > 1.5, "{iss:?}");
    assert!(r.ai_block.unwrap() > 1.5);
}

#[test]
fn a_missing_curve_day_keeps_the_falling_state() {
    assert!(next_falling_state(true, &FallSignal::default()));
    assert!(!next_falling_state(false, &FallSignal::default()));
}

#[test]
fn fsi_changes_skip_carried_days_and_composition_changes() {
    let day = |fsi: f64, composition: u64, data_stamp: u64| StressDay {
        fsi: Some(fsi),
        composition,
        data_stamp,
        ..Default::default()
    };
    let fri = day(1.0, 0b111, 1);
    let sat = day(1.0, 0b111, 1);
    let mon = day(1.4, 0b111, 2);
    let with_ai = day(2.0, 0b1111, 3);
    assert_eq!(sat.fsi_change_since(&fri, true), None, "Wochenende ohne neue Daten");
    assert!((mon.fsi_change_since(&sat, true).unwrap() - 0.4).abs() < 1e-12);
    assert_eq!(with_ai.fsi_change_since(&mon, true), None, "KI-Block kommt hinzu");
    // Für die 7-Tage-Kopplung zählt nur die Zusammensetzung
    assert!(sat.fsi_change_since(&fri, false).is_some());
}

#[test]
fn implied_position_is_continuous_with_an_inverted_grid() {
    // AAA zeitweise über AA
    let grid = [Some(55.0), Some(45.0), Some(70.0), Some(110.0), Some(190.0), Some(330.0), Some(800.0)];
    let below = implied_position(54.9, &grid).unwrap();
    let above = implied_position(55.1, &grid).unwrap();
    assert!((above - below).abs() < 0.05, "{below} vs {above}");
    // Oberer Rand invertiert: weite Spreads bekommen trotzdem eine Position
    let top = [Some(40.0), Some(60.0), Some(90.0), Some(140.0), Some(250.0), Some(500.0), Some(450.0)];
    assert!(implied_position(1500.0, &top).unwrap() > 6.0);
}

#[test]
fn the_first_regime_needs_no_confirmation() {
    let (inputs, today) = build(&Scenario { stress: calm, price: flat, spot: flat });
    let r = compute_regime(&inputs, today);
    let first_raw = r.history.iter().find(|h| h.raw != Regime::Unbekannt).unwrap();
    assert_eq!(first_raw.regime, Regime::Ruhig, "{}", first_raw.date);
}

#[test]
fn distress_alone_is_a_hint_not_an_early_warning() {
    let (mut inputs, today) = build(&Scenario { stress: late_stress, price: flat, spot: flat });
    let mut spreads = Series::new();
    for i in 0..120 {
        spreads.insert(today - Duration::days(i), 3000.0);
    }
    inputs.issuers.push(IssuerSeries {
        key: "X".into(),
        label: "X".into(),
        agency_rating: "B".into(),
        ratings: vec![],
        members: BTreeMap::new(),
        linked_bp: spreads.clone(),
        spreads_bp: spreads,
        bonds: vec![],
    });
    let r = compute_regime(&inputs, today);
    assert_eq!(r.regime, Regime::Entkoppelt);
    assert!(r.warnings.iter().any(|w| w.contains("Notlagenbereich")), "{:?}", r.warnings);
    assert!(!r.early_warning);
}

#[test]
fn oas_values_count_from_the_next_business_day() {
    // 27.11.2026 ist ein Freitag
    let fri = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
    let day = |n: i64| fri + Duration::days(n);
    assert_eq!(oas_known_until(fri), day(-1));
    assert_eq!(oas_known_until(day(1)), day(-1), "Samstag: Freitag noch nicht veröffentlicht");
    assert_eq!(oas_known_until(day(2)), day(-1), "Sonntag");
    assert_eq!(oas_known_until(day(3)), fri, "Montag: Freitag bekannt");
    assert_eq!(oas_known_until(day(4)), day(3), "Dienstag");
}

fn issuer(spreads: Series, ratings: Vec<(NaiveDate, String)>, members: BTreeMap<NaiveDate, u64>) -> IssuerSeries {
    IssuerSeries {
        key: "X".into(),
        label: "X".into(),
        agency_rating: ratings.last().map(|r| r.1.clone()).unwrap_or_else(|| "BBB".into()),
        ratings,
        members,
        linked_bp: spreads.clone(),
        spreads_bp: spreads,
        bonds: vec![],
    }
}

#[test]
fn a_rating_change_keeps_history_and_is_no_credit_move() {
    let (base, today) = build(&Scenario { stress: calm, price: flat, spot: flat });
    let spreads: Series = (0..200).map(|i| (today - Duration::days(i), 300.0)).collect();
    let change = today - Duration::days(20);
    let mut before = base.clone();
    before.issuers.push(issuer(spreads.clone(), vec![(NaiveDate::MIN, "BBB".into())], BTreeMap::new()));
    let mut after = base;
    after.issuers.push(issuer(
        spreads,
        vec![(NaiveDate::MIN, "BBB".into()), (change, "BB".into())],
        BTreeMap::new(),
    ));
    let (a, b) = (compute_regime(&before, today), compute_regime(&after, today));
    for (x, y) in a.history.iter().zip(&b.history) {
        if x.date < change {
            assert_eq!(x.fsi, y.fsi, "{}", x.date);
            assert_eq!(x.credit_break_prob, y.credit_break_prob, "{}", x.date);
        }
    }
    let at = |r: &RegimeReport| r.history.iter().find(|h| h.date == change).unwrap().fsi.unwrap();
    assert!(at(&b) < at(&a), "besseres Rating, kleinere Lücke");
    assert_eq!(b.issuers[0].agency_rating, "BB");
}

#[test]
fn a_basket_change_is_no_credit_move() {
    let (base, today) = build(&Scenario { stress: calm, price: flat, spot: flat });
    let switch = today - Duration::days(10);
    let spreads: Series = (0..200)
        .map(|i| {
            let d = today - Duration::days(i);
            (d, if d < switch { 300.0 } else { 600.0 })
        })
        .collect();
    let change_at_switch = |members: BTreeMap<NaiveDate, u64>| {
        let mut inputs = base.clone();
        inputs.issuers.push(issuer(spreads.clone(), vec![], members));
        let macro_z: Vec<(&str, &str, Series)> = MACRO_COMPONENTS
            .iter()
            .map(|(id, label)| (*id, *label, rolling_robust_z(&inputs.oas[*id], OAS_Z_WINDOW, OAS_Z_MIN_OBS)))
            .collect();
        let stress = stress_days(&inputs, &macro_z, &issuer_positions(&inputs), today - Duration::days(60), today);
        stress[&switch].fsi_change_since(&stress[&(switch - Duration::days(1))], false)
    };
    // Gleiche Anleihen: Der Sprung ist eine Kreditbewegung
    let same = change_at_switch(spreads.keys().map(|d| (*d, 1)).collect());
    assert!(same.unwrap() > 0.1, "{same:?}");
    // Neue Anleihe im Korb: derselbe Sprung zählt nicht als Bewegung
    let switched = change_at_switch(spreads.keys().map(|d| (*d, if *d < switch { 1 } else { 2 })).collect());
    assert_eq!(switched, None);
}
