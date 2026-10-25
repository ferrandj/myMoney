//! OFX / QFX (exported by most French banks: Fortuneo, BforBank, Boursorama...) and QIF files.
//!
//! The OFX side is `ofxparse` 0.21 (`OfxParser.parse(fh)`, which fails on the first error) over BeautifulSoup's
//! "html.parser" tree, reproduced step by step so that the same files give the same statements, and the same errors
//! with the same messages.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;

use super::htmlsoup::{self, Node, Soup, ROOT};
use super::pycodecs::{self, Codec};
use crate::adapters::base::{ParsedStatement, TransactionData};
use crate::adapters::common::{find_institution, int, parse_amount, parse_date, py_decimal, unicode_digit};
use crate::adapters::re::re;
use crate::py;
use crate::services::rates::guess_account_type;

/// A Python exception: its type matters where ofxparse catches some and not others; `str(exc)` is what the user sees.
#[derive(Debug, Clone)]
pub enum PyErr {
    Index,
    Type(String),
    Value(String),
    InvalidOperation,
    Attribute(String),
    Ofx(String),
    Other(String),
}

impl PyErr {
    pub fn message(&self) -> String {
        match self {
            PyErr::Index => "list index out of range".into(),
            PyErr::InvalidOperation => "[<class 'decimal.ConversionSyntax'>]".into(),
            PyErr::Type(m) | PyErr::Value(m) | PyErr::Attribute(m) | PyErr::Ofx(m) | PyErr::Other(m) => m.clone(),
        }
    }
}

type R<T> = Result<T, PyErr>;

const NOT_CALLABLE: &str = "'NoneType' object is not callable";
const NONE_CONTENTS: &str = "'NoneType' object has no attribute 'contents'";

// ---------------------------------------------------------------------------------------- file and headers

fn bytes_strip(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map(|p| p + 1).unwrap_or(start);
    &b[start..end.max(start)]
}

