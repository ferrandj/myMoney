//! Fortuneo Vie (contract managed by Suravenir): the "Relevé d'information annuel" of a life-insurance contract.
//!
//! It is a statement without operations: it gives the value of the contract on 31 December (the balance), the one of
//! the year before (the opening balance), the funds the contract holds (with their units, unit value and amount) and
//! what happened during the year (payments, arbitrages, distributions, revalorisation of the euro funds, social
//! contributions). The balance goes to the account, from which the evolution of the contract is drawn; the funds and the
//! movements are kept with the document and shown. The funds are NOT imported as positions: the contract value already
//! is their sum, and counting both would count the contract twice.
//!
//! The PDF loses its spaces, so everything is read from the words and their position on the page (the columns of the
//! tables are told apart by where they end).

use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use super::fees_pdf::amount_str;
use super::identity::read_address_block;
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, W};
use crate::adapters::boursobank::statement_pdf::last4;
use crate::adapters::common::{french_date, norm, parse_amount};
use crate::adapters::pdftools;
use crate::adapters::re::{search, sub};
use crate::py;

const MONTH: &str = r"(janvier|fevrier|mars|avril|mai|juin|juillet|aout|septembre|octobre|novembre|decembre)";
const AMOUNT: &str = r"\d[\d ]*,\d{2}";
const ISIN: &str = r"^[A-Z]{2}[A-Z0-9]{9}\d$";
/// Where the columns of the fund table end (points from the left of the page): name, ISIN, units, unit value, amount.
const NAME_END: f64 = 262.0;

pub struct FortuneoLifeAnnualPdf;

fn num_re() -> Regex {
    Regex::new(r"^\d[\d ]*,\d+$").unwrap()
}

fn money(text: &str) -> String {
    amount_str(text)
}

fn sum(values: &[Decimal]) -> Decimal {
    values.iter().fold(Decimal::ZERO, |a, b| a + b)
}

fn title_case(text: &str) -> String {
    py::split_ws(&py::lower(text)).iter().map(|w| py::capitalize(w)).collect::<Vec<_>>().join(" ")
}

fn date_of(day: &str, month: &str, year: &str) -> Option<NaiveDate> {
    french_date(Some(&format!("{day} {month} {year}")))
}

/// One row of the table of funds: the name, then (units, unit value,) amount. The euro fund has the amount only.
fn read_support(row: &[W], numbers: &Regex) -> Option<Map<String, Value>> {
    let words: Vec<&W> = row.iter().filter(|w| w.x0 >= 30.0).collect(); // the margin carries a rotated reference
    let isin_word = words.iter().find(|w| crate::adapters::re::py_match(ISIN, &w.text).is_some());
    let name_end = isin_word.map(|w| w.x0).unwrap_or(NAME_END).min(NAME_END);
    let name: Vec<&str> = words.iter().filter(|w| w.x1 <= name_end + 0.5 && !numbers.is_match(&w.text)).map(|w| w.text.as_str()).collect();
    let isin = isin_word.map(|w| w.text.clone());
    let nums: Vec<&str> = words.iter().filter(|w| w.x0 >= NAME_END && numbers.is_match(&w.text)).map(|w| w.text.as_str()).collect();
    if name.is_empty() || nums.is_empty() {
        return None;
    }
    let mut fund = name.join(" ");
    let guaranteed = fund.ends_with("(1)");
    if guaranteed {
        fund = py::strip(fund.trim_end_matches("(1)")).to_string();
    }
    let mut e = Map::new();
    e.insert("fund".into(), fund.into());
    e.insert("isin".into(), isin.map(Value::String).unwrap_or(Value::Null));
    if guaranteed {
        e.insert("capital_guaranteed".into(), true.into());
    }
    match nums.as_slice() {
        [amount] => {
            e.insert("amount".into(), money(amount).into());
        }
        [units, price, amount] => {
            e.insert("units".into(), money(units).into());
            e.insert("unit_value".into(), money(price).into());
            e.insert("amount".into(), money(amount).into());
        }
        _ => return None,
    }
    Some(e)
}

