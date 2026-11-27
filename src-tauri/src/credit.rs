//! Anleihemathematik für die Emittenten-Spreads, ohne Ein- und Ausgabe.
//!
//! - Beschreibungen wie "ORCL 6 1/8 07/08/39" in Kupon und Fälligkeit zerlegen
//!   (die IB-Felder `coupon` und `maturity` sind bei Unternehmensanleihen oft leer).
//! - Rendite aus dem Clean-Kurs nach US-Unternehmenskonvention (30/360,
//!   halbjährlicher Kupon, Valuta einen Geschäftstag später).
//! - G-Spread gegen die lineare Treasury-Kurve (Par-Kurve von Treasury.gov,
//!   konstante Laufzeiten).
//! - Auswahl repräsentativer Anleihen je Emittent, Tagesniveau als Mittelwert und
//!   verkettete Veränderungsreihe, beide stichtagsgetreu.

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Treasury-Laufzeiten: interne Reihenkennung, Spaltenname in der Treasury.gov-CSV,
/// Laufzeit in Jahren.
pub const TREASURY_SERIES: [(&str, &str, f64); 11] = [
    ("UST1M", "1 Mo", 1.0 / 12.0),
    ("UST3M", "3 Mo", 0.25),
    ("UST6M", "6 Mo", 0.5),
    ("UST1Y", "1 Yr", 1.0),
    ("UST2Y", "2 Yr", 2.0),
    ("UST3Y", "3 Yr", 3.0),
    ("UST5Y", "5 Yr", 5.0),
    ("UST7Y", "7 Yr", 7.0),
    ("UST10Y", "10 Yr", 10.0),
    ("UST20Y", "20 Yr", 20.0),
    ("UST30Y", "30 Yr", 30.0),
];

pub fn is_treasury_series(id: &str) -> bool {
    TREASURY_SERIES.iter().any(|(k, _, _)| *k == id)
}

/// Laufzeitfenster der ausgewählten Anleihen (Restlaufzeit in Jahren) und Ziel in der Mitte.
pub const SELECT_MIN_YEARS: f64 = 2.0;
pub const SELECT_MAX_YEARS: f64 = 12.0;
pub const SELECT_TARGET_YEARS: f64 = 5.0;
/// Bestehende Auswahl bleibt, solange jede Anleihe noch so lange läuft.
pub const KEEP_MIN_YEARS: f64 = 1.5;
/// Fehlt einer Anleihe ein Kurstag, gilt ihr letzter Spread so viele Kalendertage weiter
/// (Wochenende plus Feiertag), damit Lücken keine Sprünge im Tagesmittel erzeugen.
pub const SPREAD_CARRY_DAYS: i64 = 4;
pub const BONDS_PER_ISSUER: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct BondTerms {
    pub coupon_pct: f64,
    pub maturity: NaiveDate,
}

/// Zerlegt "ORCL 4.2 09/27/29", "ORCL 6 1/8 07/08/39" oder "CRWV 9 02/01/31".
/// Variable Kupons ("Float") und unlesbare Beschreibungen ergeben None.
pub fn parse_bond_description(desc: &str) -> Option<BondTerms> {
    let tokens: Vec<&str> = desc.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }
    let date_tok = *tokens.last()?;
    let maturity = parse_us_date(date_tok)?;
    let mut coupon = 0.0;
    let mut seen = false;
    for tok in &tokens[1..tokens.len() - 1] {
        if let Some((n, d)) = tok.split_once('/') {
            let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
            if d <= 0.0 {
                return None;
            }
            coupon += n / d;
        } else {
            coupon += tok.parse::<f64>().ok()?;
        }
        seen = true;
    }
    (seen && coupon.is_finite() && (0.0..30.0).contains(&coupon)).then_some(BondTerms {
        coupon_pct: coupon,
        maturity,
    })
}

fn parse_us_date(tok: &str) -> Option<NaiveDate> {
    let mut parts = tok.split('/');
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    let y_raw: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let y = if y_raw < 100 { 2000 + y_raw } else { y_raw };
    NaiveDate::from_ymd_opt(y, m, d)
}

