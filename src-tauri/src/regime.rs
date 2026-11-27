//! Finanzierungsregime: verknüpft Kreditaufschläge mit der GPU-Forward-Kurve
//! und erkennt Regimewechsel.
//!
//! Reiner Rechenkern ohne Ein- und Ausgabe. Eingaben sind Tagesreihen aus der
//! lokalen Datenbank, Ausgabe ist ein [`RegimeReport`] für die Oberfläche.
//!
//! Modellannahme: Der GPU-Mietpreis hängt in zwei Richtungen an der Finanzierung.
//! Teure Finanzierung bremst neue Rechenzentren und stützt damit die Mieten
//! bestehender GPUs (Angebotskanal). Geraten die Mieter und Betreiber in eine
//! Notlage, kommen Kapazität und Flotten auf den Markt und die Mieten fallen mit
//! dem Stress (Notlagenkanal). Welcher Kanal überwiegt, bestimmt das Regime:
//!
//! - Ruhig: Kreditaufschläge normal, kein gemeinsamer Preisrückgang.
//! - Preisdruck: Mieten fallen, die Finanzierung ist ruhig (Angebot, Generationswechsel).
//! - Entkoppelt: Kredit unter Stress, die Kurve hält (Angebotskanal überwiegt).
//! - Übertragung: Kredit unter Stress und naher Median, unteres Quartil und
//!   Spot relativ zur Kurve fallen gemeinsam (Notlagenkanal überwiegt).
//!
//! Der Ornn-Spot geht nur als Basis zur nahen Kurve ein (Vergleich mit der
//! Forward-Kurve), nie als eigene Preisreihe.

use chrono::{Datelike, Duration, NaiveDate};
use serde::Serialize;
use std::collections::BTreeMap;

pub type Series = BTreeMap<NaiveDate, f64>;

// ---------------------------------------------------------------------------
// Parameter (eigene Setzungen, im README dokumentiert)
// ---------------------------------------------------------------------------

/// Horizont der nahen Kurve in Tagen (konstante Laufzeit statt rollierendem Verfall).
pub const NEAR_HORIZON_DAYS: i64 = 90;
/// Horizont des fernen Kurvenpunkts in Tagen.
pub const FAR_HORIZON_DAYS: i64 = 365;
/// Höchstabstand für flache Fortschreibung über den ersten bzw. letzten Verfall hinaus.
const MAX_FLAT_EXTRAPOLATION_DAYS: i64 = 45;

/// OAS-Werte gelten so lange fort (Wochenenden, Feiertage).
const OAS_MAX_AGE_DAYS: i64 = 5;
/// Emittenten-Spreads gelten so lange fort, danach als veraltet ausgeblendet.
const ISSUER_MAX_AGE_DAYS: i64 = 7;
/// Fenster und Mindestumfang des robusten z-Werts für OAS-Niveaus (Beobachtungen).
const OAS_Z_WINDOW: usize = 756;
const OAS_Z_MIN_OBS: usize = 120;

/// Stress gilt ab diesem Indexwert als hoch und endet erst unter dem Austrittswert.
pub const STRESS_ENTER: f64 = 1.0;
pub const STRESS_EXIT: f64 = 0.5;

/// Gemeinsamer Rückgang: naher Median und unteres Quartil fallen über 14 Tage um
/// mindestens 5 % oder über 30 Tage um mindestens 8 %.
const FALL_SHORT_DAYS: i64 = 14;
const FALL_LONG_DAYS: i64 = 30;
const FALL_SHORT_ENTER: f64 = -0.05;
const FALL_LONG_ENTER: f64 = -0.08;
/// Der Rückgang gilt als beendet, wenn der nahe Median über 14 Tage kaum noch fällt.
const FALL_SHORT_EXIT: f64 = -0.02;
const FALL_LONG_EXIT: f64 = -0.04;

/// Ein neues Regime gilt erst nach so vielen Tagen in Folge.
const CONFIRM_DAYS: usize = 3;
/// Übertragung ohne Bruchsignal oder negative Kopplung braucht längere Bestätigung.
const CONFIRM_DAYS_WITHOUT_EVIDENCE: usize = 7;

/// Kopplung: Änderung über 7 Tage, Finanzierung 7 Tage vorlaufend, Fenster 90 Tage.
const COUPLING_STEP_DAYS: i64 = 7;
const COUPLING_LAG_DAYS: i64 = 7;
const COUPLING_WINDOW_DAYS: i64 = 90;
const COUPLING_MIN_POINTS: usize = 20;
const COUPLING_NEGATIVE_CORR: f64 = -0.3;

/// Bruchpunkt-Erkennung: erwartete Regimedauer 100 Tage, "frisch" heißt jünger als 7 Tage.
const BOCPD_HAZARD_LAMBDA: f64 = 100.0;
const BOCPD_MAX_RUN: usize = 400;
const BOCPD_RECENT: usize = 7;
const BOCPD_SIGNAL: f64 = 0.5;
/// Vor so vielen Beobachtungen ist die Bruchwahrscheinlichkeit nicht aussagekräftig.
/// Posterior-Updates, bevor ein Bruch zählt; dazu kommen [`BOCPD_SCALE_MIN_OBS`] Beobachtungen für die Skala.
const BOCPD_WARMUP: usize = 20;
/// Normierung der Bruchpunkt-Erkennung: robuste Streuung der vorherigen Beobachtungen,
/// höchstens so viele, und erst ab so vielen.
const BOCPD_SCALE_WINDOW: usize = 756;
const BOCPD_SCALE_MIN_OBS: usize = 10;
/// Untergrenze der Skala, wenn alle vorherigen Werte gleich sind (etwa eine flache Kurve).
const BOCPD_SCALE_FLOOR: f64 = 1e-6;
/// Liegt die letzte vollständige Einstufung länger zurück, gilt das Regime als unbekannt.
const REGIME_STALE_DAYS: i64 = 3;

/// Die GPU-Seite braucht mindestens so viele Kurventage (14-Tage-Änderung plus heute).
pub const CURVE_DAYS_NEEDED: usize = FALL_SHORT_DAYS as usize + 1;

/// Implizite Ratingstufe, ab der ein Emittent als Notlage gilt (CCC-Bereich).
const DISTRESS_POSITION: f64 = 5.75;

/// Länge der ausgegebenen Historie.
const HISTORY_DAYS: i64 = 400;

// ---------------------------------------------------------------------------
// Ratingstufen und OAS-Reihen
// ---------------------------------------------------------------------------

/// Ratingstufen der ICE-BofA-OAS-Indizes (je Stufe drei Notches, CCC inklusive darunter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RatingBucket {
    Aaa,
    Aa,
    A,
    Bbb,
    Bb,
    B,
    Ccc,
}