/// The summary of the movements of the year: a label, then the amounts in the debit, credit or fees column.
fn read_movements(rows: &[Vec<W>], numbers: &Regex) -> Vec<Map<String, Value>> {
    let texts: Vec<String> = rows.iter().map(|r| norm(&pdftools::row_text(r))).collect();
    let Some(start) = texts.iter().position(|t| t.starts_with("synthese des mouvements")) else { return vec![] };
    let Some(header) = rows.iter().skip(start).take(4).find(|r| r.iter().any(|w| norm(&w.text).starts_with("debit"))) else { return vec![] };
    let x_of = |prefix: &str| header.iter().find(|w| norm(&w.text).starts_with(prefix)).map(|w| w.x0);
    let (Some(credit_x), Some(fees_x)) = (x_of("credit"), x_of("frais")) else { return vec![] };

    let mut movements: Vec<Map<String, Value>> = vec![];
    let mut pending: Vec<String> = vec![]; // label rows seen before the row that carries the amounts
    let mut attached = false; // the last movement took its amounts from a row of its own: more label rows continue it
    for (row, text) in rows.iter().zip(&texts).skip(start + 1) {
        if text.starts_with("(4)") || text.starts_with("ce document") {
            break;
        }
        let words: Vec<&W> = row.iter().filter(|w| w.x0 >= 30.0).collect();
        if words.iter().any(|w| norm(&w.text).starts_with("debit")) {
            continue;
        }
        let label = words.iter().filter(|w| w.x0 < NAME_END && !numbers.is_match(&w.text)).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        let amounts: Vec<&&W> = words.iter().filter(|w| w.x0 >= NAME_END && numbers.is_match(&w.text)).collect();
        if amounts.is_empty() {
            if label.is_empty() {
                continue;
            }
            match (attached, movements.last_mut()) {
                (true, Some(last)) => {
                    let joined = format!("{} {}", last["label"].as_str().unwrap_or(""), label);
                    last.insert("label".into(), joined.into());
                }
                _ => {
                    pending.push(label);
                    attached = false;
                }
            }
            continue;
        }
        let mut e = Map::new();
        let full = if label.is_empty() { pending.join(" ") } else { label.clone() };
        attached = label.is_empty();
        pending.clear();
        e.insert("label".into(), full.into());
        for w in amounts {
            let column = if w.x1 < credit_x { "debit" } else if w.x1 < fees_x { "credit" } else { "fees" };
            e.insert(column.into(), money(&w.text).into());
        }
        movements.push(e);
    }
    movements
}

impl Adapter for FortuneoLifeAnnualPdf {
    fn id(&self) -> &'static str {
        "fortuneo.life_annual_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Fortuneo Vie · relevé d'information annuel (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le relevé annuel de ton contrat d'assurance-vie (Suravenir) : valeur au 31 décembre, supports détenus et mouvements de l'année."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = sub(r"[^a-z0-9]", "", doc.compact()?);
        let (mut s, reasons) = score(&[
            (c.contains("fortuneo"), 0.2, "mentionne Fortuneo"),
            (c.contains("releveinformationannuelle") || c.contains("relevedinformationannuelle"), 0.4, "« Relevé d'information annuelle »"),
            (c.contains("lasituationdevotrecontrat"), 0.15, "« La situation de votre contrat »"),
            (c.contains("synthesedesmouvements"), 0.15, "« Synthèse des mouvements »"),
            (c.contains("suravenir"), 0.1, "mentionne Suravenir"),
        ]);
        if !c.contains("fortuneo") {
            s = s.min(0.3);
        }
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut st = ParsedStatement::new("pdf");
        st.institution = Some("Fortuneo".into());
        st.adapter_id = Some(self.id().into());
        st.document_kind = Some(self.kind().into());
        st.account_type = Some("life_insurance".into());
        st.confidence = "high".into();
        let numbers = num_re();

