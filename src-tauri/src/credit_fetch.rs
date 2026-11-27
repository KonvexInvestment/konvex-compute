//! Abrufe für das Finanzierungsregime.
//!
//! - Treasury.gov: tägliche Par-Kurve der US-Staatsanleihen (gemeinfrei, CSV je Jahr).
//! - FRED: ICE-BofA-OAS je Ratingstufe als Vergleichsraster (CSV ohne Schlüssel,
//!   laut ICE nur zur internen Nutzung, deshalb nur lokal abgerufen und gespeichert).
//! - Interactive Brokers: Anleihelisten und tägliche Kurse der Emittenten über die
//!   lokale TWS oder das IB Gateway. Nur lesende Anfragen; läuft keine TWS, entfällt
//!   der Emittententeil und das Modell rechnet mit den OAS-Reihen allein.

use crate::credit::{rank_bond_candidates, BondListing, SelectedBond, BONDS_PER_ISSUER, TREASURY_SERIES};
use chrono::Datelike;
use crate::regime::{RatingBucket, OAS_HY};
use chrono::NaiveDate;
use ibapi::contracts::SecurityType;
use ibapi::market_data::historical::{Bar, BarTimestamp};
use ibapi::prelude::*;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

const FRED_CSV: &str = "https://fred.stlouisfed.org/graph/fredgraph.csv";
/// FRED weist Kennungen ohne Kontaktadresse ab; Polymarket, Ornn und Treasury.gov
/// behalten die bisherige Kennung der Software.
const FRED_USER_AGENT: &str = "KonvexCompute/1.0 (+https://github.com/KonvexInvestment/konvex-compute)";
/// So viele Tage nach Neujahr darf die Datei des neuen Jahres noch leer sein.
const TREASURY_NEW_YEAR_GRACE_DAYS: u32 = 10;
const TREASURY_CSV: &str = "https://home.treasury.gov/resource-center/data-chart-center/interest-rates/daily-treasury-rates.csv";
/// Kennung, an der der Datenstand der Treasury-Kurve gemessen wird.
pub const TREASURY_ANCHOR_SERIES: &str = "UST10Y";
/// Erstabruf: drei Jahre plus Vorlauf für das z-Wert-Fenster.
pub const FRED_HISTORY_DAYS: i64 = 3 * 365 + 30;
/// Folgeabrufe überlappen, damit nachträgliche Korrekturen ankommen.
pub const FRED_OVERLAP_DAYS: i64 = 21;

/// Standard-Ports in Suchreihenfolge: TWS live, TWS Papierkonto, Gateway live, Gateway Papier.
pub const IB_DEFAULT_PORTS: [u16; 4] = [7496, 7497, 4001, 4002];
/// Eigener Bereich für Client-IDs, damit andere API-Programme nicht verdrängt werden.
const IB_CLIENT_ID_RANGE: (i32, i32) = (8300, 8399);
/// Obergrenze für eine fest eingestellte Client-ID.
pub const IB_CLIENT_ID_MAX: i32 = 999_999;
/// IB erlaubt höchstens 60 Historienabfragen je zehn Minuten; etwas Abstand dazu.
const IB_HIST_PACING: (usize, Duration) = (55, Duration::from_secs(600));
const IB_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IB_DETAILS_TIMEOUT: Duration = Duration::from_secs(30);
/// IB beantwortet Anleihehistorien langsam (ein Monat 5 bis 15 s, ein Jahr 35 bis 45 s)
/// und für Anleihen ohne Historie oft gar nicht. Deshalb erst ein kurzer Monatsabruf als
/// Probe, das volle Jahr nur einmal je neuer Anleihe, danach nur noch der letzte Monat.
const IB_MONTH_TIMEOUT: Duration = Duration::from_secs(30);
const IB_YEAR_TIMEOUT: Duration = Duration::from_secs(90);
/// Abstand zwischen historischen Anfragen (IB begrenzt auf 60 je 10 Minuten).
const IB_REQUEST_GAP: Duration = Duration::from_millis(400);
/// Höchstens so viele neue Anleihen je Emittent probieren, bis drei mit Historie gefunden sind.
const IB_MAX_NEW_TRIES_PER_ISSUER: usize = 5;
/// Gesamtbudget eines IB-Abrufs über alle Emittenten (läuft im Hintergrund).
const IB_TOTAL_BUDGET: Duration = Duration::from_secs(20 * 60);
/// Unter diesem Restbudget startet keine neue Anfrage mehr.
const IB_MIN_REQUEST_BUDGET: Duration = Duration::from_secs(5);
/// Mindestzahl an Handelstagen, damit eine Anleihe in den Korb kommt (etwa drei Monate).
const MIN_HISTORY_BARS: usize = 60;
/// Eine bisherige Anleihe ohne neue Kurse bleibt so lange im Korb, danach wird neu gewählt.
const KEEP_WITHOUT_UPDATE_DAYS: i64 = 14;
/// Kursquelle der Anleihereihen: Geldkurs, für alle Anleihen gleich.
pub const BOND_PRICE_SOURCE: &str = "bid";

