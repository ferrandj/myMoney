//! What an adapter receives (a dropped file), what it returns (parsed statements) and the contract it follows.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use super::common::strip_accents;
use super::pdftools;
use crate::py;

#[derive(Clone, Debug, Default)]
pub struct HoldingData {
    pub symbol: Option<String>,
    pub quantity: Decimal,
    pub name: Option<String>,
    pub isin: Option<String>,
    pub asset_class: String,
    pub unit_price: Option<Decimal>,
    pub value: Option<Decimal>,
    pub value_currency: Option<String>,
    pub cost_basis: Option<Decimal>,
    pub extra: Option<Map<String, Value>>,
}

#[derive(Clone, Debug)]
pub struct TransactionData {
    pub date: NaiveDate,
    pub label: String,
    pub amount: Decimal,
    pub currency: String,
    /// Id given by the source when it has a really unique one (bank transfer reference, OFX FITID...).
    pub external_id: Option<String>,
    pub value_date: Option<NaiveDate>,
    /// card | transfer | instant_transfer | direct_debit | loan_payment | fee | cash | cheque | interest | other
    pub kind: Option<String>,
    /// Who the money went to / came from, as printed by the bank, and their IBAN when it shows.
    pub counterparty: Option<String>,
    pub counterparty_iban: Option<String>,
    pub reference: Option<String>,
    pub memo: Option<String>,
    /// The full original text of the line: fingerprints are computed from it so they survive label clean-ups.
    pub raw: Option<String>,
    pub details: Option<Map<String, Value>>,
}

