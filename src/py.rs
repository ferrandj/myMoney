//! Python semantics the application relies on: string methods, `str(Decimal)`, `repr(float)`, `json.dumps`,
//! HTML escaping (markupsafe) and URL quoting, so that what is stored and shown is byte for byte what the
//! Python version produced.

use rust_decimal::Decimal;
use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory as G};

// ------------------------------------------------------------------------------------------- strings

/// `str.isspace()` for one character.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c' | '\x1d' | '\x1e' | '\x1f' | ' ' | '\u{85}' | '\u{a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

pub fn isspace(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_space)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `str.split()` without arguments.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|x| !x.is_empty()).collect()
}

/// `" ".join(s.split())`.
pub fn squash_ws(s: &str) -> String {
    split_ws(s).join(" ")
}

pub fn is_alpha_char(c: char) -> bool {
    matches!(get_general_category(c), G::UppercaseLetter | G::LowercaseLetter | G::TitlecaseLetter | G::ModifierLetter | G::OtherLetter)
}

pub fn is_decimal_char(c: char) -> bool {
    get_general_category(c) == G::DecimalNumber
}

/// `str.isdigit()` for one character: decimal digits and the "digit" numerics (superscripts, circled...).
pub fn is_digit_char(c: char) -> bool {
    is_decimal_char(c)
        || matches!(c, '²' | '³' | '¹' | '\u{2070}' | '\u{2074}'..='\u{2079}' | '\u{2080}'..='\u{2089}' | '\u{2460}'..='\u{2468}' | '\u{2474}'..='\u{247c}' | '\u{2488}'..='\u{2490}' | '\u{24f5}'..='\u{24fd}' | '\u{24ea}' | '\u{24ff}' | '\u{2776}'..='\u{277e}' | '\u{2780}'..='\u{2788}' | '\u{278a}'..='\u{2792}')
}

pub fn is_numeric_char(c: char) -> bool {
    matches!(get_general_category(c), G::DecimalNumber | G::LetterNumber | G::OtherNumber) || is_digit_char(c)
}

pub fn is_alnum_char(c: char) -> bool {
    is_alpha_char(c) || is_numeric_char(c)
}

pub fn isdigit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_digit_char)
}

pub fn isalnum(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_alnum_char)
}

pub fn isalpha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_alpha_char)
}

/// `str.lower()` / `str.upper()`: Rust's Unicode case mapping matches Python's for the text we handle.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// `str.capitalize()`.
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => {
            let first: String = if f == 'ß' { "Ss".into() } else { f.to_uppercase().collect() };
            first + &c.as_str().to_lowercase()
        }
    }
}

/// Python's slicing `s[:n]` by characters.
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

// ------------------------------------------------------------------------------------------- numbers

/// `str(Decimal)`: plain notation, or scientific when Python would use it (very small values).
pub fn dec_str(d: &Decimal) -> String {
    let s = d.to_string();
    let scale = d.scale() as i64;
    let mantissa = d.mantissa().unsigned_abs();
    let digits = if mantissa == 0 { 1 } else { mantissa.to_string().len() as i64 };
    let exponent = -scale;
    let adjusted = exponent + digits - 1;
    if exponent <= 0 && adjusted >= -6 {
        return s;
    }
    // Scientific notation like Python: "1E-7", "1.5E-7", "0E-8".
    let neg = d.is_sign_negative() && mantissa != 0 || (d.is_sign_negative() && mantissa == 0);
    let ds = mantissa.to_string();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if mantissa == 0 {
        out.push_str(&format!("0E{}", exponent));
        return out;
    }
    out.push_str(&ds[..1]);
    if ds.len() > 1 {
        out.push('.');
        out.push_str(&ds[1..]);
    }
    out.push('E');
    if adjusted >= 0 {
        out.push('+');
    }
    out.push_str(&adjusted.to_string());
    out
}

