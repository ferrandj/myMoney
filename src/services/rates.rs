//! Reference interest rates of French regulated savings accounts.
//!
//! Rates are set by the government on 1 February and 1 August. Add a line to RATE_HISTORY when a new rate is
//! announced; the right one is picked by date. A rate typed on an account always wins over these references.

use chrono::NaiveDate;

use crate::services::public_rates;
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
    effective_rate_with(public_rates::current().as_ref(), crate::services::prices::now(), account_type, over, on)
}

/// `effective_rate` with the online reading given (the one of the day, when asked about today).
pub fn effective_rate_with(online: Option<&public_rates::State>, at: f64, account_type: &str, over: Option<Decimal>, on: Option<NaiveDate>) -> (Option<Decimal>, String) {
    if let Some(o) = over {
        return (Some(o), "saisi manuellement".into());
    }
    if on.is_none() {
        let key = floor_of(account_type).unwrap_or(account_type);
        if let Some((rate, read_at)) = online.and_then(|s| public_rates::online_rate(s, key, at)) {
            let day = chrono::DateTime::from_timestamp(read_at as i64, 0).map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y").to_string()).unwrap_or_default();
            let suffix = if floor_of(account_type).is_some() { " (minimum légal)" } else { "" };
            return (Some(rate), format!("taux officiel lu le {day} sur {}{suffix}", public_rates::SOURCE));
        }
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

/// Products that pay a rate nobody publishes (a bank's own savings account, a PEL which depends on its opening date, a
/// term deposit, a life insurance fund, a crowdlending line): the application asks for it.
pub const ASK_RATE_TYPES: &[&str] = &["savings", "pel", "term_deposit", "life_insurance", "crowdfunding"];

pub fn needs_rate(account_type: &str, typed: Option<Decimal>) -> bool {
    typed.is_none() && ASK_RATE_TYPES.contains(&account_type)
}

// ------------------------------------------------------------------------------------------------ gross and net

/// Built-in flat tax of 2026: 12,8 % of income tax and 18,6 % of social levies. Used until the tax office's page has
/// been read, unless a value was typed in the settings.
pub const DEFAULT_IR: &str = "12.8";
pub const DEFAULT_PS: &str = "18.6";

/// The flat tax (PFU) in force: income tax, social levies, and where the figures come from.
#[derive(Clone, Debug, PartialEq)]
pub struct FlatTax {
    pub ir: Decimal,
    pub social: Decimal,
    pub source: String,
}

impl FlatTax {
    pub fn total(&self) -> Decimal {
        self.ir + self.social
    }
}

/// A total typed in the settings wins; else what the tax office's page says (both parts, read recently); else the
/// built-in figures.
pub fn flat_tax(typed_total: Option<Decimal>, online: Option<&public_rates::State>, at: f64) -> FlatTax {
    let read = |key: &str| online.and_then(|s| public_rates::online_rate(s, key, at));
    let (ir, ps) = (read("pfu_ir"), read("pfu_ps"));
    let ir_value = ir.map(|(r, _)| r).unwrap_or_else(|| d(DEFAULT_IR));
    if let Some(total) = typed_total {
        return FlatTax { ir: ir_value.min(total), social: (total - ir_value).max(Decimal::ZERO), source: "saisi dans les paramètres".into() };
    }
    match (ir, ps) {
        (Some((ir, read_at)), Some((ps, _))) => {
            let day = chrono::DateTime::from_timestamp(read_at as i64, 0).map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y").to_string()).unwrap_or_default();
            FlatTax { ir, social: ps, source: format!("lu le {day} sur impots.gouv.fr") }
        }
        _ => FlatTax { ir: d(DEFAULT_IR), social: d(DEFAULT_PS), source: "valeur intégrée (2026), à confirmer dans les paramètres".into() },
    }
}

/// Products whose interest is not taxed at all: the rate is the same gross and net.
pub const TAX_FREE_TYPES: &[&str] = &["livret_a", "ldds", "lep", "livret_jeune"];

/// The part of the interest the taxes take: nothing for the regulated livrets, the social levies only for the CEL
/// (exempt of income tax), the whole flat tax for the rest.
pub fn tax_share(account_type: &str, flat: &FlatTax) -> Decimal {
    let pct = if TAX_FREE_TYPES.contains(&account_type) {
        Decimal::ZERO
    } else if account_type == "cel" {
        flat.social
    } else {
        flat.total()
    };
    pct / Decimal::from(100)
}

/// (gross, net) of a rate given on `basis` ("gross" unless "net"), after `share` of the interest goes to taxes.
pub fn gross_and_net(rate: Decimal, basis: Option<&str>, share: Decimal) -> (Decimal, Decimal) {
    let keep = Decimal::ONE - share;
    if keep <= Decimal::ZERO {
        return (rate, rate);
    }
    if basis == Some("net") {
        ((rate / keep).round_dp(3), rate)
    } else {
        (rate, (rate * keep).round_dp(3))
    }
}

pub fn valid_basis(value: &str) -> Option<&'static str> {
    match value {
        "gross" | "brut" => Some("gross"),
        "net" => Some("net"),
        _ => None,
    }
}