impl TransactionData {
    pub fn new(date: NaiveDate, label: impl Into<String>, amount: Decimal) -> Self {
        TransactionData {
            date,
            label: label.into(),
            amount,
            currency: "EUR".into(),
            external_id: None,
            value_date: None,
            kind: None,
            counterparty: None,
            counterparty_iban: None,
            reference: None,
            memo: None,
            raw: None,
            details: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParsedStatement {
    pub source_format: String,
    pub institution: Option<String>,
    pub adapter_id: Option<String>,
    pub document_kind: Option<String>,
    pub account_label: Option<String>,
    pub account_type: Option<String>,
    pub currency: String,
    pub holder_name: Option<String>,
    pub holder_address: Option<String>,
    pub iban: Option<String>,
    pub bic: Option<String>,
    pub bank_code: Option<String>,
    pub branch_code: Option<String>,
    pub account_number: Option<String>,
    pub rib_key: Option<String>,
    pub balance: Option<Decimal>,
    pub balance_date: Option<NaiveDate>,
    pub opening_balance: Option<Decimal>,
    pub opening_date: Option<NaiveDate>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub transactions: Vec<TransactionData>,
    pub holdings: Option<Vec<HoldingData>>,
    /// Amounts found in documents that are not statements (e.g. "valeur de rachat" of a life insurance).
    pub candidate_balances: Vec<(String, Decimal)>,
    pub warnings: Vec<String>,
    /// high | medium | low
    pub confidence: String,
    pub extra: Map<String, Value>,
}

impl ParsedStatement {
    pub fn new(source_format: &str) -> Self {
        ParsedStatement {
            source_format: source_format.into(),
            institution: None,
            adapter_id: None,
            document_kind: None,
            account_label: None,
            account_type: None,
            currency: "EUR".into(),
            holder_name: None,
            holder_address: None,
            iban: None,
            bic: None,
            bank_code: None,
            branch_code: None,
            account_number: None,
            rib_key: None,
            balance: None,
            balance_date: None,
            opening_balance: None,
            opening_date: None,
            period_start: None,
            period_end: None,
            transactions: vec![],
            holdings: None,
            candidate_balances: vec![],
            warnings: vec![],
            confidence: "medium".into(),
            extra: Map::new(),
        }
    }

    pub fn set_extra(&mut self, key: &str, v: impl Into<Value>) {
        self.extra.insert(key.to_string(), v.into());
    }

    pub fn extra_str(&self, key: &str) -> Option<String> {
        self.extra.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    }

    pub fn extra_truthy(&self, key: &str) -> bool {
        self.extra.get(key).map(crate::models::truthy).unwrap_or(false)
    }

    /// The JSON stored with the document (same keys and order as the Python `to_json`).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("transactions".into(), Value::Array(self.transactions.iter().map(tx_json).collect()));
        m.insert("holdings".into(), match &self.holdings {
            None => Value::Null,
            Some(h) => Value::Array(h.iter().map(holding_json).collect()),
        });
        m.insert(
            "candidate_balances".into(),
            Value::Array(self.candidate_balances.iter().map(|(l, a)| Value::Array(vec![Value::String(l.clone()), Value::String(py::dec_str(a))])).collect()),
        );
        m.insert("source_format".into(), self.source_format.clone().into());
        m.insert("institution".into(), opt_s(&self.institution));
        m.insert("adapter_id".into(), opt_s(&self.adapter_id));
        m.insert("document_kind".into(), opt_s(&self.document_kind));
        m.insert("account_label".into(), opt_s(&self.account_label));
        m.insert("account_type".into(), opt_s(&self.account_type));
        m.insert("currency".into(), self.currency.clone().into());
        m.insert("holder_name".into(), opt_s(&self.holder_name));
        m.insert("holder_address".into(), opt_s(&self.holder_address));
        m.insert("iban".into(), opt_s(&self.iban));
        m.insert("bic".into(), opt_s(&self.bic));
        m.insert("bank_code".into(), opt_s(&self.bank_code));
        m.insert("branch_code".into(), opt_s(&self.branch_code));
        m.insert("account_number".into(), opt_s(&self.account_number));
        m.insert("rib_key".into(), opt_s(&self.rib_key));
        m.insert("balance".into(), opt_d(&self.balance));
        m.insert("balance_date".into(), opt_date(&self.balance_date));
        m.insert("opening_balance".into(), opt_d(&self.opening_balance));
        m.insert("opening_date".into(), opt_date(&self.opening_date));
        m.insert("period_start".into(), opt_date(&self.period_start));
        m.insert("period_end".into(), opt_date(&self.period_end));
        m.insert("warnings".into(), Value::Array(self.warnings.iter().map(|w| Value::String(w.clone())).collect()));
        m.insert("confidence".into(), self.confidence.clone().into());
        m.insert("extra".into(), Value::Object(self.extra.clone()));
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> ParsedStatement {
        let mut st = ParsedStatement::new(v.get("source_format").and_then(|x| x.as_str()).unwrap_or(""));
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
        st.institution = s("institution");
        st.adapter_id = s("adapter_id");
        st.document_kind = s("document_kind");
        st.account_label = s("account_label");
        st.account_type = s("account_type");
        if let Some(c) = s("currency") {
            st.currency = c;
        }
        st.holder_name = s("holder_name");
        st.holder_address = s("holder_address");
        st.iban = s("iban");
        st.bic = s("bic");
        st.bank_code = s("bank_code");
        st.branch_code = s("branch_code");
        st.account_number = s("account_number");
        st.rib_key = s("rib_key");
        st.balance = s("balance").and_then(|x| py::dec_parse(&x));
        st.balance_date = s("balance_date").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok());
        st.opening_balance = s("opening_balance").and_then(|x| py::dec_parse(&x));
        st.opening_date = s("opening_date").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok());
        st.period_start = s("period_start").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok());
        st.period_end = s("period_end").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok());
        if let Some(Value::Array(w)) = v.get("warnings") {
            st.warnings = w.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        }
        if let Some(c) = s("confidence") {
            st.confidence = c;
        }
        if let Some(Value::Object(e)) = v.get("extra") {
            st.extra = e.clone();
        }
        if let Some(Value::Array(ts)) = v.get("transactions") {
            st.transactions = ts.iter().map(tx_from_json).collect();
        }
        if let Some(Value::Array(hs)) = v.get("holdings") {
            st.holdings = Some(hs.iter().map(holding_from_json).collect());
        }
        if let Some(Value::Array(cb)) = v.get("candidate_balances") {
            st.candidate_balances = cb
                .iter()
                .filter_map(|p| {
                    let a = p.as_array()?;
                    Some((a.first()?.as_str()?.to_string(), py::dec_parse(a.get(1)?.as_str()?)?))
                })
                .collect();
        }
        st
    }
}

fn opt_s(v: &Option<String>) -> Value {
    v.clone().map(Value::String).unwrap_or(Value::Null)
}

fn opt_d(v: &Option<Decimal>) -> Value {
    v.map(|d| Value::String(py::dec_str(&d))).unwrap_or(Value::Null)
}

fn opt_date(v: &Option<NaiveDate>) -> Value {
    v.map(|d| Value::String(d.format("%Y-%m-%d").to_string())).unwrap_or(Value::Null)
}