/// `Decimal(str)` for what the application parses (plain or exponent notation).
pub fn dec_parse(s: &str) -> Option<Decimal> {
    let t = strip(s);
    if t.is_empty() {
        return None;
    }
    if let Ok(d) = t.parse::<Decimal>() {
        return Some(d);
    }
    Decimal::from_scientific(t).ok()
}

/// `repr(float)`.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let abs = f.abs();
    if abs >= 1e16 || abs < 1e-4 {
        let s = format!("{:e}", f);
        let (mant, exp) = s.split_once('e').unwrap();
        let e: i32 = exp.parse().unwrap();
        return format!("{}e{}{:02}", mant, if e < 0 { '-' } else { '+' }, e.abs());
    }
    let s = format!("{}", f);
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Python's `round(x)` on a float: banker's rounding to an integer.
pub fn py_round(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        let t = x.trunc();
        if (t as i64) % 2 == 0 {
            t as i64
        } else {
            r as i64
        }
    } else {
        r as i64
    }
}

// ------------------------------------------------------------------------------------------- json

/// `json.dumps(value)` with Python's defaults (", " and ": " separators, non-ASCII escaped).
pub fn json_dumps(v: &Value) -> String {
    let mut out = String::new();
    write_json(v, &mut out, false);
    out
}

/// `json.dumps(value, ensure_ascii=False)`.
pub fn json_dumps_unicode(v: &Value) -> String {
    let mut out = String::new();
    write_json(v, &mut out, true);
    out
}

fn write_json(v: &Value, out: &mut String, unicode: bool) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_nan() {
                    out.push_str("NaN");
                } else if f.is_infinite() {
                    out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" });
                } else {
                    out.push_str(&float_repr(f));
                }
            }
        }
        Value::String(s) => write_json_str(s, out, unicode),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(x, out, unicode);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json_str(k, out, unicode);
                out.push_str(": ");
                write_json(x, out, unicode);
            }
            out.push('}');
        }
    }
}

fn write_json_str(s: &str, out: &mut String, unicode: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if !unicode && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ------------------------------------------------------------------------------------------- html / urls

/// `markupsafe.escape`.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `html.escape(s)` of the standard library (quote=True): `&#x27;` for the apostrophe.
pub fn std_html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

const ALWAYS_SAFE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-~";

/// `urllib.parse.quote(s, safe)`.
pub fn quote(s: &str, safe: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        if ALWAYS_SAFE.contains(b) || safe.as_bytes().contains(b) {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// `urllib.parse.quote_plus(s)`.
pub fn quote_plus(s: &str) -> String {
    if !s.contains(' ') {
        return quote(s, "");
    }
    quote(s, " ").replace(' ', "+")
}

/// `urllib.parse.urlencode(pairs)`.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect::<Vec<_>>().join("&")
}

/// `urllib.parse.unquote_plus`.
pub fn unquote_plus(s: &str) -> String {
    let b = s.replace('+', " ");
    let bytes = b.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&b[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decimal_str_like_python() {
        assert_eq!(dec_str(&Decimal::from_str("1320.92").unwrap()), "1320.92");
        assert_eq!(dec_str(&Decimal::from_str("0.00").unwrap()), "0.00");
        assert_eq!(dec_str(&Decimal::from_str("-0.99").unwrap()), "-0.99");
        assert_eq!(dec_str(&Decimal::from_str("0.0000001").unwrap()), "1E-7");
        assert_eq!(dec_str(&Decimal::from_str("0.000001").unwrap()), "0.000001");
    }

    #[test]
    fn json_like_python() {
        let v: Value = serde_json::from_str(r#"{"a": 1, "b": [1.0, 0.85, "é"], "c": null}"#).unwrap();
        assert_eq!(json_dumps(&v), r#"{"a": 1, "b": [1.0, 0.85, "é"], "c": null}"#);
    }

    #[test]
    fn floats_like_python() {
        assert_eq!(float_repr(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(float_repr(100.0), "100.0");
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(3.5), 4);
        assert_eq!(py_round(-2.5), -2);
    }
}
