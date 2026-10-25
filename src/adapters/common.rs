//! Text, number, date and identifier helpers shared by all adapters.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use chrono::NaiveDate;
use rust_decimal::Decimal;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use super::re::{fullmatch, py_match, re, search};
use crate::py;

fn memo(table: &'static OnceLock<Mutex<HashMap<String, String>>>, key: &str, f: impl FnOnce(&str) -> String) -> String {
    let m = table.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = m.lock().unwrap().get(key) {
        return v.clone();
    }
    let v = f(key);
    let mut g = m.lock().unwrap();
    if g.len() > 65536 {
        g.clear();
    }
    g.insert(key.to_string(), v.clone());
    v
}

/// NFD, then without the combining marks of category Mn.
pub fn strip_accents(text: &str) -> String {
    static T: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    memo(&T, text, |t| {
        t.nfd()
            .filter(|c| unicode_general_category::get_general_category(*c) != unicode_general_category::GeneralCategory::NonspacingMark)
            .collect()
    })
}

#[allow(dead_code)]
fn _combining(c: char) -> bool {
    is_combining_mark(c)
}

/// Lower-case, accent-free, single-spaced.
pub fn norm(text: &str) -> String {
    static T: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    memo(&T, text, |t| py::split_ws(&py::lower(&strip_accents(t))).join(" "))
}

/// Parse "1 234,56", "-1,234.56", "1.234,56 €", "12,00-", "(45.10)".
pub fn parse_amount(raw: Option<&str>) -> Option<Decimal> {
    let raw = raw?;
    let cleaned = re(r"[€$£\s\x{a0}\x{202f}']|(?i:EUR|USD)").replace_all(raw, "");
    let mut text: String = py::strip(&cleaned).to_string();
    if text.is_empty() {
        return None;
    }
    let mut negative = false;
    if text.starts_with('(') && text.ends_with(')') && text.len() >= 2 {
        negative = true;
        text = text[1..text.len() - 1].to_string();
    }
    if text.ends_with('-') {
        negative = true;
        text.pop();
    }
    if let Some(t) = text.strip_prefix('+') {
        text = t.to_string();
    }
    if let Some(t) = text.strip_prefix('-') {
        negative = true;
        text = t.to_string();
    }
    if text.contains(',') && text.contains('.') {
        if text.rfind(',') > text.rfind('.') {
            text = text.replace('.', "").replace(',', ".");
        } else {
            text = text.replace(',', "");
        }
    } else if text.contains(',') {
        if fullmatch(r"\d{1,3}(,\d{3})+", &text).is_some() && search(r",\d{1,2}$", &text).is_none() {
            text = text.replace(',', "");
        } else {
            text = text.replace(',', ".");
        }
    } else if text.matches('.').count() > 1 && fullmatch(r"\d{1,3}(\.\d{3})+", &text).is_some() {
        text = text.replace('.', "");
    }
    let value = py_decimal(&text)?;
    Some(if negative { py::neg(value) } else { value })
}

/// Python's `Decimal(text)` for what amounts look like (digits with Unicode digits accepted, a dot, an exponent).
pub fn py_decimal(text: &str) -> Option<Decimal> {
    let t = py::strip(text);
    if t.is_empty() {
        return None;
    }
    // Decimal() accepts any Unicode decimal digit: map them to ASCII first.
    let ascii: String = t
        .chars()
        .map(|c| if c.is_ascii() { c } else if py::is_decimal_char(c) { char::from_digit(c.to_digit(10).unwrap_or_else(|| unicode_digit(c)), 10).unwrap_or(c) } else { c })
        .collect();
    let ok = re(r"^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$").is_match(&ascii);
    if !ok {
        return None;
    }
    let d = if ascii.contains(['e', 'E']) {
        Decimal::from_scientific(&ascii).ok()
    } else {
        let s = ascii.strip_suffix('.').unwrap_or(&ascii);
        s.parse::<Decimal>().ok()
    }?;
    Some(py::signed_zero(d, &ascii))
}

pub fn unicode_digit(c: char) -> u32 {
    // the value of a decimal digit: its offset in its block of ten (all Nd blocks are contiguous 0..9)
    let mut base = c as u32;
    while base > 0 && py::is_decimal_char(char::from_u32(base - 1).unwrap_or('x')) && (c as u32 - (base - 1)) < 10 {
        base -= 1;
    }
    (c as u32 - base) % 10
}

pub fn month_of(name: &str) -> Option<u32> {
    let m = match name {
        "janv" | "jan" | "janvier" => 1,
        "fevr" | "fev" | "feb" | "fevrier" => 2,
        "mars" | "mar" => 3,
        "avr" | "apr" | "avril" => 4,
        "mai" | "may" => 5,
        "juin" | "jun" => 6,
        "juil" | "jul" | "juillet" => 7,
        "aout" | "aug" => 8,
        "sept" | "sep" | "septembre" => 9,
        "oct" | "octobre" => 10,
        "nov" | "novembre" => 11,
        "dec" | "decembre" => 12,
        _ => return None,
    };
    Some(m)
}