/// `bytes.splitlines()`: "\n", "\r" and "\r\n".
fn bytes_splitlines(b: &[u8]) -> Vec<&[u8]> {
    let mut out = vec![];
    let (mut start, mut i) = (0, 0);
    while i < b.len() {
        if b[i] == b'\n' || b[i] == b'\r' {
            out.push(&b[start..i]);
            if b[i] == b'\r' && i + 1 < b.len() && b[i + 1] == b'\n' {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    if start < b.len() {
        out.push(&b[start..]);
    }
    out
}

/// `OfxPreprocessedFile(fh)`: the headers read and the body decoded as they say, then every tag that is never closed
/// in the file closed right before the next tag.
fn preprocessed_text(data: &[u8]) -> R<String> {
    // read_headers
    let head_all = &data[..data.len().min(10240)];
    let head = match head_all.iter().position(|b| *b == b'<') {
        Some(p) => &head_all[..p],
        None => &head_all[..head_all.len().saturating_sub(1)], // find() == -1: everything but the last byte
    };
    let mut headers: Vec<(Vec<u8>, Vec<u8>)> = vec![];
    for line in bytes_splitlines(head) {
        if bytes_strip(line).is_empty() {
            break;
        }
        let parts: Vec<&[u8]> = line.split(|b| *b == b':').collect();
        if parts.len() < 2 {
            return Err(PyErr::Value(format!("not enough values to unpack (expected 2, got {})", parts.len())));
        }
        if parts.len() > 2 {
            return Err(PyErr::Value("too many values to unpack (expected 2)".into()));
        }
        let key = bytes_strip(parts[0]).to_ascii_uppercase();
        let value = bytes_strip(parts[1]).to_vec();
        match headers.iter_mut().find(|(k, _)| *k == key) {
            Some(e) => e.1 = value,
            None => headers.push((key, value)),
        }
    }
    // handle_encoding
    let ascii: Vec<(String, String)> = headers.iter().map(|(k, v)| (pycodecs::ascii_replace(k), pycodecs::ascii_replace(v))).collect();
    let get = |k: &str| ascii.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    let enc_type = get("ENCODING").unwrap_or_default();
    let codec: Codec = if enc_type.is_empty() {
        Codec::Ascii
    } else {
        let encoding = if enc_type == "USASCII" {
            let cp = get("CHARSET").unwrap_or_else(|| "1252".into());
            if cp == "8859-1" {
                "iso-8859-1".to_string()
            } else {
                format!("cp{cp}")
            }
        } else if enc_type == "UNICODE" || enc_type == "UTF-8" {
            "utf-8".to_string()
        } else {
            return Err(PyErr::Other("cannot access local variable 'encoding' where it is not associated with a value".into()));
        };
        let codec = pycodecs::lookup(&encoding).map_err(PyErr::Other)?;
        for (k, v) in &headers {
            pycodecs::decode(k, codec, true).map_err(PyErr::Other)?;
            pycodecs::decode(v, codec, true).map_err(PyErr::Other)?;
        }
        codec
    };
    let text = pycodecs::decode(data, codec, false).map_err(PyErr::Other)?;

    // OfxPreprocessedFile: close the tags that are never closed
    let name_class = r"[a-zA-Z0-9_.\x{130}\x{131}\x{17f}\x{212a}]"; // (?i)[a-z0-9_\.] as Python matches it
    let closing: Vec<String> = re(&format!(r"</({name_class}+)>")).captures_iter(&text).map(|c| py::upper(&c[1])).collect();
    let tag_re = re(&format!(r"</?{name_class}+>"));
    let open_re = re(&format!(r"<({name_class}+)>"));
    let mut tokens: Vec<&str> = vec![];
    let mut last = 0;
    for m in tag_re.find_iter(&text) {
        tokens.push(&text[last..m.start()]);
        tokens.push(m.as_str());
        last = m.end();
    }
    tokens.push(&text[last..]);
    let mut out = String::with_capacity(text.len() + 1024);
    let mut last_open: Option<String> = None;
    for token in tokens {
        let is_closing = token.starts_with("</");
        let is_processing = token.starts_with("<?");
        let is_cdata = token.starts_with("<!");
        let is_tag = token.starts_with('<') && !is_cdata;
        let is_open = is_tag && !is_closing && !is_processing;
        if is_tag {
            if let Some(t) = last_open.take() {
                out.push_str(&format!("</{t}>"));
            }
        }
        if is_open {
            let name = open_re.captures(token).map(|c| c[1].to_string()).ok_or(PyErr::Index)?;
            if !closing.contains(&py::upper(&name)) {
                last_open = Some(name);
            }
        }
        out.push_str(token);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------- tree access

struct T<'a> {
    soup: &'a Soup,
}

impl<'a> T<'a> {
    fn find(&self, id: usize, name: &str) -> Option<usize> {
        self.soup.find(id, name)
    }
    fn find_all(&self, id: usize, name: &str) -> Vec<usize> {
        self.soup.find_all(id, name)
    }
    fn has_contents(&self, id: usize) -> bool {
        !self.soup.children(id).is_empty()
    }
    /// `tag.contents[0]` as text: IndexError when empty, None for a tag (whose `.strip` is a failed lookup).
    fn first(&self, id: usize) -> R<Option<&'a str>> {
        let c = self.soup.children(id).first().ok_or(PyErr::Index)?;
        Ok(match &self.soup.nodes[*c] {
            Node::Text(t) => Some(t.as_str()),
            Node::Tag { .. } => None,
        })
    }
    /// `tag.contents[0].strip()`.
    fn strip0(&self, id: usize) -> R<String> {
        match self.first(id)? {
            Some(t) => Ok(py::strip(t).to_string()),
            None => Err(PyErr::Type(NOT_CALLABLE.into())),
        }
    }
    /// `tag.contents[0].lower().strip()`.
    fn lower_strip0(&self, id: usize) -> R<String> {
        match self.first(id)? {
            Some(t) => Ok(py::strip(&py::lower(t)).to_string()),
            None => Err(PyErr::Type(NOT_CALLABLE.into())),
        }
    }
    /// `find(name).contents[0].strip()`, the tag being possibly missing.
    fn find_strip0(&self, id: usize, name: &str) -> R<String> {
        match self.find(id, name) {
            Some(t) => self.strip0(t),
            None => Err(PyErr::Attribute(NONE_CONTENTS.into())),
        }
    }
}

/// `decimal.Decimal(text)` for the decimal forms (an infinity or a NaN is refused like a syntax error).
fn py_decimal_str(text: &str) -> Option<Decimal> {
    let t = py::strip(text);
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let digit = |j: Option<&char>| j.map(|c| py::is_decimal_char(*c)).unwrap_or(false);
            if i == 0 || !digit(chars.get(i - 1)) || !digit(chars.get(i + 1)) {
                return None;
            }
        }
    }
    py_decimal(&t.replace('_', ""))
}

/// `OfxParser.toDecimal(tag)`.
fn to_decimal(t: &T, tag: usize) -> R<Decimal> {
    let mut d = t.strip0(tag)?;
    if re(r".*\..*,").is_match(&d) {
        d = d.replace('.', "");
    }
    if re(r".*,.*\.").is_match(&d) {
        d = d.replace(',', "");
    }
    if !d.contains('.') && d.contains(',') {
        d = d.replace(',', ".");
    }
    d = d.replace(' ', "").replace('+', "");
    py_decimal_str(&d).ok_or(PyErr::InvalidOperation)
}

fn ascii_digits(s: &str) -> String {
    s.chars().map(|c| if !c.is_ascii() && py::is_decimal_char(c) { char::from_digit(unicode_digit(c), 10).unwrap() } else { c }).collect()
}

/// `datetime.strptime(data, "%Y%m%d%H%M%S")` (with_time) or `"%Y%m%d"`.
fn strptime(data: &str, with_time: bool) -> R<NaiveDateTime> {
    let (pattern, fmt) = if with_time {
        (
            r"\A(?P<Y>\d\d\d\d)(?P<m>1[0-2]|0[1-9]|[1-9])(?P<d>3[0-1]|[1-2]\d|0[1-9]|[1-9]| [1-9])(?P<H>2[0-3]|[0-1]\d|\d| \d)(?P<M>[0-5]\d|\d)(?P<S>6[0-1]|[0-5]\d|\d)",
            "%Y%m%d%H%M%S",
        )
    } else {
        (r"\A(?P<Y>\d\d\d\d)(?P<m>1[0-2]|0[1-9]|[1-9])(?P<d>3[0-1]|[1-2]\d|0[1-9]|[1-9]| [1-9])", "%Y%m%d")
    };
    let Some(m) = re(pattern).captures(data) else {
        return Err(PyErr::Value(format!("time data {} does not match format {}", py::repr_str(data), py::repr_str(fmt))));
    };
    let end = m.get(0).unwrap().end();
    if end != data.len() {
        return Err(PyErr::Value(format!("unconverted data remains: {}", &data[end..])));
    }
    let n = |k: &str| m.name(k).map(|x| int(x.as_str())).unwrap_or(0);
    let date = NaiveDate::from_ymd_opt(n("Y") as i32, n("m") as u32, n("d") as u32)
        .filter(|_| n("Y") >= 1)
        .ok_or_else(|| PyErr::Value(if n("Y") < 1 { format!("year {} is out of range", n("Y")) } else { "day is out of range for month".into() }))?;
    if n("S") > 59 {
        return Err(PyErr::Value("second must be in 0..59".into()));
    }
    Ok(date.and_hms_opt(n("H") as u32, n("M") as u32, n("S") as u32).unwrap())
}

fn in_range(d: NaiveDateTime) -> R<NaiveDateTime> {
    use chrono::Datelike;
    if (1..=9999).contains(&d.year()) {
        Ok(d)
    } else {
        Err(PyErr::Other("date value out of range".into()))
    }
}

/// `OfxParser.parseOfxDateTime`: None for "00000000".
fn parse_ofx_datetime(s: &str) -> R<Option<NaiveDateTime>> {
    let tz: f64 = match re(r"\[(?P<tz>[-+]?\d+\.?\d*):[\p{L}\p{N}_]*\]$").captures(s) {
        Some(m) => ascii_digits(&m["tz"]).parse().unwrap_or(0.0),
        None => 0.0,
    };
    let offset_us = (tz * 3_600_000_000.0).round_ties_even();
    if !offset_us.is_finite() || offset_us.abs() > 8.64e19 {
        return Err(PyErr::Other("date value out of range".into()));
    }
    let offset = Duration::microseconds(offset_us as i64);
    let msec = match re(r"^[0-9]*\.([0-9]{0,5})").captures(s) {
        Some(m) => {
            let secs: f64 = format!("0.{}", &m[1]).parse().unwrap_or(0.0);
            Duration::microseconds((secs * 1_000_000.0).round_ties_even() as i64)
        }
        None => Duration::zero(),
    };
    let shift = |d: NaiveDateTime| -> R<NaiveDateTime> {
        let a = in_range(d.checked_sub_signed(offset).ok_or_else(|| PyErr::Other("date value out of range".into()))?)?;
        in_range(a.checked_add_signed(msec).ok_or_else(|| PyErr::Other("date value out of range".into()))?)
    };
    let first14: String = s.chars().take(14).collect();
    match strptime(&first14, true) {
        Ok(d) => shift(d).map(Some),
        Err(PyErr::Value(_)) => {
            let first8: String = s.chars().take(8).collect();
            if first8 == "00000000" {
                return Ok(None);
            }
            shift(strptime(&first8, false)?).map(Some)
        }
        Err(e) => Err(e),
    }
}

// ------------------------------------------------------------------------------------------------ model

#[derive(Default)]
struct Transaction {
    payee: String,
    memo: String,
    date: Option<NaiveDateTime>,
    amount: Decimal,
    id: String,
}

enum Txn {
    Bank(Transaction),
    /// An investment operation: ofxparse's InvestmentTransaction, which the import cannot read (no payee).
    Investment,
}

#[derive(Default)]
struct Statement {
    /// None when the attribute does not exist (an investment statement without CURDEF).
    currency: Option<String>,
    balance: Option<Decimal>,
    /// The attribute exists (Some) only when DTASOF was read; its value is None for "00000000".
    balance_date: Option<Option<NaiveDateTime>>,
    transactions: Vec<Txn>,
}

struct Account {
    account_id: String,
    account_type: String,
    desc: Option<String>,
    statement: Statement,
}

fn parse_transaction(t: &T, txn: usize) -> R<Transaction> {
    let mut tr = Transaction::default();
    if let Some(tag) = t.find(txn, "trntype") {
        match t.lower_strip0(tag) {
            Ok(_) => {}
            Err(PyErr::Index) => return Err(PyErr::Ofx("Empty transaction type".into())),
            Err(PyErr::Type(_)) => return Err(PyErr::Ofx("No Transaction type (a required field)".into())),
            Err(e) => return Err(e),
        }
    }
    if let Some(tag) = t.find(txn, "name") {
        match t.strip0(tag) {
            Ok(v) => tr.payee = v,
            Err(PyErr::Index) => return Err(PyErr::Ofx("Empty transaction name".into())),
            Err(PyErr::Type(_)) => return Err(PyErr::Ofx("No Transaction name (a required field)".into())),
            Err(e) => return Err(e),
        }
    }
    if let Some(tag) = t.find(txn, "memo") {
        if let Ok(v) = t.strip0(tag) {
            tr.memo = v; // memo can be empty
        }
    }
    match t.find(txn, "trnamt") {
        Some(tag) => match to_decimal(t, tag) {
            Ok(v) => tr.amount = v,
            Err(PyErr::Index) => return Err(PyErr::Ofx("Invalid Transaction Date".into())),
            Err(PyErr::InvalidOperation) => {
                let raw = t.first(tag)?.unwrap_or("");
                if ["null", "-null"].contains(&py::strip(raw)) {
                    tr.amount = Decimal::ZERO;
                } else {
                    return Err(PyErr::Ofx(format!("Invalid Transaction Amount: '{raw}'")));
                }
            }
            Err(PyErr::Type(_)) => return Err(PyErr::Ofx("No Transaction Amount (a required field)".into())),
            Err(e) => return Err(e),
        },
        None => return Err(PyErr::Ofx("Missing Transaction Amount (a required field)".into())),
    }
    match t.find(txn, "dtposted") {
        Some(tag) => match t.strip0(tag).and_then(|s| parse_ofx_datetime(&s)) {
            Ok(d) => tr.date = d,
            Err(PyErr::Index) => return Err(PyErr::Ofx("Invalid Transaction Date".into())),
            Err(PyErr::Value(m)) => return Err(PyErr::Ofx(m)),
            Err(PyErr::Type(_)) => return Err(PyErr::Ofx("No Transaction Date (a required field)".into())),
            Err(e) => return Err(e),
        },
        None => return Err(PyErr::Ofx("Missing Transaction Date (a required field)".into())),
    }
    if let Some(tag) = t.find(txn, "dtuser") {
        match t.strip0(tag).and_then(|s| parse_ofx_datetime(&s)) {
            Ok(_) | Err(PyErr::Type(_)) => {}
            Err(PyErr::Index) => return Err(PyErr::Ofx("Invalid Transaction User Date".into())),
            Err(PyErr::Value(m)) => return Err(PyErr::Ofx(m)),
            Err(e) => return Err(e),
        }
    }
    match t.find(txn, "fitid") {
        Some(tag) => match t.strip0(tag) {
            Ok(v) => tr.id = v,
            Err(PyErr::Index) => return Err(PyErr::Ofx("Empty FIT id (a required                     field)".into())),
            Err(PyErr::Type(_)) => return Err(PyErr::Ofx("No FIT id (a required field)".into())),
            Err(e) => return Err(e),
        },
        None => return Err(PyErr::Ofx("Missing FIT id (a required                                      field)".into())),
    }
    if let Some(tag) = t.find(txn, "sic") {
        match t.strip0(tag) {
            Ok(_) => {}
            Err(PyErr::Index) => return Err(PyErr::Ofx("Empty transaction Standard                                          Industry Code (SIC)".into())),
            Err(e) => return Err(e),
        }
    }
    if let Some(tag) = t.find(txn, "checknum") {
        match t.strip0(tag) {
            Ok(_) => {}
            Err(PyErr::Index) => return Err(PyErr::Ofx("Empty Check (or other reference)                     number".into())),
            Err(e) => return Err(e),
        }
    }
    Ok(tr)
}

/// `parseBalance`: (amount, date attribute) of LEDGERBAL or AVAILBAL.
fn parse_balance(t: &T, stmt: usize, tag_name: &str) -> R<(Option<Decimal>, Option<Option<NaiveDateTime>>)> {
    let Some(bal) = t.find(stmt, tag_name) else { return Ok((None, None)) };
    let (balamt, dtasof) = (t.find(bal, "balamt"), t.find(bal, "dtasof"));
    let mut out = (None, None);
    if let Some(a) = balamt {
        match to_decimal(t, a) {
            Ok(v) => out.0 = Some(v),
            Err(PyErr::Index) | Err(PyErr::InvalidOperation) => return Err(PyErr::Ofx("Empty ledger balance                            ".into())),
            Err(e) => return Err(e),
        }
    }
    if let Some(d) = dtasof {
        out.1 = Some(parse_ofx_datetime(&t.strip0(d)?)?);
    }
    Ok(out)
}

fn parse_statement(t: &T, stmt: usize) -> R<Statement> {
    let mut st = Statement { currency: Some(String::new()), ..Default::default() };
    if let Some(tag) = t.find(stmt, "dtstart") {
        parse_ofx_datetime(&t.strip0(tag)?)?;
    }
    if let Some(tag) = t.find(stmt, "dtend") {
        parse_ofx_datetime(&t.strip0(tag)?)?;
    }
    if let Some(tag) = t.find(stmt, "curdef") {
        st.currency = Some(py::lower(&t.strip0(tag)?));
    }
    let (balance, date) = parse_balance(t, stmt, "ledgerbal")?;
    st.balance = balance;
    st.balance_date = date;
    parse_balance(t, stmt, "availbal")?;
    for txn in t.find_all(stmt, "stmttrn") {
        st.transactions.push(Txn::Bank(parse_transaction(t, txn)?));
    }
    Ok(st)
}

fn parse_stmtrs(t: &T, list: &[usize]) -> R<Vec<Account>> {
    let mut out = vec![];
    for &s in list {
        let mut a = Account { account_id: String::new(), account_type: String::new(), desc: None, statement: Statement::default() };
        let read = |name: &str| -> R<Option<String>> {
            match t.find(s, name) {
                Some(tag) if t.has_contents(tag) => t.strip0(tag).map(Some),
                _ => Ok(None),
            }
        };
        read("curdef")?;
        if let Some(v) = read("acctid")? {
            a.account_id = v;
        }
        read("bankid")?;
        read("branchid")?;
        if let Some(v) = read("accttype")? {
            a.account_type = v;
        }
        a.statement = parse_statement(t, s)?;
        out.push(a);
    }
    Ok(out)
}

const AGGREGATE_TYPES: &[&str] = &[
    "buydebt", "buymf", "buyopt", "buyother", "buystock", "closureopt", "income", "invexpense", "jrnlfund", "jrnlsec", "margininterest", "reinvest",
    "retofcap", "selldebt", "sellmf", "sellopt", "sellother", "sellstock", "split", "transfer",
];

fn parse_investment_statement(t: &T, inv: usize) -> R<Statement> {
    let mut st = Statement::default();
    if let Some(tag) = t.find(inv, "curdef") {
        st.currency = Some(py::lower(&t.strip0(tag)?));
    }
    if let Some(list) = t.find(inv, "invtranlist") {
        if let Some(tag) = t.find(list, "dtstart") {
            parse_ofx_datetime(&t.strip0(tag)?)?;
        }
        if let Some(tag) = t.find(list, "dtend") {
            match t.strip0(tag).and_then(|s| parse_ofx_datetime(&s)) {
                Ok(_) | Err(PyErr::Index) => {} // an empty end date is only a warning
                Err(e) => return Err(e),
            }
        }
    }
    for kind in ["posmf", "posstock", "posopt", "posother", "posdebt"] {
        for pos in t.find_all(inv, kind) {
            if let Some(tag) = t.find(pos, "uniqueid") {
                t.strip0(tag)?;
            }
            for name in ["units", "unitprice", "mktval"] {
                if let Some(tag) = t.find(pos, name) {
                    to_decimal(t, tag)?;
                }
            }
            if let Some(tag) = t.find(pos, "dtpriceasof") {
                parse_ofx_datetime(&t.strip0(tag)?)?;
            }
        }
    }
    for kind in AGGREGATE_TYPES {
        for txn in t.find_all(inv, kind) {
            for name in ["fitid", "memo"] {
                if let Some(tag) = t.find(txn, name) {
                    t.strip0(tag)?;
                }
            }
            for name in ["dttrade", "dtsettle"] {
                if let Some(tag) = t.find(txn, name) {
                    parse_ofx_datetime(&t.strip0(tag)?)?;
                }
            }
            for name in ["uniqueid", "incometype"] {
                if let Some(tag) = t.find(txn, name) {
                    t.strip0(tag)?;
                }
            }
            for name in ["units", "unitprice", "commission", "fees", "total"] {
                if let Some(tag) = t.find(txn, name) {
                    to_decimal(t, tag)?;
                }
            }
            for name in ["inv401ksource", "tferaction"] {
                if let Some(tag) = t.find(txn, name) {
                    t.strip0(tag)?;
                }
            }
            st.transactions.push(Txn::Investment);
        }
    }
    for bank in t.find_all(inv, "invbanktran") {
        for txn in t.find_all(bank, "stmttrn") {
            st.transactions.push(Txn::Bank(parse_transaction(t, txn)?));
        }
    }
    if let Some(bal) = t.find(inv, "invbal") {
        for name in ["availcash", "marginbalance", "shortbalance", "buypower"] {
            if let Some(tag) = t.find(bal, name) {
                to_decimal(t, tag)?;
            }
        }
        if let Some(list) = t.find(bal, "ballist") {
            for b in t.find_all(list, "bal") {
                for name in ["name", "desc"] {
                    if let Some(tag) = t.find(b, name) {
                        t.strip0(tag)?;
                    }
                }
                if let Some(tag) = t.find(b, "value") {
                    to_decimal(t, tag)?;
                }
            }
        }
    }
    Ok(st)
}

fn parse_invstmtrs(t: &T, list: &[usize]) -> R<Vec<Account>> {
    let mut out = vec![];
    for &s in list {
        let mut a = Account { account_id: String::new(), account_type: String::new(), desc: None, statement: Statement::default() };
        if let Some(tag) = t.find(s, "acctid") {
            a.account_id = t.strip0(tag)?;
        }
        if let Some(tag) = t.find(s, "brokerid") {
            t.strip0(tag)?;
        }
        a.statement = parse_investment_statement(t, s)?;
        out.push(a);
    }
    Ok(out)
}

fn parse_seclist(t: &T, seclist: usize) -> R<()> {
    for sec in t.find_all(seclist, "secinfo") {
        let (uniqueid, name) = (t.find(sec, "uniqueid"), t.find(sec, "secname"));
        let (ticker, memo) = (t.find(sec, "ticker"), t.find(sec, "memo"));
        if let (Some(u), Some(n)) = (uniqueid, name) {
            for tag in [ticker, memo].into_iter().flatten() {
                t.strip0(tag)?; // a missing tag is fine (AttributeError caught), an empty one is not
            }
            t.strip0(u)?;
            t.strip0(n)?;
        }
    }
    Ok(())
}

fn parse_org(t: &T, fi: usize) -> R<()> {
    for name in ["org", "fid"] {
        if let Some(tag) = t.find(fi, name) {
            t.strip0(tag)?;
        }
    }
    Ok(())
}

fn parse_acctinfors(t: &T, acctinfors: usize) -> R<Vec<Account>> {
    let mut all: Vec<Account> = vec![];
    for i in t.find_all(acctinfors, "acctinfo") {
        let mut accounts = if t.find(i, "invacctinfo").is_some() {
            parse_invstmtrs(t, &[i])?
        } else if t.find(i, "ccacctinfo").is_some() || t.find(i, "bankacctinfo").is_some() {
            parse_stmtrs(t, &[i])?
        } else {
            continue;
        };
        if let Some(fi) = t.find(ROOT, "fi") {
            for _ in &all {
                parse_org(t, fi)?;
            }
        }
        if let Some(desc) = t.find(i, "desc") {
            for a in accounts.iter_mut() {
                a.desc = Some(t.strip0(desc)?);
            }
        }
        all.extend(accounts);
    }
    Ok(all)
}

fn parse_status(t: &T, trnrs: usize) -> R<()> {
    if let Some(tag) = t.find(trnrs, "trnuid") {
        t.strip0(tag)?;
    }
    if let Some(status) = t.find(trnrs, "status") {
        let code = t.find_strip0(status, "code")?;
        py::py_int(&code).map_err(PyErr::Value)?;
        t.find_strip0(status, "severity")?;
        if let Some(m) = t.find(status, "message") {
            t.strip0(m)?;
        }
    }
    Ok(())
}

/// `OfxParser.parse(io.BytesIO(data))`, as far as the import reads it.
fn ofx_parse(data: &[u8]) -> R<Vec<Account>> {
    let text = preprocessed_text(data)?;
    let soup = htmlsoup::parse(&text);
    let t = T { soup: &soup };
    if t.find(ROOT, "ofx").is_none() {
        return Err(PyErr::Ofx("The ofx file is empty!".into()));
    }
    if let Some(sonrs) = t.find(ROOT, "sonrs") {
        // every field is optional (any error reads as None), but the code must be a number
        let code = t.find_strip0(sonrs, "code").ok();
        match code {
            None => return Err(PyErr::Type("int() argument must be a string, a bytes-like object or a real number, not 'NoneType'".into())),
            Some(c) => {
                py::py_int(&c).map_err(PyErr::Value)?;
            }
        }
    }
    for name in ["stmttrnrs", "ccstmttrnrs"] {
        if let Some(trnrs) = t.find(ROOT, name) {
            parse_status(&t, trnrs)?;
        }
    }
    let mut accounts = vec![];
    let stmtrs = t.find_all(ROOT, "stmtrs");
    if !stmtrs.is_empty() {
        accounts.extend(parse_stmtrs(&t, &stmtrs)?);
    }
    let ccstmtrs = t.find_all(ROOT, "ccstmtrs");
    if !ccstmtrs.is_empty() {
        accounts.extend(parse_stmtrs(&t, &ccstmtrs)?);
    }
    let invstmtrs = t.find_all(ROOT, "invstmtrs");
    if !invstmtrs.is_empty() {
        accounts.extend(parse_invstmtrs(&t, &invstmtrs)?);
        if let Some(seclist) = t.find(ROOT, "seclist") {
            parse_seclist(&t, seclist)?;
        }
    }
    if let Some(acctinfors) = t.find(ROOT, "acctinfors") {
        accounts.extend(parse_acctinfors(&t, acctinfors)?);
    }
    if let Some(fi) = t.find(ROOT, "fi") {
        for _ in &accounts {
            parse_org(&t, fi)?;
        }
    }
    Ok(accounts)
}

pub fn parse_ofx(data: &[u8], _filename: &str) -> Result<Vec<ParsedStatement>, String> {
    let accounts = ofx_parse(data).map_err(|e| e.message())?;
    let head: String = data[..data.len().min(4000)].iter().map(|&b| b as char).collect();
    let institution = find_institution(&head);
    let mut statements = vec![];
    for account in accounts {
        let stmt = &account.statement;
        let mut st = ParsedStatement::new("ofx");
        st.institution = institution.clone();
        st.account_number = Some(account.account_id.clone());
        st.account_label = Some(match account.desc.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => d.to_string(),
            None => {
                let c: Vec<char> = account.account_id.chars().collect();
                format!("Compte {}", c[c.len().saturating_sub(4)..].iter().collect::<String>())
            }
        });
        st.currency = py::upper(stmt.currency.as_deref().filter(|c| !c.is_empty()).unwrap_or("EUR"));
        st.confidence = "high".into();
        let default = if py::upper(&account.account_type).contains("SAVINGS") { "savings" } else { "checking" };
        st.account_type = Some(guess_account_type(st.account_label.as_deref().unwrap_or(""), default));
        if let Some(b) = stmt.balance {
            st.balance = Some(b);
            st.balance_date = match stmt.balance_date {
                Some(Some(d)) => Some(d.date()),
                _ => None,
            };
        }
        for txn in &stmt.transactions {
            let Txn::Bank(t) = txn else {
                return Err("'InvestmentTransaction' object has no attribute 'payee'".into());
            };
            let parts: Vec<&str> = [t.payee.as_str(), t.memo.as_str()].into_iter().filter(|p| !p.is_empty() && !py::strip(p).is_empty()).collect();
            let joined = parts.join(" ");
            let label = if joined.is_empty() { "Opération".to_string() } else { joined };
            let date = t.date.ok_or_else(|| "'NoneType' object has no attribute 'date'".to_string())?.date();
            let mut tx = TransactionData::new(date, py::strip(&label), t.amount);
            tx.currency = st.currency.clone();
            tx.external_id = if t.id.is_empty() { None } else { Some(t.id.clone()) };
            st.transactions.push(tx);
        }
        if !st.transactions.is_empty() {
            st.transactions.sort_by_key(|t| t.date);
            st.period_start = Some(st.transactions[0].date);
            st.period_end = Some(st.transactions.last().unwrap().date);
        }
        statements.push(st);
    }
    if statements.is_empty() {
        return Err("Aucun compte trouvé dans le fichier OFX".into());
    }
    Ok(statements)
}

