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

/// `str.splitlines()`: every line break Python knows, "\r\n" counted once, no trailing empty line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
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

/// `-d` with Python's decimal: a zero comes out positive.
pub fn neg(d: Decimal) -> Decimal {
    if d.is_zero() {
        d.abs()
    } else {
        -d
    }
}

/// `x or Decimal(0)`: a zero (falsy, whatever its exponent) becomes the plain `Decimal(0)`.
pub fn or_zero(d: Option<Decimal>) -> Decimal {
    match d {
        Some(v) if !v.is_zero() => v,
        _ => Decimal::ZERO,
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `int(s)` (base 10): surrounding whitespace, a sign, Unicode decimal digits, single underscores between digits.
pub fn py_int(s: &str) -> Result<i64, String> {
    let err = || format!("invalid literal for int() with base 10: {}", repr_str(s));
    let t = strip(s);
    let (neg, body) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return Err(err());
    }
    let mut v: i64 = 0;
    for c in body.chars() {
        if c == '_' {
            continue;
        }
        if !is_decimal_char(c) {
            return Err(err());
        }
        let d = c.to_digit(10).unwrap_or_else(|| crate::adapters::common::unicode_digit(c)) as i64;
        v = v.checked_mul(10).and_then(|x| x.checked_add(d)).ok_or_else(err)?;
    }
    Ok(if neg { -v } else { v })
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
    let d = t.parse::<Decimal>().ok().or_else(|| Decimal::from_scientific(t).ok())?;
    Some(signed_zero(d, t))
}

// ------------------------------------------------------------------------- decimal arithmetic

const PREC: usize = 28;

fn parts(d: &Decimal) -> (bool, num_bigint::BigUint, i64) {
    (d.is_sign_negative(), num_bigint::BigUint::from(d.mantissa().unsigned_abs()), -(d.scale() as i64))
}

fn pow10(n: u32) -> num_bigint::BigUint {
    num_bigint::BigUint::from(10u32).pow(n)
}

/// Round a coefficient to `PREC` digits, half to even (decimal's `_fix` under the default context).
fn fix(coeff: num_bigint::BigUint, exp: i64) -> (num_bigint::BigUint, i64) {
    use num_traits::{One, Zero};
    let digits = coeff.to_string().len();
    if coeff.is_zero() || digits <= PREC {
        return (coeff, exp);
    }
    let drop = (digits - PREC) as u32;
    let div = pow10(drop);
    let (mut q, r) = (&coeff / &div, &coeff % &div);
    let half = &div / num_bigint::BigUint::from(2u32);
    let odd = (&q % num_bigint::BigUint::from(2u32)).is_one();
    if r > half || (r == half && odd) {
        q += num_bigint::BigUint::one();
    }
    // a carry can make it PREC + 1 digits (999.. -> 1000..)
    if q.to_string().len() > PREC {
        return (q / num_bigint::BigUint::from(10u32), exp + drop as i64 + 1);
    }
    (q, exp + drop as i64)
}

/// Back to rust_decimal (a positive exponent becomes plain digits; more than 28 decimals are rounded half to even).
fn build(neg: bool, coeff: num_bigint::BigUint, exp: i64) -> Decimal {
    use num_traits::ToPrimitive;
    let (coeff, scale) = if exp > 0 {
        (coeff * pow10(exp as u32), 0u32)
    } else if exp < -28 {
        let drop = (-exp - 28) as u32;
        let div = pow10(drop);
        let (q, r) = (&coeff / &div, &coeff % &div);
        let half = &div / num_bigint::BigUint::from(2u32);
        let odd = (&q % num_bigint::BigUint::from(2u32)) == num_bigint::BigUint::from(1u32);
        let q = if r > half || (r == half && odd) { q + num_bigint::BigUint::from(1u32) } else { q };
        (q, 28)
    } else {
        (coeff, (-exp) as u32)
    };
    let m = coeff.to_i128().unwrap_or(i128::MAX);
    let mut d = Decimal::try_from_i128_with_scale(if neg { -m } else { m }, scale).unwrap_or(Decimal::MAX);
    if neg && d.is_zero() {
        d.set_sign_negative(true);
    }
    d
}

/// A decimal as Python's `decimal` holds it (any exponent), for chains of operations whose intermediate results
/// rust_decimal cannot hold (more than 28 decimals).
#[derive(Clone, Debug)]
pub struct PyDec {
    neg: bool,
    coeff: num_bigint::BigUint,
    exp: i64,
}

impl PyDec {
    pub fn from(d: &Decimal) -> PyDec {
        let (neg, coeff, exp) = parts(d);
        PyDec { neg, coeff, exp }
    }

    pub fn int(n: i64) -> PyDec {
        PyDec::from(&Decimal::from(n))
    }

    /// `self / other` (28 significant digits, half to even, the ideal exponent when exact). None for a zero divisor.
    pub fn div(&self, other: &PyDec) -> Option<PyDec> {
        use num_traits::Zero;
        if other.coeff.is_zero() {
            return None;
        }
        let neg = self.neg != other.neg;
        if self.coeff.is_zero() {
            return Some(PyDec { neg, coeff: self.coeff.clone(), exp: self.exp - other.exp });
        }
        let shift = other.coeff.to_string().len() as i64 - self.coeff.to_string().len() as i64 + PREC as i64 + 1;
        let mut exp = self.exp - other.exp - shift;
        let (mut coeff, rem) = if shift >= 0 {
            let n = &self.coeff * pow10(shift as u32);
            (&n / &other.coeff, &n % &other.coeff)
        } else {
            let d = &other.coeff * pow10((-shift) as u32);
            (&self.coeff / &d, &self.coeff % &d)
        };
        if !rem.is_zero() {
            // not exact: make sure the rounding sees it
            if (&coeff % num_bigint::BigUint::from(5u32)).is_zero() {
                coeff += num_bigint::BigUint::from(1u32);
            }
        } else {
            let ideal = self.exp - other.exp;
            while exp < ideal && (&coeff % num_bigint::BigUint::from(10u32)).is_zero() {
                coeff /= num_bigint::BigUint::from(10u32);
                exp += 1;
            }
        }
        let (coeff, exp) = fix(coeff, exp);
        Some(PyDec { neg, coeff, exp })
    }

    /// `self * other` (exact, then 28 significant digits).
    pub fn mul(&self, other: &PyDec) -> PyDec {
        let (coeff, exp) = fix(&self.coeff * &other.coeff, self.exp + other.exp);
        PyDec { neg: self.neg != other.neg, coeff, exp }
    }

    pub fn to_decimal(&self) -> Decimal {
        build(self.neg, self.coeff.clone(), self.exp)
    }

    /// `str(Decimal)`.
    pub fn py_str(&self) -> String {
        let digits = self.coeff.to_string();
        let n = digits.len() as i64;
        let leftdigits = self.exp + n;
        let dotplace = if self.exp <= 0 && leftdigits > -6 { leftdigits } else { 1 };
        let (intpart, fracpart) = if dotplace <= 0 {
            ("0".to_string(), format!(".{}{}", "0".repeat((-dotplace) as usize), digits))
        } else if dotplace >= n {
            (format!("{}{}", digits, "0".repeat((dotplace - n) as usize)), String::new())
        } else {
            (digits[..dotplace as usize].to_string(), format!(".{}", &digits[dotplace as usize..]))
        };
        let exp = if leftdigits == dotplace { String::new() } else { format!("E{:+}", leftdigits - dotplace) };
        format!("{}{intpart}{fracpart}{exp}", if self.neg { "-" } else { "" })
    }

    /// `float(Decimal)`: the closest double.
    pub fn to_f64(&self) -> f64 {
        self.py_str().parse().unwrap_or(0.0)
    }

    pub fn is_zero(&self) -> bool {
        use num_traits::Zero;
        self.coeff.is_zero()
    }
}

/// `a / b` with Python's decimal. None for b = 0.
pub fn py_div(a: &Decimal, b: &Decimal) -> Option<Decimal> {
    PyDec::from(a).div(&PyDec::from(b)).map(|q| q.to_decimal())
}

/// `a * b` with Python's decimal (exact, then 28 significant digits).
pub fn py_mul(a: &Decimal, b: &Decimal) -> Decimal {
    PyDec::from(a).mul(&PyDec::from(b)).to_decimal()
}

/// `str(Decimal(d).normalize())`: trailing zeros dropped, a positive exponent written out ("100" is "1E+2").
pub fn dec_normalize_str(d: &Decimal) -> String {
    let neg = d.is_sign_negative();
    let mut digits = d.mantissa().unsigned_abs().to_string();
    let mut exp: i64 = -(d.scale() as i64);
    if digits == "0" {
        return if neg { "-0".into() } else { "0".into() };
    }
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
        exp += 1;
    }
    let sign = if neg { "-" } else { "" };
    let n = digits.len() as i64;
    let adjusted = exp + n - 1;
    if exp <= 0 && adjusted >= -6 {
        if exp == 0 {
            return format!("{sign}{digits}");
        }
        let point = n + exp; // digits before the point
        return if point > 0 {
            format!("{sign}{}.{}", &digits[..point as usize], &digits[point as usize..])
        } else {
            format!("{sign}0.{}{}", "0".repeat((-point) as usize), digits)
        };
    }
    let mut out = format!("{sign}{}", &digits[..1]);
    if n > 1 {
        out.push('.');
        out.push_str(&digits[1..]);
    }
    out.push('E');
    if adjusted >= 0 {
        out.push('+');
    }
    out.push_str(&adjusted.to_string());
    out
}

