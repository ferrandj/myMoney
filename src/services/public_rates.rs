//! The rates of the regulated savings accounts, read from the official pages of service-public.gouv.fr. The application
//! asks once at start-up and then every 24 hours; only these public pages are requested, nothing about the user is
//! sent. What was read is kept in `public_rates.json` (in the data folder, readable before the vault is unlocked) so
//! that a restart without network still knows it. When the pages cannot be read, the table of `rates.rs` applies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use regex::Regex;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;

use crate::services::prices::now;

/// (key, page, pattern with the rate as its first group, highest plausible value). The official file of each regulated
/// product states its rate in one sentence; the tax office's page states the flat tax (12,8 % of income tax, and the
/// social levies, which rose to 18,6 % in 2026).
pub const PAGES: &[(&str, &str, &str, u32)] = &[
    ("livret_a", "https://www.service-public.gouv.fr/particuliers/vosdroits/F2365", r"(?i)taux d.int.r.t annuel du livret A est de (\d{1,2}(?:[,.]\d{1,3})?) ?%", 10),
    ("ldds", "https://www.service-public.gouv.fr/particuliers/vosdroits/F2368", r"(?i)taux de r.mun.ration du LDDS.{0,80}?est de (\d{1,2}(?:[,.]\d{1,3})?) ?%", 10),
    ("lep", "https://www.service-public.gouv.fr/particuliers/vosdroits/F2367", r"(?i)taux d.int.r.t du LEP est de (\d{1,2}(?:[,.]\d{1,3})?) ?%", 10),
    ("cel", "https://www.service-public.gouv.fr/particuliers/vosdroits/F16136", r"(?i)taux d.int.r.t du CEL(?:, hors prime d.[ée]tat,)? est de (\d{1,2}(?:[,.]\d{1,3})?) ?%", 10),
    ("pfu_ir", "https://www.impots.gouv.fr/particulier/les-revenus-mobiliers", r"(?i)pr.l.vement forfai\w* unique \(PFU\) de (\d{1,2}(?:[,.]\d{1,2})?) ?%", 40),
    (
        "pfu_ps",
        "https://www.impots.gouv.fr/particulier/les-revenus-mobiliers",
        r"(?i)(?:taux des pr.l.vements sociaux pass\w* . |pr.l.vements sociaux (?:est|sont) (?:de |fix\w+ . ))(\d{1,2}(?:[,.]\d{1,2})?) ?%",
        40,
    ),
];

pub const SOURCE: &str = "service-public.gouv.fr";
/// Asked again after this long when it worked, and after this long when it did not.
pub const REFRESH_SECONDS: f64 = 24.0 * 3600.0;
pub const RETRY_SECONDS: f64 = 3600.0;
/// An old reading is not trusted beyond this: the table of `rates.rs` takes over.
pub const MAX_AGE_SECONDS: f64 = 45.0 * 24.0 * 3600.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub rates: BTreeMap<String, Decimal>,
    /// when the rates were last read, and when the pages were last asked (unix seconds)
    pub read_at: f64,
    pub checked_at: f64,
    pub error: Option<String>,
}

static CURRENT: RwLock<Option<State>> = RwLock::new(None);

pub fn file(data_dir: &Path) -> PathBuf {
    data_dir.join("public_rates.json")
}

fn to_json(s: &State) -> Value {
    json!({
        "rates": s.rates.iter().map(|(k, v)| (k.clone(), Value::String(v.to_string()))).collect::<serde_json::Map<_, _>>(),
        "read_at": s.read_at, "checked_at": s.checked_at, "error": s.error, "source": SOURCE,
    })
}

fn from_json(v: &Value) -> State {
    State {
        rates: v.get("rates").and_then(|r| r.as_object()).map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), Decimal::from_str(v.as_str()?).ok()?))).collect()).unwrap_or_default(),
        read_at: v.get("read_at").and_then(|x| x.as_f64()).unwrap_or(0.0),
        checked_at: v.get("checked_at").and_then(|x| x.as_f64()).unwrap_or(0.0),
        error: v.get("error").and_then(|x| x.as_str()).map(str::to_string),
    }
}

pub fn load(data_dir: &Path) -> State {
    std::fs::read_to_string(file(data_dir)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| from_json(&v)).unwrap_or_default()
}

