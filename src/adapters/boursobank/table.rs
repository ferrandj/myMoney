//! Shared reading of BoursoBank (Boursorama) PDF statements: the "Mouvements en EUR" table.
//!
//! Every BoursoBank statement has the same table, whatever the product (current account, PEL, savings...):
//! operation date | label (first line, then detail lines) | value date | debit | credit, opened by the opening
//! balance ("SOLDE AU" / "SOLDE EN EUR AU") and closed by "Nouveau solde en EUR". Debit and credit are told apart
//! by the column the amount sits in, then double-checked against the balances.

use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use crate::adapters::base::{ParsedStatement, SourceDocument, TransactionData, W};
use crate::adapters::common::{name_key, norm, parse_amount, parse_date_s};
use crate::adapters::lines::{describe, is_own, isupper, join_wrapped, label as make_label};
use crate::adapters::pdftools;
use crate::adapters::re::{g, py_match, re, sub};
use crate::py;

pub const DATE_RE: &str = r"^\d{2}/\d{2}/\d{4}$";
pub const AMOUNT_RE: &str = r"^\d{1,3}(?:\.\d{3})*,\d{2}$|^\d+,\d{2}$";
pub const PERIOD_RE: &str = r"du\s+(\d{2}/\d{2}/\d{4})\s+au\s+(\d{2}/\d{2}/\d{4})";
const REF_RE: &str = r"^R[^\W\d_]f\s*:?\s*(\S.*)$";
const RUM_RE: &str = r"^RUM\s+(\S.*)$";
const CARD_RE: &str = r"^CARTE\s+(\d{2}/\d{2}/\d{2})\s+(.*?)(?:\s+CB\*(\d{4}))?\s*$";
const STREET_WORDS: &[&str] =
    &["rue", "avenue", "av", "bd", "boulevard", "chemin", "route", "impasse", "allee", "place", "quai", "residence", "lieu", "cours", "passage", "square"];
const DEFAULT_BOUNDARY: f64 = 486.0;
const HOLDER_CUT: usize = 28;

pub fn is_date(t: &str) -> bool {
    py_match(DATE_RE, t).is_some()
}

pub fn is_amount(t: &str) -> bool {
    py_match(AMOUNT_RE, t).is_some()
}

struct Entry {
    date: NaiveDate,
    value_date: Option<NaiveDate>,
    amount: Decimal,
    first: String,
    cont: Vec<String>,
}

/// What differs between the BoursoBank adapters: the header, and the identity checks.
pub trait Header {
    fn default_account_type(&self) -> &'static str;
    fn read_header(&self, st: &mut ParsedStatement, rows: &[Vec<W>]);
    fn check_identity(&self, _st: &mut ParsedStatement) {}
}