/// Python keeps the sign of "-0.00"; rust_decimal reads it as a plain zero.
pub fn signed_zero(d: Decimal, text: &str) -> Decimal {
    if d.is_zero() && text.starts_with('-') && !d.is_sign_negative() {
        -d
    } else {
        d
    }
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
    fn normalize_like_python() {
        for (a, b) in [("100", "1E+2"), ("12.50", "12.5"), ("0.00", "0"), ("1200.00", "1.2E+3"), ("0.0000001", "1E-7"), ("-5", "-5"), ("0.001", "0.001"), ("-12.340", "-12.34")] {
            assert_eq!(dec_normalize_str(&Decimal::from_str(a).unwrap()), b);
        }
    }

    #[test]
    fn arithmetic_like_python() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        for (a, b, q) in [("10", "3", "3.333333333333333333333333333"), ("1.00", "2", "0.50"), ("100", "4", "25"), ("1", "3", "0.3333333333333333333333333333"), ("12.50", "12.50", "1"), ("-7", "2", "-3.5"), ("2", "3", "0.6666666666666666666666666667")] {
            assert_eq!(dec_str(&py_div(&d(a), &d(b)).unwrap()), q, "{a}/{b}");
        }
        assert_eq!(dec_str(&py_mul(&d("1.5"), &d("2.25"))), "3.375");
        let share = PyDec::from(&d("12.34")).div(&PyDec::from(&d("311616.21"))).unwrap().mul(&PyDec::int(100));
        assert_eq!(share.py_str(), "0.003959999385141100329793498227");
        assert_eq!(PyDec::from(&d("1E+2")).py_str(), "100");
        assert_eq!(dec_str(&py_mul(&d("3.3333333333333333333333333333"), &d("3"))), "10.00000000000000000000000000");
    }

    #[test]
    fn json_like_python() {
        let v: Value = serde_json::from_str(r#"{"a": 1, "b": [1.0, 0.85, "é"], "c": null}"#).unwrap();
        assert_eq!(json_dumps(&v), r#"{"a": 1, "b": [1.0, 0.85, "\u00e9"], "c": null}"#);
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

