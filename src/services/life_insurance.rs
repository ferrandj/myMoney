//! The lines of a life-insurance contract: which funds it holds, how many units of each, what they are worth.
//!
//! Nobody gives the user this list today. It is rebuilt from the papers of the contract, which is as good as they are:
//!
//! 1. the base is the most recent of the yearly statements (the funds held on 31 December, with units and unit
//!    values) and of the arbitrage letters that list the whole portfolio afterwards;
//! 2. every arbitrage letter after the base sells units of some funds and buys units of others: they are applied in
//!    order (what a letter says was sold must be held: when it is not, a letter is missing, and it is said);
//! 3. the payments of the operations export after the base go to the euro fund, whatever the contract did with
//!    them next (the total is right, the split between funds is an estimate).
//!
//! The value of a line is its live price when the fund is found by its ISIN (a price far from the last known one is
//! ignored: it is another class of the fund), otherwise the last unit value the papers gave. The contract's value is
//! the sum of its lines, so the account's cash is set to zero (the lines already hold the value).

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::adapters::base::ParsedStatement;
use crate::adapters::common::norm;
use crate::error::AppResult;
use crate::models::{setting_get, setting_set, Account, Document, Holding, Transaction};
use crate::py;

pub const SOURCE: &str = "life_insurance";
const EPSILON: &str = "0.0005";