        let mut rows: Vec<Vec<W>> = vec![];
        for words in doc.words(1.0)?.iter() {
            rows.extend(pdftools::lines(words, 3.0, true));
        }
        let texts: Vec<String> = rows.iter().map(|r| py::strip(&pdftools::row_text(&r.iter().filter(|w| w.x0 >= 30.0).cloned().collect::<Vec<_>>())).to_string()).collect();
        let norms: Vec<String> = texts.iter().map(|t| norm(t)).collect();

        // contract number and name, from the reference line ("N° Contrat : 000 ... 01") and "Contrat FORTUNEO VIE N° :"
        let contract_row = norms.iter().position(|t| t.starts_with("n° contrat"));
        if let Some(t) = contract_row.map(|i| &texts[i]) {
            let digits: String = t.split_once(':').map(|(_, n)| n).unwrap_or("").chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 8 {
                st.account_number = Some(digits.clone());
                st.set_extra("contract_number", digits);
            }
        }
        let product = texts.iter().find_map(|t| search(r"(?i)^N/R[ée]f\s*:\s*Contrat\s+(.+?)\s+N°", t).map(|m| py::strip(&m[1]).to_string()));
        if let Some(p) = &product {
            st.set_extra("contract", title_case(p));
        }
        let name = product.as_deref().map(title_case).unwrap_or_else(|| "Fortuneo Vie".into());
        st.account_label = Some(match &st.account_number {
            Some(n) => format!("{name} …{}", last4(n)),
            None => name.clone(),
        });

        // the day of the letter: "Brest, le 19 février 2026"
        if let Some(d) = texts.iter().find_map(|t| search(r"^[^\d]+,\s*le\s+(\d{1,2}(?:er)?\s+[^\W\d_]+\s+\d{4})\s*$", t).and_then(|m| french_date(Some(&m[1])))) {
            st.set_extra("issued_on", d.format("%Y-%m-%d").to_string());
        }

        // the holder, printed in the block on the right of the first page
        if let (Some(after), Some(before)) = (contract_row, norms.iter().position(|t| t.starts_with("n/ref"))) {
            if before > after {
                let (holder, address) = read_address_block(&rows.iter().map(|r| r.iter().filter(|w| w.x0 >= 30.0).cloned().collect()).collect::<Vec<Vec<W>>>(), after, before, 300.0);
                st.holder_name = holder;
                st.holder_address = address;
            }
        }

        // the value of the contract on 31 December, and the one of the year before
        let pattern = format!(r"valeur\s*\(\*\)\s*de votre contrat au (\d{{1,2}}) {MONTH} (\d{{4}})\s*:\s*({AMOUNT})");
        let value = norms.iter().find_map(|t| search(&pattern, t));
        if let Some(m) = &value {
            if let (Some(day), Some(v)) = (date_of(&m[1], &m[2], &m[3]), parse_amount(Some(&m[4]))) {
                st.balance = Some(v);
                st.balance_date = Some(day);
                st.period_end = Some(day);
                st.period_start = NaiveDate::from_ymd_opt(day.year(), 1, 1);
                st.set_extra("year", day.year());
            }
        }
        let pattern = format!(r"pour rappel, la valeur de votre contrat au (\d{{1,2}}) {MONTH} (\d{{4}}) etait de ({AMOUNT})");
        if let Some(m) = norms.iter().find_map(|t| search(&pattern, t)) {
            if let (Some(day), Some(v)) = (date_of(&m[1], &m[2], &m[3]), parse_amount(Some(&m[4]))) {
                st.opening_balance = Some(v);
                st.opening_date = Some(day);
            }
        }

        // funds held: from "La situation de votre contrat" (and its "suite") to the footnote
        let mut supports: Vec<Map<String, Value>> = vec![];
        let mut inside = false;
        for (row, norm_text) in rows.iter().zip(&norms) {
            if norm_text.starts_with("la situation de votre contrat") {
                inside = true;
                continue;
            }
            if inside && (norm_text.starts_with("(1)") || norm_text.starts_with("pour rappel") || norm_text.starts_with("synthese")) {
                inside = false;
            }
            if inside {
                if let Some(s) = read_support(row, &numbers) {
                    supports.push(s);
                }
            }
        }
        let amount_of = |m: &Map<String, Value>| m.get("amount").and_then(|a| a.as_str()).and_then(py::dec_parse).unwrap_or(Decimal::ZERO);
        let supports_total = sum(&supports.iter().map(amount_of).collect::<Vec<_>>());
        st.set_extra("supports", Value::Array(supports.iter().cloned().map(Value::Object).collect()));