pub fn parse_qif(data: &[u8], filename: &str) -> Result<Vec<ParsedStatement>, String> {
    let text = if data.contains(&0xc3) { pycodecs::utf8_ignore(data, true) } else { data.iter().map(|&b| b as char).collect() };
    let mut st = ParsedStatement::new("qif");
    let stem = filename.rsplit_once('.').map(|(a, _)| a).unwrap_or(filename);
    st.account_label = if stem.is_empty() { None } else { Some(stem.to_string()) };
    st.confidence = "high".into();
    let mut current: Vec<(char, String)> = vec![];
    let get = |cur: &[(char, String)], k: char| cur.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
    for line in py::splitlines(&text) {
        let Some(code) = line.chars().next() else { continue };
        let value = py::strip(&line[code.len_utf8()..]).to_string();
        if code == '^' {
            let when = parse_date(Some(&get(&current, 'D').unwrap_or_default().replace('\'', "/")), None);
            let amount_text = match get(&current, 'T') {
                Some(t) if !t.is_empty() => Some(t),
                _ => get(&current, 'U'),
            };
            let amount = parse_amount(amount_text.as_deref());
            if let (Some(when), Some(amount)) = (when, amount) {
                let parts: Vec<String> = [get(&current, 'P'), get(&current, 'M')].into_iter().flatten().filter(|v| !v.is_empty()).collect();
                let label = if parts.is_empty() { "Opération".to_string() } else { parts.join(" ") };
                st.transactions.push(TransactionData::new(when, label, amount));
            }
            current.clear();
        } else if "DTUPM".contains(code) {
            match current.iter_mut().find(|(c, _)| *c == code) {
                Some(e) => e.1 = value,
                None => current.push((code, value)),
            }
        }
    }
    if st.transactions.is_empty() {
        return Err("Aucune opération dans le fichier QIF".into());
    }
    st.transactions.sort_by_key(|t| t.date);
    st.period_start = Some(st.transactions[0].date);
    st.period_end = Some(st.transactions.last().unwrap().date);
    st.account_type = Some(guess_account_type(st.account_label.as_deref().unwrap_or(""), "checking"));
    Ok(vec![st])
}