fn tx_json(t: &TransactionData) -> Value {
    let mut m = Map::new();
    m.insert("date".into(), t.date.format("%Y-%m-%d").to_string().into());
    m.insert("label".into(), t.label.clone().into());
    m.insert("amount".into(), py::dec_str(&t.amount).into());
    m.insert("currency".into(), t.currency.clone().into());
    m.insert("external_id".into(), opt_s(&t.external_id));
    m.insert("value_date".into(), opt_date(&t.value_date));
    m.insert("kind".into(), opt_s(&t.kind));
    m.insert("counterparty".into(), opt_s(&t.counterparty));
    m.insert("counterparty_iban".into(), opt_s(&t.counterparty_iban));
    m.insert("reference".into(), opt_s(&t.reference));
    m.insert("memo".into(), opt_s(&t.memo));
    m.insert("raw".into(), opt_s(&t.raw));
    m.insert("details".into(), t.details.clone().map(Value::Object).unwrap_or(Value::Null));
    Value::Object(m)
}

fn tx_from_json(v: &Value) -> TransactionData {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let mut t = TransactionData::new(
        s("date").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok()).unwrap_or_default(),
        s("label").unwrap_or_default(),
        s("amount").and_then(|x| py::dec_parse(&x)).unwrap_or_default(),
    );
    if let Some(c) = s("currency") {
        t.currency = c;
    }
    t.external_id = s("external_id");
    t.value_date = s("value_date").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok());
    t.kind = s("kind");
    t.counterparty = s("counterparty");
    t.counterparty_iban = s("counterparty_iban");
    t.reference = s("reference");
    t.memo = s("memo");
    t.raw = s("raw");
    t.details = v.get("details").and_then(|d| d.as_object().cloned());
    t
}

fn holding_json(h: &HoldingData) -> Value {
    let mut m = Map::new();
    m.insert("symbol".into(), opt_s(&h.symbol));
    m.insert("quantity".into(), py::dec_str(&h.quantity).into());
    m.insert("name".into(), opt_s(&h.name));
    m.insert("isin".into(), opt_s(&h.isin));
    m.insert("asset_class".into(), h.asset_class.clone().into());
    m.insert("unit_price".into(), opt_d(&h.unit_price));
    m.insert("value".into(), opt_d(&h.value));
    m.insert("value_currency".into(), opt_s(&h.value_currency));
    m.insert("cost_basis".into(), opt_d(&h.cost_basis));
    m.insert("extra".into(), h.extra.clone().map(Value::Object).unwrap_or(Value::Null));
    Value::Object(m)
}

fn holding_from_json(v: &Value) -> HoldingData {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    HoldingData {
        symbol: s("symbol"),
        quantity: s("quantity").and_then(|x| py::dec_parse(&x)).unwrap_or_default(),
        name: s("name"),
        isin: s("isin"),
        asset_class: s("asset_class").unwrap_or_else(|| "other".into()),
        unit_price: s("unit_price").and_then(|x| py::dec_parse(&x)),
        value: s("value").and_then(|x| py::dec_parse(&x)),
        value_currency: s("value_currency"),
        cost_basis: s("cost_basis").and_then(|x| py::dec_parse(&x)),
        extra: v.get("extra").and_then(|d| d.as_object().cloned()),
    }
}

// ------------------------------------------------------------------------------------- source document

/// A word of a page, with its position (pdfplumber's coordinates), accents repaired.
#[derive(Clone, Debug)]
pub struct W {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
}

/// A dropped file. Text and layout are extracted once and shared by every adapter that looks at it.
pub struct SourceDocument {
    pub filename: String,
    pub data: Vec<u8>,
    format: OnceCell<String>,
    text: OnceCell<Result<String, String>>,
    compact: OnceCell<Result<String, String>>,
    words: RefCell<HashMap<u64, Result<Rc<Vec<Vec<W>>>, String>>>,
}

impl SourceDocument {
    pub fn new(filename: &str, data: Vec<u8>) -> Self {
        SourceDocument {
            filename: filename.to_string(),
            data,
            format: OnceCell::new(),
            text: OnceCell::new(),
            compact: OnceCell::new(),
            words: RefCell::new(HashMap::new()),
        }
    }

