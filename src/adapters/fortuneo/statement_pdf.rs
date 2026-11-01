//! Fortuneo: monthly "Relevé de Compte" PDF (and "Relevé Livret").
//!
//! A title ("Relevé de Compte N° 9 - 2026", the product, "Arrêté au <date>"), the holder's address on the right, a
//! box with the holder(s), the account number (RIB parts), the IBAN and the BIC, then the table Date | Date de
//! valeur | Opération | Débit | Crédit between "ANCIEN SOLDE" and "NOUVEAU SOLDE", with "TOTAL DES OPÉRATIONS DU
//! RELEVÉ" just before. Dates of operations have no year: it comes from the period.

use std::str::FromStr;

use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use super::identity::{read_address_block, read_identity};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, TransactionData, W};
use crate::adapters::boursobank::statement_pdf::last4;
use crate::adapters::boursobank::table::fmt2;
use crate::adapters::common::{french_date, name_key, norm, parse_amount, parse_date_s, ymd};
use crate::adapters::lines::{describe, identity_warnings, is_own, label as make_label};
use crate::adapters::pdftools;
use crate::adapters::re::{fullmatch, g, gs, py_match, search, sub};
use crate::py;
use crate::services::rates::guess_account_type;

const DAY_MONTH_RE: &str = r"^(\d{2})/(\d{2})$";
const FULL_DATE_RE: &str = r"^\d{2}/\d{2}/\d{4}$";
const MARGIN_X: f64 = 40.0; // nothing of the statement starts left of this
pub const AMOUNT_RE: &str = r"^\d{1,3}(?:[ .]\d{3})*,\d{2}$|^\d+,\d{2}$";
const CARD_RE: &str = r"(?i)^CARTE\s+(\d{2}/\d{2})\s+(.*)$";
const ICS_RE: &str = r"^[A-Z]{2}\d{2}ZZZ[A-Z0-9]+$";
const FEE_RE: &str = r"(?i)^(COTISATION|FRAIS|INACTIVIT[EÉ]|COMMISSION|ABONNEMENT)\b";
const OWN_FORTUNEO_RE: &str = r"(?i)^virement\s+(avec|vers|depuis)\s+fortuneo";

struct Entry {
    day: i64,
    month: i64,
    value_date: Option<NaiveDate>,
    amount: Decimal,
    first: String,
    cont: Vec<String>,
}

pub struct FortuneoStatementPdf;