/// Tage nach 30/360 (US, Bond Basis).
pub fn days_30_360(a: NaiveDate, b: NaiveDate) -> i64 {
    let (y1, m1, mut d1) = (a.year() as i64, a.month() as i64, a.day() as i64);
    let (y2, m2, mut d2) = (b.year() as i64, b.month() as i64, b.day() as i64);
    if d1 == 31 {
        d1 = 30;
    }
    if d2 == 31 && d1 >= 30 {
        d2 = 30;
    }
    360 * (y2 - y1) + 30 * (m2 - m1) + (d2 - d1)
}

fn add_months_clamped(date: NaiveDate, months: i32) -> NaiveDate {
    let total = date.year() * 12 + date.month0() as i32 + months;
    let (y, m0) = (total.div_euclid(12), total.rem_euclid(12) as u32);
    let mut day = date.day();
    loop {
        if let Some(d) = NaiveDate::from_ymd_opt(y, m0 + 1, day) {
            return d;
        }
        day -= 1;
    }
}

/// Valuta: nächster Wochentag nach dem Handelstag (Feiertage bleiben unberücksichtigt).
pub fn settlement_date(trade: NaiveDate) -> NaiveDate {
    let mut d = trade + Duration::days(1);
    while matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
        d += Duration::days(1);
    }
    d
}

/// Kupontermine: vorheriger Termin bis einschließlich Valuta und alle späteren bis zur Fälligkeit.
fn coupon_schedule(settle: NaiveDate, maturity: NaiveDate) -> Option<(NaiveDate, Vec<NaiveDate>)> {
    if settle >= maturity {
        return None;
    }
    let mut future = Vec::new();
    let mut k = 0;
    loop {
        let d = add_months_clamped(maturity, -6 * k);
        if d <= settle {
            future.reverse();
            return Some((d, future));
        }
        future.push(d);
        k += 1;
        if k > 400 {
            return None;
        }
    }
}

/// Anteil der laufenden Kuponperiode, der bis zur Valuta verstrichen ist (30/360).
/// Stückzinsen und Abzinsung nutzen denselben Anteil; 30/360 ist über einen 31. als
/// Zwischentermin nicht additiv, getrennte Zählungen wichen sonst um einen Tag ab.
fn elapsed_fraction(settle: NaiveDate, prev: NaiveDate, next: NaiveDate) -> f64 {
    let period = days_30_360(prev, next).max(1);
    // Mindestens ein Tag Rest: Sonst wäre der letzte Kupon unabhängig von der Rendite
    let elapsed = days_30_360(prev, settle).clamp(0, period - 1);
    elapsed as f64 / period as f64
}

/// Dirty-Kurs je 100 Nominal zur Rendite `y` (Dezimal, halbjährlich verzinst).
fn dirty_price(y: f64, coupon_pct: f64, settle: NaiveDate, prev: NaiveDate, future: &[NaiveDate]) -> f64 {
    let c = coupon_pct / 2.0;
    let w = 1.0 - elapsed_fraction(settle, prev, future[0]);
    let v = 1.0 / (1.0 + y / 2.0);
    let n = future.len();
    let mut pv = 0.0;
    for k in 0..n {
        let t = k as f64 + w;
        pv += c * v.powf(t);
    }
    pv + 100.0 * v.powf((n - 1) as f64 + w)
}

/// Stückzinsen je 100 Nominal.
pub fn accrued_interest(coupon_pct: f64, settle: NaiveDate, maturity: NaiveDate) -> Option<f64> {
    let (prev, future) = coupon_schedule(settle, maturity)?;
    Some(coupon_pct / 2.0 * elapsed_fraction(settle, prev, future[0]))
}

