//! Reference interest rates of French regulated savings accounts.
//!
//! Rates are set by the government on 1 February and 1 August. Add a line to RATE_HISTORY when a new rate is
//! announced; the right one is picked by date. A rate typed on an account always wins over these references.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::str::FromStr;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// (effective date, [(account type, % per year)])
pub fn rate_history() -> Vec<(NaiveDate, Vec<(&'static str, Decimal)>)> {
    let day = |y, m| NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    vec![
        (day(2023, 2), vec![("livret_a", d("3.0")), ("ldds", d("3.0")), ("lep", d("6.1")), ("cel", d("2.0"))]),
        (day(2023, 8), vec![("livret_a", d("3.0")), ("ldds", d("3.0")), ("lep", d("6.0")), ("cel", d("2.0"))]),
        (day(2024, 2), vec![("livret_a", d("3.0")), ("ldds", d("3.0")), ("lep", d("5.0")), ("cel", d("2.0"))]),
        (day(2025, 2), vec![("livret_a", d("2.4")), ("ldds", d("2.4")), ("lep", d("3.5")), ("cel", d("1.5"))]),
        (day(2025, 8), vec![("livret_a", d("1.7")), ("ldds", d("1.7")), ("lep", d("2.7")), ("cel", d("1.25"))]),
        (day(2026, 2), vec![("livret_a", d("1.5")), ("ldds", d("1.5")), ("lep", d("2.5")), ("cel", d("1.0"))]),
        (day(2026, 8), vec![("livret_a", d("1.7")), ("ldds", d("1.7")), ("lep", d("2.5")), ("cel", d("1.25"))]),
    ]
}

/// The Livret Jeune pays at least the Livret A rate (each bank may pay more).
fn floor_of(t: &str) -> Option<&'static str> {
    if t == "livret_jeune" {
        Some("livret_a")
    } else {
        None
    }
}

pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// (rate %, effective since) for a regulated account type, or None.
pub fn reference_rate(account_type: &str, on: Option<NaiveDate>) -> Option<(Decimal, NaiveDate)> {
    let on = on.unwrap_or_else(today);
    let key = floor_of(account_type).unwrap_or(account_type);
    let mut hist = rate_history();
    hist.sort_by(|a, b| b.0.cmp(&a.0));
    for (since, rates) in hist {
        if since <= on {
            if let Some((_, r)) = rates.iter().find(|(k, _)| *k == key) {
                return Some((*r, since));
            }
        }
    }
    None
}

/// The rate that applies to an account and where it comes from.
pub fn effective_rate(account_type: &str, over: Option<Decimal>, on: Option<NaiveDate>) -> (Option<Decimal>, String) {
    if let Some(o) = over {
        return (Some(o), "saisi manuellement".into());
    }
    match reference_rate(account_type, on) {
        None => (None, String::new()),
        Some((rate, since)) => {
            let suffix = if floor_of(account_type).is_some() { " (minimum légal)" } else { "" };
            (Some(rate), format!("taux réglementé depuis le {}{}", since.format("%d/%m/%Y"), suffix))
        }
    }
}

const KEYWORDS: &[(&str, &[&str])] = &[
    ("livret_a", &["livret a", "livret bleu"]),
    ("ldds", &["ldds", "ldd ", "developpement durable", "développement durable"]),
    ("lep", &["lep", "epargne populaire", "épargne populaire"]),
    ("livret_jeune", &["livret jeune"]),
    ("pel", &["pel", "plan epargne logement", "plan d'épargne logement", "plan epargne-logement"]),
    ("cel", &["cel", "compte epargne logement", "compte d'épargne logement"]),
    ("pea", &["pea"]),
    ("per", &["per ", "plan epargne retraite", "plan d'épargne retraite"]),
    ("life_insurance", &["assurance vie", "assurance-vie", "contrat vie"]),
    ("cto", &["compte titres", "compte-titres", "cto", "ord. titres"]),
    ("term_deposit", &["compte a terme", "compte à terme", "cat "]),
    ("mortgage", &["pret immo", "prêt immo", "credit immo", "crédit immo"]),
    ("loan", &["pret ", "prêt ", "credit ", "crédit "]),
    ("savings", &["livret", "epargne", "épargne", "compte sur livret", "csl"]),
    ("checking", &["compte courant", "compte de depot", "compte de dépôt", "compte cheque", "compte chèque"]),
];

/// Python's `\w` for one character.
pub fn is_word(c: char) -> bool {
    c == '_' || crate::py::is_alnum_char(c)
}

/// `re.search(r"(?<!\w)" + re.escape(word) + r"(?!\w)", text)`.
pub fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return true;
    }
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let at = start + pos;
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        if !before.map(is_word).unwrap_or(false) && !after.map(is_word).unwrap_or(false) {
            return true;
        }
        start = at + text[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    false
}

pub fn guess_account_type(label: &str, default: &str) -> String {
    let text = label.to_lowercase();
    for (code, words) in KEYWORDS {
        for w in *words {
            if contains_word(&text, crate::py::strip(w)) {
                return code.to_string();
            }
        }
    }
    default.to_string()
}