pub fn parse_table(h: &dyn Header, id: &str, institution: &str, kind: &str, doc: &SourceDocument) -> Result<Vec<ParsedStatement>, String> {
    let pages = doc.words(1.0)?;
    let mut st = ParsedStatement::new("pdf");
    st.institution = Some(institution.into());
    st.adapter_id = Some(id.into());
    st.document_kind = Some(kind.into());
    st.account_type = Some(h.default_account_type().into());
    let mut entries: Vec<Entry> = vec![];
    let mut current: Option<usize> = None;
    let (mut boundary, mut value_x) = (DEFAULT_BOUNDARY, 381.0);
    let mut closing_seen = false;

    for (page_no, words) in pages.iter().enumerate() {
        let rows = pdftools::lines(words, 3.0, false);
        if page_no == 0 {
            h.read_header(&mut st, &rows);
        }
        let mut in_table = false;
        for row in &rows {
            let texts: Vec<&str> = row.iter().map(|w| w.text.as_str()).collect();
            let compact = norm(&texts.concat()).replace(' ', "");
            if is_table_header(&texts) {
                in_table = true;
                (boundary, value_x) = columns(row, boundary, value_x);
                continue;
            }
            if !in_table || closing_seen {
                continue;
            }
            if compact == "operation" || compact.starts_with("areception") {
                in_table = compact == "operation";
                continue;
            }
            let amounts: Vec<&W> = row.iter().filter(|w| is_amount(&w.text) && w.x0 > value_x + 20.0).collect();
            if compact.contains("nouveausolde") {
                if let Some(a) = amounts.last() {
                    st.balance = Some(signed(a, boundary));
                }
                st.balance_date = st.period_end;
                closing_seen = true;
                in_table = false;
                continue;
            }
            if compact.starts_with("solde") {
                let day = row.iter().find(|w| is_date(&w.text)).and_then(|w| parse_date_s(&w.text));
                if let Some(a) = amounts.last() {
                    st.opening_balance = Some(signed(a, boundary));
                    st.opening_date = day;
                }
                continue;
            }
            let first = &row[0];
            if is_date(&first.text) && first.x0 < 70.0 {
                let when = parse_date_s(&first.text);
                let (Some(when), Some(a0)) = (when, amounts.first()) else { continue };
                let value_date = row[1..].iter().find(|w| is_date(&w.text) && w.x0 >= value_x - 12.0).and_then(|w| parse_date_s(&w.text));
                let label = py::strip(&row[1..].iter().filter(|w| w.x0 < value_x - 12.0).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
                entries.push(Entry { date: when, value_date, amount: signed(a0, boundary), first: label, cont: vec![] });
                current = Some(entries.len() - 1);
            } else if let Some(c) = current {
                let text = py::strip(&row.iter().filter(|w| w.x0 < value_x - 12.0).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
                if !text.is_empty() {
                    entries[c].cont.push(text);
                }
            }
        }
    }

    let holder_key = st.holder_name.as_deref().filter(|s| !s.is_empty()).map(name_key);
    st.transactions = entries.iter().map(|e| to_transaction(e, &st.currency, holder_key.as_deref())).collect();
    check(h, &mut st, entries.is_empty());
    Ok(vec![st])
}

/// Holder and address: the lines under the title block ("Extrait de votre ..."), above the account banner.
pub fn read_holder(st: &mut ParsedStatement, rows: &[Vec<W>], texts: &[String]) {
    let title = texts.iter().position(|t| norm(t).contains("extrait de votre"));
    let banner = texts.iter().position(|t| norm(t).contains("devise") && norm(t).contains("periode"));
    let (Some(title), Some(banner)) = (title, banner) else { return };
    let mut end = title;
    while end + 1 < banner && rows[end + 1][0].top - rows[end][0].top < 20.0 {
        end += 1;
    }
    let block: Vec<String> = (end + 1..banner.max(end + 1))
        .filter(|&i| rows[i][0].x0 > 250.0 && !py::strip(&texts[i]).is_empty())
        .map(|i| py::strip(&texts[i]).to_string())
        .collect();
    if block.is_empty() {
        return;
    }
    let default_start = if block.len() >= 2 { (block.len() - 2).max(1) } else { 1 };
    let start = block
        .iter()
        .enumerate()
        .position(|(i, t)| {
            i > 0 && (t.chars().next().map(py::is_digit_char).unwrap_or(false) || py::split_ws(&norm(t)).iter().any(|w| STREET_WORDS.contains(w)))
        })
        .unwrap_or(default_start);
    let name = block[..start.min(block.len())].join(" ");
    st.holder_name = if name.is_empty() { None } else { Some(name) };
    let addr = block[start.min(block.len())..].join("\n");
    st.holder_address = if addr.is_empty() { None } else { Some(addr) };
    // The bank cuts the holder line at about 29 characters: such a line may be missing the end of a name.
    if block[..start.min(block.len())].iter().any(|l| py::char_len(l) >= HOLDER_CUT) {
        st.set_extra("holder_maybe_truncated", true);
    }
}

fn check(h: &dyn Header, st: &mut ParsedStatement, no_entries: bool) {
    let total: Decimal = st.transactions.iter().map(|t| t.amount).sum();
    if let (Some(open), Some(bal)) = (st.opening_balance, st.balance) {
        let gap = bal - open - total;
        if gap.abs() < Decimal::from_str("0.01").unwrap() {
            st.confidence = "high".into();
        } else {
            st.confidence = "low".into();
            st.warnings.push(format!(
                "Les opérations lues ne tombent pas juste : écart de {} entre le solde initial, les opérations et le nouveau solde",
                fmt2(&gap)
            ));
        }
    } else {
        st.confidence = "low".into();
        st.warnings.push("Solde initial ou nouveau solde introuvable : impossible de contrôler les opérations lues".into());
    }
    if no_entries && st.confidence != "high" {
        st.warnings.push("Aucune opération lue dans ce relevé".into());
    }
    if let (Some(ps), Some(pe)) = (st.period_start, st.period_end) {
        let outside = st.transactions.iter().filter(|t| !(ps <= t.date && t.date <= pe)).count();
        if outside > 0 {
            st.confidence = "low".into();
            st.warnings.push(format!("{outside} opération(s) datée(s) hors de la période du relevé"));
        }
    }
    h.check_identity(st);
    if st.confidence == "high" && !st.warnings.is_empty() {
        st.confidence = "medium".into();
    }
}

/// `f"{decimal:.2f}"`: half-even rounding, two decimals, "-0.00" kept for a negative value rounding to zero.
pub fn fmt2(d: &Decimal) -> String {
    let r = d.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointNearestEven);
    let s = format!("{:.2}", r.abs());
    if d.is_sign_negative() && !d.is_zero() {
        format!("-{s}")
    } else {
        s
    }
}

fn is_table_header(texts: &[&str]) -> bool {
    let tokens: Vec<String> = texts.iter().map(|t| norm(t).trim_matches(|c| c == ' ' || c == ':').to_string()).collect();
    ["libelle", "valeur", "debit", "credit"].iter().all(|k| tokens.iter().any(|t| t == k))
}

fn columns(row: &[W], boundary: f64, value_x: f64) -> (f64, f64) {
    let mut by: std::collections::HashMap<String, &W> = std::collections::HashMap::new();
    for w in row {
        by.insert(norm(&w.text).trim_matches(|c| c == ' ' || c == ':').to_string(), w);
    }
    let mut boundary = boundary;
    let mut value_x = value_x;
    if let (Some(d), Some(c)) = (by.get("debit"), by.get("credit")) {
        // Amounts are right-aligned in their column: the middle of the gap between the two headings splits them.
        boundary = (d.x1 + c.x0) / 2.0;
    }
    if let Some(v) = by.get("valeur") {
        value_x = v.x0;
    }
    (boundary, value_x)
}

fn signed(word: &W, boundary: f64) -> Decimal {
    let value = py::or_zero(parse_amount(Some(&word.text)));
    if word.x1 > boundary {
        value.abs()
    } else {
        py::neg(value.abs())
    }
}

/// (other lines, "Réf" value, "RUM" value): the bank prints them on their own lines.
fn split_reference(lines: &[String]) -> (Vec<String>, Option<String>, Option<String>) {
    let (mut other, mut r, mut rum) = (vec![], None, None);
    for line in lines {
        if let Some(m) = py_match(REF_RE, line) {
            r = Some(py::strip(&m[1]).to_string());
        } else if let Some(m) = py_match(RUM_RE, line) {
            rum = Some(py::strip(&m[1]).to_string());
        } else {
            other.push(line.clone());
        }
    }
    (other, r, rum)
}

fn none_if_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn to_transaction(e: &Entry, currency: &str, holder_key: Option<&str>) -> TransactionData {
    let raw = std::iter::once(e.first.clone()).chain(e.cont.iter().cloned()).collect::<Vec<_>>().join(" | ");
    let mut details: Map<String, Value> = Map::new();
    let mut kind = Some("other".to_string());
    let mut counterparty: Option<String> = None;
    let mut reference: Option<String> = None;
    let mut memo: Option<String>;
    let label;
    let first = py::strip(&e.first).to_string();
    let upper = py::upper(&first);
    let (others, r, rum) = split_reference(&e.cont);
    let card = if upper.starts_with("CARTE") { py_match(CARD_RE, &first) } else { None };
    let vir = py_match(r"(?i)^VIR\s+(SEPA|INST)\b\s*(.*)$", &first);
    let pret = py_match(r"(?i)^ECH\s+PRET\s*:?\s*(\S+)", &first);

    if let Some(m) = card {
        kind = Some("card".into());
        counterparty = none_if_empty(py::strip(&m[2]).to_string());
        let bought = parse_date_s(&m[1]);
        details.insert("purchase_date".into(), bought.map(|b| Value::String(b.format("%Y-%m-%d").to_string())).unwrap_or(Value::Null));
        details.insert("card_last4".into(), g(&m, 3).map(|s| Value::String(s.into())).unwrap_or(Value::Null));
        memo = none_if_empty(others.join(" "));
        label = make_label(counterparty.as_deref(), memo.as_deref());
    } else if upper.starts_with("PRLV") {
        kind = Some("direct_debit".into());
        counterparty = none_if_empty(py::strip(&sub(r"(?i)^PRLV(\s+SEPA)?\s*", "", &first)).to_string());
        memo = none_if_empty(others.join(" "));
        reference = r.clone();
        details.insert("rum".into(), rum.clone().map(Value::String).unwrap_or(Value::Null));
        label = make_label(counterparty.as_deref(), describe(&others).as_deref());
    } else if let Some(m) = vir {
        kind = Some(if py::upper(&m[1]) == "INST" { "instant_transfer".into() } else { "transfer".into() });
        counterparty = none_if_empty(py::strip(&m[2]).to_string());
        memo = none_if_empty(others.join(" "));
        reference = r.clone();
        label = make_label(Some(counterparty.as_deref().unwrap_or("Virement")), describe(&others).as_deref());
    } else if upper.starts_with("VIR") {
        // Transfers between Boursorama accounts: the first line is the reason, the last line the other party.
        kind = Some("transfer".into());
        reference = r.clone();
        let reason = py::strip(&first.chars().skip(3).collect::<String>()).to_string();
        memo = None;
        if !others.is_empty() {
            counterparty = Some(others.last().unwrap().clone());
            let mut parts = vec![reason.clone()];
            parts.extend(others[..others.len() - 1].iter().cloned());
            memo = none_if_empty(join_wrapped(&parts));
        } else if !reason.is_empty() && (isupper(&reason) || holder_key.map(|h| !h.is_empty() && name_key(&reason) == h).unwrap_or(false)) {
            counterparty = Some(reason.clone());
        } else {
            memo = none_if_empty(reason.clone());
        }
        label = make_label(Some(memo.as_deref().unwrap_or("Virement")), counterparty.as_deref());
    } else if let Some(m) = pret {
        kind = Some("loan_payment".into());
        details.insert("loan_number".into(), Value::String(m[1].to_string()));
        memo = none_if_empty(others.join(" "));
        let n: String = m[1].chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        label = format!("Échéance de prêt …{n}");
        reference = Some(m[1].to_string());
    } else if re(r"(?i)^(INT[EÉ]R[EÊ]TS?|CAPITALISATION)\b").is_match(&first) {
        kind = Some("interest".into());
        memo = none_if_empty(others.join(" "));
        label = make_label(Some(&first), memo.as_deref());
    } else if re(r"(?i)^\*?COTIS").is_match(&first) {
        kind = Some("fee".into());
        memo = none_if_empty(others.join(" "));
        label = make_label(Some(first.trim_start_matches('*')), memo.as_deref());
    } else {
        memo = none_if_empty(others.join(" "));
        label = make_label(Some(&first), memo.as_deref());
        reference = r.clone();
    }

    if is_own(counterparty.as_deref(), holder_key) && matches!(kind.as_deref(), Some("transfer") | Some("instant_transfer")) {
        details.insert("internal".into(), Value::Bool(true));
    }
    let external_id = r.filter(|x| x.starts_with("SCT"));
    let details: Map<String, Value> = details.into_iter().filter(|(_, v)| !v.is_null()).collect();
    let mut t = TransactionData::new(e.date, if label.is_empty() { first.clone() } else { label }, e.amount);
    t.currency = currency.to_string();
    t.external_id = external_id;
    t.value_date = e.value_date;
    t.kind = kind;
    t.counterparty = counterparty;
    t.reference = reference;
    t.memo = memo;
    t.raw = Some(raw);
    t.details = if details.is_empty() { None } else { Some(details) };
    t
}
