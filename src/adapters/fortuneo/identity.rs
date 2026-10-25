//! What every Fortuneo document says about the account and its holder (statement, fees recap...).

use crate::adapters::base::{ParsedStatement, W};
use crate::adapters::common::{find_iban, norm};
use crate::adapters::re::search;
use crate::py;

const ACCOUNT_RE: &str = r"\b(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\b";
const BIC_RE: &str = r"\bB\s?IC\s+([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b"; // the font splits "BIC" into "B" and "IC"
const HOLDER_RE: &str = r"(?i)titulaire\(s\)\s+du\s+compte\s*:?\s*(.+)$";
pub const STREET_WORDS: &[&str] =
    &["rue", "avenue", "av", "bd", "boulevard", "chemin", "route", "impasse", "allee", "place", "quai", "residence", "lieu", "cours", "passage", "square"];

pub const WRAP_WIDTH: usize = 30; // the bank cuts a name line at 30 characters, even in the middle of a word
const NOT_A_NAME: &[&str] = &["numero de compte", "iban", "periode", "date", "identification", "intitule", "bic", "adresse"];

/// The holder line of a joint account runs over several rows: a row cut at the width continues the word on the
/// next one, a shorter row ended where the name did.
pub fn join_wrapped_name(parts: &[String]) -> String {
    let mut out = String::new();
    for p in parts {
        let part = py::strip(p);
        if part.is_empty() {
            continue;
        }
        if out.is_empty() {
            out = part.to_string();
        } else {
            let sep = if py::char_len(&out) >= WRAP_WIDTH { "" } else { " " };
            out = format!("{out}{sep}{part}");
        }
    }
    out
}

/// Account number (RIB parts), IBAN, BIC and holder(s), wherever they are on the page.
pub fn read_identity(st: &mut ParsedStatement, texts: &[String]) {
    for (i, text) in texts.iter().enumerate() {
        if norm(text).contains("numero de compte") {
            if let Some(m) = search(ACCOUNT_RE, text) {
                st.bank_code = Some(m[1].to_string());
                st.branch_code = Some(m[2].to_string());
                st.account_number = Some(m[3].to_string());
                st.rib_key = Some(m[4].to_string());
            }
        }
        if text.contains("IBAN") {
            st.iban = find_iban(text).or(st.iban.take());
        }
        if let Some(m) = search(BIC_RE, text) {
            st.bic = Some(m[1].to_string());
        }
        if let Some(m) = search(HOLDER_RE, text) {
            let mut parts = vec![py::strip(&m[1]).to_string()];
            let mut j = i + 1;
            while py::char_len(parts.last().unwrap()) >= WRAP_WIDTH - 1
                && j < texts.len()
                && py::char_len(&texts[j]) <= 40
                && !NOT_A_NAME.iter().any(|p| norm(&texts[j]).starts_with(p))
            {
                parts.push(py::strip(&texts[j]).to_string()); // the holder line continues on the next row
                j += 1;
            }
            let joined = join_wrapped_name(&parts);
            if !joined.is_empty() {
                st.holder_name = Some(joined);
            }
            if parts.len() > 1 {
                // glued from rows cut at the width: probably right, but only the user can be sure of the spelling
                st.set_extra("holder_maybe_truncated", true);
            }
        }
    }
}

/// (name, address) of the block printed on the right, between two rows.
pub fn read_address_block(rows: &[Vec<W>], after: usize, before: usize, from_x: f64) -> (Option<String>, Option<String>) {
    let mut block = vec![];
    for row in rows.iter().take(before).skip(after + 1) {
        let text = py::strip(&row.iter().filter(|w| w.x0 >= from_x).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
        if !text.is_empty() {
            block.push(text);
        }
    }
    if block.is_empty() {
        return (None, None);
    }
    let default_start = if block.len() >= 2 { (block.len() - 2).max(1) } else { 1 };
    let start = block
        .iter()
        .enumerate()
        .position(|(i, t)| i > 0 && (t.chars().next().map(py::is_digit_char).unwrap_or(false) || py::split_ws(&norm(t)).iter().any(|w| STREET_WORDS.contains(w))))
        .unwrap_or(default_start);
    let s = start.min(block.len());
    let name = join_wrapped_name(&block[..s]);
    let addr = block[s..].join("\n");
    (if name.is_empty() { None } else { Some(name) }, if addr.is_empty() { None } else { Some(addr) })
}