/// Alle FRED-Reihen, die das Modell braucht (nur ICE-BofA-OAS).
pub fn fred_series_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = vec![OAS_HY];
    ids.extend(RatingBucket::ALL.iter().map(|b| b.oas_series()));
    ids
}

/// Alle intern gespeicherten Kredit- und Zinsreihen (OAS und Treasury-Laufzeiten).
pub fn all_credit_series_ids() -> Vec<&'static str> {
    let mut ids = fred_series_ids();
    ids.extend(TREASURY_SERIES.iter().map(|(id, _, _)| *id));
    ids
}

pub fn build_fred_client() -> Result<HttpClient, String> {
    HttpClient::builder()
        .user_agent(FRED_USER_AGENT)
        .gzip(true)
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|e| format!("HTTP-Client für FRED konnte nicht initialisiert werden: {e}"))
}

// ---------------------------------------------------------------------------
// Einstellungen
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssuerSettings {
    /// IB-Symbol des Emittenten (z. B. "ORCL").
    pub key: String,
    pub label: String,
    /// Agenturrating als Vergleich zum Marktniveau, z. B. "BBB-" oder "Baa2".
    pub agency_rating: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreditSettings {
    pub ib_enabled: bool,
    pub ib_host: String,
    /// 0 = Standard-Ports der Reihe nach probieren.
    pub ib_port: u16,
    /// 0 = zufällig aus 8300 bis 8399.
    pub ib_client_id: i32,
    pub issuers: Vec<IssuerSettings>,
}

impl Default for CreditSettings {
    fn default() -> Self {
        let issuer = |key: &str, label: &str, rating: &str| IssuerSettings {
            key: key.into(),
            label: label.into(),
            agency_rating: rating.into(),
            enabled: true,
        };
        Self {
            ib_enabled: true,
            ib_host: "127.0.0.1".into(),
            ib_port: 0,
            ib_client_id: 0,
            issuers: vec![
                issuer("ORCL", "Oracle", "BBB-"),
                issuer("CRWV", "CoreWeave", "B+"),
                issuer("SPCX", "SpaceX", "BBB"),
            ],
        }
    }
}

impl CreditSettings {
    /// Prüft und bereinigt Eingaben aus der Oberfläche.
    pub fn validated(mut self) -> Result<Self, String> {
        self.ib_host = self.ib_host.trim().to_string();
        if self.ib_host.is_empty() {
            self.ib_host = "127.0.0.1".into();
        }
        if self.ib_host.len() > 253 || !self.ib_host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c)) {
            return Err("Ungültiger Hostname für die IB-Verbindung.".into());
        }
        if !(0..=IB_CLIENT_ID_MAX).contains(&self.ib_client_id) {
            return Err(format!("Die Client-ID muss 0 (automatisch) oder höchstens {IB_CLIENT_ID_MAX} sein."));
        }
        if self.issuers.len() > 10 {
            return Err("Höchstens zehn Emittenten.".into());
        }
        let mut seen = std::collections::HashSet::new();
        for iss in self.issuers.iter_mut() {
            iss.key = iss.key.trim().to_ascii_uppercase();
            iss.label = iss.label.trim().chars().take(40).collect();
            iss.agency_rating = iss.agency_rating.trim().chars().take(8).collect();
            if iss.key.is_empty() || iss.key.len() > 12 || !iss.key.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
                return Err(format!("Ungültiges Symbol: {}", iss.key));
            }
            if !seen.insert(iss.key.clone()) {
                return Err(format!("Symbol doppelt: {}", iss.key));
            }
            if iss.label.is_empty() {
                iss.label = iss.key.clone();
            }
            if !iss.agency_rating.is_empty() && RatingBucket::from_rating(&iss.agency_rating).is_none() {
                return Err(format!("Rating nicht lesbar: {} ({})", iss.agency_rating, iss.key));
            }
        }
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// FRED
// ---------------------------------------------------------------------------