pub fn ymd(y: i64, m: i64, d: i64) -> Option<NaiveDate> {
    if !(1..=9999).contains(&y) {
        return None;
    }
    NaiveDate::from_ymd_opt(y as i32, m.try_into().ok()?, d.try_into().ok()?)
}

pub fn int(s: &str) -> i64 {
    s.chars().filter_map(|c| c.to_digit(10).or_else(|| if py::is_decimal_char(c) { Some(unicode_digit(c)) } else { None })).fold(0i64, |a, d| a * 10 + d as i64)
}

pub fn parse_date(raw: Option<&str>, default_year: Option<i64>) -> Option<NaiveDate> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    let text = py::strip(raw);
    let pats: [(&str, [char; 3]); 4] = [
        (r"^(\d{4})-(\d{1,2})-(\d{1,2})", ['y', 'm', 'd']),
        (r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{4})", ['d', 'm', 'y']),
        (r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2})$", ['d', 'm', 'Y']),
        (r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2})\b", ['d', 'm', 'Y']),
    ];
    for (p, order) in pats {
        if let Some(m) = py_match(p, text) {
            let mut parts: HashMap<char, &str> = HashMap::new();
            for (i, k) in order.iter().enumerate() {
                parts.insert(*k, m.get(i + 1).unwrap().as_str());
            }
            let year = match parts.get(&'y') {
                Some(y) if int(y) != 0 => int(y),
                _ => 2000 + int(parts.get(&'Y').copied().unwrap_or("0")),
            };
            return ymd(year, int(parts[&'m']), int(parts[&'d']));
        }
    }
    if let Some(m) = py_match(r"^(\d{1,2})\s+([a-zA-Zéèûô]+)\.?\s+(\d{4})", text) {
        if let Some(month) = month_of(&py::lower(&strip_accents(&m[2])).to_string()) {
            return ymd(int(&m[3]), month as i64, int(&m[1]));
        }
    }
    if let Some(m) = py_match(r"^(\d{1,2})[/.](\d{1,2})$", text) {
        if let Some(y) = default_year {
            if y != 0 {
                return ymd(y, int(&m[2]), int(&m[1]));
            }
        }
    }
    None
}

/// `datetime.date(y, m, d)`, failing with Python's message.
pub fn py_date(y: i64, m: i64, d: i64) -> Result<NaiveDate, String> {
    if !(1..=9999).contains(&y) {
        return Err(format!("year {y} is out of range"));
    }
    if !(1..=12).contains(&m) {
        return Err("month must be in 1..12".into());
    }
    ymd(y, m, d).ok_or_else(|| "day is out of range for month".to_string())
}

pub fn parse_date_s(raw: &str) -> Option<NaiveDate> {
    parse_date(Some(raw), None)
}

/// "31 AOÛT 2026", "1er janvier 2026": the first long French date found in the text.
pub fn french_date(text: Option<&str>) -> Option<NaiveDate> {
    let text = text.unwrap_or("");
    for m in re(r"(\d{1,2})(?:er)?\s+([^\W\d_]+)\.?\s+(\d{4})").captures_iter(text) {
        if let Some(month) = month_of(&py::lower(&strip_accents(&m[2]))) {
            return ymd(int(&m[3]), month as i64, int(&m[1]));
        }
    }
    None
}

fn base36(c: char) -> Option<u32> {
    c.to_digit(36)
}

pub fn iban_valid(iban: &str) -> bool {
    let iban = iban.replace(' ', "").to_uppercase();
    if fullmatch(r"[A-Z]{2}\d{2}[A-Z0-9]{10,30}", &iban).is_none() {
        return false;
    }
    let chars: Vec<char> = iban.chars().collect();
    let rearranged: Vec<char> = chars[4..].iter().chain(chars[..4].iter()).copied().collect();
    let mut rem: u64 = 0;
    for c in rearranged {
        let Some(v) = base36(c).or_else(|| if py::is_decimal_char(c) { Some(unicode_digit(c)) } else { None }) else { return false };
        for d in v.to_string().chars() {
            rem = (rem * 10 + d.to_digit(10).unwrap() as u64) % 97;
        }
    }
    rem == 1
}

pub fn find_iban(text: &str) -> Option<String> {
    let upper = py::upper(text);
    for m in re(r"\b([A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){3,7}(?:[ ]?[A-Z0-9]{1,3})?)\b").captures_iter(&upper) {
        let candidate: String = m[1].replace(' ', "");
        let chars: Vec<char> = candidate.chars().collect();
        let mut end = chars.len();
        while end > 13 {
            let c: String = chars[..end].iter().collect();
            if iban_valid(&c) {
                return Some(c);
            }
            end -= 1;
        }
    }
    None
}

