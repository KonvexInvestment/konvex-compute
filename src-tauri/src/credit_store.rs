//! Speicherung der Kredit- und Anleihedaten in der lokalen Datenbank sowie das
//! Zusammenstellen der Modelleingaben.

use crate::credit::{is_treasury_series, issuer_spread_series, SelectedBond};
use crate::credit_fetch::{all_credit_series_ids, CreditSettings, IbFetchOutcome, StoredBond};
use crate::regime::{CurveDay, CurveNode, IssuerSeries, ModelInputs, Series};
use crate::store::ComputeStore;
use chrono::NaiveDate;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use std::collections::BTreeMap;

const SETTINGS_KEY: &str = "credit_settings";

/// Nur die Felder eines gespeicherten Verfalls, die das Modell braucht.
#[derive(Deserialize)]
struct CurvePointLight {
    delivery_date: String,
    median: Option<f64>,
    p25: Option<f64>,
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

    pub fn save_credit_settings(&self, settings: &CreditSettings) -> Result<(), String> {
        let json = serde_json::to_string(settings).map_err(|e| format!("Einstellungen: {e}"))?;
        self.set_meta(SETTINGS_KEY, &json)
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
        let conn = self.connect()?;
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

    /// Ersetzt Auswahl und Kurse der abgerufenen Emittenten in einer Transaktion.
    /// Kurse je Anleihe werden vollständig ersetzt, damit eine Reihe nie Quellen mischt.
    pub fn save_ib_outcome(&self, outcome: &IbFetchOutcome) -> Result<usize, String> {
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("IB-Daten: {e}"))?;
        let mut rows = 0;
        for (issuer, bonds) in &outcome.selections {
            tx.execute("DELETE FROM credit_bonds WHERE issuer = ?1", params![issuer])
                .map_err(|e| format!("credit_bonds: {e}"))?;
            for b in bonds {
                tx.execute(
                    "INSERT INTO credit_bonds (issuer, conid, description, coupon, maturity) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![issuer, b.conid, b.description, b.coupon_pct, b.maturity.format("%Y-%m-%d").to_string()],
                )
                .map_err(|e| format!("credit_bonds: {e}"))?;
            }
        }
        for h in &outcome.histories {
            // Volle Historie ersetzt die Reihe; ein Monatsnachtrag ergänzt sie nur, wenn die
            // gespeicherte Reihe dieselbe Kursquelle hat, sonst würde sie Quellen mischen.
            let stored_source: Option<String> = tx
                .query_row("SELECT source FROM bond_prices WHERE conid = ?1 LIMIT 1", params![h.conid], |r| r.get(0))
                .optional()
                .map_err(|e| format!("bond_prices: {e}"))?;
            if !h.full && stored_source.as_deref() != Some(h.source.as_str()) {
                continue;
            }
            if h.full {
                tx.execute("DELETE FROM bond_prices WHERE conid = ?1", params![h.conid])
                    .map_err(|e| format!("bond_prices: {e}"))?;
            }
            for (d, p) in &h.prices {
                tx.execute(
                    "INSERT INTO bond_prices (conid, date_iso, price, source) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(conid, date_iso) DO UPDATE SET price = excluded.price, source = excluded.source",
                    params![h.conid, d.format("%Y-%m-%d").to_string(), p, h.source],
                )
                .map_err(|e| format!("bond_prices: {e}"))?;
                rows += 1;
            }
        }
        // Kurse nicht mehr ausgewählter Anleihen entfernen
        tx.execute("DELETE FROM bond_prices WHERE conid NOT IN (SELECT conid FROM credit_bonds)", [])
            .map_err(|e| format!("bond_prices: {e}"))?;
        tx.commit().map_err(|e| format!("IB-Daten: {e}"))?;
        Ok(rows)
    }

