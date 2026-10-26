//! The tax rates the application relies on (today: how interest, dividends, capital gains, life insurance... are taxed,
//! to turn a gross rate into a net one; later a tax calculator). They are prefilled with the figures of 2026, kept in
//! the settings, edited by the administrator, and the administrator is reminded once a month to check them: they change
//! with each finance law. Rows added in a later version appear by themselves, with their default.

use std::collections::BTreeMap;
use std::str::FromStr;

use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::error::AppResult;
use crate::models::{setting_get, setting_set};
use crate::services::public_rates;

pub const SETTING: &str = "tax_table";
pub const SOURCE_URL: &str = "https://www.legifiscal.fr/reperes-fiscaux/flat-tax-pfu-champ-application-taux-applicables.html";
pub const SOURCE_NAME: &str = "LégiFiscal : Flat-tax (PFU), champ d'application et taux applicables";
pub const DEFAULT_YEAR: i32 = 2026;
/// Reminded again when the last check is older than this, and the "remind me later" lasts this long.
pub const REVIEW_SECONDS: f64 = 30.0 * 24.0 * 3600.0;
pub const SNOOZE_SECONDS: f64 = 24.0 * 3600.0;

#[derive(Clone, Debug, PartialEq)]
pub struct TaxRate {
    pub key: &'static str,
    pub label: &'static str,
    /// income tax rate in %, None when the progressive scale applies instead
    pub ir: Option<Decimal>,
    /// social levies in %
    pub social: Decimal,
    pub note: &'static str,
}

impl TaxRate {
    /// The global rate in %, None when the income tax is not a flat one.
    pub fn total(&self) -> Option<Decimal> {
        self.ir.map(|ir| ir + self.social)
    }
}

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn row(key: &'static str, label: &'static str, ir: Option<&str>, social: &str, note: &'static str) -> TaxRate {
    TaxRate { key, label, ir: ir.map(d), social: d(social), note }
}

/// The table of 2026 (LégiFiscal, updated 26/08/2026: the social levies rose from 17,2 % to 18,6 % on 1 January 2026,
/// except for life insurance and real estate income).
pub fn defaults() -> Vec<TaxRate> {
    vec![
        row("interets", "Intérêts, livrets fiscalisés, comptes à terme, coupons d'obligations", Some("12.8"), "18.6", "Flat tax. C'est le taux qui passe un taux brut en net pour les livrets bancaires, comptes à terme, PEL, crowdlending."),
        row("dividendes", "Dividendes et distributions assimilées", Some("12.8"), "18.6", "Sans l'abattement de 40 %."),
        row("plus_values_mobilieres", "Plus-values sur valeurs mobilières (actions, obligations)", Some("12.8"), "18.6", "Sur le gain net, sans abattement pour durée de détention."),
        row("crypto", "Gains sur crypto-actifs", Some("12.8"), "18.6", "Régime de l'article 150 VH bis du CGI."),
        row("pea_5_ans", "PEA : retrait après 5 ans", Some("0"), "18.6", "Exonération d'impôt sur le revenu, prélèvements sociaux dus."),
        row("per_capital", "PER : sortie en capital (part des gains)", Some("12.8"), "18.6", ""),
        row("av_moins_8_ans", "Assurance-vie : rachat avant 8 ans (versements après le 27/09/2017)", Some("12.8"), "17.2", "Sert aussi à estimer le net d'un fonds en euros."),
        row("av_plus_8_ans", "Assurance-vie : rachat après 8 ans (primes versées jusqu'à 150 000 €)", Some("7.5"), "17.2", "Après abattement annuel de 4 600 € (célibataire) ou 9 200 € (couple)."),
        row("av_plus_8_ans_150k", "Assurance-vie : rachat après 8 ans (part des primes au-delà de 150 000 €)", Some("12.8"), "17.2", ""),
        row("revenus_fonciers", "Revenus fonciers (loyers nus, SCPI fiscales)", None, "17.2", "Hors flat tax : barème de l'impôt sur le revenu."),
        row("plus_values_immo", "Plus-values immobilières des particuliers", Some("19"), "17.2", "Abattements pour durée de détention distincts."),
    ]
}

#[derive(Clone, Debug)]
pub struct TaxTable {
    pub year: i32,
    pub rows: Vec<TaxRate>,
    /// when the administrator last confirmed the table is current (unix seconds), and until when the reminder sleeps
    pub reviewed_at: Option<f64>,
    pub snoozed_until: Option<f64>,
    /// keys whose figures differ from the defaults (typed by the administrator)
    pub edited: Vec<&'static str>,
}

impl Default for TaxTable {
    fn default() -> Self {
        TaxTable { year: DEFAULT_YEAR, rows: defaults(), reviewed_at: None, snoozed_until: None, edited: vec![] }
    }
}

impl TaxTable {
    pub fn get(&self, key: &str) -> Option<&TaxRate> {
        self.rows.iter().find(|r| r.key == key)
    }

    /// Due for a check: never confirmed, or not for a month, and not put off.
    pub fn due(&self, at: f64) -> bool {
        let stale = self.reviewed_at.map(|t| at - t >= REVIEW_SECONDS).unwrap_or(true);
        stale && self.snoozed_until.map(|t| at >= t).unwrap_or(true)
    }

    /// Days since the last check, None when it never happened.
    pub fn days_since_review(&self, at: f64) -> Option<i64> {
        self.reviewed_at.map(|t| ((at - t) / 86400.0).floor().max(0.0) as i64)
    }

