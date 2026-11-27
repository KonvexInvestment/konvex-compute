//! Speicherung der Kredit- und Anleihedaten in der lokalen Datenbank sowie das
//! Zusammenstellen der Modelleingaben.

use crate::credit::{is_treasury_series, issuer_spread_series, BasketBond, BasketInterval, SelectedBond};
use crate::credit_fetch::{all_credit_series_ids, CreditSettings, IbFetchOutcome, StoredBond};
use crate::regime::{CurveDay, CurveNode, IssuerSeries, ModelInputs, Series};
use crate::store::ComputeStore;
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use std::collections::BTreeMap;

const SETTINGS_KEY: &str = "credit_settings";
/// Untergrenze beim Laden: vor allen Daten, die die Software speichern kann.
const EARLIEST: NaiveDate = NaiveDate::from_ymd_opt(1990, 1, 1).unwrap();

/// Nur die Felder eines gespeicherten Verfalls, die das Modell braucht.
#[derive(Deserialize)]
struct CurvePointLight {
    delivery_date: String,
    median: Option<f64>,
    p25: Option<f64>,
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// Überführt den Korb aus der ersten Fassung (ohne Gültigkeit) in `credit_basket`:
/// Die bisherigen Anleihen gelten als erster Korb über die ganze Historie.
pub(crate) fn migrate_credit_tables(conn: &Connection) -> Result<(), String> {
    let old: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'credit_bonds')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("Migration credit_bonds: {e}"))?;
    if old {
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             INSERT OR IGNORE INTO credit_basket (issuer, conid, description, coupon, maturity, valid_from, valid_to)
                 SELECT issuer, conid, description, coupon, maturity, '{}', NULL FROM credit_bonds;
             DROP TABLE credit_bonds;
             COMMIT;",
            iso(EARLIEST)
        ))
        .map_err(|e| format!("Migration credit_bonds: {e}"))?;
    }
    Ok(())
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