    pub fn load_bond_selection(&self) -> Result<BTreeMap<String, Vec<SelectedBond>>, String> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT issuer, conid, description, coupon, maturity FROM credit_bonds ORDER BY issuer, maturity")
            .map_err(|e| format!("credit_bonds: {e}"))?;
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
            .map_err(|e| format!("credit_bonds: {e}"))?;
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
        let conn = self.connect()?;
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
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT snapshot_date, spot_price, expiries_json FROM curve_snapshots
                 WHERE gpu = ?1 AND snapshot_date >= ?2
                 ORDER BY snapshot_date,
                          CASE snapshot_kind WHEN 'live' THEN 1 ELSE 0 END,
                          captured_at, id",
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
        // Spätere Zeilen desselben Tages überschreiben frühere (live nach Backfill, neueste zuletzt)
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
        today: NaiveDate,
    ) -> Result<(ModelInputs, Vec<IssuerBondInfo>), String> {
        let credit_from = today - chrono::Duration::days(crate::credit_fetch::FRED_HISTORY_DAYS + 30);
        let all = self.load_credit_series(&all_credit_series_ids(), credit_from)?;
        let (treasury, oas): (BTreeMap<String, Series>, BTreeMap<String, Series>) =
            all.into_iter().partition(|(k, _)| is_treasury_series(k));

        let selection = self.load_bond_selection()?;
        let mut issuers = Vec::new();
        let mut infos = Vec::new();
        for iss in settings.issuers.iter().filter(|i| i.enabled) {
            let bonds = selection.get(&iss.key).cloned().unwrap_or_default();
            let conids: Vec<i32> = bonds.iter().map(|b| b.conid).collect();
            let prices = self.load_bond_prices(&conids)?;
            let price_series: BTreeMap<i32, Series> = prices.iter().map(|(k, (s, _))| (*k, s.clone())).collect();
            let spreads = issuer_spread_series(&bonds, &price_series, &treasury);
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
                spreads_bp: spreads,
                bonds: bonds.iter().map(|b| b.description.clone()).collect(),
            });
        }

        let curve_from = today - chrono::Duration::days(800);
        let curve = self.load_curve_days(gpu, curve_from)?;
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

    #[test]
    fn ib_outcome_replaces_selection_and_drops_orphans() {
        let store = temp_store("ib");
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
        store.save_ib_outcome(&first).unwrap();

        // Monatsnachtrag mit anderer Quelle wird verworfen, volle Historie ersetzt
        let mut second = IbFetchOutcome::default();
        second.selections.insert("XYZ".into(), vec![bond(1, 31), bond(3, 34)]);
        second.histories.push(hist(1, "bid", vec![(d(2026, 11, 21), 98.0)], false));
        store.save_ib_outcome(&second).unwrap();
        assert_eq!(store.load_bond_prices(&[1]).unwrap()[&1].1, "mid");
        second.histories[0].full = true;
        store.save_ib_outcome(&second).unwrap();

        // Gleiche Quelle: Nachtrag ergänzt und überschreibt den Tag
        let mut third = IbFetchOutcome::default();
        third.selections.insert("XYZ".into(), vec![bond(1, 31), bond(3, 34)]);
        third.histories.push(hist(1, "bid", vec![(d(2026, 11, 21), 98.5), (d(2026, 11, 22), 98.7)], false));
        store.save_ib_outcome(&third).unwrap();

        let sel = store.load_bond_selection().unwrap();
        let ids: Vec<i32> = sel["XYZ"].iter().map(|b| b.conid).collect();
        assert_eq!(ids, vec![1, 3]);
        let prices = store.load_bond_prices(&[1, 2]).unwrap();
        assert_eq!(prices[&1].0.len(), 2);
        assert_eq!(prices[&1].0[&d(2026, 11, 21)], 98.5);
        assert_eq!(prices[&1].1, "bid");
        // Anleihe 2 ist nicht mehr im Korb, ihre Kurse sind entfernt
        assert!(!prices.contains_key(&2));
        let stored = store.load_stored_bonds().unwrap();
        assert_eq!(stored[&1].last_date, d(2026, 11, 22));
    }

    #[test]
    fn settings_round_trip_with_default_fallback() {
        let store = temp_store("settings");
        assert_eq!(store.load_credit_settings(), CreditSettings::default());
        let s = CreditSettings {
            ib_port: 7497,
            ..Default::default()
        };
        store.save_credit_settings(&s).unwrap();
        assert_eq!(store.load_credit_settings().ib_port, 7497);
    }
}