impl RatingBucket {
    pub const ALL: [RatingBucket; 7] = [
        RatingBucket::Aaa,
        RatingBucket::Aa,
        RatingBucket::A,
        RatingBucket::Bbb,
        RatingBucket::Bb,
        RatingBucket::B,
        RatingBucket::Ccc,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            RatingBucket::Aaa => "AAA",
            RatingBucket::Aa => "AA",
            RatingBucket::A => "A",
            RatingBucket::Bbb => "BBB",
            RatingBucket::Bb => "BB",
            RatingBucket::B => "B",
            RatingBucket::Ccc => "CCC",
        }
    }

    /// FRED-Kennung des ICE-BofA-OAS-Index dieser Stufe.
    pub fn oas_series(self) -> &'static str {
        match self {
            RatingBucket::Aaa => "BAMLC0A1CAAA",
            RatingBucket::Aa => "BAMLC0A2CAA",
            RatingBucket::A => "BAMLC0A3CA",
            RatingBucket::Bbb => "BAMLC0A4CBBB",
            RatingBucket::Bb => "BAMLH0A1HYBB",
            RatingBucket::B => "BAMLH0A2HYB",
            RatingBucket::Ccc => "BAMLH0A3HYC",
        }
    }

    /// Liest Agenturratings wie "BBB-", "B+", "Baa2" oder "CCC" und ordnet sie der Stufe zu.
    pub fn from_rating(raw: &str) -> Option<RatingBucket> {
        let s: String = raw
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_uppercase();
        // Moody's-Schreibweise zuerst, weil "BAA" sonst als "B" gelesen würde
        let moodys = [
            ("AAA", RatingBucket::Aaa),
            ("AA", RatingBucket::Aa),
            ("A", RatingBucket::A),
            ("BAA", RatingBucket::Bbb),
            ("BA", RatingBucket::Bb),
            ("B", RatingBucket::B),
            ("CAA", RatingBucket::Ccc),
            ("CA", RatingBucket::Ccc),
            ("C", RatingBucket::Ccc),
        ];
        let sp = [
            ("AAA", RatingBucket::Aaa),
            ("AA", RatingBucket::Aa),
            ("A", RatingBucket::A),
            ("BBB", RatingBucket::Bbb),
            ("BB", RatingBucket::Bb),
            ("B", RatingBucket::B),
            ("CCC", RatingBucket::Ccc),
            ("CC", RatingBucket::Ccc),
            ("C", RatingBucket::Ccc),
            ("D", RatingBucket::Ccc),
        ];
        let is_moodys = raw.chars().any(|c| c.is_ascii_digit()) || s.starts_with("BAA") || s.starts_with("CAA") || s == "BA";
        let table: &[(&str, RatingBucket)] = if is_moodys { &moodys } else { &sp };
        table.iter().find(|(k, _)| *k == s).map(|(_, b)| *b)
    }
}

/// FRED-Kennungen der Makro-Komponenten des Stressindex.
pub const OAS_HY: &str = "BAMLH0A0HYM2";

/// Makro-Komponenten: Hochzins gesamt, CCC und BBB (Investment-Grade-Rand).
const MACRO_COMPONENTS: [(&str, &str); 3] = [
    (OAS_HY, "Hochzins-OAS"),
    ("BAMLH0A3HYC", "CCC-OAS"),
    ("BAMLC0A4CBBB", "BBB-OAS"),
];

// ---------------------------------------------------------------------------
// Eingaben
// ---------------------------------------------------------------------------

/// Ein Verfall der Polymarket-Kurve an einem Tag. Die Märkte rechnen gegen den
/// Monatsdurchschnitt ab, deshalb zählt die Monatsmitte als Laufzeitpunkt. Sobald der
/// Abrechnungsmonat begonnen hat, enthält der Kurs bereits realisierte Tage und fällt
/// aus der Kurve heraus.
#[derive(Debug, Clone)]
pub struct CurveNode {
    pub period_start: NaiveDate,
    pub expiry: NaiveDate,
    pub median: Option<f64>,
    pub p25: Option<f64>,
}

impl CurveNode {
    /// Baut einen Knoten aus dem Liefertag (letzter Tag des Abrechnungsmonats).
    pub fn from_delivery(delivery: NaiveDate, median: Option<f64>, p25: Option<f64>) -> CurveNode {
        let period_start = delivery.with_day(1).unwrap_or(delivery);
        let half = (delivery - period_start).num_days() / 2;
        CurveNode {
            period_start,
            expiry: period_start + Duration::days(half),
            median,
            p25,
        }
    }
}

/// Kurvenstand eines Tages; `spot` ist der Ornn-Index und dient nur als Basis zur Kurve.
#[derive(Debug, Clone)]
pub struct CurveDay {
    pub date: NaiveDate,
    pub spot: Option<f64>,
    pub nodes: Vec<CurveNode>,
}

/// Spread-Reihe eines Emittenten (G-Spread zur Treasury-Kurve, Basispunkte).
#[derive(Debug, Clone)]
pub struct IssuerSeries {
    pub key: String,
    pub label: String,
    /// Aktuelles Agenturrating aus den Einstellungen.
    pub agency_rating: String,
    /// Agenturrating je Gültigkeitsbeginn, aufsteigend. Leer: `agency_rating` gilt für alle Tage.
    pub ratings: Vec<(NaiveDate, String)>,
    /// Fingerabdruck der Anleihen, die je Beobachtungstag ins Niveau eingehen.
    pub members: BTreeMap<NaiveDate, u64>,
    /// Stichtagsgetreues Niveau (Mittel der Anleihen, die bis dahin notiert haben).
    pub spreads_bp: Series,
    /// Verkettete Veränderungen ohne Sprünge bei Korbwechseln, nur für Differenzen.
    pub linked_bp: Series,
    /// Kurzbeschreibung der verwendeten Anleihen (z. B. "ORCL 5 1/4 02/03/32").
    pub bonds: Vec<String>,
}