/// Rendite in Prozent aus dem Clean-Kurs (je 100 Nominal), per Bisektion.
pub fn yield_from_clean_price(clean: f64, terms: &BondTerms, trade_date: NaiveDate) -> Option<f64> {
    if !(clean.is_finite() && clean > 1.0 && clean < 300.0) {
        return None;
    }
    let settle = settlement_date(trade_date);
    let (prev, future) = coupon_schedule(settle, terms.maturity)?;
    let accrued = accrued_interest(terms.coupon_pct, settle, terms.maturity)?;
    let target = clean + accrued;
    let f = |y: f64| dirty_price(y, terms.coupon_pct, settle, prev, &future) - target;
    let (mut lo, mut hi) = (-0.2_f64, 2.0_f64);
    let (mut flo, fhi) = (f(lo), f(hi));
    if !(flo > 0.0 && fhi < 0.0) {
        return None;
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        let fm = f(mid);
        if fm.abs() < 1e-10 || hi - lo < 1e-12 {
            return Some(mid * 100.0);
        }
        if (fm > 0.0) == (flo > 0.0) {
            lo = mid;
            flo = fm;
        } else {
            hi = mid;
        }
    }
    Some(0.5 * (lo + hi) * 100.0)
}

/// Treasury-Rendite (Prozent) für eine Restlaufzeit, linear zwischen den Stützstellen,
/// außerhalb flach. Braucht mindestens drei Stützstellen.
pub fn treasury_yield(curve: &[(f64, f64)], years: f64) -> Option<f64> {
    let mut pts: Vec<(f64, f64)> = curve.iter().copied().filter(|(t, y)| t.is_finite() && y.is_finite()).collect();
    if pts.len() < 3 || !years.is_finite() {
        return None;
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    if years <= pts[0].0 {
        return Some(pts[0].1);
    }
    if years >= pts[pts.len() - 1].0 {
        return Some(pts[pts.len() - 1].1);
    }
    pts.windows(2).find_map(|w| {
        let ((t0, y0), (t1, y1)) = (w[0], w[1]);
        (years >= t0 && years <= t1).then(|| y0 + (y1 - y0) * (years - t0) / (t1 - t0))
    })
}

pub fn years_between(a: NaiveDate, b: NaiveDate) -> f64 {
    (b - a).num_days() as f64 / 365.25
}

/// Treasury-Kurve je Tag, fehlende Tageswerte (Feiertage) bis 5 Tage fortgeschrieben.
pub fn treasury_curve_asof(series: &BTreeMap<String, BTreeMap<NaiveDate, f64>>, day: NaiveDate) -> Vec<(f64, f64)> {
    TREASURY_SERIES
        .iter()
        .filter_map(|(id, _, tenor)| {
            let s = series.get(*id)?;
            let (d, v) = s.range(..=day).next_back()?;
            ((day - *d).num_days() <= 5).then_some((*tenor, *v))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Auswahl und Aggregation
// ---------------------------------------------------------------------------

/// Eine Anleihe aus der IB-Vertragssuche.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BondListing {
    pub conid: i32,
    pub description: String,
    pub cusip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectedBond {
    pub conid: i32,
    pub description: String,
    pub coupon_pct: f64,
    pub maturity: NaiveDate,
}

/// Reiht die Anleihen eines Emittenten für den Abruf: zuerst die bisher verwendeten
/// (solange gelistet und mindestens 1,5 Jahre Restlaufzeit), dann Anleihen mit fester
/// Kuponrate und 2 bis 12 Jahren Restlaufzeit, die nächsten an fünf Jahren vorn.
/// Mehrfach gelistete Tranchen (144A, Reg S) zählen einmal, bevorzugt mit CUSIP.
/// Der Abruf nimmt der Reihe nach die ersten drei mit ausreichender Kurshistorie; so
/// bleibt der Korb stabil und Anleihen ohne Kurse bei IB fallen von selbst heraus.
pub fn rank_bond_candidates(listings: &[BondListing], previous: &[SelectedBond], today: NaiveDate) -> Vec<SelectedBond> {
    let listed: std::collections::HashSet<i32> = listings.iter().map(|l| l.conid).collect();
    let mut out: Vec<SelectedBond> = previous
        .iter()
        .filter(|b| listed.contains(&b.conid) && years_between(today, b.maturity) >= KEEP_MIN_YEARS)
        .cloned()
        .collect();
    let term_key = |coupon: f64, maturity: NaiveDate| ((coupon * 1000.0).round() as i64, maturity);
    let taken: std::collections::HashSet<(i64, NaiveDate)> =
        out.iter().map(|b| term_key(b.coupon_pct, b.maturity)).collect();

    let mut by_terms: BTreeMap<(i64, NaiveDate), (&BondListing, BondTerms)> = BTreeMap::new();
    for l in listings {
        let Some(terms) = parse_bond_description(&l.description) else {
            continue;
        };
        let years = years_between(today, terms.maturity);
        let key = term_key(terms.coupon_pct, terms.maturity);
        if !(SELECT_MIN_YEARS..=SELECT_MAX_YEARS).contains(&years) || taken.contains(&key) {
            continue;
        }
        let better = match by_terms.get(&key) {
            None => true,
            Some((cur, _)) => {
                let (cur_has, new_has) = (!cur.cusip.is_empty(), !l.cusip.is_empty());
                (new_has && !cur_has) || (new_has == cur_has && l.conid < cur.conid)
            }
        };
        if better {
            by_terms.insert(key, (l, terms));
        }
    }
    let mut candidates: Vec<(&BondListing, BondTerms)> = by_terms.into_values().collect();
    candidates.sort_by(|a, b| {
        let da = (years_between(today, a.1.maturity) - SELECT_TARGET_YEARS).abs();
        let db = (years_between(today, b.1.maturity) - SELECT_TARGET_YEARS).abs();
        da.total_cmp(&db).then(a.0.conid.cmp(&b.0.conid))
    });
    out.extend(candidates.into_iter().map(|(l, t)| SelectedBond {
        conid: l.conid,
        description: l.description.clone(),
        coupon_pct: t.coupon_pct,
        maturity: t.maturity,
    }));
    out
}

/// Spread (Basispunkte) je Anleihe und Tag: Rendite aus dem Kurs minus Treasury bei
/// gleicher Restlaufzeit.
pub fn bond_spread_series(
    bond: &SelectedBond,
    prices: &BTreeMap<NaiveDate, f64>,
    treasury: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
) -> BTreeMap<NaiveDate, f64> {
    let terms = BondTerms {
        coupon_pct: bond.coupon_pct,
        maturity: bond.maturity,
    };
    prices
        .iter()
        .filter_map(|(d, clean)| {
            let ytm = yield_from_clean_price(*clean, &terms, *d)?;
            let curve = treasury_curve_asof(treasury, *d);
            let tsy = treasury_yield(&curve, years_between(*d, bond.maturity))?;
            Some((*d, (ytm - tsy) * 100.0))
        })
        .collect()
}

/// Zeitraum, in dem eine Anleihe zum Korb gehörte: von `from` bis einschließlich `to`
/// (`None`: gehört weiter dazu).
pub type BasketInterval = (NaiveDate, Option<NaiveDate>);

/// Eine Anleihe des Korbs mit den Zeiträumen ihrer Mitgliedschaft.
#[derive(Debug, Clone, PartialEq)]
pub struct BasketBond {
    pub bond: SelectedBond,
    pub active: Vec<BasketInterval>,
}

impl BasketBond {
    /// Mitglied über die ganze Historie (erster Korb eines Emittenten).
    pub fn always(bond: SelectedBond) -> Self {
        Self { bond, active: vec![(NaiveDate::MIN, None)] }
    }
}

/// Tagesspreads einer Anleihe, beschränkt auf ihre Zeit im Korb.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BondSpreads {
    pub conid: i32,
    pub spreads: BTreeMap<NaiveDate, f64>,
    pub active: Vec<BasketInterval>,
}

impl BondSpreads {
    pub fn new(conid: i32, spreads: BTreeMap<NaiveDate, f64>, active: Vec<BasketInterval>) -> Self {
        let inside = |d: &NaiveDate| active.iter().any(|(f, t)| *d >= *f && t.is_none_or(|t| *d <= t));
        let spreads = spreads.into_iter().filter(|(d, _)| inside(d)).collect();
        Self { conid, spreads, active }
    }

    /// Beginn des Korbzeitraums, in dem `day` liegt.
    fn interval_start(&self, day: NaiveDate) -> Option<NaiveDate> {
        self.active
            .iter()
            .filter(|(f, t)| day >= *f && t.is_none_or(|t| day <= t))
            .map(|(f, _)| *f)
            .max()
    }

    /// Letzter Spread bis einschließlich `day`, höchstens [`SPREAD_CARRY_DAYS`] alt und nur
    /// innerhalb des laufenden Korbzeitraums: Nach dem Austritt zählt die Anleihe nicht mehr.
    fn carried(&self, day: NaiveDate) -> Option<f64> {
        let from = self.interval_start(day)?;
        let (d, v) = self.spreads.range(from..=day).next_back()?;
        ((day - *d).num_days() <= SPREAD_CARRY_DAYS).then_some(*v)
    }
}

fn union_dates(bonds: &[BondSpreads]) -> Vec<NaiveDate> {
    bonds
        .iter()
        .flat_map(|b| b.spreads.keys().copied())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Stichtagsgetreues Niveau: je Tag der Mittelwert der Anleihen, die an diesem Tag zum Korb
/// gehören und bis dahin notiert haben (fehlende Tage mit dem letzten Spread überbrückt).
/// Spätere Kurse, später hinzukommende oder ausscheidende Anleihen ändern frühere Tage nicht.
/// Kommt eine Anleihe neu dazu, verschiebt sich das Niveau an diesem Tag um ihren Beitrag.
pub fn point_in_time_mean(bonds: &[BondSpreads]) -> BTreeMap<NaiveDate, f64> {
    union_dates(bonds)
        .into_iter()
        .filter_map(|d| {
            let v: Vec<f64> = bonds.iter().filter_map(|b| b.carried(d)).collect();
            (!v.is_empty()).then(|| (d, v.iter().sum::<f64>() / v.len() as f64))
        })
        .collect()
}

/// Fingerabdruck der Anleihen, die an einem Tag ins Niveau eingehen. Ändert er sich, ist ein
/// Sprung im Niveau ein Korbwechsel und keine Kreditbewegung.
pub fn basket_members(bonds: &[BondSpreads]) -> BTreeMap<NaiveDate, u64> {
    use std::hash::{Hash, Hasher};
    union_dates(bonds)
        .into_iter()
        .filter_map(|d| {
            let ids: Vec<i32> = bonds.iter().filter(|b| b.carried(d).is_some()).map(|b| b.conid).collect();
            if ids.is_empty() {
                return None;
            }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            ids.hash(&mut h);
            Some((d, h.finish()))
        })
        .collect()
}

/// Verkettete Veränderungsreihe in Basispunkten, ab 0 am ersten Tag: Je Tag zählt die
/// mittlere Veränderung der Anleihen, die an beiden Tagen einen Spread haben. Ein Wechsel im
/// Korb erzeugt deshalb keinen Sprung. Nur für Veränderungen über die Zeit gedacht, nicht als
/// Niveau. Rechnet vorwärts, spätere Daten ändern frühere Werte nicht.
pub fn linked_changes(bonds: &[BondSpreads]) -> BTreeMap<NaiveDate, f64> {
    let dates = union_dates(bonds);
    let mut out = BTreeMap::new();
    let Some(first) = dates.first() else {
        return out;
    };
    let mut level = 0.0;
    out.insert(*first, level);
    for w in dates.windows(2) {
        let (d0, d1) = (w[0], w[1]);
        let moves: Vec<f64> = bonds
            .iter()
            .filter_map(|b| Some(b.carried(d1)? - b.carried(d0)?))
            .collect();
        if !moves.is_empty() {
            level += moves.iter().sum::<f64>() / moves.len() as f64;
        }
        out.insert(d1, level);
    }
    out
}

/// Tagesniveau, verkettete Veränderungen und Korbzusammensetzung eines Emittenten.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IssuerSpreads {
    pub level: BTreeMap<NaiveDate, f64>,
    pub linked: BTreeMap<NaiveDate, f64>,
    pub members: BTreeMap<NaiveDate, u64>,
}

/// Tagesspreads (Basispunkte) eines Emittenten über die Anleihen seines Korbs, jede nur in
/// den Zeiträumen, in denen sie dazugehörte.
pub fn issuer_spread_series(
    basket: &[BasketBond],
    prices: &BTreeMap<i32, BTreeMap<NaiveDate, f64>>,
    treasury: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
) -> IssuerSpreads {
    let bonds: Vec<BondSpreads> = basket
        .iter()
        .filter_map(|b| {
            let px = prices.get(&b.bond.conid)?;
            Some(BondSpreads::new(b.bond.conid, bond_spread_series(&b.bond, px, treasury), b.active.clone()))
        })
        .filter(|s| !s.spreads.is_empty())
        .collect();
    IssuerSpreads {
        level: point_in_time_mean(&bonds),
        linked: linked_changes(&bonds),
        members: basket_members(&bonds),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn parses_ib_bond_descriptions() {
        assert_eq!(
            parse_bond_description("ORCL 4.2 09/27/29"),
            Some(BondTerms { coupon_pct: 4.2, maturity: d(2029, 9, 27) })
        );
        assert_eq!(
            parse_bond_description("ORCL 6 1/8 07/08/39"),
            Some(BondTerms { coupon_pct: 6.125, maturity: d(2039, 7, 8) })
        );
        assert_eq!(
            parse_bond_description("XYZ 9 02/01/31"),
            Some(BondTerms { coupon_pct: 9.0, maturity: d(2031, 2, 1) })
        );
        assert_eq!(parse_bond_description("XYZ Float 09/29/30"), None);
        assert_eq!(parse_bond_description("XYZ 5 3/8"), None);
        assert_eq!(parse_bond_description(""), None);
    }

    #[test]
    fn thirty_360_counts_like_bond_basis() {
        assert_eq!(days_30_360(d(2026, 1, 15), d(2026, 7, 15)), 180);
        assert_eq!(days_30_360(d(2026, 1, 31), d(2026, 7, 31)), 180);
        assert_eq!(days_30_360(d(2026, 2, 28), d(2026, 3, 31)), 33);
    }

    #[test]
    fn par_bond_on_coupon_date_yields_its_coupon() {
        // Handelstag so gewählt, dass die Valuta genau auf einen Kupontermin fällt
        let terms = BondTerms { coupon_pct: 5.0, maturity: d(2031, 7, 15) };
        let trade = d(2026, 7, 14); // Dienstag, Valuta Mittwoch 15.7.
        let y = yield_from_clean_price(100.0, &terms, trade).unwrap();
        assert!((y - 5.0).abs() < 1e-6, "{y}");
    }

    #[test]
    fn price_and_yield_round_trip_between_coupon_dates() {
        let terms = BondTerms { coupon_pct: 6.125, maturity: d(2034, 3, 1) };
        let trade = d(2026, 11, 27);
        let settle = settlement_date(trade);
        let (prev, future) = coupon_schedule(settle, terms.maturity).unwrap();
        for y in [0.03, 0.065, 0.12, 0.25] {
            let dirty = dirty_price(y, terms.coupon_pct, settle, prev, &future);
            let clean = dirty - accrued_interest(terms.coupon_pct, settle, terms.maturity).unwrap();
            let back = yield_from_clean_price(clean, &terms, trade).unwrap();
            assert!((back - y * 100.0).abs() < 1e-6, "{y} -> {back}");
        }
    }

    #[test]
    fn discount_bond_yields_more_than_coupon() {
        let terms = BondTerms { coupon_pct: 4.0, maturity: d(2031, 6, 1) };
        let y = yield_from_clean_price(90.0, &terms, d(2026, 11, 27)).unwrap();
        assert!(y > 6.0 && y < 7.0, "{y}");
        assert_eq!(yield_from_clean_price(f64::NAN, &terms, d(2026, 11, 27)), None);
        assert_eq!(yield_from_clean_price(95.0, &terms, d(2031, 6, 1)), None);
    }

    #[test]
    fn treasury_curve_interpolates_and_holds_flat_outside() {
        let curve = vec![(1.0, 4.0), (5.0, 4.4), (10.0, 4.9)];
        assert!((treasury_yield(&curve, 3.0).unwrap() - 4.2).abs() < 1e-12);
        assert_eq!(treasury_yield(&curve, 0.5), Some(4.0));
        assert_eq!(treasury_yield(&curve, 40.0), Some(4.9));
        assert_eq!(treasury_yield(&curve[..2], 3.0), None);
    }

    fn listing(conid: i32, desc: &str, cusip: &str) -> BondListing {
        BondListing { conid, description: desc.into(), cusip: cusip.into() }
    }

    #[test]
    fn ranking_prefers_five_years_dedupes_tranches_and_keeps_previous_first() {
        let today = d(2026, 11, 27);
        let listings = vec![
            listing(1, "XYZ 3 1/4 11/15/27", "A1"), // zu kurz
            listing(2, "XYZ 5 1/4 02/03/32", ""),   // Reg S ohne CUSIP
            listing(3, "XYZ 5 1/4 02/03/32", "A3"), // gleiche Tranche mit CUSIP
            listing(4, "XYZ 4.2 09/27/29", "A4"),
            listing(5, "XYZ 4.7 09/27/34", "A5"),
            listing(6, "XYZ Float 09/29/30", "A6"),
            listing(7, "XYZ 6 08/03/55", "A7"), // zu lang
            listing(8, "XYZ 6 1/4 11/09/32", "A8"),
        ];
        let ranked = rank_bond_candidates(&listings, &[], today);
        let ids: Vec<i32> = ranked.iter().map(|b| b.conid).collect();
        assert_eq!(ids, vec![3, 8, 4, 5]);
        // Bisheriger Korb steht vorn, auch wenn eine neue Anleihe näher an fünf Jahren läge
        let mut more = listings.clone();
        more.push(listing(9, "XYZ 5 11/27/31", "A9"));
        let prev: Vec<SelectedBond> = ranked[..2].to_vec();
        let re = rank_bond_candidates(&more, &prev, today);
        let ids: Vec<i32> = re.iter().map(|b| b.conid).collect();
        assert_eq!(ids, vec![3, 8, 9, 4, 5]);
        // Nicht mehr gelistete bisherige Anleihen fallen heraus, ihre Zwillingstranche nicht
        let without_3: Vec<BondListing> = more.into_iter().filter(|l| l.conid != 3).collect();
        let re = rank_bond_candidates(&without_3, &prev, today);
        assert_eq!(re[0].conid, 8);
        assert!(re.iter().any(|b| b.conid == 2));
    }

    fn pts(values: &[(u32, f64)]) -> BTreeMap<NaiveDate, f64> {
        values.iter().map(|(day, v)| (d(2026, 11, *day), *v)).collect()
    }

    fn always(conid: i32, values: &[(u32, f64)]) -> BondSpreads {
        BondSpreads::new(conid, pts(values), vec![(NaiveDate::MIN, None)])
    }

    #[test]
    fn level_is_point_in_time_and_later_bonds_leave_history_alone() {
        // Anleihe A steigt am 2. um 10 bp, Anleihe B kommt am 3. mit höherem Niveau dazu
        let a = always(1, &[(2, 300.0), (3, 310.0), (4, 310.0)]);
        let b = always(2, &[(4, 410.0)]);
        let only_a = point_in_time_mean(std::slice::from_ref(&a));
        let both = point_in_time_mean(&[a, b]);
        assert_eq!(both[&d(2026, 11, 2)], 300.0);
        assert_eq!(both[&d(2026, 11, 3)], 310.0);
        assert_eq!(both[&d(2026, 11, 4)], 360.0);
        // Frühere Tage sind identisch mit der Reihe ohne die spätere Anleihe
        for (day, v) in &only_a {
            if *day < d(2026, 11, 4) {
                assert_eq!(both[day], *v);
            }
        }
        assert!(point_in_time_mean(&[]).is_empty());
    }

    #[test]
    fn level_bridges_short_gaps_without_spikes() {
        // B fehlt am 3.: Ohne Überbrückung fiele das Mittel auf A allein
        let a = always(1, &[(2, 300.0), (3, 300.0), (4, 300.0)]);
        let b = always(2, &[(2, 500.0), (4, 500.0)]);
        let out = point_in_time_mean(&[a, b.clone()]);
        assert_eq!(out[&d(2026, 11, 3)], 400.0);
        // Nach mehr als SPREAD_CARRY_DAYS fällt eine Anleihe ohne Kurse heraus
        let a_long = always(1, &[(2, 300.0), (12, 300.0)]);
        let out = point_in_time_mean(&[a_long, b]);
        assert_eq!(out[&d(2026, 11, 12)], 300.0);
    }

    #[test]
    fn linked_changes_ignore_basket_changes_but_keep_moves() {
        let a = always(1, &[(2, 300.0), (3, 310.0), (4, 310.0), (5, 320.0)]);
        let b = always(2, &[(4, 410.0), (5, 430.0)]);
        let out = linked_changes(&[a, b]);
        assert_eq!(out[&d(2026, 11, 2)], 0.0);
        assert_eq!(out[&d(2026, 11, 3)], 10.0);
        // Eintritt von B am 4.: kein Sprung
        assert_eq!(out[&d(2026, 11, 4)], 10.0);
        // Am 5. steigen A um 10 und B um 20: Mittel 15
        assert_eq!(out[&d(2026, 11, 5)], 25.0);
        assert!(linked_changes(&[]).is_empty());
    }

    #[test]
    fn basket_intervals_keep_history_and_mark_changes() {
        // A gehört bis zum 4. zum Korb, B ab dem 5.; B hat rückwirkend Kurse ab dem 2.
        let a = BondSpreads::new(1, pts(&[(2, 300.0), (3, 300.0), (4, 300.0), (5, 300.0)]), vec![(d(2026, 11, 1), Some(d(2026, 11, 4)))]);
        let b = BondSpreads::new(2, pts(&[(2, 500.0), (3, 500.0), (4, 500.0), (5, 500.0), (6, 500.0)]), vec![(d(2026, 11, 5), None)]);
        let level = point_in_time_mean(&[a.clone(), b.clone()]);
        // Vor dem Eintritt zählen die rückwirkenden Kurse von B nicht, nach dem Austritt A nicht mehr
        assert_eq!(level[&d(2026, 11, 4)], 300.0);
        assert_eq!(level[&d(2026, 11, 5)], 500.0);
        assert_eq!(level[&d(2026, 11, 6)], 500.0);
        let members = basket_members(&[a.clone(), b.clone()]);
        assert_eq!(members[&d(2026, 11, 3)], members[&d(2026, 11, 4)]);
        assert_ne!(members[&d(2026, 11, 4)], members[&d(2026, 11, 5)]);
        // Der Wechsel ist keine Bewegung
        let linked = linked_changes(&[a, b]);
        assert_eq!(linked[&d(2026, 11, 6)], 0.0);
        // Ein Wiedereintritt überbrückt nicht die Zeit außerhalb des Korbs
        let back = BondSpreads::new(
            3,
            pts(&[(2, 400.0), (9, 410.0)]),
            vec![(d(2026, 11, 1), Some(d(2026, 11, 2))), (d(2026, 11, 5), None)],
        );
        assert_eq!(back.carried(d(2026, 11, 5)), None);
        assert_eq!(back.carried(d(2026, 11, 9)), Some(410.0));
    }

    #[test]
    fn issuer_spread_is_mean_over_bonds_against_matching_treasury() {
        let day = d(2026, 11, 27);
        let mut tsy = BTreeMap::new();
        for (id, _, _) in TREASURY_SERIES {
            tsy.insert(id.to_string(), BTreeMap::from([(day, 4.0)]));
        }
        let bonds = [
            SelectedBond { conid: 1, description: "A".into(), coupon_pct: 5.0, maturity: d(2031, 7, 15) },
            SelectedBond { conid: 2, description: "B".into(), coupon_pct: 5.0, maturity: d(2033, 7, 15) },
            SelectedBond { conid: 3, description: "C".into(), coupon_pct: 5.0, maturity: d(2035, 7, 15) },
        ];
        let mut prices = BTreeMap::new();
        prices.insert(1, BTreeMap::from([(day, 100.0)]));
        prices.insert(2, BTreeMap::from([(day, 95.0)]));
        prices.insert(3, BTreeMap::from([(day, 80.0)]));
        let basket: Vec<BasketBond> = bonds.iter().cloned().map(BasketBond::always).collect();
        let s = issuer_spread_series(&basket, &prices, &tsy).level;
        let expect: f64 = bonds
            .iter()
            .map(|b| {
                let px = prices[&b.conid][&day];
                let terms = BondTerms { coupon_pct: b.coupon_pct, maturity: b.maturity };
                (yield_from_clean_price(px, &terms, day).unwrap() - 4.0) * 100.0
            })
            .sum::<f64>()
            / 3.0;
        assert!((s[&day] - expect).abs() < 1e-9);
    }
}
