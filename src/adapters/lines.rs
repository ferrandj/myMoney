//! Helpers shared by the adapters that turn the lines of a statement into labelled operations.

use super::common::{iban_matches_rib, iban_valid, name_key, norm, rib_valid};
use super::re::search;
use super::ParsedStatement;
use crate::py;

/// Words that show a transfer goes to / comes from one of the holder's own products rather than a person.
const OWN_PRODUCT_WORDS: &[&str] = &["livret", "epargne", "pel", "pea", "ldds", "ldd", "lep", "cel", "compte", "assurance"];
/// A label line at least this long was probably wrapped by the bank, even in the middle of a word.
pub const WRAP_WIDTH: usize = 32;

/// The line that says what the operation is for: not an identifier (long digit runs), and made of real words.
pub fn describe(lines: &[String]) -> Option<String> {
    for line in lines {
        let words = py::split_ws(line);
        if !words.iter().any(|w| search(r"\d{6,}", w).is_some()) && words.iter().any(|w| py::char_len(w) >= 3 && py::isalpha(w)) {
            return Some(line.clone());
        }
    }
    lines.first().cloned()
}

/// `str.islower()`: at least one cased character, and no upper-case one.
pub fn islower(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        if c.is_uppercase() || is_titlecase(c) {
            return false;
        }
        if c.is_lowercase() {
            cased = true;
        }
    }
    cased
}

/// `str.isupper()`.
pub fn isupper(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        if c.is_lowercase() || is_titlecase(c) {
            return false;
        }
        if c.is_uppercase() {
            cased = true;
        }
    }
    cased
}

fn is_titlecase(c: char) -> bool {
    unicode_general_category::get_general_category(c) == unicode_general_category::GeneralCategory::TitlecaseLetter
}

/// Join detail lines; a short lowercase fragment under a full-width line is the end of a word the bank cut.
pub fn join_wrapped(lines: &[String]) -> String {
    let mut text = String::new();
    let mut previous = String::new();
    for raw in lines {
        let line = py::strip(raw);
        if line.is_empty() {
            continue;
        }
        let glued = !text.is_empty() && py::char_len(&previous) >= WRAP_WIDTH && !line.contains(' ') && islower(line) && py::char_len(line) <= 4;
        text = if glued { format!("{text}{line}") } else { py::strip(&format!("{text} {line}")).to_string() };
        previous = line.to_string();
    }
    text
}

/// `" — ".join(parts)[:300]` of the non-empty parts.
pub fn label(primary: Option<&str>, secondary: Option<&str>) -> String {
    let parts: Vec<&str> = [primary, secondary].into_iter().flatten().map(py::strip).filter(|p| !p.is_empty()).collect();
    py::take_chars(&parts.join(" — "), 300)
}

/// The other party is the holder (same name, whatever the order) or one of the holder's own products.
pub fn is_own(name: Option<&str>, holder_key: Option<&str>) -> bool {
    let Some(name) = name.filter(|n| !n.is_empty()) else { return false };
    if let Some(h) = holder_key {
        if !h.is_empty() && name_key(name) == h {
            return true;
        }
    }
    let n = norm(name);
    py::split_ws(&n).iter().any(|w| OWN_PRODUCT_WORDS.contains(w))
}

/// Checks on what identifies the account: the IBAN, the RIB key, and the IBAN against the RIB.
pub fn identity_warnings(st: &ParsedStatement) -> Vec<String> {
    let mut w = vec![];
    if let Some(iban) = st.iban.as_deref().filter(|s| !s.is_empty()) {
        if !iban_valid(iban) {
            w.push("L'IBAN lu n'est pas valide (clé de contrôle)".to_string());
        }
    }
    if let (Some(b), Some(g), Some(a), Some(k)) = (&st.bank_code, &st.branch_code, &st.account_number, &st.rib_key) {
        if !b.is_empty() && !g.is_empty() && !a.is_empty() && !k.is_empty() {
            if !rib_valid(b, g, a, k) {
                w.push("La clé RIB lue ne correspond pas au numéro de compte".to_string());
            }
            if let Some(iban) = st.iban.as_deref().filter(|s| !s.is_empty()) {
                if !iban_matches_rib(iban, b, g, a, k) {
                    w.push("L'IBAN ne correspond pas au RIB du document".to_string());
                }
            }
        }
    }
    w
}