pub static INSTITUTIONS: &[(&str, &[&str])] = &[
    ("Fortuneo", &["fortuneo"]),
    ("BforBank", &["bforbank"]),
    ("Hello bank!", &["hello bank", "hellobank"]),
    ("BoursoBank", &["boursorama", "boursobank"]),
    ("BNP Paribas", &["bnp paribas"]),
    ("Société Générale", &["societe generale"]),
    ("Crédit Agricole", &["credit agricole"]),
    ("LCL", &["lcl", "credit lyonnais"]),
    ("Caisse d'Épargne", &["caisse d'epargne", "caisse depargne"]),
    ("Banque Populaire", &["banque populaire"]),
    ("Crédit Mutuel", &["credit mutuel"]),
    ("CIC", &["cic "]),
    ("La Banque Postale", &["banque postale"]),
    ("ING", &["ing direct", "ing bank"]),
    ("Revolut", &["revolut"]),
    ("Trade Republic", &["trade republic"]),
    ("Cashbee", &["cashbee"]),
    ("Nexo", &["nexo"]),
    ("Linxea", &["linxea"]),
    ("Spirica", &["spirica"]),
    ("Generali", &["generali"]),
    ("Suravenir", &["suravenir"]),
    ("Yomoni", &["yomoni"]),
    ("Nalo", &["nalo"]),
    ("Binance", &["binance"]),
    ("Kraken", &["kraken", "payward"]),
    ("Monabanq", &["monabanq"]),
    ("Shine", &["shine"]),
    ("Qonto", &["qonto"]),
    ("N26", &["n26"]),
    ("AXA", &["axa banque", "axa "]),
    ("Ramify", &["ramify"]),
];

pub fn find_institution(text: &str) -> Option<String> {
    let head: String = text.chars().take(5000).collect();
    let lowered = norm(&head);
    let mut best: Option<(usize, &str)> = None;
    for (name, keys) in INSTITUTIONS {
        for key in *keys {
            if let Some(pos) = lowered.find(key) {
                // Python positions count characters, not bytes
                let cpos = lowered[..pos].chars().count();
                if best.map(|b| cpos < b.0).unwrap_or(true) {
                    best = Some((cpos, name));
                }
            }
        }
    }
    best.map(|b| b.1.to_string())
}

const NAME_NOISE: &[&str] = &["m", "mr", "mme", "mlle", "monsieur", "madame", "mademoiselle", "ou", "et", "or", "and", "sas", "sarl", "sa", "sasu", "eurl"];

/// Order- and civility-insensitive key: "M DUPONT Jean" and "Jean Dupont" are the same person.
pub fn name_key(name: &str) -> String {
    static T: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    memo(&T, name, |n| {
        let low = py::lower(&strip_accents(n));
        let tokens: BTreeSet<String> = re(r"[a-z0-9]+").find_iter(&low).map(|m| m.as_str().to_string()).filter(|t| !NAME_NOISE.contains(&t.as_str())).collect();
        let joined = tokens.into_iter().collect::<Vec<_>>().join(" ");
        if joined.is_empty() {
            norm(n)
        } else {
            joined
        }
    })
}

fn rib_digits(value: &str) -> Option<num_big::Big> {
    let mut digits = String::new();
    for c in value.to_uppercase().chars() {
        let d = match c {
            'A'..='R' => char::from_digit(((c as u32 - 'A' as u32) % 9) + 1, 10).unwrap().to_string(),
            'S'..='Z' => char::from_digit(((c as u32 - 'S' as u32) % 9) + 2, 10).unwrap().to_string(),
            c => c.to_string(),
        };
        digits.push_str(&d);
    }
    num_big::Big::parse(&digits)
}

pub fn rib_key(bank: &str, branch: &str, account: &str) -> Option<String> {
    let (b, g, a) = (rib_digits(bank)?, rib_digits(branch)?, rib_digits(account)?);
    let sum = (b.rem97() * 89 + g.rem97() * 15 + a.rem97() * 3) % 97;
    Some(format!("{:02}", 97 - sum))
}

pub fn rib_valid(bank: &str, branch: &str, account: &str, key: &str) -> bool {
    rib_key(bank, branch, account).map(|k| k == key).unwrap_or(false)
}

/// A French IBAN is "FR" + 2 check digits + the RIB.
pub fn iban_matches_rib(iban: &str, bank: &str, branch: &str, account: &str, key: &str) -> bool {
    let i = iban.replace(' ', "").to_uppercase();
    let tail: String = i.chars().skip(4).collect();
    tail == format!("{bank}{branch}{account}{key}").to_uppercase()
}

/// Arbitrary-length non-negative integers given as decimal digits: only the remainder by 97 is needed.
mod num_big {
    pub struct Big(Vec<u8>);
    impl Big {
        /// Python's `int(s)`: digits (Unicode decimal digits accepted), optional surrounding spaces.
        pub fn parse(s: &str) -> Option<Big> {
            let t = crate::py::strip(s);
            let t = t.strip_prefix('+').unwrap_or(t);
            if t.is_empty() {
                return None;
            }
            let mut v = vec![];
            for c in t.chars() {
                if c == '_' {
                    continue;
                }
                v.push(c.to_digit(10)? as u8);
            }
            Some(Big(v))
        }
        pub fn rem97(&self) -> u64 {
            self.0.iter().fold(0u64, |r, d| (r * 10 + *d as u64) % 97)
        }
    }
}