    pub fn format(&self) -> &str {
        self.format.get_or_init(|| {
            let name = self.filename.to_lowercase();
            let head_all = &self.data[..self.data.len().min(2048)];
            let start = head_all.iter().position(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)).unwrap_or(head_all.len());
            let head = &head_all[start..];
            let contains = |h: &[u8], n: &[u8]| h.windows(n.len()).any(|w| w == n);
            if head.starts_with(b"%PDF") {
                return "pdf".into();
            }
            let upper: Vec<u8> = head.to_ascii_uppercase();
            if name.ends_with(".ofx") || name.ends_with(".qfx") || contains(head, b"OFXHEADER") || contains(&upper, b"<OFX>") {
                return "ofx".into();
            }
            if name.ends_with(".qif") || head.starts_with(b"!Type") {
                return "qif".into();
            }
            if name.ends_with(".csv") || name.ends_with(".txt") || name.ends_with(".tsv") {
                return "csv".into();
            }
            "unknown".into()
        })
    }

    /// Plain text: the text layer of a PDF, or the decoded content of a text file.
    pub fn text(&self) -> Result<&str, String> {
        let r = self.text.get_or_init(|| {
            if self.format() == "pdf" {
                pdftools::extract_text(&self.data)
            } else {
                Ok(decode_text(&self.data))
            }
        });
        r.as_deref().map_err(|e| e.clone())
    }

    /// Lower-case, accent-free text without any whitespace: markers survive PDFs that lose their spaces.
    pub fn compact(&self) -> Result<&str, String> {
        let r = self.compact.get_or_init(|| {
            let t = self.text()?;
            Ok(py::split_ws(&py::lower(&strip_accents(t))).join(""))
        });
        r.as_deref().map_err(|e| e.clone())
    }

    /// Words of each PDF page with their position, accents repaired.
    pub fn words(&self, x_tolerance: f64) -> Result<Rc<Vec<Vec<W>>>, String> {
        let key = x_tolerance.to_bits();
        if let Some(r) = self.words.borrow().get(&key) {
            return r.clone();
        }
        let r = pdftools::extract_words(&self.data, x_tolerance).map(Rc::new);
        self.words.borrow_mut().insert(key, r.clone());
        r
    }
}

/// Python's cp1252 codec (strict): the five unassigned bytes make it fail.
pub fn cp1252_char(b: u8) -> Option<char> {
    const HIGH: [u16; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0, 0, 0x2018,
        0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
    if (0x80..0xa0).contains(&b) {
        let c = HIGH[(b - 0x80) as usize];
        if c == 0 {
            None
        } else {
            char::from_u32(c as u32)
        }
    } else {
        Some(b as char)
    }
}

pub fn decode_text(data: &[u8]) -> String {
    let body = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    if let Ok(s) = std::str::from_utf8(body) {
        return s.to_string();
    }
    let mut out = String::with_capacity(data.len());
    let mut ok = true;
    for &b in data {
        match cp1252_char(b) {
            Some(c) => out.push(c),
            None => {
                ok = false;
                break;
            }
        }
    }
    if ok {
        return out;
    }
    data.iter().map(|&b| b as char).collect()
}

// --------------------------------------------------------------------------------------------- contract

/// How sure an adapter is that a document is one it knows, and why (shown to the user when they must choose).
#[derive(Clone, Debug, Default)]
pub struct Detection {
    pub score: f64,
    pub reasons: Vec<String>,
}

pub type ParseResult = Result<Vec<ParsedStatement>, String>;

/// Reads one kind of document from one institution. `detect` must be cheap and never read anything personal
/// into the result: it only scores.
pub trait Adapter: Sync {
    fn id(&self) -> &'static str;
    fn institution(&self) -> Option<&'static str>;
    fn kind(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn description(&self) -> &'static str {
        ""
    }
    fn formats(&self) -> &'static [&'static str] {
        &[]
    }
    /// Generic fallbacks guess from the content: they are never trusted without the user's confirmation.
    fn generic(&self) -> bool {
        false
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String>;
    fn parse(&self, doc: &SourceDocument) -> ParseResult;
}

/// Python's `round(x, 2)` (correctly rounded, half to even on the exact binary value).
pub fn round2(x: f64) -> f64 {
    format!("{:.2}", x).parse().unwrap_or(x)
}

/// Sum of the points of the checks that pass, like the adapters' `for ok, points, why in (...)` loops.
pub fn score(checks: &[(bool, f64, &str)]) -> (f64, Vec<String>) {
    let mut s = 0.0;
    let mut reasons = vec![];
    for (ok, points, why) in checks {
        if *ok {
            s += points;
            reasons.push(why.to_string());
        }
    }
    (s, reasons)
}