        let movements = read_movements(&rows, &numbers);
        st.set_extra("movements", Value::Array(movements.iter().cloned().map(Value::Object).collect()));
        for m in &movements {
            let label = norm(m["label"].as_str().unwrap_or(""));
            if label.starts_with("versement") {
                if let Some(c) = m.get("credit") {
                    st.set_extra("payments", c.clone());
                }
            }
        }

        // rates of the euro funds: "2,50% pour le FDS EUROS ... (dont 0,40% de bonification sur encours)."
        let rate = Regex::new(r"^\S{0,2}\s*(\d+,\d+)\s*%\s+pour\s+(?:le|la|les)\s+(.+?)\s*(?:\(dont\s+(\d+,\d+)\s*%[^)]*\))?\s*\.?(?:\s+Votre montant.*)?$").unwrap();
        let mut rates: Vec<Value> = vec![];
        let value_row = norms.iter().position(|t| t.starts_with("valeur (*)")).unwrap_or(texts.len()); // later "pour les contrats ..." lines are market averages
        for t in texts.iter().take(value_row) {
            if let Some(m) = rate.captures(t) {
                let mut e = Map::new();
                e.insert("fund".into(), py::strip(&m[2]).to_string().into());
                e.insert("rate".into(), money(&m[1]).into());
                if let Some(b) = m.get(3) {
                    e.insert("bonus".into(), money(b.as_str()).into());
                }
                rates.push(Value::Object(e));
            }
        }
        if !rates.is_empty() {
            st.set_extra("euro_rates", Value::Array(rates));
        }

        // total fees of the year and premiums not reimbursed (needed for the tax return after a withdrawal)
        let fees = format!(r"montant total des frais imputes sur votre contrat pour l.annee \d{{4}} est de ({AMOUNT})");
        if let Some(m) = norms.iter().find_map(|t| search(&fees, t)) {
            st.set_extra("fees_total", money(&m[1]));
        }
        let after_2017 = format!(r"primes versees a compter du \d{{2}}/\d{{2}}/\d{{4}} et non remboursees\s+({AMOUNT})\s*$");
        if let Some(m) = norms.iter().find_map(|t| search(&after_2017, t)) {
            st.set_extra("premiums_unrefunded", money(&m[1]));
        }

        let title = match st.period_end {
            Some(d) => format!("Relevé d'information annuel {} · {name}", d.year()),
            None => format!("Relevé d'information annuel · {name}"),
        };
        st.set_extra("title", title);
        st.set_extra("category", "Relevé d'information annuel");
        st.set_extra("balance_only", true);

        let mut problems: Vec<String> = vec![];
        let tolerance = Decimal::from_str("0.05").unwrap();
        if st.balance.is_none() {
            problems.push("La valeur du contrat au 31 décembre n'a pas été trouvée".into());
        }
        if st.account_number.is_none() {
            problems.push("Le numéro du contrat n'a pas été trouvé".into());
        }
        if supports.is_empty() {
            problems.push("Les supports détenus n'ont pas pu être lus".into());
        } else if let Some(b) = st.balance {
            if (supports_total - b).abs() > tolerance {
                problems.push(format!("Les supports ({}) ne totalisent pas la valeur du contrat ({})", py::dec_str(&supports_total), py::dec_str(&b)));
            }
        }
        let has_problems = !problems.is_empty();
        st.warnings.extend(problems);
        if has_problems {
            st.confidence = if st.balance.is_none() || st.account_number.is_none() { "low".into() } else { "medium".into() };
        }
        Ok(vec![st])
    }
}