impl IssuerSeries {
    /// Agenturrating, das am Stichtag galt. Vor der ersten Eintragung gilt die erste.
    pub fn rating_on(&self, day: NaiveDate) -> &str {
        self.ratings
            .iter()
            .rev()
            .find(|(from, _)| *from <= day)
            .or(self.ratings.first())
            .map_or(self.agency_rating.as_str(), |(_, r)| r.as_str())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelInputs {
    pub gpu: String,
    /// OAS-Reihen in Prozentpunkten je FRED-Kennung.
    pub oas: BTreeMap<String, Series>,
    pub issuers: Vec<IssuerSeries>,
    pub curve: Vec<CurveDay>,
}

// ---------------------------------------------------------------------------
// Ausgaben
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    Unbekannt,
    Ruhig,
    Preisdruck,
    Entkoppelt,
    Uebertragung,
}

impl Regime {
    pub fn label(self) -> &'static str {
        match self {
            Regime::Unbekannt => "Zu wenig Daten",
            Regime::Ruhig => "Ruhig",
            Regime::Preisdruck => "Preisdruck ohne Kreditstress",
            Regime::Entkoppelt => "Kreditstress, Mieten fest",
            Regime::Uebertragung => "Übertragung auf die Mieten",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Regime::Unbekannt => "Für eine Einstufung fehlen Kredit- oder Kurvendaten. Die GPU-Seite braucht mindestens 15 Tage Kurvenhistorie.",
            Regime::Ruhig => "Kreditaufschläge im üblichen Rahmen, die nahe Kurve fällt nicht gemeinsam mit dem unteren Quartil.",
            Regime::Preisdruck => "Die Mieten fallen, die Finanzierung ist aber ruhig. Das spricht für Angebot, Generationswechsel oder eine Nachfragepause, nicht für eine Kreditkrise.",
            Regime::Entkoppelt => "Der Kredit steht unter Stress, die Kurve hält. Teure Finanzierung bremst neue Rechenzentren und stützt die Mieten bestehender GPUs.",
            Regime::Uebertragung => "Kreditstress und gemeinsamer Rückgang von nahem Median, unterem Quartil und Spot relativ zur Kurve: Der Stress erreicht die Mieten.",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentReading {
    pub key: String,
    pub label: String,
    /// Rohwert (OAS in Prozentpunkten).
    pub value: Option<f64>,
    /// Robuster z-Wert über drei Jahre.
    pub score: Option<f64>,
    pub as_of: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuerReading {
    pub key: String,
    pub label: String,
    pub bonds: Vec<String>,
    pub spread_bp: Option<f64>,
    pub as_of: Option<NaiveDate>,
    pub stale: bool,
    pub agency_rating: String,
    pub agency_bucket: Option<String>,
    /// Stufe, die der Spread am Anleihemarkt entspricht (0 = AAA, 6 = CCC, stetig).
    pub implied_position: Option<f64>,
    pub implied_bucket: Option<String>,
    /// Abstand implizite Stufe minus Agenturstufe (1 Stufe = etwa 3 Notches).
    pub gap_buckets: Option<f64>,
    pub change_30d_bp: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceReading {
    pub near_median: Option<f64>,
    pub near_p25: Option<f64>,
    pub far_median: Option<f64>,
    /// ln(Spot / naher Median) in Prozent.
    pub basis_pct: Option<f64>,
    pub near_change_14d_pct: Option<f64>,
    pub p25_change_14d_pct: Option<f64>,
    pub near_change_30d_pct: Option<f64>,
    pub p25_change_30d_pct: Option<f64>,
    pub basis_change_14d_pp: Option<f64>,
    pub slope_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CouplingReading {
    /// Prozentuale Kursänderung über 7 Tage je Indexpunkt Stressänderung 7 Tage zuvor.
    pub beta: Option<f64>,
    pub corr: Option<f64>,
    pub points: usize,
    pub lag_days: i64,
    pub window_days: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegimeDay {
    pub date: NaiveDate,
    pub fsi: Option<f64>,
    pub macro_block: Option<f64>,
    pub ai_block: Option<f64>,
    pub near_median: Option<f64>,
    pub near_p25: Option<f64>,
    pub basis_pct: Option<f64>,
    pub price_break_prob: Option<f64>,
    pub credit_break_prob: Option<f64>,
    pub raw: Regime,
    pub regime: Regime,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegimeTransition {
    /// Tag der Bestätigung: ab hier gilt das neue Regime, auch im Verlauf.
    pub date: NaiveDate,
    /// Erster Tag des neuen Regimes in der Rohsignal-Folge.
    pub signal_since: NaiveDate,
    pub from: Regime,
    pub to: Regime,
    pub to_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegimeReport {
    pub gpu: String,
    pub as_of: Option<NaiveDate>,
    pub regime: Regime,
    pub regime_label: String,
    pub regime_explanation: String,
    pub regime_since: Option<NaiveDate>,
    /// Letzter Tag mit vollständiger Einstufung (Kredit- und Kurvendaten).
    pub classified_until: Option<NaiveDate>,
    pub early_warning: bool,
    pub warnings: Vec<String>,
    /// Stresszustand der Kreditseite nach Hysterese, auch wenn die GPU-Seite noch fehlt.
    pub credit_stress: bool,
    /// Tage mit nahem Kurvenpunkt und benötigte Mindestzahl für die GPU-Seite.
    pub curve_days: usize,
    pub curve_days_needed: usize,
    pub fsi: Option<f64>,
    pub macro_block: Option<f64>,
    pub ai_block: Option<f64>,
    pub macro_components: Vec<ComponentReading>,
    pub issuers: Vec<IssuerReading>,
    pub price: PriceReading,
    pub coupling: CouplingReading,
    pub price_break_prob: Option<f64>,
    pub credit_break_prob: Option<f64>,
    pub history: Vec<RegimeDay>,
    pub transitions: Vec<RegimeTransition>,
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Statistische Bausteine
// ---------------------------------------------------------------------------

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    })
}

/// Robuster z-Wert: Abstand zum Median in Einheiten der skalierten MAD, auf ±4 begrenzt.
pub fn robust_z(window: &[f64], x: f64) -> Option<f64> {
    let mut vals: Vec<f64> = window.iter().copied().filter(|v| v.is_finite()).collect();
    let med = median(&mut vals)?;
    let mut dev: Vec<f64> = vals.iter().map(|v| (v - med).abs()).collect();
    let mad = median(&mut dev)? * 1.4826;
    if mad.is_nan() || mad <= 1e-12 || !x.is_finite() {
        return None;
    }
    Some(((x - med) / mad).clamp(-4.0, 4.0))
}

/// z-Wert je Beobachtung über ein gleitendes Fenster der eigenen Vergangenheit.
fn rolling_robust_z(series: &Series, window: usize, min_obs: usize) -> Series {
    let pts: Vec<(NaiveDate, f64)> = series
        .iter()
        .filter(|(_, v)| v.is_finite())
        .map(|(d, v)| (*d, *v))
        .collect();
    let mut out = Series::new();
    for i in 0..pts.len() {
        let start = (i + 1).saturating_sub(window);
        if i + 1 - start < min_obs {
            continue;
        }
        let win: Vec<f64> = pts[start..=i].iter().map(|p| p.1).collect();
        if let Some(z) = robust_z(&win, pts[i].1) {
            out.insert(pts[i].0, z);
        }
    }
    out
}

/// Letzter Wert bis einschließlich `day`, höchstens `max_age_days` alt.
pub fn value_asof(series: &Series, day: NaiveDate, max_age_days: i64) -> Option<(NaiveDate, f64)> {
    let (d, v) = series.range(..=day).next_back()?;
    ((day - *d).num_days() <= max_age_days && v.is_finite()).then_some((*d, *v))
}

/// Lineare Regression y = a + b x mit Korrelation.
fn ols(xs: &[f64], ys: &[f64]) -> Option<(f64, f64)> {
    let n = xs.len();
    if n < 3 || n != ys.len() {
        return None;
    }
    let mx = xs.iter().sum::<f64>() / n as f64;
    let my = ys.iter().sum::<f64>() / n as f64;
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(ys) {
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
        sxy += (x - mx) * (y - my);
    }
    if sxx <= 1e-12 || syy <= 1e-12 {
        return None;
    }
    Some((sxy / sxx, sxy / (sxx * syy).sqrt()))
}

/// ln Γ(x) für x > 0 (Lanczos, g = 7).
pub fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Spiegelung: Γ(x) Γ(1 - x) = π / sin(πx)
        return std::f64::consts::PI.ln() - (std::f64::consts::PI * x).sin().abs().ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + G + 0.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

fn logsumexp(values: &[f64]) -> f64 {
    let m = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !m.is_finite() {
        return m;
    }
    m + values.iter().map(|v| (v - m).exp()).sum::<f64>().ln()
}

#[derive(Debug, Clone, Copy)]
struct NigParams {
    mu: f64,
    kappa: f64,
    alpha: f64,
    beta: f64,
}

impl NigParams {
    const PRIOR: NigParams = NigParams {
        mu: 0.0,
        kappa: 1.0,
        alpha: 3.0,
        beta: 2.0,
    };

    /// Log-Dichte der Student-t-Vorhersage für x.
    fn predictive_logpdf(&self, x: f64) -> f64 {
        let nu = 2.0 * self.alpha;
        let scale2 = self.beta * (self.kappa + 1.0) / (self.alpha * self.kappa);
        let z = (x - self.mu) * (x - self.mu) / (nu * scale2);
        ln_gamma((nu + 1.0) / 2.0)
            - ln_gamma(nu / 2.0)
            - 0.5 * (nu * std::f64::consts::PI * scale2).ln()
            - (nu + 1.0) / 2.0 * (1.0 + z).ln()
    }

    fn update(&self, x: f64) -> NigParams {
        NigParams {
            mu: (self.kappa * self.mu + x) / (self.kappa + 1.0),
            kappa: self.kappa + 1.0,
            alpha: self.alpha + 0.5,
            beta: self.beta + self.kappa * (x - self.mu) * (x - self.mu) / (2.0 * (self.kappa + 1.0)),
        }
    }
}

/// Bayesian Online Changepoint Detection (Adams und MacKay 2007) mit
/// Normal-Invers-Gamma-Modell und konstanter Bruchrate.
///
/// Gibt je Beobachtung die Wahrscheinlichkeit zurück, dass der letzte Bruch
/// weniger als `recent` Beobachtungen zurückliegt. Jede Beobachtung wird auf die
/// robuste Streuung der vorherigen normiert (höchstens [`BOCPD_SCALE_WINDOW`]), damit
/// die Vorannahmen einheitenfrei bleiben und spätere Daten frühere Werte nicht ändern.
/// Ist der Median der Abweichungen null (mehr als die Hälfte der Werte gleich), zählt die
/// mittlere absolute Abweichung, bei lauter gleichen Werten [`BOCPD_SCALE_FLOOR`]: So geht
/// gerade der erste Sprung nach einer flachen Phase nicht verloren.
/// Vor [`BOCPD_SCALE_MIN_OBS`] vorherigen Beobachtungen bleibt der Wert NaN.
pub fn bocpd_recent_break(xs: &[f64], hazard_lambda: f64, recent: usize, max_run: usize) -> Vec<f64> {
    if xs.is_empty() {
        return Vec::new();
    }
    let mut seen: Vec<f64> = Vec::new();
    let mut scale_before = |x: f64| -> Option<f64> {
        let start = seen.len().saturating_sub(BOCPD_SCALE_WINDOW);
        let scale = (seen.len() >= BOCPD_SCALE_MIN_OBS)
            .then(|| {
                let mut win = seen[start..].to_vec();
                let med = median(&mut win)?;
                let mut dev: Vec<f64> = win.iter().map(|v| (v - med).abs()).collect();
                let mean_dev = dev.iter().sum::<f64>() / dev.len() as f64;
                let mad = median(&mut dev)? * 1.4826;
                Some(if mad > 1e-12 {
                    mad
                } else if mean_dev > 1e-12 {
                    mean_dev * 1.2533
                } else {
                    BOCPD_SCALE_FLOOR
                })
            })
            .flatten();
        seen.push(x);
        scale
    };
    let log_h = (1.0 / hazard_lambda).ln();
    let log_1mh = (1.0 - 1.0 / hazard_lambda).ln();

    let mut log_run: Vec<f64> = vec![0.0];
    let mut params: Vec<NigParams> = vec![NigParams::PRIOR];
    let mut out = Vec::with_capacity(xs.len());
    for &raw in xs {
        if !raw.is_finite() {
            out.push(f64::NAN);
            continue;
        }
        let Some(scale) = scale_before(raw) else {
            out.push(f64::NAN);
            continue;
        };
        let x = raw / scale;
        let pred: Vec<f64> = params.iter().map(|p| p.predictive_logpdf(x)).collect();
        let joint: Vec<f64> = log_run.iter().zip(&pred).map(|(r, p)| r + p).collect();
        let mut next = Vec::with_capacity(joint.len() + 1);
        next.push(logsumexp(&joint) + log_h);
        next.extend(joint.iter().map(|j| j + log_1mh));
        let norm = logsumexp(&next);
        for v in next.iter_mut() {
            *v -= norm;
        }
        let mut next_params = Vec::with_capacity(params.len() + 1);
        next_params.push(NigParams::PRIOR);
        next_params.extend(params.iter().map(|p| p.update(x)));
        if next.len() > max_run {
            let tail = logsumexp(&next[max_run - 1..]);
            next.truncate(max_run);
            next[max_run - 1] = tail;
            next_params.truncate(max_run);
        }
        log_run = next;
        params = next_params;
        out.push(log_run.iter().take(recent).map(|v| v.exp()).sum::<f64>().min(1.0));
    }
    out
}

/// Setzt die Bruchwahrscheinlichkeit auf NaN, solange weniger als `warmup` gültige
/// Beobachtungen vorliegen: Mit wenigen Punkten meldet das Verfahren fast immer einen Bruch.
pub fn mask_warmup(probs: &mut [f64], warmup: usize) {
    let mut seen = 0usize;
    for p in probs.iter_mut() {
        // Gezählt werden echte Posterior-Updates, nicht Beobachtungen ohne Skala
        if p.is_finite() {
            seen += 1;
        }
        if seen < warmup {
            *p = f64::NAN;
        }
    }
}

// ---------------------------------------------------------------------------
// Kurve: konstante Laufzeit
// ---------------------------------------------------------------------------

/// Wert der Kurve bei fester Restlaufzeit: linear zwischen den umliegenden
/// Verfällen, flach fortgeschrieben bis höchstens 45 Tage über den Rand hinaus.
pub fn constant_maturity(
    day: NaiveDate,
    nodes: &[CurveNode],
    horizon_days: i64,
    pick: fn(&CurveNode) -> Option<f64>,
) -> Option<f64> {
    let mut pts: Vec<(i64, f64)> = nodes
        .iter()
        .filter_map(|n| {
            let v = pick(n)?;
            let dte = (n.expiry - day).num_days();
            (v.is_finite() && v > 0.0 && day < n.period_start && dte > 0).then_some((dte, v))
        })
        .collect();
    pts.sort_by_key(|p| p.0);
    pts.dedup_by_key(|p| p.0);
    let first = *pts.first()?;
    let last = *pts.last()?;
    if horizon_days <= first.0 {
        return (first.0 - horizon_days <= MAX_FLAT_EXTRAPOLATION_DAYS).then_some(first.1);
    }
    if horizon_days >= last.0 {
        return (horizon_days - last.0 <= MAX_FLAT_EXTRAPOLATION_DAYS).then_some(last.1);
    }
    pts.windows(2).find_map(|w| {
        let ((d0, v0), (d1, v1)) = (w[0], w[1]);
        (horizon_days >= d0 && horizon_days <= d1).then(|| {
            let t = (horizon_days - d0) as f64 / (d1 - d0) as f64;
            v0 + t * (v1 - v0)
        })
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct PriceDay {
    near: Option<f64>,
    p25: Option<f64>,
    far: Option<f64>,
    basis: Option<f64>,
}

fn price_days(curve: &[CurveDay]) -> BTreeMap<NaiveDate, PriceDay> {
    let mut out = BTreeMap::new();
    for day in curve {
        let near = constant_maturity(day.date, &day.nodes, NEAR_HORIZON_DAYS, |n| n.median);
        let p25 = constant_maturity(day.date, &day.nodes, NEAR_HORIZON_DAYS, |n| n.p25);
        let far = constant_maturity(day.date, &day.nodes, FAR_HORIZON_DAYS, |n| n.median);
        let basis = match (day.spot, near) {
            (Some(s), Some(n)) if s > 0.0 && n > 0.0 => Some((s / n).ln()),
            _ => None,
        };
        out.insert(day.date, PriceDay { near, p25, far, basis });
    }
    out
}

fn lookup_price(days: &BTreeMap<NaiveDate, PriceDay>, day: NaiveDate, max_age: i64) -> Option<PriceDay> {
    let (d, v) = days.range(..=day).next_back()?;
    ((day - *d).num_days() <= max_age).then_some(*v)
}

fn log_change(now: Option<f64>, then: Option<f64>) -> Option<f64> {
    match (now, then) {
        (Some(a), Some(b)) if a > 0.0 && b > 0.0 => Some((a / b).ln()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Emittenten: implizite Ratingstufe
// ---------------------------------------------------------------------------

/// Stetige Ratingposition eines Spreads (bp) gegen die OAS-Stufen (bp) desselben Tages:
/// 0 = AAA, 6 = CCC, logarithmisch interpoliert, über CCC hinaus bis 7 fortgeschrieben.
pub fn implied_position(spread_bp: f64, bucket_oas_bp: &[Option<f64>; 7]) -> Option<f64> {
    if spread_bp.is_nan() || spread_bp <= 0.0 {
        return None;
    }
    let pts: Vec<(usize, f64)> = bucket_oas_bp
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.filter(|x| *x > 0.0).map(|x| (i, x.ln())))
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let s = spread_bp.ln();
    if s <= pts[0].1 {
        return Some(pts[0].0 as f64);
    }
    // Abschnitte von Stufe zu Stufe, aber nur ansteigend: Liegt eine schlechtere Stufe zeitweise
    // nicht über der vorherigen (etwa AA unter AAA), reicht der Abschnitt über sie hinweg.
    // So bleibt die Position stetig und springt nicht um eine ganze Stufe.
    let mut seg = pts[0];
    let mut last_rise: Option<((usize, f64), (usize, f64))> = None;
    for p in &pts[1..] {
        if p.1 > seg.1 {
            if s <= p.1 {
                return Some(seg.0 as f64 + (p.0 - seg.0) as f64 * (s - seg.1) / (p.1 - seg.1));
            }
            last_rise = Some((seg, *p));
            seg = *p;
        }
    }
    // Oberhalb der höchsten Stufe: mit der Steigung des letzten ansteigenden Abschnitts
    let ((i0, l0), (i1, l1)) = last_rise?;
    let ext = i1 as f64 + (i1 - i0) as f64 * (s - l1) / (l1 - l0);
    Some(ext.min(7.0))
}

fn bucket_label_for(position: f64) -> &'static str {
    let idx = position.round().clamp(0.0, 6.0) as usize;
    RatingBucket::ALL[idx].label()
}

fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
}

/// Letztes Beobachtungsdatum der OAS-Reihen, das am Stichtag schon veröffentlicht war.
/// ICE BofA veröffentlicht den Wert eines Tages am folgenden Geschäftstag: Am Montag ist der
/// Freitag bekannt, am Samstag und Sonntag erst der Donnerstag. Feiertage bleiben unberücksichtigt.
pub(crate) fn oas_known_until(day: NaiveDate) -> NaiveDate {
    let mut business = day;
    while is_weekend(business) {
        business -= Duration::days(1);
    }
    let mut known = business - Duration::days(1);
    while is_weekend(known) {
        known -= Duration::days(1);
    }
    known
}

/// OAS-Raster (Basispunkte) je Ratingstufe, wie es am Stichtag bekannt war.
fn bucket_oas_asof(oas: &BTreeMap<String, Series>, day: NaiveDate) -> [Option<f64>; 7] {
    let mut out = [None; 7];
    for b in RatingBucket::ALL {
        out[b.index()] = oas
            .get(b.oas_series())
            .and_then(|s| value_asof(s, oas_known_until(day), OAS_MAX_AGE_DAYS))
            .map(|(_, v)| v * 100.0);
    }
    out
}

// ---------------------------------------------------------------------------
// Modell
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct StressDay {
    macro_block: Option<f64>,
    ai_block: Option<f64>,
    fsi: Option<f64>,
    max_position: Option<f64>,
    /// Fingerabdruck der Zusammensetzung: welche Quellen eingehen, je Emittent mit Korb und Rating.
    composition: u64,
    /// Fingerabdruck der Beobachtungsdaten aller Quellen: ändert sich nur mit neuen Daten.
    data_stamp: u64,
}

impl StressDay {
    /// Veränderung des Stressindex gegenüber `prev`, nur bei neuen Daten und gleicher
    /// Zusammensetzung: Fortgeschriebene Wochenenden wären sonst künstliche Nullen, ein
    /// hinzukommender oder wegfallender KI-Block ein künstlicher Sprung.
    fn fsi_change_since(&self, prev: &StressDay, require_new_data: bool) -> Option<f64> {
        if self.composition != prev.composition || (require_new_data && self.data_stamp == prev.data_stamp) {
            return None;
        }
        Some(self.fsi? - prev.fsi?)
    }
}

fn stamp_of(dates: &[Option<NaiveDate>]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dates.hash(&mut h);
    h.finish()
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Je Emittent die Position im Ratingraster pro Beobachtungstag. Die Lücke zum Agenturrating
/// entsteht erst im Tageslauf, mit dem Rating, das an diesem Tag galt.
fn issuer_positions(inputs: &ModelInputs) -> Vec<Series> {
    inputs
        .issuers
        .iter()
        .map(|iss| {
            iss.spreads_bp
                .iter()
                .filter_map(|(d, s)| implied_position(*s, &bucket_oas_asof(&inputs.oas, *d)).map(|p| (*d, p)))
                .collect()
        })
        .collect()
}

/// Stressindex je Kalendertag von `start` bis `today`, mit Zusammensetzung und Datenstempel.
fn stress_days(
    inputs: &ModelInputs,
    macro_z: &[(&str, &str, Series)],
    issuer_positions: &[Series],
    start: NaiveDate,
    today: NaiveDate,
) -> BTreeMap<NaiveDate, StressDay> {
    let mut stress: BTreeMap<NaiveDate, StressDay> = BTreeMap::new();
    let mut day = start;
    while day <= today {
        let mut source_dates: Vec<Option<NaiveDate>> = Vec::new();
        let macro_scores: Vec<f64> = macro_z
            .iter()
            .filter_map(|(_, _, z)| {
                let hit = value_asof(z, oas_known_until(day), OAS_MAX_AGE_DAYS);
                source_dates.push(hit.map(|(d, _)| d));
                hit.map(|(_, v)| v)
            })
            .collect();
        let macro_block = (macro_scores.len() >= 2).then(|| mean(&macro_scores)).flatten();
        // Zusammensetzung: welche Quellen eingehen, je Emittent mit Anleihekorb und Rating.
        // Ändert sich eines davon, ist ein Sprung im Index keine Kreditbewegung.
        let mut parts: Vec<Option<(u64, usize)>> = source_dates.iter().map(|d| d.map(|_| (0, 0))).collect();
        let mut gaps = Vec::new();
        let mut max_pos: Option<f64> = None;
        for (iss, pos) in inputs.issuers.iter().zip(issuer_positions) {
            let hit = value_asof(pos, day, ISSUER_MAX_AGE_DAYS);
            if let Some((_, p)) = hit {
                max_pos = Some(max_pos.map_or(p, |m: f64| m.max(p)));
            }
            let agency = RatingBucket::from_rating(iss.rating_on(day));
            match (hit, agency) {
                (Some((hd, p)), Some(a)) => {
                    gaps.push(p - a.index() as f64);
                    source_dates.push(Some(hd));
                    parts.push(Some((iss.members.get(&hd).copied().unwrap_or(0), a.index())));
                }
                _ => {
                    source_dates.push(None);
                    parts.push(None);
                }
            }
        }
        let ai_block = mean(&gaps);
        let fsi = match (macro_block, ai_block) {
            (Some(m), Some(a)) => Some(0.5 * m + 0.5 * a),
            (Some(m), None) => Some(m),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        let composition = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            parts.hash(&mut h);
            h.finish()
        };
        stress.insert(
            day,
            StressDay {
                macro_block,
                ai_block,
                fsi,
                max_position: max_pos,
                composition,
                data_stamp: stamp_of(&source_dates),
            },
        );
        day += Duration::days(1);
    }
    stress
}

pub fn compute_regime(inputs: &ModelInputs, today: NaiveDate) -> RegimeReport {
    let mut notes = Vec::new();

    // 1. Makro-z-Werte je OAS-Reihe
    let macro_z: Vec<(&str, &str, Series)> = MACRO_COMPONENTS
        .iter()
        .map(|(id, label)| {
            let z = inputs
                .oas
                .get(*id)
                .map(|s| rolling_robust_z(s, OAS_Z_WINDOW, OAS_Z_MIN_OBS))
                .unwrap_or_default();
            (*id, *label, z)
        })
        .collect();
    let issuer_positions = issuer_positions(inputs);

    // 2. Kalender: vom frühesten Kredit- oder Kurvendatum bis heute. Der Start hängt nur an
    // den Daten, nicht am heutigen Datum: So bleiben frühere Tageswerte bei späteren Läufen gleich.
    let price = price_days(&inputs.curve);
    let first_credit = inputs.oas.values().filter_map(|s| s.keys().next()).min().copied();
    let first_price = price.keys().next().copied();
    let start = [first_credit, first_price].into_iter().flatten().min().filter(|d| *d <= today);
    let Some(start) = start else {
        notes.push("Noch keine Kredit- oder Kurvendaten vorhanden.".into());
        return empty_report(inputs, today, notes);
    };

    let stress = stress_days(inputs, &macro_z, &issuer_positions, start, today);

    // 3. Bruchpunkte: tägliche Änderungen des nahen Medians und des Stressindex
    let days: Vec<NaiveDate> = stress.keys().copied().collect();
    let price_changes: Vec<f64> = days
        .iter()
        .map(|d| {
            let now = lookup_price(&price, *d, 0).and_then(|p| p.near);
            let prev = lookup_price(&price, *d - Duration::days(1), 3).and_then(|p| p.near);
            log_change(now, prev).unwrap_or(f64::NAN)
        })
        .collect();
    let fsi_changes: Vec<f64> = days
        .iter()
        .map(|d| {
            stress
                .get(&(*d - Duration::days(1)))
                .and_then(|prev| stress[d].fsi_change_since(prev, true))
                .unwrap_or(f64::NAN)
        })
        .collect();
    let mut price_break = bocpd_recent_break(&price_changes, BOCPD_HAZARD_LAMBDA, BOCPD_RECENT, BOCPD_MAX_RUN);
    let mut credit_break = bocpd_recent_break(&fsi_changes, BOCPD_HAZARD_LAMBDA, BOCPD_RECENT, BOCPD_MAX_RUN);
    mask_warmup(&mut price_break, BOCPD_WARMUP);
    mask_warmup(&mut credit_break, BOCPD_WARMUP);

    // 4. Tagesweise Einstufung mit Hysterese und Bestätigung
    let mut stress_high = false;
    let mut falling = false;
    let mut confirmed = Regime::Unbekannt;
    let mut candidate = Regime::Unbekannt;
    let mut candidate_days = 0usize;
    let mut candidate_start = start;
    let mut regime_since: Option<NaiveDate> = None;
    let mut last_classified: Option<NaiveDate> = None;
    // Zuletzt bestätigtes Regime vor einem Verfall wegen fehlender Daten (für den Hinweis)
    let mut expired: Option<Regime> = None;
    let mut needed = CONFIRM_DAYS;
    let mut transitions = Vec::new();
    let mut history = Vec::new();
    let history_start = today - Duration::days(HISTORY_DAYS);

    for (i, d) in days.iter().enumerate() {
        let s = stress[d];
        let p_now = lookup_price(&price, *d, 0);
        let p_14 = lookup_price(&price, *d - Duration::days(FALL_SHORT_DAYS), 3);
        let p_30 = lookup_price(&price, *d - Duration::days(FALL_LONG_DAYS), 3);
        let dn14 = log_change(p_now.and_then(|p| p.near), p_14.and_then(|p| p.near));
        let dp14 = log_change(p_now.and_then(|p| p.p25), p_14.and_then(|p| p.p25));
        let dn30 = log_change(p_now.and_then(|p| p.near), p_30.and_then(|p| p.near));
        let dp30 = log_change(p_now.and_then(|p| p.p25), p_30.and_then(|p| p.p25));
        let dbasis = match (p_now.and_then(|p| p.basis), p_14.and_then(|p| p.basis)) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        };

        stress_high = next_stress_state(stress_high, s.fsi);
        let fall = FallSignal { dn14, dp14, dn30, dp30, dbasis };
        falling = next_falling_state(falling, &fall);
        let (have_short, have_long) = (fall.has_short(), fall.has_long());

        let raw = if s.fsi.is_none() || p_now.and_then(|p| p.near).is_none() || !(have_short || have_long) {
            Regime::Unbekannt
        } else {
            match (stress_high, falling) {
                (false, false) => Regime::Ruhig,
                (false, true) => Regime::Preisdruck,
                (true, false) => Regime::Entkoppelt,
                (true, true) => Regime::Uebertragung,
            }
        };

        // Bestätigung: Fehlende Daten ändern das bestätigte Regime nicht, unterbrechen aber
        // die Zählung eines neuen Kandidaten.
        if raw == Regime::Unbekannt {
            // Nach mehr als REGIME_STALE_DAYS ohne Einstufung verfällt das Regime, auch im Verlauf:
            // Die nächste Einstufung ist dann wieder eine erste.
            let stale = last_classified.is_none_or(|lc| (*d - lc).num_days() > REGIME_STALE_DAYS);
            if stale && confirmed != Regime::Unbekannt {
                expired = Some(confirmed);
                confirmed = Regime::Unbekannt;
                regime_since = None;
            }
            candidate = confirmed;
            candidate_days = 0;
        } else {
            last_classified = Some(*d);
            if raw == confirmed {
                candidate = confirmed;
                candidate_days = 0;
            } else {
                if raw == candidate {
                    candidate_days += 1;
                } else {
                    candidate = raw;
                    candidate_days = 1;
                    candidate_start = *d;
                }
                let window_start = i.saturating_sub(14);
                let evidence = price_break[window_start..=i]
                    .iter()
                    .any(|p| p.is_finite() && *p >= BOCPD_SIGNAL)
                    || coupling_at(&price, &stress, *d).corr.is_some_and(|c| c <= COUPLING_NEGATIVE_CORR);
                // Die erste Einstufung nach „Unbekannt“ braucht keine Bestätigung: Die Regel soll ein
                // Hin und Her zwischen Regimen dämpfen, nicht die erste Einordnung verzögern.
                // Eine Übertragung braucht sie immer.
                needed = if raw == Regime::Uebertragung && !evidence {
                    CONFIRM_DAYS_WITHOUT_EVIDENCE
                } else if confirmed == Regime::Unbekannt && raw != Regime::Uebertragung {
                    1
                } else {
                    CONFIRM_DAYS
                };
                if candidate_days >= needed {
                    if confirmed != Regime::Unbekannt {
                        transitions.push(RegimeTransition {
                            date: *d,
                            signal_since: candidate_start,
                            from: confirmed,
                            to: raw,
                            to_label: raw.label().into(),
                        });
                    }
                    confirmed = raw;
                    regime_since = Some(*d);
                    expired = None;
                    candidate_days = 0;
                }
            }
        }

        if *d >= history_start {
            history.push(RegimeDay {
                date: *d,
                fsi: s.fsi,
                macro_block: s.macro_block,
                ai_block: s.ai_block,
                near_median: p_now.and_then(|p| p.near),
                near_p25: p_now.and_then(|p| p.p25),
                basis_pct: p_now.and_then(|p| p.basis).map(|b| b * 100.0),
                price_break_prob: Some(price_break[i]).filter(|v| v.is_finite()),
                credit_break_prob: Some(credit_break[i]).filter(|v| v.is_finite()),
                raw,
                regime: confirmed,
            });
        }
    }

    // 5. Aktueller Stand
    let last = days.last().copied().unwrap_or(today);
    let s_last = stress.get(&last).copied().unwrap_or_default();
    let p_last = lookup_price(&price, last, 3);
    let p_14 = lookup_price(&price, last - Duration::days(FALL_SHORT_DAYS), 3);
    let p_30 = lookup_price(&price, last - Duration::days(FALL_LONG_DAYS), 3);
    let pct = |v: Option<f64>| v.map(|x| x * 100.0);
    let price_reading = PriceReading {
        near_median: p_last.and_then(|p| p.near),
        near_p25: p_last.and_then(|p| p.p25),
        far_median: p_last.and_then(|p| p.far),
        basis_pct: pct(p_last.and_then(|p| p.basis)),
        near_change_14d_pct: pct(log_change(p_last.and_then(|p| p.near), p_14.and_then(|p| p.near))),
        p25_change_14d_pct: pct(log_change(p_last.and_then(|p| p.p25), p_14.and_then(|p| p.p25))),
        near_change_30d_pct: pct(log_change(p_last.and_then(|p| p.near), p_30.and_then(|p| p.near))),
        p25_change_30d_pct: pct(log_change(p_last.and_then(|p| p.p25), p_30.and_then(|p| p.p25))),
        basis_change_14d_pp: match (p_last.and_then(|p| p.basis), p_14.and_then(|p| p.basis)) {
            (Some(a), Some(b)) => Some((a - b) * 100.0),
            _ => None,
        },
        slope_pct: pct(log_change(p_last.and_then(|p| p.far), p_last.and_then(|p| p.near))),
    };
    let coupling = coupling_at(&price, &stress, last);

    let macro_components = macro_z
        .iter()
        .map(|(id, label, z)| {
            let raw = inputs.oas.get(*id).and_then(|s| value_asof(s, oas_known_until(last), OAS_MAX_AGE_DAYS));
            ComponentReading {
                key: (*id).into(),
                label: (*label).into(),
                value: raw.map(|(_, v)| v),
                score: value_asof(z, oas_known_until(last), OAS_MAX_AGE_DAYS).map(|(_, v)| v),
                as_of: raw.map(|(d, _)| d),
            }
        })
        .collect();

    let issuers: Vec<IssuerReading> = inputs
        .issuers
        .iter()
        .map(|iss| {
            let latest = iss.spreads_bp.range(..=last).next_back().map(|(d, v)| (*d, *v));
            let fresh = latest.filter(|(d, _)| (last - *d).num_days() <= ISSUER_MAX_AGE_DAYS);
            let rating = iss.rating_on(last);
            let agency = RatingBucket::from_rating(rating);
            let position = latest.and_then(|(d, v)| implied_position(v, &bucket_oas_asof(&inputs.oas, d)));
            let change_30d = latest.and_then(|(d, _)| {
                let (_, now) = value_asof(&iss.linked_bp, d, 0)?;
                let (_, then) = value_asof(&iss.linked_bp, d - Duration::days(30), ISSUER_MAX_AGE_DAYS)?;
                Some(now - then)
            });
            IssuerReading {
                key: iss.key.clone(),
                label: iss.label.clone(),
                bonds: iss.bonds.clone(),
                spread_bp: latest.map(|(_, v)| v),
                as_of: latest.map(|(d, _)| d),
                stale: latest.is_some() && fresh.is_none(),
                agency_rating: rating.to_string(),
                agency_bucket: agency.map(|a| a.label().to_string()),
                implied_position: position,
                implied_bucket: position.map(|p| bucket_label_for(p).to_string()),
                gap_buckets: match (position, agency) {
                    (Some(p), Some(a)) => Some(p - a.index() as f64),
                    _ => None,
                },
                change_30d_bp: change_30d,
            }
        })
        .collect();

    // 6. Vorwarnung: Entkoppelt, aber Anzeichen, dass der Stress auf die Mieten übergreift
    let mut warnings = Vec::new();
    let mut gpu_signals = 0usize;
    if let Some(p) = s_last.max_position.filter(|p| *p >= DISTRESS_POSITION) {
        warnings.push(format!(
            "Mindestens ein beobachteter Emittent handelt am Anleihemarkt im Notlagenbereich (Stufe {}).",
            bucket_label_for(p)
        ));
    }
    if stress_high {
        if price_reading.basis_change_14d_pp.is_some_and(|b| b <= -3.0)
            && price_reading.near_change_14d_pct.is_some_and(|n| n < 0.0)
        {
            warnings.push("Der Spot fällt stärker als die nahe Kurve, die Basis sinkt.".into());
            gpu_signals += 1;
        }
        if coupling.corr.is_some_and(|c| c <= COUPLING_NEGATIVE_CORR) && coupling.beta.is_some_and(|b| b < 0.0) {
            warnings.push("Steigender Kreditstress ging zuletzt fallenden Kurvenpreisen voraus (negative Kopplung).".into());
            gpu_signals += 1;
        }
        let n = price_break.len();
        if price_break[n.saturating_sub(BOCPD_RECENT)..]
            .iter()
            .any(|p| p.is_finite() && *p >= BOCPD_SIGNAL)
        {
            warnings.push("Die Bruchpunkt-Erkennung meldet einen frischen Bruch in der nahen Kurve.".into());
            gpu_signals += 1;
        }
    }
    // Ohne vollständige Einstufung in den letzten Tagen kein altes Regime mit heutigem Datum zeigen
    let (shown, shown_since) = match (last_classified, expired) {
        (Some(d), _) if (last - d).num_days() <= REGIME_STALE_DAYS => (confirmed, regime_since),
        (Some(d), Some(prev)) => {
            notes.push(format!(
                "Seit dem {} fehlen Kredit- oder Kurvendaten für eine Einstufung. Zuletzt bestätigt: {}.",
                d.format("%d.%m.%Y"),
                prev.label()
            ));
            (Regime::Unbekannt, None)
        }
        _ => (Regime::Unbekannt, None),
    };
    // Vorwarnung nur bei Anzeichen auf der GPU-Seite; eine Notlage am Anleihemarkt allein
    // bleibt ein Hinweis auf der Kreditseite.
    let early_warning = shown == Regime::Entkoppelt && gpu_signals > 0;
    if candidate != confirmed && candidate_days > 0 && last_classified == Some(last) {
        notes.push(format!(
            "Neues Regime in Prüfung: {} seit {} Tag(en), bestätigt nach {}.",
            candidate.label(),
            candidate_days,
            needed
        ));
    }

    if inputs.issuers.iter().all(|i| i.spreads_bp.is_empty()) {
        notes.push("Keine Emittenten-Spreads: Der Stressindex beruht nur auf den OAS-Reihen.".into());
    }
    if price.is_empty() {
        notes.push("Noch keine Kurvendaten für diese GPU.".into());
    }

    RegimeReport {
        gpu: inputs.gpu.clone(),
        as_of: Some(last),
        regime: shown,
        regime_label: shown.label().into(),
        regime_explanation: shown.explanation().into(),
        regime_since: shown_since,
        classified_until: last_classified,
        early_warning,
        warnings,
        credit_stress: stress_high,
        curve_days: price.values().filter(|p| p.near.is_some()).count(),
        curve_days_needed: CURVE_DAYS_NEEDED,
        fsi: s_last.fsi,
        macro_block: s_last.macro_block,
        ai_block: s_last.ai_block,
        macro_components,
        issuers,
        price: price_reading,
        coupling,
        price_break_prob: price_break.last().copied().filter(|v| v.is_finite()),
        credit_break_prob: credit_break.last().copied().filter(|v| v.is_finite()),
        history,
        transitions,
        notes,
    }
}

/// Hysterese des Stresszustands: Eintritt ab [`STRESS_ENTER`], Austritt erst unter
/// [`STRESS_EXIT`]. Fehlt der Index an einem Tag, bleibt der Zustand.
pub fn next_stress_state(high: bool, fsi: Option<f64>) -> bool {
    match fsi {
        Some(f) if high && f < STRESS_EXIT => false,
        Some(f) if !high && f >= STRESS_ENTER => true,
        _ => high,
    }
}

/// Logarithmische Änderungen der nahen Kurve (Median, unteres Quartil) und der Basis.
#[derive(Debug, Clone, Copy, Default)]
pub struct FallSignal {
    pub dn14: Option<f64>,
    pub dp14: Option<f64>,
    pub dn30: Option<f64>,
    pub dp30: Option<f64>,
    pub dbasis: Option<f64>,
}

impl FallSignal {
    fn has_short(&self) -> bool {
        self.dn14.is_some() && self.dp14.is_some()
    }
    fn has_long(&self) -> bool {
        self.dn30.is_some() && self.dp30.is_some()
    }
}

/// Gemeinsamer Rückgang: Median und unteres Quartil fallen über 14 oder 30 Tage über die
/// Schwelle, und der Spot fällt mindestens so stark wie die Kurve (Basis sinkt) oder fehlt.
/// Der Zustand endet erst, wenn der Median über beide Fenster kaum noch fällt.
pub fn next_falling_state(falling: bool, f: &FallSignal) -> bool {
    if !falling {
        let short = f.has_short() && f.dn14.unwrap() <= FALL_SHORT_ENTER && f.dp14.unwrap() <= FALL_SHORT_ENTER;
        let long = f.has_long() && f.dn30.unwrap() <= FALL_LONG_ENTER && f.dp30.unwrap() <= FALL_LONG_ENTER;
        let spot_confirms = f.dbasis.is_none_or(|b| b <= 0.0);
        (short || long) && spot_confirms
    } else {
        // Ohne Kursänderungen (fehlender Kurventag) bleibt der Zustand wie beim Stress
        if f.dn14.is_none() && f.dn30.is_none() {
            return true;
        }
        let short_ok = f.dn14.is_none_or(|v| v > FALL_SHORT_EXIT);
        let long_ok = f.dn30.is_none_or(|v| v > FALL_LONG_EXIT);
        !(short_ok && long_ok)
    }
}

/// Kopplung am Tag `day`: Regression der 7-Tage-Kursänderung auf die
/// 7-Tage-Stressänderung, die 7 Tage früher endet, über 90 Tage.
fn coupling_at(
    price: &BTreeMap<NaiveDate, PriceDay>,
    stress: &BTreeMap<NaiveDate, StressDay>,
    day: NaiveDate,
) -> CouplingReading {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    let mut t = day - Duration::days(COUPLING_WINDOW_DAYS);
    while t <= day {
        let y = log_change(
            lookup_price(price, t, 0).and_then(|p| p.near),
            // Exakt sieben Tage: Ein früherer Ersatztag würde das Preisfenster in das
            // Stressfenster hinein verlängern.
            lookup_price(price, t - Duration::days(COUPLING_STEP_DAYS), 0).and_then(|p| p.near),
        );
        let x_end = t - Duration::days(COUPLING_LAG_DAYS);
        let x = match (stress.get(&x_end), stress.get(&(x_end - Duration::days(COUPLING_STEP_DAYS)))) {
            (Some(a), Some(b)) => a.fsi_change_since(b, false),
            _ => None,
        };
        if let (Some(x), Some(y)) = (x, y) {
            xs.push(x);
            ys.push(y * 100.0);
        }
        t += Duration::days(1);
    }
    let fit = (xs.len() >= COUPLING_MIN_POINTS).then(|| ols(&xs, &ys)).flatten();
    CouplingReading {
        beta: fit.map(|f| f.0),
        corr: fit.map(|f| f.1),
        points: xs.len(),
        lag_days: COUPLING_LAG_DAYS,
        window_days: COUPLING_WINDOW_DAYS,
    }
}

fn empty_report(inputs: &ModelInputs, today: NaiveDate, notes: Vec<String>) -> RegimeReport {
    RegimeReport {
        gpu: inputs.gpu.clone(),
        as_of: Some(today),
        regime: Regime::Unbekannt,
        regime_label: Regime::Unbekannt.label().into(),
        regime_explanation: Regime::Unbekannt.explanation().into(),
        regime_since: None,
        classified_until: None,
        early_warning: false,
        warnings: Vec::new(),
        credit_stress: false,
        curve_days: 0,
        curve_days_needed: CURVE_DAYS_NEEDED,
        fsi: None,
        macro_block: None,
        ai_block: None,
        macro_components: Vec::new(),
        issuers: Vec::new(),
        price: PriceReading {
            near_median: None,
            near_p25: None,
            far_median: None,
            basis_pct: None,
            near_change_14d_pct: None,
            p25_change_14d_pct: None,
            near_change_30d_pct: None,
            p25_change_30d_pct: None,
            basis_change_14d_pp: None,
            slope_pct: None,
        },
        coupling: CouplingReading {
            beta: None,
            corr: None,
            points: 0,
            lag_days: COUPLING_LAG_DAYS,
            window_days: COUPLING_WINDOW_DAYS,
        },
        price_break_prob: None,
        credit_break_prob: None,
        history: Vec::new(),
        transitions: Vec::new(),
        notes,
    }
}

#[cfg(test)]
mod tests;