fn save(data_dir: &Path, s: &State) {
    let path = file(data_dir);
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(&to_json(s)).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The page as plain text: no scripts, no tags, one space between words.
pub fn plain_text(html: &str) -> String {
    let no_code = Regex::new(r"(?is)<(script|style)[^>]*>.*?</(script|style)>").unwrap().replace_all(html, " ");
    let no_tags = Regex::new(r"(?s)<[^>]+>").unwrap().replace_all(&no_code, " ");
    let text = no_tags.replace("&nbsp;", " ").replace("&#39;", "'").replace("&#x27;", "'").replace("&rsquo;", "'").replace("&amp;", "&").replace('\u{a0}', " ").replace('\u{202f}', " ").replace('\u{2019}', "'");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The rate (in %) a page states with `pattern` (the first group that matched), if it is a plausible one.
pub fn parse_rate(html: &str, pattern: &str, ceiling: u32) -> Option<Decimal> {
    let text = plain_text(html);
    let caps = Regex::new(pattern).ok()?.captures(&text)?;
    let found = caps.iter().skip(1).flatten().next()?;
    let rate = Decimal::from_str(&found.as_str().replace(',', ".")).ok()?;
    (rate > Decimal::ZERO && rate <= Decimal::from(ceiling)).then_some(rate)
}

/// Read every page (each address once); what could not be read keeps its previous value. Err when nothing at all could
/// be read.
pub fn fetch_all(get: &dyn Fn(&str) -> Result<String, String>, previous: &BTreeMap<String, Decimal>) -> Result<BTreeMap<String, Decimal>, String> {
    let mut rates = previous.clone();
    let mut read = 0;
    let mut problems = vec![];
    let mut pages: BTreeMap<&str, Result<String, String>> = BTreeMap::new();
    for (kind, url, pattern, ceiling) in PAGES {
        let html = pages.entry(url).or_insert_with(|| get(url));
        match html {
            Ok(html) => match parse_rate(html, pattern, *ceiling) {
                Some(rate) => {
                    rates.insert(kind.to_string(), rate);
                    read += 1;
                }
                None => problems.push(format!("{kind} : taux introuvable sur la page")),
            },
            Err(e) => problems.push(format!("{kind} : {e}")),
        }
    }
    if read == 0 {
        return Err(problems.join(" ; "));
    }
    Ok(rates)
}

pub fn http_get(url: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120 Safari/537.36 mymoney").try_proxy_from_env(true).build();
    agent.get(url).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())
}

/// Ask the pages if it is time (or `force`) and keep the answer in the file. Returns the state after.
pub fn refresh_state(data_dir: &Path, force: bool, at: f64, get: &dyn Fn(&str) -> Result<String, String>) -> State {
    let mut state = load(data_dir);
    let due = force || at - state.checked_at >= if state.error.is_some() { RETRY_SECONDS } else { REFRESH_SECONDS };
    if due {
        match fetch_all(get, &state.rates) {
            Ok(rates) => state = State { rates, read_at: at, checked_at: at, error: None },
            Err(e) => {
                log::warn!("Taux réglementés : lecture impossible ({e}), la table intégrée s'applique");
                state.checked_at = at;
                state.error = Some(e);
            }
        }
        save(data_dir, &state);
    }
    state
}

/// `refresh_state`, and what the application uses from now on.
pub fn refresh(data_dir: &Path, force: bool, at: f64, get: &dyn Fn(&str) -> Result<String, String>) -> State {
    let state = refresh_state(data_dir, force, at, get);
    *CURRENT.write().unwrap() = Some(state.clone());
    state
}

pub fn current() -> Option<State> {
    CURRENT.read().unwrap().clone()
}

/// The rate read online for a product, if it was read recently enough. Returns (rate %, when it was read).
pub fn online_rate(state: &State, kind: &str, at: f64) -> Option<(Decimal, f64)> {
    if state.read_at <= 0.0 || at - state.read_at > MAX_AGE_SECONDS {
        return None;
    }
    state.rates.get(kind).map(|r| (*r, state.read_at))
}

/// Start-up: read at once, then every 24 hours (an hour after a failure), in the background.
pub fn start(data_dir: PathBuf, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    *CURRENT.write().unwrap() = Some(load(&data_dir)); // what the last run read, while the pages are being asked
    std::thread::Builder::new()
        .name("public-rates".into())
        .spawn(move || {
            refresh(&data_dir, true, now(), &http_get);
            loop {
                for _ in 0..600 {
                    std::thread::sleep(Duration::from_secs(1));
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                }
                refresh(&data_dir, false, now(), &http_get);
            }
        })
        .ok();
}