/// Liest eine FRED-CSV ("observation_date,ID"); fehlende Werte (".", leer) entfallen.
pub fn parse_fred_csv(body: &str, series_id: &str) -> Result<Vec<(NaiveDate, f64)>, String> {
    let mut lines = body.lines();
    let header = lines.next().unwrap_or_default().trim().trim_start_matches('\u{feff}');
    let cols: Vec<&str> = header.split(',').map(str::trim).collect();
    if cols.len() != 2 || !cols[0].eq_ignore_ascii_case("observation_date") && !cols[0].eq_ignore_ascii_case("DATE") || cols[1] != series_id {
        return Err(format!("FRED {series_id}: unerwarteter Kopf \"{}\"", header.chars().take(80).collect::<String>()));
    }
    let mut out = Vec::new();
    for line in lines {
        let Some((d, v)) = line.trim().split_once(',') else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d") else {
            continue;
        };
        if let Ok(x) = v.trim().parse::<f64>() {
            if x.is_finite() && x.abs() < 100.0 {
                out.push((date, x));
            }
        }
    }
    Ok(out)
}

/// Textabruf mit bis zu drei Versuchen und Größenlimit.
async fn get_text_with_retry(client: &HttpClient, url: &str, label: &str) -> Result<String, String> {
    let mut last_err = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(800 * attempt)).await;
        }
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.map_err(|e| format!("{label}: {e}"))?;
                if body.len() > 4_000_000 {
                    return Err(format!("{label}: Antwort zu groß"));
                }
                return Ok(body);
            }
            Ok(resp) => last_err = format!("{label}: HTTP {}", resp.status().as_u16()),
            Err(e) => last_err = format!("{label}: {e}"),
        }
    }
    Err(last_err)
}

pub async fn fetch_fred_series(client: &HttpClient, series_id: &str, from: NaiveDate) -> Result<Vec<(NaiveDate, f64)>, String> {
    let url = format!("{FRED_CSV}?id={series_id}&cosd={}", from.format("%Y-%m-%d"));
    let body = get_text_with_retry(client, &url, &format!("FRED {series_id}")).await?;
    parse_fred_csv(&body, series_id)
}

// ---------------------------------------------------------------------------
// Treasury.gov
// ---------------------------------------------------------------------------

/// Liest die Jahres-CSV der Par-Kurve ("Date","1 Mo",...,"30 Yr"; Datum MM/DD/YYYY)
/// in Punkte je interner Reihenkennung. Fehlende Laufzeiten (leere Felder) entfallen.
pub fn parse_treasury_csv(body: &str) -> Result<BTreeMap<String, Vec<(NaiveDate, f64)>>, String> {
    let clean = |s: &str| s.trim().trim_matches('"').trim().to_string();
    let mut lines = body.lines();
    let header: Vec<String> = lines
        .next()
        .unwrap_or_default()
        .trim_start_matches('\u{feff}')
        .split(',')
        .map(clean)
        .collect();
    if header.first().map(String::as_str) != Some("Date") {
        return Err("Treasury.gov: unerwarteter Kopf der Zinskurve".into());
    }
    let columns: Vec<(usize, &str)> = TREASURY_SERIES
        .iter()
        .filter_map(|(id, col, _)| header.iter().position(|h| h == col).map(|i| (i, *id)))
        .collect();
    if columns.len() < 6 {
        return Err("Treasury.gov: zu wenige Laufzeiten in der Zinskurve".into());
    }
    let mut out: BTreeMap<String, Vec<(NaiveDate, f64)>> = BTreeMap::new();
    for line in lines {
        let cells: Vec<String> = line.split(',').map(clean).collect();
        let Some(date) = cells.first().and_then(|d| NaiveDate::parse_from_str(d, "%m/%d/%Y").ok()) else {
            continue;
        };
        for (idx, id) in &columns {
            if let Some(v) = cells.get(*idx).and_then(|c| c.parse::<f64>().ok()) {
                if v.is_finite() && v.abs() < 30.0 {
                    out.entry((*id).to_string()).or_default().push((date, v));
                }
            }
        }
    }
    for points in out.values_mut() {
        points.sort_by_key(|p| p.0);
    }
    Ok(out)
}