#[derive(Clone, Debug)]
struct Position {
    name: String,
    isin: Option<String>,
    units: Decimal,
    unit_value: Decimal,
    as_of: Option<NaiveDate>,
    euro: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub base_date: Option<NaiveDate>,
    pub base_from: String,
    pub letters: usize,
    pub payments: Decimal,
    pub lines: usize,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

fn dec(v: Option<&Value>) -> Option<Decimal> {
    v.and_then(|x| x.as_str()).and_then(py::dec_parse)
}

fn day(v: Option<&Value>) -> Option<NaiveDate> {
    v.and_then(|x| x.as_str()).and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

fn key_of(isin: &Option<String>, name: &str) -> String {
    match isin {
        Some(i) => i.clone(),
        None => format!("euro:{}", norm(name)),
    }
}

/// One line of a table of funds (annual statement, letter): isin, name, units, unit value, amount.
fn position(line: &Value) -> Option<Position> {
    let name = line.get("fund")?.as_str()?.to_string();
    let isin = line.get("isin").and_then(|i| i.as_str()).filter(|i| !i.is_empty()).map(|i| i.to_string());
    let amount = dec(line.get("amount"))?;
    let euro = isin.is_none();
    // the euro fund is counted in euros: one unit is one euro
    let (units, unit_value) = if euro { (amount, Decimal::ONE) } else { (dec(line.get("units"))?, dec(line.get("unit_value"))?) };
    Some(Position { name, isin, units, unit_value, as_of: day(line.get("date")), euro })
}

fn statement_of(doc: &Document) -> Option<ParsedStatement> {
    doc.parse_result.as_ref()?.get("statements")?.as_array()?.first().map(ParsedStatement::from_json)
}

struct Letter {
    date: NaiveDate,
    sold: Vec<Value>,
    bought: Vec<Value>,
    portfolio_after: Vec<Value>,
}

/// Rebuild the lines of a life-insurance account from its papers. Nothing changes when the papers give no base.
pub fn rebuild(c: &Connection, account_id: i64) -> AppResult<Option<Estimate>> {
    let Some(mut account) = Account::get(c, account_id)? else { return Ok(None) };
    if account.account_type != "life_insurance" {
        return Ok(None);
    }
    let docs = Document::query(c, "WHERE account_id = ? AND status = 'imported'", [account_id])?;
    let mut yearly: Vec<(NaiveDate, Vec<Value>)> = vec![];
    let mut letters: Vec<Letter> = vec![];
    let mut csv_arbitrages: Vec<(NaiveDate, String)> = vec![];
    for doc in &docs {
        let Some(st) = statement_of(doc) else { continue };
        match doc.adapter_id.as_deref() {
            Some("fortuneo.life_annual_pdf") => {
                if let (Some(date), Some(supports)) = (st.balance_date, st.extra.get("supports").and_then(|s| s.as_array())) {
                    if !supports.is_empty() {
                        yearly.push((date, supports.clone()));
                    }
                }
            }
            Some("fortuneo.life_arbitrage_pdf") => {
                let list = |k: &str| st.extra.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if let Some(date) = day(st.extra.get("arbitrage_date")) {
                    letters.push(Letter { date, sold: list("sold"), bought: list("bought"), portfolio_after: list("portfolio_after") });
                }
            }
            Some("fortuneo.life_operations_csv") => {
                for a in st.extra.get("arbitrages").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                    if let Some(d) = day(a.get("date")) {
                        csv_arbitrages.push((d, a.get("amount").and_then(|x| x.as_str()).unwrap_or("").to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    letters.sort_by_key(|l| l.date);

    // the base: the latest portfolio the papers give in full
    let mut base: Option<(NaiveDate, String, Vec<Value>)> = yearly.iter().max_by_key(|(d, _)| *d).map(|(d, s)| (*d, "relevé annuel".to_string(), s.clone()));
    for l in letters.iter().filter(|l| !l.portfolio_after.is_empty()) {
        if base.as_ref().map(|b| l.date > b.0).unwrap_or(true) {
            base = Some((l.date, "lettre d'arbitrage".into(), l.portfolio_after.clone()));
        }
    }
    let Some((base_date, base_from, base_lines)) = base else { return Ok(None) };

    let mut positions: BTreeMap<String, Position> = BTreeMap::new();
    for line in &base_lines {
        if let Some(p) = position(line) {
            positions.insert(key_of(&p.isin, &p.name), p);
        }
    }
    let mut estimate = Estimate { base_date: Some(base_date), base_from, ..Default::default() };
    let tolerance = py::dec_parse(EPSILON).unwrap_or_default();

    // payments since the base go to the euro fund
    let mut payments = Decimal::ZERO;
    for t in Transaction::query(c, "WHERE account_id = ? AND date > ? AND amount > 0", rusqlite::params![account_id, base_date.format("%Y-%m-%d").to_string()])? {
        let is_payment = t.details.as_ref().and_then(|d| d.get("life_insurance")).and_then(|v| v.as_str()) == Some("versement");
        if is_payment {
            payments += t.amount;
        }
    }
    if payments > Decimal::ZERO {
        let euro = positions.iter().find(|(_, p)| p.euro).map(|(k, _)| k.clone());
        let key = euro.unwrap_or_else(|| "euro:fonds en euros".to_string());
        let entry = positions.entry(key).or_insert(Position { name: "Fonds en euros".into(), isin: None, units: Decimal::ZERO, unit_value: Decimal::ONE, as_of: None, euro: true });
        entry.units += payments;
    }
    estimate.payments = payments;
    let mut deficit = Decimal::ZERO; // sold by the letters, held by none of the lines the papers gave
    for l in letters.iter().filter(|l| l.date > base_date) {
        estimate.letters += 1;
        for line in &l.sold {
            let Some(p) = position(line) else { continue };
            let key = key_of(&p.isin, &p.name);
            let held = positions.get(&key).map(|h| h.units).unwrap_or(Decimal::ZERO);
            if p.units - held > tolerance {
                // the monthly payments are invested by the mandate straight into funds: what is sold beyond what the
                // last statement showed comes from the payments counted in the euro fund
                deficit += (p.units - held) * p.unit_value;
            }
            if let Some(h) = positions.get_mut(&key) {
                h.units = (h.units - p.units).max(Decimal::ZERO);
                h.unit_value = p.unit_value;
                h.as_of = p.as_of.or(h.as_of);
            }
        }
        for line in &l.bought {
            let Some(p) = position(line) else { continue };
            let key = key_of(&p.isin, &p.name);
            match positions.get_mut(&key) {
                Some(held) => {
                    held.units += p.units;
                    held.unit_value = p.unit_value;
                    held.as_of = p.as_of.or(held.as_of);
                }
                None => {
                    positions.insert(key, p);
                }
            }
        }
    }
    if deficit > tolerance {
        let key = positions.iter().find(|(_, p)| p.euro).map(|(k, _)| k.clone());
        match key.and_then(|k| positions.get_mut(&k)) {
            Some(euro) if euro.units >= deficit => {
                euro.units -= deficit;
                estimate.notes.push(format!("{} € vendus par les lettres venaient de versements investis par le mandat depuis le dernier relevé : pris sur les versements comptés dans le fonds en euros", py::dec_str(&deficit.round_dp(2))));
            }
            Some(euro) => {
                euro.units = Decimal::ZERO;
                estimate.warnings.push("Les lettres vendent plus que le contrat ne détenait d'après les papiers déposés : une lettre ou un relevé manque sans doute".into());
            }
            None => estimate.warnings.push("Les lettres vendent des parts que le contrat ne détenait pas d'après les papiers déposés : un relevé manque sans doute".into()),
        }
    }
    // an arbitrage of the operations export that no letter explains: the lines may be off
    for (date, amount) in csv_arbitrages.iter().filter(|(d, _)| *d > base_date) {
        if !letters.iter().any(|l| (l.date - *date).num_days().abs() <= 1) {
            estimate.warnings.push(format!("L'arbitrage du {} ({} €) n'a pas sa lettre : les lignes peuvent être inexactes, dépose-la", date.format("%d/%m/%Y"), amount));
        }
    }

    // write the lines: the ones this module made before are replaced, the user's own are left alone
    for old in Holding::query(c, "WHERE account_id = ?", [account_id])? {
        if old.extra.as_ref().and_then(|e| e.get("source")).and_then(|s| s.as_str()) == Some(SOURCE) {
            Holding::delete(c, old.id)?;
        }
    }
    for p in positions.values().filter(|p| p.units > tolerance) {
        Holding {
            account_id,
            isin: p.isin.clone(),
            name: Some(p.name.clone()),
            asset_class: if p.euro { "other".into() } else { "fund".into() },
            quantity: p.units,
            unit_price: Some(p.unit_value),
            // the euro fund has no market price: its units are euros
            value: if p.euro { Some(p.units) } else { None },
            value_currency: Some("EUR".into()),
            extra: Some(json!({"source": SOURCE, "guard": true, "euro_fund": p.euro, "last_price_date": p.as_of.map(|d| d.format("%Y-%m-%d").to_string())})),
            manual: false,
            ..Default::default()
        }
        .insert(c)?;
        estimate.lines += 1;
    }
    if estimate.lines > 0 && !account.balance.is_zero() {
        let before = account.clone();
        account.balance = Decimal::ZERO; // the value is in the lines
        account.save(c, &before)?;
    }
    setting_set(
        c,
        &format!("life_estimate_{account_id}"),
        &json!({
            "base_date": base_date.format("%Y-%m-%d").to_string(),
            "base_from": estimate.base_from,
            "letters": estimate.letters,
            "payments": py::dec_str(&estimate.payments),
            "lines": estimate.lines,
            "warnings": estimate.warnings,
            "notes": estimate.notes,
        })
        .to_string(),
    )?;
    Ok(Some(estimate))
}

/// What the account page says about how its lines were made (None when they were not rebuilt from papers).
pub fn summary(c: &Connection, account_id: i64) -> Option<Value> {
    setting_get(c, &format!("life_estimate_{account_id}")).ok().flatten().and_then(|s| serde_json::from_str(&s).ok())
}

/// Rebuild after something changed, without ever failing the operation that changed it.
pub fn refresh(c: &Connection, account_id: i64) {
    if let Err(e) = rebuild(c, account_id) {
        log::warn!("Lignes du contrat {account_id} : {e}");
    }
}