fn some(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

impl Adapter for FortuneoStatementPdf {
    fn id(&self) -> &'static str {
        "fortuneo.statement_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Fortuneo · relevé de compte ou de livret (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le PDF « Relevé de Compte » (compte courant) ou « Relevé Livret » de l'espace client Fortuneo."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = doc.compact()?;
        let (mut s, reasons) = score(&[
            (c.contains("fortuneo"), 0.25, "mentionne Fortuneo"),
            (c.contains("relevedecompte") || c.contains("relevelivret"), 0.25, "titre « Relevé de Compte » ou « Relevé Livret »"),
            (c.contains("anciensolde") && c.contains("nouveausolde"), 0.2, "« Ancien solde » et « Nouveau solde »"),
            (c.contains("totaldesoperationsdureleve"), 0.15, "« Total des opérations du relevé »"),
            (c.contains("datedevaleur"), 0.15, "colonne « Date de valeur »"),
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
        st.account_type = Some("checking".into());
        let mut entries: Vec<Entry> = vec![];
        let mut current: Option<usize> = None;
        let (mut boundary, mut amount_x, mut label_x) = (461.0f64, 380.0f64, 130.0f64);
        let mut declared: Map<String, Value> = Map::new();
        let mut declared_d: Vec<(String, Decimal)> = vec![];
        let mut closing_seen = false;

        let pages = doc.words(1.0)?;
        for (page_no, words) in pages.iter().enumerate() {
            let kept: Vec<W> = words.iter().filter(|w| w.x0 >= MARGIN_X).cloned().collect();
            let rows: Vec<Vec<W>> = pdftools::lines(&kept, 3.0, true).into_iter().filter(|r| !r.is_empty()).collect();
            let texts: Vec<String> = rows.iter().map(|r| pdftools::row_text(r)).collect();
            if page_no == 0 {
                read_header(&mut st, &rows, &texts);
            }
            let mut in_table = false;
            for (row, text) in rows.iter().zip(texts.iter()) {
                let tokens: Vec<&str> = row.iter().map(|w| w.text.as_str()).collect();
                let compact = norm(&tokens.concat()).replace(' ', "");
                let normed: Vec<String> = tokens.iter().map(|t| norm(t)).collect();
                if ["valeur", "operation", "debit", "credit"].iter().all(|k| normed.iter().any(|t| t == k)) {
                    let mut by: std::collections::HashMap<String, &W> = std::collections::HashMap::new();
                    for w in row {
                        by.insert(norm(&w.text), w);
                    }
                    // Amounts are right-aligned: the middle of the two headings' right edges splits debit and credit.
                    boundary = (by["debit"].x1 + by["credit"].x1) / 2.0;
                    amount_x = by["debit"].x0 - 40.0;
                    in_table = true;
                    continue;
                }
                if !in_table || closing_seen {
                    continue;
                }
                let amount_idx: Vec<usize> = (0..row.len()).filter(|&i| py_match(AMOUNT_RE, &row[i].text).is_some() && row[i].x0 > amount_x).collect();
                if compact.starts_with("anciensolde") {
                    if let Some(&i) = amount_idx.last() {
                        let value = py::or_zero(parse_amount(Some(&row[i].text)));
                        st.opening_balance = Some(if compact.contains("debiteur") { py::neg(value.abs()) } else { value.abs() });
                        st.opening_date = french_date(Some(text));
                    }
                } else if compact.starts_with("totaldesoperations") {
                    for &i in &amount_idx {
                        let key = if row[i].x1 > boundary { "credit" } else { "debit" };
                        let v = py::or_zero(parse_amount(Some(&row[i].text)));
                        declared.insert(key.into(), Value::String(py::dec_str(&v)));
                        match declared_d.iter_mut().find(|(k, _)| k == key) {
                            Some(e) => e.1 = v,
                            None => declared_d.push((key.into(), v)),
                        }
                    }
                } else if compact.starts_with("nouveausolde") {
                    if let Some(&i) = amount_idx.last() {
                        let value = py::or_zero(parse_amount(Some(&row[i].text)));
                        st.balance = Some(if compact.contains("debiteur") { py::neg(value.abs()) } else { value.abs() });
                    }
                    st.balance_date = french_date(Some(text)).or(st.period_end);
                    closing_seen = true;
                    in_table = false;
                } else if tokens.len() >= 3 && py_match(DAY_MONTH_RE, tokens[0]).is_some() && py_match(FULL_DATE_RE, tokens[1]).is_some() && row[0].x0 < 70.0 {
                    if amount_idx.is_empty() {
                        continue;
                    }
                    let m = py_match(DAY_MONTH_RE, tokens[0]).unwrap();
                    let (day, month): (i64, i64) = (m[1].parse().unwrap(), m[2].parse().unwrap());
                    if row.len() > 2 {
                        label_x = row[2].x0;
                    }
                    let first = py::strip(&(2..row.len()).filter(|i| !amount_idx.contains(i)).map(|i| row[i].text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
                    let a0 = &row[amount_idx[0]];
                    let value_amount = py::or_zero(parse_amount(Some(&a0.text)));
                    let signed = if a0.x1 > boundary { value_amount.abs() } else { py::neg(value_amount.abs()) };
                    entries.push(Entry { day, month, value_date: parse_date_s(tokens[1]), amount: signed, first, cont: vec![] });
                    current = Some(entries.len() - 1);
                } else if let Some(c) = current {
                    if row[0].x0 >= label_x - 5.0 && amount_idx.is_empty() {
                        let extra = py::strip(&row.iter().filter(|w| w.x0 < amount_x).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
                        if !extra.is_empty() {
                            entries[c].cont.push(extra);
                        }
                    }
                }
            }
        }

        if let (Some(od), None) = (st.opening_date, st.period_start) {
            st.period_start = Some(od + Duration::days(1));
        }
        st.period_end = st.period_end.or(st.balance_date);
        let holder_key = st.holder_name.as_deref().filter(|s| !s.is_empty()).map(name_key);
        st.transactions = entries.iter().filter_map(|e| to_transaction(e, &st, holder_key.as_deref())).collect();
        if st.period_start.is_none() {
            if let Some(pe) = st.period_end {
                // The first statement of an account has no "ancien solde" date: it starts the month of its first operation.
                let first = st.transactions.iter().map(|t| t.date).min().unwrap_or(pe);
                st.period_start = first.with_day(1);
                st.set_extra("first_statement", true);
            }
        }
        check(&mut st, &declared, &declared_d);
        Ok(vec![st])
    }
}

fn read_header(st: &mut ParsedStatement, rows: &[Vec<W>], texts: &[String]) {
    read_identity(st, texts);
    let title = texts.iter().position(|t| {
        let n = norm(t);
        n.starts_with("releve de compte") || n.starts_with("releve livret")
    });
    if let Some(title) = title {
        for text in texts.iter().skip(title).take(4) {
            if let Some(m) = search(r"N°\s*(\d+)(?:\s*-\s*(\d{4}))?", text) {
                let v = match g(&m, 2) {
                    Some(y) => format!("{}/{}", &m[1], y),
                    None => m[1].to_string(),
                };
                st.set_extra("number", v);
            }
        }
        if norm(&texts[title]).starts_with("releve livret") {
            // A savings book: "Relevé Livret +", then "Compte 12345678 01" (client number and rank): no RIB, no IBAN.
            let product = py::strip(&sub(r"(?i)^relev[ée]\s+", "", &sub(r"\s*N°.*$", "", &texts[title]))).to_string();
            st.set_extra("product", product.clone());
            st.account_type = Some(guess_account_type(&product, "savings"));
            for text in texts.iter().skip(title + 1).take(3) {
                if let Some(m) = py_match(r"(?i)^compte\s+(\d{8})\s+(\d{2})\b", text) {
                    st.account_number = Some(format!("{}{}", &m[1], &m[2]));
                }
            }
        } else {
            // The product ("COMPTE COURANT") is the first line under the title that is neither the number nor the page.
            for text in texts.iter().skip(title + 1).take(3) {
                let cleaned = py::strip(&sub(r"(?i)\s*page\s*\d+\s*/\s*\d+|N°\s*\d+(\s*-\s*\d{4})?", "", text)).to_string();
                if !cleaned.is_empty() && !norm(&cleaned).starts_with("arrete") {
                    st.set_extra("product", py::capitalize(&cleaned));
                    st.account_type = Some(guess_account_type(&cleaned, "checking"));
                    break;
                }
            }
        }
    }
    for text in texts {
        if norm(text).starts_with("arrete au") {
            st.period_end = french_date(Some(text));
        }
    }
    let arrete = texts.iter().position(|t| norm(t).starts_with("arrete au"));
    let titulaire = texts.iter().position(|t| norm(t).contains("titulaire"));
    if let (Some(a), Some(t)) = (arrete, titulaire) {
        if t > a {
            let (name, address) = read_address_block(rows, a, t, 280.0);
            if st.holder_name.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
                st.holder_name = name;
            }
            st.holder_address = address;
        }
    }
    // What the bank billed for its services the month before (the box at the bottom).
    for text in texts {
        if norm(text).starts_with("total facture") {
            if let Some(m) = search(r"(\d[\d ]*,\d{2})", text) {
                st.set_extra("fees_billed", parse_amount(Some(&m[1])).map(|d| py::dec_str(&d)).unwrap_or_else(|| "None".into()));
            }
        }
    }
    let product = st.extra_str("product").unwrap_or_else(|| "Compte".into());
    if let Some(n) = &st.account_number {
        st.account_label = Some(format!("{product} Fortuneo …{}", last4(n)));
    }
}

pub fn check(st: &mut ParsedStatement, declared: &Map<String, Value>, declared_d: &[(String, Decimal)]) {
    let total: Decimal = st.transactions.iter().map(|t| t.amount).sum();
    if let (Some(o), Some(b)) = (st.opening_balance, st.balance) {
        let gap = b - o - total;
        if gap.abs() < Decimal::from_str("0.01").unwrap() {
            st.confidence = "high".into();
        } else {
            st.confidence = "low".into();
            st.warnings.push(format!("Les opérations lues ne tombent pas juste : écart de {} entre l'ancien solde, les opérations et le nouveau solde", fmt2(&gap)));
        }
    } else {
        st.confidence = "low".into();
        st.warnings.push("Ancien solde ou nouveau solde introuvable : impossible de contrôler les opérations lues".into());
    }
    let debits: Decimal = st.transactions.iter().filter(|t| t.amount < Decimal::ZERO).map(|t| -t.amount).fold(Decimal::ZERO, |a, b| a + b);
    let credits: Decimal = st.transactions.iter().filter(|t| t.amount > Decimal::ZERO).map(|t| t.amount).fold(Decimal::ZERO, |a, b| a + b);
    if !declared_d.is_empty() {
        let get = |k: &str| declared_d.iter().find(|(x, _)| x == k).map(|(_, v)| *v);
        let ok = get("debit").unwrap_or(Decimal::ZERO) == debits && get("credit").unwrap_or(Decimal::ZERO) == credits;
        if !ok {
            st.confidence = "low".into();
            let show = |k: &str| get(k).map(|d| py::dec_str(&d)).unwrap_or_else(|| "0".into());
            st.warnings.push(format!(
                "Les totaux imprimés par la banque (débits {}, crédits {}) ne correspondent pas aux opérations lues ({}, {})",
                show("debit"),
                show("credit"),
                py::dec_str(&debits),
                py::dec_str(&credits)
            ));
        }
        st.set_extra("declared_totals", Value::Object(declared.clone()));
    }
    if let (Some(ps), Some(pe)) = (st.period_start, st.period_end) {
        let outside = st.transactions.iter().filter(|t| !(ps - Duration::days(7) <= t.date && t.date <= pe)).count();
        if outside > 0 {
            st.confidence = "low".into();
            st.warnings.push(format!("{outside} opération(s) datée(s) hors de la période du relevé"));
        }
    }
    let w = identity_warnings(st);
    st.warnings.extend(w);
    if st.confidence == "high" && !st.warnings.is_empty() {
        st.confidence = "medium".into();
    }
}

/// Operation dates have no year: take the one that puts the date inside (or just around) the statement's period.
pub fn resolve(day: i64, month: i64, st: &ParsedStatement) -> Option<NaiveDate> {
    let anchor = st.period_end.or(st.balance_date)?;
    let ay = anchor.year() as i64;
    match st.period_start {
        None => {
            for year in [ay, ay - 1] {
                if let Some(c) = ymd(year, month, day) {
                    if c <= anchor + Duration::days(7) {
                        return Some(c);
                    }
                }
            }
            None
        }
        Some(start) => {
            for year in [ay, ay - 1, ay + 1] {
                if let Some(c) = ymd(year, month, day) {
                    if start - Duration::days(7) <= c && c <= anchor + Duration::days(7) {
                        return Some(c);
                    }
                }
            }
            None
        }
    }
}

fn to_transaction(e: &Entry, st: &ParsedStatement, holder_key: Option<&str>) -> Option<TransactionData> {
    let when = resolve(e.day, e.month, st)?;
    let raw = std::iter::once(e.first.clone()).chain(e.cont.iter().cloned()).collect::<Vec<_>>().join(" | ");
    let mut details: Map<String, Value> = Map::new();
    let mut kind = "other".to_string();
    let mut counterparty: Option<String> = None;
    let mut reference: Option<String> = None;
    let mut memo = some(e.cont.join(" "));
    let label;
    let first = py::strip(&e.first).to_string();
    let ids: Vec<String> =
        e.cont.iter().filter(|l| !l.contains(' ') && py::char_len(l) >= 10 && fullmatch(r"[A-Za-z0-9]+", l).is_some()).cloned().collect();
    let words: Vec<String> = e.cont.iter().filter(|l| !ids.contains(l)).cloned().collect();

    if let Some(m) = py_match(CARD_RE, &first) {
        kind = "card".into();
        counterparty = some(py::strip(&m[2]).to_string());
        let bought = if st.period_end.is_some() {
            let p: Vec<i64> = m[1].split('/').map(|x| x.parse().unwrap()).collect();
            resolve(p[0], p[1], st)
        } else {
            None
        };
        details.insert("purchase_date".into(), bought.map(|b| Value::String(b.format("%Y-%m-%d").to_string())).unwrap_or(Value::Null));
        memo = some(e.cont.join(" "));
        label = make_label(counterparty.as_deref(), memo.as_deref());
    } else if let Some(m) = py_match(r"(?i)^PRLV\s+(?:SEPA\s+)?(.*)$", &first) {
        kind = "direct_debit".into();
        counterparty = some(py::strip(&m[1]).to_string());
        let ics = ids.iter().find(|i| py_match(ICS_RE, i).is_some()).cloned();
        let others: Vec<String> = ids.iter().filter(|i| Some(*i) != ics.as_ref()).cloned().collect();
        details.insert("ics".into(), ics.clone().map(Value::String).unwrap_or(Value::Null));
        details.insert("ids".into(), Value::Array(others.iter().map(|x| Value::String(x.clone())).collect()));
        reference = others.first().cloned();
        memo = some(words.join(" "));
        label = make_label(counterparty.as_deref(), describe(&words).as_deref());
    } else if let Some(m) = py_match(r"(?i)^VIR\s+(?:(SEPA|INST)\s+)?(.*)$", &first) {
        kind = if py::upper(gs(&m, 1)) == "INST" { "instant_transfer".into() } else { "transfer".into() };
        let rest = py::strip(&m[2]).to_string();
        if py_match(OWN_FORTUNEO_RE, &rest).is_some() {
            details.insert("internal".into(), Value::Bool(true));
            memo = Some(py::strip(&std::iter::once(rest.clone()).chain(e.cont.iter().cloned()).collect::<Vec<_>>().join(" ")).to_string());
            label = make_label(Some(&rest), describe(&e.cont).as_deref());
        } else {
            counterparty = some(rest.clone());
            reference = ids.first().cloned();
            memo = some(words.join(" "));
            label = make_label(Some(counterparty.as_deref().unwrap_or("Virement")), describe(&words).as_deref());
        }
    } else if let Some(m) = py_match(r"(?i)^ECH\s+PRET\s+(\S+)", &first) {
        kind = "loan_payment".into();
        reference = Some(m[1].to_string());
        details.insert("loan_number".into(), Value::String(m[1].to_string()));
        label = format!("Échéance de prêt …{}", last4(&m[1]));
    } else if let Some(m) = py_match(r"(?i)^CR[EÉ]DIT\s+LOGT\s+(\S+)", &first) {
        kind = "loan_payment".into();
        reference = Some(m[1].to_string());
        details.insert("loan_number".into(), Value::String(m[1].to_string()));
        label = format!("Prêt logement …{}", last4(&m[1]));
    } else if py_match(FEE_RE, &first).is_some() {
        kind = "fee".into();
        label = make_label(Some(&first), memo.as_deref());
    } else if py_match(r"(?i)^INT[EÉ]R[EÊ]TS?\b", &first).is_some() {
        kind = "interest".into();
        label = make_label(Some(&first), memo.as_deref());
    } else if let Some(m) = py_match(r"(?i)^(?:REM(?:ISE)?\.?\s+)?CH(?:Q|[EÈ]QUE)\b\s*(.*)$", &first) {
        kind = "cheque".into();
        reference = py::split_ws(&m[1]).first().map(|s| s.to_string());
        let what = if py::upper(&first).starts_with("REM") { "Remise de chèque" } else { "Chèque" };
        let n = reference.as_ref().map(|r| format!("n° {r}"));
        label = make_label(Some(what), n.as_deref());
    } else {
        label = make_label(Some(&first), memo.as_deref());
    }
    if (kind == "transfer" || kind == "instant_transfer") && is_own(counterparty.as_deref(), holder_key) {
        details.insert("internal".into(), Value::Bool(true));
    }
    let details: Map<String, Value> = details.into_iter().filter(|(_, v)| !v.is_null() && v != &Value::Array(vec![])).collect();
    let mut t = TransactionData::new(when, if label.is_empty() { first.clone() } else { label }, e.amount);
    t.currency = st.currency.clone();
    t.value_date = e.value_date;
    t.kind = Some(kind);
    t.counterparty = counterparty;
    t.reference = reference;
    t.memo = memo;
    t.raw = Some(raw);
    t.details = if details.is_empty() { None } else { Some(details) };
    Some(t)
}