/// Lädt die Par-Kurve aller Jahre ab `from` bis heute (eine Datei je Kalenderjahr).
pub async fn fetch_treasury_curve(
    client: &HttpClient,
    from: NaiveDate,
    today: NaiveDate,
) -> Result<BTreeMap<String, Vec<(NaiveDate, f64)>>, String> {
    let mut out: BTreeMap<String, Vec<(NaiveDate, f64)>> = BTreeMap::new();
    for year in from.year()..=today.year() {
        let url = format!(
            "{TREASURY_CSV}/{year}/all?type=daily_treasury_yield_curve&field_tdr_date_value={year}&page&_format=csv"
        );
        // HTTP- und Formatfehler zählen immer. Nur die Datei des neuen Jahres darf in den ersten
        // Januartagen leer sein; eine leere Datei eines anderen Jahres hinterließe sonst eine
        // Lücke, die spätere Abrufe (ab dem letzten gespeicherten Tag) nie mehr füllen.
        let grace = year == today.year() && today.ordinal() <= TREASURY_NEW_YEAR_GRACE_DAYS;
        let body = get_text_with_retry(client, &url, &format!("Treasury.gov {year}")).await?;
        if body.trim().is_empty() && grace {
            continue;
        }
        let curve = parse_treasury_csv(&body)?;
        if curve.values().all(|v| v.is_empty()) {
            if grace {
                continue;
            }
            return Err(format!("Treasury.gov {year}: keine Renditen in der Jahresdatei"));
        }
        for (id, pts) in curve {
            out.entry(id).or_default().extend(pts.into_iter().filter(|(d, _)| *d >= from));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Interactive Brokers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BondPriceHistory {
    pub conid: i32,
    /// Quelle der Tageskurse, siehe [`BOND_PRICE_SOURCE`].
    pub source: String,
    pub prices: Vec<(NaiveDate, f64)>,
    /// true: volle Jahreshistorie, ersetzt die gespeicherte Reihe; false: Monatsnachtrag.
    pub full: bool,
}

/// Was zu einer Anleihe bereits gespeichert ist.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredBond {
    pub source: String,
    pub last_date: NaiveDate,
}

#[derive(Debug, Default)]
pub struct IbFetchOutcome {
    pub port: Option<u16>,
    pub selections: BTreeMap<String, Vec<SelectedBond>>,
    pub histories: Vec<BondPriceHistory>,
    /// Übersprungene Anleihen (keine oder zu kurze Historie), nur zur Anzeige.
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

/// Client-ID je Versuch: eine fest eingestellte ID bleibt immer dieselbe, sonst eine ID aus
/// [`IB_CLIENT_ID_RANGE`], je Verbindungslauf zufällig gestartet und je Versuch versetzt.
fn pick_client_id(configured: i32, seed: u32, attempt: u32) -> i32 {
    if configured > 0 {
        return configured;
    }
    let span = (IB_CLIENT_ID_RANGE.1 - IB_CLIENT_ID_RANGE.0 + 1) as u32;
    IB_CLIENT_ID_RANGE.0 + ((seed.wrapping_add(attempt.wrapping_mul(37))) % span) as i32
}

/// Adresse für den Socket: IPv6-Literale brauchen eckige Klammern.
fn socket_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn port_open(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

async fn connect_ib(settings: &CreditSettings) -> Result<(Client, u16), String> {
    let ports: Vec<u16> = if settings.ib_port == 0 {
        IB_DEFAULT_PORTS.to_vec()
    } else {
        vec![settings.ib_port]
    };
    let mut last_err = String::from("keine TWS und kein IB Gateway erreichbar");
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_micros())
        .unwrap_or(0);
    // Fest eingestellte ID: ein Versuch, keine fremden IDs ausprobieren
    let attempts = if settings.ib_client_id > 0 { 1 } else { 3 };
    for port in ports {
        if !port_open(&settings.ib_host, port).await {
            continue;
        }
        for attempt in 0..attempts {
            let client_id = pick_client_id(settings.ib_client_id, seed, attempt);
            let addr = socket_addr(&settings.ib_host, port);
            match tokio::time::timeout(IB_CONNECT_TIMEOUT, Client::connect(&addr, client_id)).await {
                Ok(Ok(client)) => return Ok((client, port)),
                Ok(Err(e)) => last_err = format!("Port {port}: {e}"),
                Err(_) => last_err = format!("Port {port}: Zeitlimit beim Verbindungsaufbau"),
            }
        }
    }
    Err(last_err)
}

fn bar_date(bar: &Bar) -> Option<NaiveDate> {
    let date = match &bar.date {
        BarTimestamp::Date(d) => *d,
        BarTimestamp::DateTime(dt) => dt.date(),
    };
    NaiveDate::from_ymd_opt(date.year(), u8::from(date.month()) as u32, date.day() as u32)
}

/// Plausibler Clean-Kurs je 100 Nominal.
fn plausible_price(p: f64) -> bool {
    p.is_finite() && p > 1.0 && p < 300.0
}

/// Tageskurse (Geldkurs) über die angegebene Dauer.
async fn fetch_bid_bars(
    client: &Client,
    contract: &Contract,
    duration: ibapi::market_data::historical::Duration,
    timeout: Duration,
) -> Result<Vec<(NaiveDate, f64)>, String> {
    let req = client
        .historical_data(contract, HistoricalBarSize::Day)
        .what_to_show(HistoricalWhatToShow::Bid)
        .duration(duration)
        .fetch();
    match tokio::time::timeout(timeout, req).await {
        Ok(Ok(data)) => Ok(data
            .bars
            .iter()
            .filter_map(|b| Some((bar_date(b)?, b.close)).filter(|(_, p)| plausible_price(*p)))
            .collect()),
        // „Keine Daten“ ist eine endgültige Antwort, kein Verbindungs- oder Pacing-Fehler
        Ok(Err(ibapi::Error::Notice(n))) if is_no_data_notice(n.code, &n.message) => Ok(Vec::new()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("keine Antwort".into()),
    }
}

/// IB-Meldung 162 „HMDS query returned no data“: Für den Zeitraum gibt es keine Kurse.
/// Andere 162er (etwa Pacing-Verstöße) bleiben Fehler.
fn is_no_data_notice(code: i32, message: &str) -> bool {
    code == 162 && message.to_ascii_lowercase().contains("returned no data")
}

/// Hält das IB-Limit für Historienabfragen ein (siehe [`IB_HIST_PACING`]).
#[derive(Default)]
struct HistPacer {
    sent: std::collections::VecDeque<tokio::time::Instant>,
}

impl HistPacer {
    /// Wartet, bis eine weitere Historienabfrage ins Limit passt. `false`, wenn das Warten
    /// über das Zeitbudget hinausginge.
    async fn ready(&mut self, deadline: tokio::time::Instant) -> bool {
        let (limit, window) = IB_HIST_PACING;
        let now = tokio::time::Instant::now();
        while self.sent.front().is_some_and(|t| now.duration_since(*t) >= window) {
            self.sent.pop_front();
        }
        if self.sent.len() >= limit {
            let until = self.sent[0] + window;
            if until + IB_MIN_REQUEST_BUDGET > deadline {
                return false;
            }
            tokio::time::sleep_until(until).await;
            self.sent.pop_front();
        }
        self.sent.push_back(tokio::time::Instant::now());
        true
    }
}

/// Zeitlimit einer Anfrage: höchstens `max`, aber nie über das Restbudget hinaus.
/// `None`, wenn das Restbudget für eine Anfrage nicht mehr reicht.
fn request_timeout(remaining: Duration, max: Duration) -> Option<Duration> {
    (remaining >= IB_MIN_REQUEST_BUDGET).then(|| remaining.min(max))
}

/// Nach einem Abbruch wegen des Zeitbudgets: bisherige Anleihen, die nicht mehr geprüft
/// wurden, bleiben mit ihren gespeicherten Kursen im Korb, sofern sie noch zulässig sind
/// (weiter gelistet, keine Wandelanleihe, nicht zu kurz vor Fälligkeit).
fn keep_unchecked(
    chosen: &mut Vec<SelectedBond>,
    prev: &[SelectedBond],
    examined: &std::collections::BTreeSet<i32>,
    eligible: &std::collections::BTreeSet<i32>,
    stored: &BTreeMap<i32, StoredBond>,
) -> Vec<String> {
    let mut notes = Vec::new();
    for b in prev {
        if chosen.len() >= BONDS_PER_ISSUER {
            break;
        }
        if examined.contains(&b.conid)
            || !eligible.contains(&b.conid)
            || chosen.iter().any(|c| c.conid == b.conid)
            || !stored.contains_key(&b.conid)
        {
            continue;
        }
        chosen.push(b.clone());
        notes.push(format!("{} nicht geprüft (Zeitbudget), bisherige Kurse bleiben", b.description));
    }
    notes
}

/// Lädt Anleihelisten und Tageskurse von bis zu drei Anleihen je Emittent.
/// Bekannte Anleihen bekommen nur den letzten Monat nach, neue erst eine Probe über
/// einen Monat und dann ein volles Jahr.
pub async fn fetch_ib_bonds(
    settings: &CreditSettings,
    previous: &BTreeMap<String, Vec<SelectedBond>>,
    stored: &BTreeMap<i32, StoredBond>,
    today: NaiveDate,
) -> IbFetchOutcome {
    let mut out = IbFetchOutcome::default();
    let (client, port) = match connect_ib(settings).await {
        Ok(c) => c,
        Err(e) => {
            out.errors.push(format!("IB: {e}"));
            return out;
        }
    };
    out.port = Some(port);
    let deadline = tokio::time::Instant::now() + IB_TOTAL_BUDGET;
    let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());
    let mut pacer = HistPacer::default();
    for iss in settings.issuers.iter().filter(|i| i.enabled) {
        let Some(details_timeout) = request_timeout(remaining(), IB_DETAILS_TIMEOUT) else {
            // Emittent bleibt unverändert: Ohne Eintrag in `selections` fasst das Speichern ihn nicht an
            out.errors.push(format!("IB {}: Zeitbudget des Abrufs erschöpft, nächster Versuch später", iss.key));
            continue;
        };
        let query = Contract {
            symbol: iss.key.as_str().into(),
            security_type: SecurityType::Bond,
            exchange: "SMART".into(),
            currency: "USD".into(),
            ..Default::default()
        };
        let details = match tokio::time::timeout(details_timeout, client.contract_details(&query)).await {
            Ok(Ok(list)) => list,
            Ok(Err(e)) => {
                out.errors.push(format!("IB {}: Anleiheliste nicht abrufbar ({e})", iss.key));
                continue;
            }
            Err(_) => {
                out.errors.push(format!("IB {}: Zeitlimit bei der Anleiheliste", iss.key));
                continue;
            }
        };
        let mut contracts: BTreeMap<i32, Contract> = BTreeMap::new();
        // Wandelanleihen folgen der Aktie, nicht dem Kredit: nicht in den Korb
        let listings: Vec<BondListing> = details
            .iter()
            .filter(|d| !d.convertible)
            .map(|d| {
                let mut c = d.contract.clone();
                c.exchange = "SMART".into();
                contracts.insert(d.contract.contract_id, c);
                BondListing {
                    conid: d.contract.contract_id,
                    description: d.desc_append.clone(),
                    cusip: d.cusip.clone(),
                }
            })
            .collect();
        let prev = previous.get(&iss.key).map(Vec::as_slice).unwrap_or(&[]);
        let candidates = rank_bond_candidates(&listings, prev, today);
        let mut chosen = Vec::new();
        let mut skipped = Vec::new();
        let mut new_tries = 0usize;
        let mut examined = std::collections::BTreeSet::new();
        let mut aborted = false;
        for bond in &candidates {
            if chosen.len() >= BONDS_PER_ISSUER {
                break;
            }
            let Some(contract) = contracts.get(&bond.conid) else {
                continue;
            };
            let known = prev.iter().any(|b| b.conid == bond.conid)
                .then(|| stored.get(&bond.conid))
                .flatten()
                .filter(|s| s.source == BOND_PRICE_SOURCE);
            // Erst prüfen, ob noch ein Versuch erlaubt ist, dann Kontingent belegen
            if known.is_none() && new_tries >= IB_MAX_NEW_TRIES_PER_ISSUER {
                break;
            }
            if !pacer.ready(deadline).await {
                aborted = true;
                break;
            }
            let Some(month_timeout) = request_timeout(remaining(), IB_MONTH_TIMEOUT) else {
                aborted = true;
                break;
            };
            if let Some(known) = known {
                examined.insert(bond.conid);
                match fetch_bid_bars(&client, contract, 1.months(), month_timeout).await {
                    Ok(prices) if !prices.is_empty() => {
                        out.histories.push(BondPriceHistory {
                            conid: bond.conid,
                            source: BOND_PRICE_SOURCE.into(),
                            prices,
                            full: false,
                        });
                        chosen.push(bond.clone());
                    }
                    // Zeitlimit, Verbindungs- oder Pacing-Fehler: ungeklärt, die Anleihe bleibt
                    Err(e) => {
                        chosen.push(bond.clone());
                        skipped.push(format!("{} ohne Nachtrag ({e})", bond.description));
                    }
                    // Erfolgreiche, aber leere Antwort: Nach KEEP_WITHOUT_UPDATE_DAYS ohne Kurse
                    // wird die Anleihe ersetzt; ihre Kurse bleiben für den Verlauf gespeichert.
                    Ok(_) => {
                        if (today - known.last_date).num_days() <= KEEP_WITHOUT_UPDATE_DAYS {
                            chosen.push(bond.clone());
                            skipped.push(format!("{} ohne Nachtrag (keine neuen Kurse)", bond.description));
                        } else {
                            skipped.push(format!("{} (seit {} keine Kurse)", bond.description, known.last_date));
                        }
                    }
                }
            } else {
                new_tries += 1;
                examined.insert(bond.conid);
                let probe = fetch_bid_bars(&client, contract, 1.months(), month_timeout).await;
                match probe {
                    Ok(p) if !p.is_empty() => {
                        tokio::time::sleep(IB_REQUEST_GAP).await;
                        let paced = pacer.ready(deadline).await;
                        let Some(year_timeout) = request_timeout(remaining(), IB_YEAR_TIMEOUT).filter(|_| paced) else {
                            // Nicht zu Ende geprüft: Eine bisherige Anleihe darf keep_unchecked behalten
                            examined.remove(&bond.conid);
                            aborted = true;
                            break;
                        };
                        match fetch_bid_bars(&client, contract, 1.years(), year_timeout).await {
                            Ok(prices) if prices.len() >= MIN_HISTORY_BARS => {
                                out.histories.push(BondPriceHistory {
                                    conid: bond.conid,
                                    source: BOND_PRICE_SOURCE.into(),
                                    prices,
                                    full: true,
                                });
                                chosen.push(bond.clone());
                            }
                            Ok(prices) => skipped.push(format!("{} (erst {} Handelstage)", bond.description, prices.len())),
                            Err(e) => skipped.push(format!("{} (Jahreshistorie: {e})", bond.description)),
                        }
                    }
                    Ok(_) => skipped.push(format!("{} (keine Kurse)", bond.description)),
                    Err(e) => skipped.push(format!("{} ({e})", bond.description)),
                }
            }
            tokio::time::sleep(IB_REQUEST_GAP).await;
        }
        if aborted {
            let eligible: std::collections::BTreeSet<i32> = candidates.iter().map(|c| c.conid).collect();
            skipped.extend(keep_unchecked(&mut chosen, prev, &examined, &eligible, stored));
            out.errors.push(format!("IB {}: Zeitbudget erschöpft, Auswahl nur teilweise geprüft", iss.key));
        }
        if chosen.is_empty() {
            let detail = if skipped.is_empty() { String::new() } else { format!(": {}", skipped.join(", ")) };
            out.errors.push(format!("IB {}: keine Anleihe mit ausreichender Kurshistorie{detail}", iss.key));
            continue;
        }
        out.skipped.extend(skipped.into_iter().map(|s| format!("{} {s}", iss.key)));
        out.selections.insert(iss.key.clone(), chosen);
    }
    drop(client);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_timeouts_never_exceed_the_remaining_budget() {
        assert_eq!(request_timeout(Duration::from_secs(600), IB_YEAR_TIMEOUT), Some(IB_YEAR_TIMEOUT));
        assert_eq!(request_timeout(Duration::from_secs(40), IB_YEAR_TIMEOUT), Some(Duration::from_secs(40)));
        assert_eq!(request_timeout(Duration::from_secs(4), IB_MONTH_TIMEOUT), None);
    }

    #[test]
    fn an_aborted_run_keeps_unchecked_bonds_of_the_previous_basket() {
        let bond = |conid: i32| SelectedBond {
            conid,
            description: format!("X {conid}"),
            coupon_pct: 5.0,
            maturity: NaiveDate::from_ymd_opt(2031, 1, 1).unwrap(),
        };
        let stored_bond = || StoredBond {
            source: BOND_PRICE_SOURCE.into(),
            last_date: NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
        };
        let prev = vec![bond(1), bond(2), bond(3)];
        // Nur Anleihe 1 wurde geprüft, 3 hat keine gespeicherten Kurse
        let stored = BTreeMap::from([(1, stored_bond()), (2, stored_bond())]);
        let examined = std::collections::BTreeSet::from([1]);
        let eligible = std::collections::BTreeSet::from([1, 2, 3]);
        let mut chosen = vec![bond(1)];
        let notes = keep_unchecked(&mut chosen, &prev, &examined, &eligible, &stored);
        assert_eq!(chosen.iter().map(|b| b.conid).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(notes.len(), 1);
        // Geprüfte und verworfene Anleihen kommen nicht zurück
        let mut chosen = Vec::new();
        let examined = std::collections::BTreeSet::from([1, 2]);
        keep_unchecked(&mut chosen, &prev, &examined, &eligible, &stored);
        assert!(chosen.is_empty());
        // Nicht mehr zulässige Anleihen (etwa kurz vor Fälligkeit) auch nicht
        let mut chosen = Vec::new();
        let examined = std::collections::BTreeSet::new();
        keep_unchecked(&mut chosen, &prev, &examined, &std::collections::BTreeSet::from([1]), &stored);
        assert_eq!(chosen.iter().map(|b| b.conid).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn only_the_no_data_notice_counts_as_definitive() {
        assert!(is_no_data_notice(162, "Historical Market Data Service error message:HMDS query returned no data: XYZ"));
        assert!(!is_no_data_notice(162, "Historical Market Data Service error message:Historical data request pacing violation"));
        assert!(!is_no_data_notice(1100, "Connectivity between IB and Trader Workstation has been lost."));
    }

    #[test]
    fn fred_csv_parsing_skips_gaps_and_checks_header() {
        let body = "observation_date,BAMLH0A0HYM2\n2026-11-20,3.10\n2026-11-21,.\n2026-11-24,3.15\n";
        let pts = parse_fred_csv(body, "BAMLH0A0HYM2").unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[1], (NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(), 3.15));
        assert!(parse_fred_csv("<html>", "BAMLH0A0HYM2").is_err());
        assert!(parse_fred_csv("observation_date,OTHER\n", "BAMLH0A0HYM2").is_err());
    }

    #[test]
    fn settings_validation_normalizes_and_rejects() {
        let mut s = CreditSettings::default();
        s.issuers[0].key = " orcl ".into();
        let v = s.clone().validated().unwrap();
        assert_eq!(v.issuers[0].key, "ORCL");
        let mut dup = CreditSettings::default();
        dup.issuers[1].key = "ORCL".into();
        assert!(dup.validated().is_err());
        let mut bad = CreditSettings::default();
        bad.issuers[0].agency_rating = "gut".into();
        assert!(bad.validated().is_err());
        let host = CreditSettings {
            ib_host: "127.0.0.1; rm".into(),
            ..Default::default()
        };
        assert!(host.validated().is_err());
    }

    #[test]
    fn client_ids_stay_in_own_range() {
        for seed in [0, 7, u32::MAX] {
            let ids: Vec<i32> = (0..3).map(|a| pick_client_id(0, seed, a)).collect();
            assert!(ids.iter().all(|id| (8300..=8399).contains(id)), "{ids:?}");
            assert_ne!(ids[0], ids[1]);
        }
        // Fest eingestellte ID: nie eine fremde probieren
        assert_eq!(pick_client_id(42, 5, 2), 42);
        assert_eq!(socket_addr("::1", 7496), "[::1]:7496");
        assert_eq!(socket_addr("127.0.0.1", 7496), "127.0.0.1:7496");
        let too_big = CreditSettings { ib_client_id: IB_CLIENT_ID_MAX + 1, ..Default::default() };
        assert!(too_big.validated().is_err());
    }

    #[test]
    fn series_lists_split_fred_and_treasury() {
        let fred = fred_series_ids();
        assert!(fred.contains(&"BAMLC0A4CBBB"));
        assert_eq!(fred.len(), 1 + 7);
        let all = all_credit_series_ids();
        assert!(all.contains(&"UST10Y"));
        assert_eq!(all.len(), 1 + 7 + 11);
    }

    #[test]
    fn treasury_csv_maps_columns_and_skips_gaps() {
        let body = "Date,\"1 Mo\",\"1.5 Month\",\"2 Mo\",\"3 Mo\",\"4 Mo\",\"6 Mo\",\"1 Yr\",\"2 Yr\",\"3 Yr\",\"5 Yr\",\"7 Yr\",\"10 Yr\",\"20 Yr\",\"30 Yr\"\n\
                    11/25/2026,4.10,4.11,4.12,4.20,4.21,4.30,4.40,4.50,4.55,4.60,4.70,4.80,5.10,5.05\n\
                    11/24/2026,,4.11,4.12,4.20,4.21,4.30,4.40,4.50,4.55,4.60,4.70,4.81,5.10,5.05\n";
        let out = parse_treasury_csv(body).unwrap();
        let ten = &out["UST10Y"];
        assert_eq!(ten.len(), 2);
        assert_eq!(ten[0], (NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(), 4.81));
        assert_eq!(out["UST1M"].len(), 1);
        assert!(!out.contains_key("1.5 Month"));
        assert!(parse_treasury_csv("<html></html>").is_err());
    }
}