impl ComputeStore {
    pub fn get_meta(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.connect()?;
        conn.query_row("SELECT value FROM meta_kv WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .map_err(|e| format!("meta_kv {key}: {e}"))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO meta_kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map(|_| ())
        .map_err(|e| format!("meta_kv {key}: {e}"))
    }

    pub fn load_credit_settings(&self) -> CreditSettings {
        self.get_meta(SETTINGS_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<CreditSettings>(&s).ok())
            .unwrap_or_default()
    }

    /// Speichert die Einstellungen in einer Transaktion. Ändert sich ein Agenturrating, gilt das
    /// neue ab `today`; frühere Tage behalten das alte. Korb, Kurse und Ratings entfernter
    /// Emittenten werden gelöscht, ausgeschaltete behalten ihre Daten.
    pub fn save_credit_settings(&self, settings: &CreditSettings, today: NaiveDate) -> Result<(), String> {
        let json = serde_json::to_string(settings).map_err(|e| format!("Einstellungen: {e}"))?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("Einstellungen: {e}"))?;
        let previous: CreditSettings = tx
            .query_row("SELECT value FROM meta_kv WHERE key = ?1", params![SETTINGS_KEY], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| format!("Einstellungen: {e}"))?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        tx.execute(
            "INSERT INTO meta_kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SETTINGS_KEY, json],
        )
        .map_err(|e| format!("Einstellungen: {e}"))?;

        for iss in &settings.issuers {
            let latest: Option<String> = tx
                .query_row(
                    "SELECT rating FROM credit_ratings WHERE issuer = ?1 ORDER BY valid_from DESC LIMIT 1",
                    params![iss.key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| format!("credit_ratings: {e}"))?;
            // Ohne Eintrag galt bisher das Rating der vorigen Einstellungen (oder der Voreinstellung)
            let base = match latest {
                Some(r) => r,
                None => {
                    let r0 = previous
                        .issuers
                        .iter()
                        .find(|p| p.key == iss.key)
                        .map_or_else(|| iss.agency_rating.clone(), |p| p.agency_rating.clone());
                    tx.execute(
                        "INSERT INTO credit_ratings (issuer, valid_from, rating) VALUES (?1, ?2, ?3)",
                        params![iss.key, iso(EARLIEST), r0],
                    )
                    .map_err(|e| format!("credit_ratings: {e}"))?;
                    r0
                }
            };
            if base != iss.agency_rating {
                tx.execute(
                    "INSERT INTO credit_ratings (issuer, valid_from, rating) VALUES (?1, ?2, ?3)
                     ON CONFLICT(issuer, valid_from) DO UPDATE SET rating = excluded.rating",
                    params![iss.key, iso(today), iss.agency_rating],
                )
                .map_err(|e| format!("credit_ratings: {e}"))?;
            }
        }

        let stored: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT issuer FROM credit_basket UNION SELECT issuer FROM credit_ratings")
                .map_err(|e| format!("credit_basket: {e}"))?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| format!("credit_basket: {e}"))?;
            rows.flatten().collect()
        };
        for issuer in stored.iter().filter(|k| !settings.issuers.iter().any(|i| &i.key == *k)) {
            tx.execute("DELETE FROM credit_basket WHERE issuer = ?1", params![issuer])
                .map_err(|e| format!("credit_basket: {e}"))?;
            tx.execute("DELETE FROM credit_ratings WHERE issuer = ?1", params![issuer])
                .map_err(|e| format!("credit_ratings: {e}"))?;
        }
        tx.execute("DELETE FROM bond_prices WHERE conid NOT IN (SELECT conid FROM credit_basket)", [])
            .map_err(|e| format!("bond_prices: {e}"))?;
        tx.commit().map_err(|e| format!("Einstellungen: {e}"))
    }

    pub fn latest_credit_date(&self, series_id: &str) -> Result<Option<NaiveDate>, String> {
        let conn = self.connect()?;
        let s: Option<String> = conn
            .query_row(
                "SELECT MAX(date_iso) FROM credit_series WHERE series_id = ?1",
                params![series_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("credit_series: {e}"))?;
        Ok(s.as_deref().and_then(parse_date))
    }

    pub fn save_credit_series(&self, series_id: &str, points: &[(NaiveDate, f64)]) -> Result<usize, String> {
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("credit_series: {e}"))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO credit_series (series_id, date_iso, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT(series_id, date_iso) DO UPDATE SET value = excluded.value",
                )
                .map_err(|e| format!("credit_series: {e}"))?;
            for (d, v) in points {
                stmt.execute(params![series_id, d.format("%Y-%m-%d").to_string(), v])
                    .map_err(|e| format!("credit_series: {e}"))?;
            }
        }
        tx.commit().map_err(|e| format!("credit_series: {e}"))?;
        Ok(points.len())
    }

    pub fn load_credit_series(&self, ids: &[&str], from: NaiveDate) -> Result<BTreeMap<String, Series>, String> {
        Self::load_credit_series_in(&self.connect()?, ids, from)
    }

    fn load_credit_series_in(conn: &Connection, ids: &[&str], from: NaiveDate) -> Result<BTreeMap<String, Series>, String> {
        let mut stmt = conn
            .prepare("SELECT date_iso, value FROM credit_series WHERE series_id = ?1 AND date_iso >= ?2 ORDER BY date_iso")
            .map_err(|e| format!("credit_series: {e}"))?;
        let mut out = BTreeMap::new();
        for id in ids {
            let rows = stmt
                .query_map(params![id, from.format("%Y-%m-%d").to_string()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| format!("credit_series: {e}"))?;
            let mut series = Series::new();
            for row in rows.flatten() {
                if let Some(d) = parse_date(&row.0) {
                    series.insert(d, row.1);
                }
            }
            out.insert((*id).to_string(), series);
        }
        Ok(out)
    }

    /// Schreibt Korb und Kurse der abgerufenen Emittenten in einer Transaktion.
    /// Neue Anleihen gehören ab `today` zum Korb, beim ersten Korb eines Emittenten über die
    /// ganze Historie; ausgeschiedene gelten bis zum Vortag und behalten ihre Kurse, damit
    /// frühere Tage gleich bleiben. Emittenten, die inzwischen aus den Einstellungen entfernt
    /// wurden, schreibt das Ergebnis nicht zurück. Eine Kursreihe mischt nie Quellen.
    pub fn save_ib_outcome(&self, outcome: &IbFetchOutcome, today: NaiveDate) -> Result<usize, String> {
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("IB-Daten: {e}"))?;
        let current: CreditSettings = tx
            .query_row("SELECT value FROM meta_kv WHERE key = ?1", params![SETTINGS_KEY], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| format!("Einstellungen: {e}"))?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut rows = 0;
        let mut written: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
        for (issuer, bonds) in &outcome.selections {
            if !current.issuers.iter().any(|i| &i.key == issuer) {
                continue;
            }
            let (has_any, open): (bool, Vec<(i32, String)>) = {
                let any: bool = tx
                    .query_row("SELECT EXISTS(SELECT 1 FROM credit_basket WHERE issuer = ?1)", params![issuer], |r| r.get(0))
                    .map_err(|e| format!("credit_basket: {e}"))?;
                let mut stmt = tx
                    .prepare("SELECT conid, valid_from FROM credit_basket WHERE issuer = ?1 AND valid_to IS NULL")
                    .map_err(|e| format!("credit_basket: {e}"))?;
                let open = stmt
                    .query_map(params![issuer], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, String>(1)?)))
                    .map_err(|e| format!("credit_basket: {e}"))?
                    .flatten()
                    .collect();
                (any, open)
            };
            let start = if has_any { today } else { EARLIEST };
            for (conid, from) in open.iter().filter(|(c, _)| !bonds.iter().any(|b| b.conid == *c)) {
                if parse_date(from).is_some_and(|f| f >= today) {
                    // Erst heute aufgenommen: Der Eintrag hat noch keinen Tag getragen
                    tx.execute(
                        "DELETE FROM credit_basket WHERE issuer = ?1 AND conid = ?2 AND valid_from = ?3",
                        params![issuer, conid, from],
                    )
                } else {
                    tx.execute(
                        "UPDATE credit_basket SET valid_to = ?4 WHERE issuer = ?1 AND conid = ?2 AND valid_from = ?3",
                        params![issuer, conid, from, iso(today - chrono::Duration::days(1))],
                    )
                }
                .map_err(|e| format!("credit_basket: {e}"))?;
            }
            for b in bonds.iter().filter(|b| !open.iter().any(|(c, _)| *c == b.conid)) {
                tx.execute(
                    "INSERT INTO credit_basket (issuer, conid, description, coupon, maturity, valid_from, valid_to)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
                     ON CONFLICT(issuer, conid, valid_from) DO UPDATE SET valid_to = NULL",
                    params![issuer, b.conid, b.description, b.coupon_pct, iso(b.maturity), iso(start)],
                )
                .map_err(|e| format!("credit_basket: {e}"))?;
            }
            written.extend(bonds.iter().map(|b| b.conid));
        }
        for h in outcome.histories.iter().filter(|h| written.contains(&h.conid)) {
            let stored_source: Option<String> = tx
                .query_row("SELECT source FROM bond_prices WHERE conid = ?1 LIMIT 1", params![h.conid], |r| r.get(0))
                .optional()
                .map_err(|e| format!("bond_prices: {e}"))?;
            let same_source = stored_source.as_deref() == Some(h.source.as_str());
            // Ein Monatsnachtrag ergänzt nur eine Reihe derselben Quelle
            if !h.full && !same_source {
                continue;
            }
            // Eine volle Historie aus anderer Quelle ersetzt die Reihe; aus derselben Quelle
            // ergänzt sie, damit ältere Kurse jenseits des Jahresfensters erhalten bleiben.
            if h.full && !same_source {
                tx.execute("DELETE FROM bond_prices WHERE conid = ?1", params![h.conid])
                    .map_err(|e| format!("bond_prices: {e}"))?;
            }
            for (d, p) in &h.prices {
                tx.execute(
                    "INSERT INTO bond_prices (conid, date_iso, price, source) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(conid, date_iso) DO UPDATE SET price = excluded.price, source = excluded.source",
                    params![h.conid, iso(*d), p, h.source],
                )
                .map_err(|e| format!("bond_prices: {e}"))?;
                rows += 1;
            }
        }
        tx.execute("DELETE FROM bond_prices WHERE conid NOT IN (SELECT conid FROM credit_basket)", [])
            .map_err(|e| format!("bond_prices: {e}"))?;
        tx.commit().map_err(|e| format!("IB-Daten: {e}"))?;
        Ok(rows)
    }

    pub fn load_bond_selection(&self) -> Result<BTreeMap<String, Vec<SelectedBond>>, String> {
        Self::load_bond_selection_in(&self.connect()?)
    }

    fn load_bond_selection_in(conn: &Connection) -> Result<BTreeMap<String, Vec<SelectedBond>>, String> {
        let mut stmt = conn
            .prepare(
                "SELECT issuer, conid, description, coupon, maturity FROM credit_basket
                 WHERE valid_to IS NULL ORDER BY issuer, maturity",
            )
            .map_err(|e| format!("credit_basket: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| format!("credit_basket: {e}"))?;
        let mut out: BTreeMap<String, Vec<SelectedBond>> = BTreeMap::new();
        for (issuer, conid, description, coupon_pct, maturity) in rows.flatten() {
            if let Some(maturity) = parse_date(&maturity) {
                out.entry(issuer).or_default().push(SelectedBond {
                    conid,
                    description,
                    coupon_pct,
                    maturity,
                });
            }
        }
        Ok(out)
    }

    /// Alle Korbeinträge je Emittent, eine Anleihe mit allen ihren Zeiträumen.
    fn load_basket_in(conn: &Connection) -> Result<BTreeMap<String, Vec<BasketBond>>, String> {
        let mut stmt = conn
            .prepare(
                "SELECT issuer, conid, description, coupon, maturity, valid_from, valid_to FROM credit_basket
                 ORDER BY issuer, maturity, conid, valid_from",
            )
            .map_err(|e| format!("credit_basket: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| format!("credit_basket: {e}"))?;
        let mut out: BTreeMap<String, Vec<BasketBond>> = BTreeMap::new();
        for (issuer, conid, description, coupon_pct, maturity, from, to) in rows.flatten() {
            let (Some(maturity), Some(from)) = (parse_date(&maturity), parse_date(&from)) else {
                continue;
            };
            let interval: BasketInterval = (from, to.as_deref().and_then(parse_date));
            let list = out.entry(issuer).or_default();
            match list.iter_mut().find(|b| b.bond.conid == conid) {
                Some(b) => b.active.push(interval),
                None => list.push(BasketBond {
                    bond: SelectedBond { conid, description, coupon_pct, maturity },
                    active: vec![interval],
                }),
            }
        }
        Ok(out)
    }

    /// Agenturratings je Emittent ab Gültigkeitsbeginn, aufsteigend.
    fn load_ratings_in(conn: &Connection) -> Result<BTreeMap<String, Vec<(NaiveDate, String)>>, String> {
        let mut stmt = conn
            .prepare("SELECT issuer, valid_from, rating FROM credit_ratings ORDER BY issuer, valid_from")
            .map_err(|e| format!("credit_ratings: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("credit_ratings: {e}"))?;
        let mut out: BTreeMap<String, Vec<(NaiveDate, String)>> = BTreeMap::new();
        for (issuer, from, rating) in rows.flatten() {
            if let Some(from) = parse_date(&from) {
                out.entry(issuer).or_default().push((from, rating));
            }
        }
        Ok(out)
    }

    /// Kursquelle und letzter gespeicherter Tag je Anleihe.
    pub fn load_stored_bonds(&self) -> Result<BTreeMap<i32, StoredBond>, String> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT conid, MIN(source), MAX(date_iso) FROM bond_prices GROUP BY conid")
            .map_err(|e| format!("bond_prices: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("bond_prices: {e}"))?;
        Ok(rows
            .flatten()
            .filter_map(|(conid, source, last)| Some((conid, StoredBond { source, last_date: parse_date(&last)? })))
            .collect())
    }

    /// Tageskurse und Kursquelle je Anleihe.
    pub fn load_bond_prices(&self, conids: &[i32]) -> Result<BTreeMap<i32, (Series, String)>, String> {
        Self::load_bond_prices_in(&self.connect()?, conids)
    }

    fn load_bond_prices_in(conn: &Connection, conids: &[i32]) -> Result<BTreeMap<i32, (Series, String)>, String> {
        let mut stmt = conn
            .prepare("SELECT date_iso, price, source FROM bond_prices WHERE conid = ?1 ORDER BY date_iso")
            .map_err(|e| format!("bond_prices: {e}"))?;
        let mut out = BTreeMap::new();
        for conid in conids {
            let rows = stmt
                .query_map(params![conid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?))
                })
                .map_err(|e| format!("bond_prices: {e}"))?;
            let mut series = Series::new();
            let mut source = String::new();
            for (d, p, s) in rows.flatten() {
                if let Some(d) = parse_date(&d) {
                    series.insert(d, p);
                    source = s;
                }
            }
            if !series.is_empty() {
                out.insert(*conid, (series, source));
            }
        }
        Ok(out)
    }

    /// Ein Kurvenstand je Tag ab `from`: der letzte Live-Snapshot, sonst der rekonstruierte Tag.
    pub fn load_curve_days(&self, gpu: &str, from: NaiveDate) -> Result<Vec<CurveDay>, String> {
        Self::load_curve_days_in(&self.connect()?, gpu, from)
    }

    fn load_curve_days_in(conn: &Connection, gpu: &str, from: NaiveDate) -> Result<Vec<CurveDay>, String> {
        // Nur eine Zeile je Tag aus SQLite holen (live vor Backfill, dann die neueste), statt alle
        // 15-Minuten-Stände der letzten 48 Stunden samt Kurven-JSON zu laden. Die Auswahl läuft
        // ohne das JSON; erst die Gewinnerzeilen lesen es.
        let mut stmt = conn
            .prepare(
                "SELECT c.snapshot_date, c.spot_price, c.expiries_json
                 FROM curve_snapshots c
                 JOIN (
                     SELECT id, ROW_NUMBER() OVER (
                                PARTITION BY snapshot_date
                                ORDER BY CASE snapshot_kind WHEN 'live' THEN 1 ELSE 0 END DESC,
                                         captured_at DESC, id DESC
                            ) AS rn
                     FROM curve_snapshots
                     WHERE gpu = ?1 AND snapshot_date >= ?2
                 ) w ON w.id = c.id
                 WHERE w.rn = 1
                 ORDER BY c.snapshot_date",
            )
            .map_err(|e| format!("curve_snapshots: {e}"))?;
        let rows = stmt
            .query_map(params![gpu, from.format("%Y-%m-%d").to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("curve_snapshots: {e}"))?;
        let mut by_day: BTreeMap<NaiveDate, (Option<f64>, String)> = BTreeMap::new();
        for (d, spot, json) in rows.flatten() {
            if let Some(d) = parse_date(&d) {
                by_day.insert(d, (spot, json));
            }
        }
        Ok(by_day
            .into_iter()
            .map(|(date, (spot, json))| {
                let points: Vec<CurvePointLight> = serde_json::from_str(&json).unwrap_or_default();
                let nodes = points
                    .into_iter()
                    .filter_map(|p| Some(CurveNode::from_delivery(parse_date(&p.delivery_date)?, p.median, p.p25)))
                    .collect();
                CurveDay {
                    date,
                    spot: spot.filter(|s| s.is_finite() && *s > 0.0),
                    nodes,
                }
            })
            .collect())
    }

    /// Stellt alle Eingaben des Regime-Modells für eine GPU zusammen.
    pub fn build_model_inputs(
        &self,
        gpu: &str,
        settings: &CreditSettings,
    ) -> Result<(ModelInputs, Vec<IssuerBondInfo>), String> {
        // Eine gemeinsame Lesetransaktion: Ein parallel gespeicherter IB-Abruf kann sonst
        // zwischen Auswahl und Kursen liegen und Anleihen ohne Kurse hinterlassen.
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|e| format!("Lesetransaktion: {e}"))?;
        // Alles Gespeicherte laden: Der Modellstart hängt dann nur an den Daten, nicht am Datum
        // des Laufs, und frühere Tageswerte bleiben bei späteren Läufen gleich.
        let all = Self::load_credit_series_in(&tx, &all_credit_series_ids(), EARLIEST)?;
        let (treasury, oas): (BTreeMap<String, Series>, BTreeMap<String, Series>) =
            all.into_iter().partition(|(k, _)| is_treasury_series(k));

        let basket = Self::load_basket_in(&tx)?;
        let ratings = Self::load_ratings_in(&tx)?;
        let mut issuers = Vec::new();
        let mut infos = Vec::new();
        for iss in settings.issuers.iter().filter(|i| i.enabled) {
            let members = basket.get(&iss.key).cloned().unwrap_or_default();
            let conids: Vec<i32> = members.iter().map(|b| b.bond.conid).collect();
            let prices = Self::load_bond_prices_in(&tx, &conids)?;
            let price_series: BTreeMap<i32, Series> = prices.iter().map(|(k, (s, _))| (*k, s.clone())).collect();
            let spreads = issuer_spread_series(&members, &price_series, &treasury);
            // Angezeigt wird der aktuelle Korb; der Verlauf nutzt alle früheren Mitglieder
            let bonds: Vec<SelectedBond> = members
                .iter()
                .filter(|b| b.active.iter().any(|(_, to)| to.is_none()))
                .map(|b| b.bond.clone())
                .collect();
            for b in &bonds {
                let (last, source) = prices
                    .get(&b.conid)
                    .map(|(s, src)| (s.iter().next_back().map(|(d, p)| (*d, *p)), src.clone()))
                    .unwrap_or((None, String::new()));
                infos.push(IssuerBondInfo {
                    issuer: iss.key.clone(),
                    description: b.description.clone(),
                    maturity: b.maturity,
                    last_price: last.map(|x| x.1),
                    last_date: last.map(|x| x.0),
                    source,
                });
            }
            issuers.push(IssuerSeries {
                key: iss.key.clone(),
                label: iss.label.clone(),
                agency_rating: iss.agency_rating.clone(),
                ratings: ratings.get(&iss.key).cloned().unwrap_or_default(),
                members: spreads.members,
                spreads_bp: spreads.level,
                linked_bp: spreads.linked,
                bonds: bonds.iter().map(|b| b.description.clone()).collect(),
            });
        }

        let curve = Self::load_curve_days_in(&tx, gpu, EARLIEST)?;
        drop(tx);
        Ok((
            ModelInputs {
                gpu: gpu.to_string(),
                oas,
                issuers,
                curve,
            },
            infos,
        ))
    }
}

/// Anzeige der verwendeten Anleihen je Emittent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IssuerBondInfo {
    pub issuer: String,
    pub description: String,
    pub maturity: NaiveDate,
    pub last_price: Option<f64>,
    pub last_date: Option<NaiveDate>,
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credit_fetch::BondPriceHistory;

    fn temp_store(name: &str) -> ComputeStore {
        let dir = std::env::temp_dir().join(format!("konvex-compute-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ComputeStore::open(dir.join("compute.db")).unwrap()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn credit_series_upsert_and_load() {
        let store = temp_store("series");
        store.save_credit_series("UST10Y", &[(d(2026, 11, 20), 4.5), (d(2026, 11, 21), 4.6)]).unwrap();
        store.save_credit_series("UST10Y", &[(d(2026, 11, 21), 4.7)]).unwrap();
        assert_eq!(store.latest_credit_date("UST10Y").unwrap(), Some(d(2026, 11, 21)));
        let all = store.load_credit_series(&["UST10Y", "UST2Y"], d(2026, 1, 1)).unwrap();
        assert_eq!(all["UST10Y"][&d(2026, 11, 21)], 4.7);
        assert!(all["UST2Y"].is_empty());
    }

    fn xyz_settings(store: &ComputeStore, rating: &str, today: NaiveDate) {
        let s = CreditSettings {
            issuers: vec![crate::credit_fetch::IssuerSettings {
                key: "XYZ".into(),
                label: "XYZ".into(),
                agency_rating: rating.into(),
                enabled: true,
            }],
            ..Default::default()
        };
        store.save_credit_settings(&s, today).unwrap();
    }

    #[test]
    fn ib_outcome_keeps_basket_history_and_never_mixes_sources() {
        let store = temp_store("ib");
        xyz_settings(&store, "BBB", d(2026, 11, 1));
        let bond = |conid, y| SelectedBond {
            conid,
            description: format!("XYZ 5 01/15/{y}"),
            coupon_pct: 5.0,
            maturity: d(2000 + y, 1, 15),
        };
        let hist = |conid, source: &str, prices: Vec<(NaiveDate, f64)>, full| BondPriceHistory {
            conid,
            source: source.into(),
            prices,
            full,
        };
        let mut first = IbFetchOutcome::default();
        first.selections.insert("XYZ".into(), vec![bond(1, 31), bond(2, 33)]);
        first.histories.push(hist(1, "mid", vec![(d(2026, 11, 20), 99.0)], true));
        first.histories.push(hist(2, "bid", vec![(d(2026, 11, 20), 97.0)], true));
        store.save_ib_outcome(&first, d(2026, 11, 20)).unwrap();

        // Monatsnachtrag mit anderer Quelle wird verworfen, volle Historie ersetzt
        let mut second = IbFetchOutcome::default();
        second.selections.insert("XYZ".into(), vec![bond(1, 31), bond(3, 34)]);
        second.histories.push(hist(1, "bid", vec![(d(2026, 11, 21), 98.0)], false));
        store.save_ib_outcome(&second, d(2026, 11, 22)).unwrap();
        assert_eq!(store.load_bond_prices(&[1]).unwrap()[&1].1, "mid");
        second.histories[0].full = true;
        store.save_ib_outcome(&second, d(2026, 11, 22)).unwrap();

        // Gleiche Quelle: Nachtrag ergänzt und überschreibt den Tag
        let mut third = IbFetchOutcome::default();
        third.selections.insert("XYZ".into(), vec![bond(1, 31), bond(3, 34)]);
        third.histories.push(hist(1, "bid", vec![(d(2026, 11, 21), 98.5), (d(2026, 11, 22), 98.7)], false));
        store.save_ib_outcome(&third, d(2026, 11, 22)).unwrap();

        let sel = store.load_bond_selection().unwrap();
        let ids: Vec<i32> = sel["XYZ"].iter().map(|b| b.conid).collect();
        assert_eq!(ids, vec![1, 3]);
        let prices = store.load_bond_prices(&[1, 2]).unwrap();
        assert_eq!(prices[&1].0.len(), 2);
        assert_eq!(prices[&1].0[&d(2026, 11, 21)], 98.5);
        assert_eq!(prices[&1].1, "bid");
        // Anleihe 2 ist ausgeschieden, ihre Kurse bleiben für den Verlauf
        assert_eq!(prices[&2].0[&d(2026, 11, 20)], 97.0);
        let basket = ComputeStore::load_basket_in(&store.connect().unwrap()).unwrap();
        let of = |c: i32| basket["XYZ"].iter().find(|b| b.bond.conid == c).unwrap().active.clone();
        assert_eq!(of(1), vec![(EARLIEST, None)]);
        assert_eq!(of(2), vec![(EARLIEST, Some(d(2026, 11, 21)))]);
        assert_eq!(of(3), vec![(d(2026, 11, 22), None)]);
        let stored = store.load_stored_bonds().unwrap();
        assert_eq!(stored[&1].last_date, d(2026, 11, 22));
    }

    #[test]
    fn removed_issuers_are_cleaned_and_not_written_back() {
        let store = temp_store("removed");
        xyz_settings(&store, "BBB", d(2026, 11, 1));
        let bond = SelectedBond { conid: 7, description: "XYZ 5 01/15/31".into(), coupon_pct: 5.0, maturity: d(2031, 1, 15) };
        let mut outcome = IbFetchOutcome::default();
        outcome.selections.insert("XYZ".into(), vec![bond]);
        outcome.histories.push(BondPriceHistory { conid: 7, source: "bid".into(), prices: vec![(d(2026, 11, 20), 99.0)], full: true });
        store.save_ib_outcome(&outcome, d(2026, 11, 20)).unwrap();
        // Ausschalten behält die Daten
        let mut s = store.load_credit_settings();
        s.issuers[0].enabled = false;
        store.save_credit_settings(&s, d(2026, 11, 21)).unwrap();
        assert!(store.load_bond_prices(&[7]).unwrap().contains_key(&7));
        // Entfernen löscht sie, und ein spät fertiger Abruf legt sie nicht wieder an
        s.issuers.clear();
        store.save_credit_settings(&s, d(2026, 11, 21)).unwrap();
        assert!(store.load_bond_prices(&[7]).unwrap().is_empty());
        store.save_ib_outcome(&outcome, d(2026, 11, 21)).unwrap();
        assert!(store.load_bond_selection().unwrap().is_empty());
        assert!(store.load_bond_prices(&[7]).unwrap().is_empty());
    }

    #[test]
    fn rating_changes_apply_from_the_day_of_the_change() {
        let store = temp_store("ratings");
        xyz_settings(&store, "BBB", d(2026, 11, 1));
        xyz_settings(&store, "BB+", d(2026, 11, 20));
        xyz_settings(&store, "BB+", d(2026, 11, 21));
        let ratings = ComputeStore::load_ratings_in(&store.connect().unwrap()).unwrap();
        assert_eq!(
            ratings["XYZ"],
            vec![(EARLIEST, "BBB".to_string()), (d(2026, 11, 20), "BB+".to_string())]
        );
    }

    #[test]
    fn the_first_basket_migrates_with_full_history() {
        let store = temp_store("migrate");
        let conn = store.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE credit_bonds (issuer TEXT NOT NULL, conid INTEGER NOT NULL, description TEXT NOT NULL,
                 coupon REAL NOT NULL, maturity TEXT NOT NULL, PRIMARY KEY (issuer, conid));
             INSERT INTO credit_bonds VALUES ('XYZ', 7, 'XYZ 5 01/15/31', 5.0, '2031-01-15');",
        )
        .unwrap();
        migrate_credit_tables(&conn).unwrap();
        let basket = ComputeStore::load_basket_in(&conn).unwrap();
        assert_eq!(basket["XYZ"][0].active, vec![(EARLIEST, None)]);
        // Die alte Tabelle ist entfernt, eine zweite Migration ändert nichts
        migrate_credit_tables(&conn).unwrap();
        assert_eq!(ComputeStore::load_basket_in(&conn).unwrap(), basket);
    }

    #[test]
    fn settings_round_trip_with_default_fallback() {
        let store = temp_store("settings");
        assert_eq!(store.load_credit_settings(), CreditSettings::default());
        let s = CreditSettings {
            ib_port: 7497,
            ..Default::default()
        };
        store.save_credit_settings(&s, d(2026, 11, 20)).unwrap();
        assert_eq!(store.load_credit_settings().ib_port, 7497);
    }

    #[test]
    fn curve_days_take_the_latest_live_snapshot_per_day() {
        let store = temp_store("curve-days");
        let conn = store.connect().unwrap();
        let json = |median: f64| format!(r#"[{{"delivery_date":"2027-01-31","median":{median},"p25":{}}}]"#, median * 0.9);
        let rows = [
            ("2026-11-20T23:00:00Z", "2026-11-20", "daily_backfill", 1.0, json(2.0)),
            ("2026-11-20T10:00:00Z", "2026-11-20", "live", 1.5, json(2.1)),
            ("2026-11-20T12:00:00Z", "2026-11-20", "live", 1.7, json(2.2)),
            ("2026-11-21T23:00:00Z", "2026-11-21", "daily_backfill", 3.0, json(2.3)),
        ];
        for (captured, day, kind, spot, body) in rows {
            conn.execute(
                "INSERT INTO curve_snapshots (
                    captured_at, snapshot_date, snapshot_kind, gpu, spot_price,
                    total_liquidity, total_volume_24h, total_volume_total, expiry_count, expiries_json
                ) VALUES (?1, ?2, ?3, 'H100', ?4, 0, 0, 0, 1, ?5)",
                params![captured, day, kind, spot, body],
            )
            .unwrap();
        }
        let days = store.load_curve_days("H100", d(2026, 11, 1)).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].spot, Some(1.7));
        assert_eq!(days[0].nodes[0].median, Some(2.2));
        assert_eq!(days[1].spot, Some(3.0));
    }
}