    /// The rate that turns the interest of a product into its net, as (name, income tax %, social levies %).
    /// Regulated livrets pay nothing, the CEL only the social levies, the rest what its category says.
    pub fn for_product(&self, account_type: &str) -> (String, Decimal, Decimal) {
        let none = (String::new(), Decimal::ZERO, Decimal::ZERO);
        let pick = |key: &str, name: &str| self.get(key).map(|r| (name.to_string(), r.ir.unwrap_or(Decimal::ZERO), r.social)).unwrap_or_else(|| none.clone());
        match account_type {
            "livret_a" | "ldds" | "lep" | "livret_jeune" => none,
            "cel" => {
                let (_, _, ps) = pick("interets", "");
                ("prélèvements sociaux (le CEL ne paie pas d'impôt sur le revenu)".into(), Decimal::ZERO, ps)
            }
            "life_insurance" => pick("av_moins_8_ans", "assurance-vie, rachat avant 8 ans"),
            _ => pick("interets", "flat tax"),
        }
    }
}

fn dec_value(r: &Option<Decimal>) -> Value {
    r.map(|d| Value::String(d.normalize().to_string())).unwrap_or(Value::Null)
}

pub fn load(c: &Connection) -> AppResult<TaxTable> {
    let mut table = TaxTable::default();
    let Some(text) = setting_get(c, SETTING)? else { return Ok(table) };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Ok(table) };
    table.year = v.get("year").and_then(|y| y.as_i64()).map(|y| y as i32).unwrap_or(DEFAULT_YEAR);
    table.reviewed_at = v.get("reviewed_at").and_then(|x| x.as_f64());
    table.snoozed_until = v.get("snoozed_until").and_then(|x| x.as_f64());
    if let Some(rows) = v.get("rates").and_then(|r| r.as_object()) {
        let defaults = defaults();
        for row in table.rows.iter_mut() {
            if let Some(saved) = rows.get(row.key) {
                let ir = saved.get("ir").map(|x| x.as_str().and_then(|s| Decimal::from_str(s).ok()));
                let social = saved.get("social").and_then(|x| x.as_str()).and_then(|s| Decimal::from_str(s).ok());
                if let (Some(ir), Some(social)) = (ir, social) {
                    row.ir = ir;
                    row.social = social;
                }
            }
            if defaults.iter().find(|d| d.key == row.key).map(|d| d.ir != row.ir || d.social != row.social).unwrap_or(false) {
                table.edited.push(row.key);
            }
        }
    }
    Ok(table)
}

pub fn save(c: &Connection, table: &TaxTable) -> AppResult<()> {
    let rates: serde_json::Map<String, Value> = table.rows.iter().map(|r| (r.key.to_string(), json!({"ir": dec_value(&r.ir), "social": r.social.normalize().to_string()}))).collect();
    let value = json!({"year": table.year, "reviewed_at": table.reviewed_at, "snoozed_until": table.snoozed_until, "rates": rates});
    Ok(setting_set(c, SETTING, &value.to_string())?)
}

/// What the tax office's page says today, put next to the table: the reminder warns when they differ.
/// Returns sentences such as "impots.gouv.fr indique 18,6 % de prélèvements sociaux, le tableau 17,2 %".
pub fn differences(table: &TaxTable, online: Option<&public_rates::State>, at: f64) -> Vec<String> {
    let mut out = vec![];
    let Some(interets) = table.get("interets") else { return out };
    let fr = |d: Decimal| d.normalize().to_string().replace('.', ",");
    if let Some((ir, _)) = online.and_then(|s| public_rates::online_rate(s, "pfu_ir", at)) {
        if interets.ir != Some(ir) {
            out.push(format!("impots.gouv.fr indique {} % d'impôt (flat tax), le tableau {} %", fr(ir), interets.ir.map(fr).unwrap_or_else(|| "barème".into())));
        }
    }
    if let Some((ps, _)) = online.and_then(|s| public_rates::online_rate(s, "pfu_ps", at)) {
        if interets.social != ps {
            out.push(format!("impots.gouv.fr indique {} % de prélèvements sociaux, le tableau {} %", fr(ps), fr(interets.social)));
        }
    }
    out
}

/// The administrator's answer: a figure typed in the form, "12,8" or "12.8"; empty means the progressive scale for the
/// income tax column (where that makes sense).
pub fn parse_percent(text: &str) -> Option<Decimal> {
    let t = text.trim().replace(',', ".").replace('%', "").replace(' ', "");
    let v = Decimal::from_str(&t).ok()?;
    (v >= Decimal::ZERO && v <= Decimal::from(100)).then_some(v)
}

/// A table with the figures typed in a form (`ir_<key>`, `social_<key>`), blanks and mistakes keeping what was there.
pub fn from_form(current: &TaxTable, form: &BTreeMap<String, String>) -> TaxTable {
    let mut table = current.clone();
    let defaults = defaults();
    for row in table.rows.iter_mut() {
        if let Some(v) = form.get(&format!("ir_{}", row.key)) {
            if v.trim().is_empty() {
                if defaults.iter().find(|d| d.key == row.key).map(|d| d.ir.is_none()).unwrap_or(false) {
                    row.ir = None; // a row of the progressive scale stays so
                }
            } else if let Some(p) = parse_percent(v) {
                row.ir = Some(p);
            }
        }
        if let Some(p) = form.get(&format!("social_{}", row.key)).and_then(|v| parse_percent(v)) {
            row.social = p;
        }
    }
    table.edited = vec![];
    for row in &table.rows {
        if defaults.iter().find(|d| d.key == row.key).map(|d| d.ir != row.ir || d.social != row.social).unwrap_or(false) {
            table.edited.push(row.key);
        }
    }
    table
}
