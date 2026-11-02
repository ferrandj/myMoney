//! What was the money spent on? A category for every operation.
//!
//! The guess comes, in this order, from:
//!  1. the user's own rule for that merchant ("Carrefour is Courses"), kept per user;
//!  2. what the bank says the operation is (a loan instalment, a fee, a cash withdrawal, interest);
//!  3. well-known merchants and words in the label (supermarkets, rail, energy, telecom...);
//!  4. the kind of operation: a transfer to a person, an unknown direct debit...
//! and falls back to "Autres". Nothing is sent anywhere: it is plain keyword matching on the operation's own text.

use std::collections::HashMap;
use std::sync::OnceLock;

use fancy_regex::Regex;
use rusqlite::Connection;
use rust_decimal::Decimal;

use crate::adapters::common::{name_key, norm};
use crate::error::{value_err, AppResult};
use crate::models::{CategoryRule, Counterparty, Transaction};
use crate::py;
use crate::services::access;

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    pub id: &'static str,
    pub label: &'static str,
    /// out | in | both
    pub direction: &'static str,
    pub keywords: &'static [&'static str],
}

const fn cat(id: &'static str, label: &'static str, direction: &'static str, keywords: &'static [&'static str]) -> Category {
    Category { id, label, direction, keywords }
}

// Order matters: the first category whose words appear wins, so specific ones come first ("uber eats" before "uber").
// Words match whole words; a trailing * lets a word start something longer ("pizz*" is pizza, pizzeria).
pub static OUT: &[Category] = &[
    cat("logement", "Logement", "out", &["loyer", "syndic", "foncia", "nexity", "charges copro", "copropriete", "agence immobiliere", "taxe fonciere", "taxe habitation"]),
    cat("impots", "Impôts et taxes", "out", &["dgfip", "direction generale des finance*", "impot", "impots", "tresor public", "urssaf", "amende", "antai", "taxe"]),
    cat("essence", "Essence et carburant", "out", &["total access", "station*", "esso", "shell", "bp", "avia", "carburant*", "essence", "gazole"]),
    cat("energie", "Énergie et eau", "out", &["edf", "engie", "totalenergies", "eaux du", "veolia", "suez", "saur", "electricite", "gaz de", "enedis", "eau"]),
    cat("telecom", "Téléphone et internet", "out", &["orange", "sfr", "bouygues*", "free mobile", "freebox", "free telecom", "ovh*", "red by sfr", "sosh", "la poste mobile", "lebara"]),
    cat("jeux_video", "Jeux vidéo", "out", &["steam*", "nintendo", "playstation*", "psn", "xbox", "epic games", "ubisoft", "blizzard", "battle.net", "riot games", "gog", "humble bundle", "electronic arts", "micromania", "twitch"]),
    cat("abonnements", "Abonnements", "out", &["netflix", "spotify", "disney*", "prime video", "amazon prime", "canal+", "canal plus", "deezer", "youtube*", "icloud", "apple.com/bill", "adobe", "dropbox", "openai", "chatgpt", "microsoft 365", "patreon", "notion"]),
    cat("epargne", "Épargne et investissement", "out", &["versement periodique", "livret*", "assurance vie", "assurance-vie", "pea", "coinbase", "binance", "bitstack", "kraken", "crypto*", "bitpanda", "trade republic", "degiro", "interactive brokers", "yomoni", "linxea", "ramify"]),
    cat("assurances", "Assurances", "out", &["assurance*", "maif", "macif", "axa", "matmut", "groupama", "allianz", "april", "maaf", "gmf", "lemonade", "hiscox"]),
    cat("sante", "Santé", "out", &["pharmacie*", "medecin*", "docteur", "dentiste", "doctolib", "hopital", "clinique", "optique", "opticien*", "laboratoire*", "kine*", "osteo*", "mutuelle", "harmonie mutuelle", "cpam"]),
    cat("restaurants", "Restaurants et sorties", "out", &["uber eats", "ubereats", "deliveroo", "just eat", "restaurant*", "resto*", "mcdo", "mcdonald*", "burger king", "kfc", "quick", "subway", "brasserie*", "pizz*", "sushi*", "kebab*", "boulangerie*", "patisserie*", "cafe", "starbucks", "bar", "pub", "traiteur", "creperie*"]),
    cat("courses", "Alimentation", "out", &["carrefour*", "leclerc", "auchan", "lidl", "intermarche", "monoprix", "franprix", "casino", "aldi", "picard", "biocoop", "super u", "hyper u", "u express", "netto", "cora", "grand frais", "naturalia", "supermarche*", "epicerie*", "primeur*", "boucherie*", "marche", "drive"]),
    cat("voyages", "Voyages et vacances", "out", &["airbnb", "booking*", "ryanair", "easyjet", "air france", "transavia", "expedia", "hotel*", "hostel*", "abritel", "vacances", "camping*", "eurostar", "lufthansa", "volotea", "trainline", "ouigo"]),
    cat("transport_commun", "Transports en commun", "out", &["sncf*", "trains de france", "ratp", "navigo", "transilien", "tcl", "tisseo", "keolis", "transdev", "ile de france mobilites", "idfm", "bus", "metro", "tramway", "tram", "rer"]),
    cat("garage", "Garage et entretien auto", "out", &["garage*", "norauto", "midas", "feu vert", "speedy", "euromaster", "point s", "controle technique", "dekra", "vulco", "carrosserie*", "pneu*", "auto bilan"]),
    cat("transport", "Autres transports", "out", &["uber", "bolt", "heetch", "blablacar", "peage*", "vinci autoroutes", "sanef", "aprr", "parking*", "indigo", "velib", "lime", "dott"]),
    cat("shopping", "Achats et maison", "out", &["amazon", "fnac", "darty", "boulanger", "decathlon", "zara", "h&m", "cdiscount", "aliexpress", "zalando", "vinted", "ikea", "leroy merlin", "castorama", "bricorama", "brico*", "action", "kiabi", "uniqlo", "shein", "temu", "cultura", "gifi", "maisons du monde", "conforama"]),
    cat("loisirs", "Loisirs et culture", "out", &["cinema*", "ugc", "pathe", "gaumont", "ticketmaster", "google play", "spectacle*", "musee*", "concert*", "theatre*", "librairie*", "gym", "basic fit", "fitness", "piscine*", "sport*"]),
];

pub static INCOME: &[Category] = &[
    cat("salaire", "Salaire et revenus", "in", &["salaire*", "paie", "remuneration*", "traitement", "indemnite*", "prime", "bulletin", "employeur"]),
    cat("aides", "Aides et allocations", "in", &["caf", "france travail", "pole emploi", "cpam", "ameli", "allocation*", "msa", "apl", "prime d'activite", "cnav", "retraite*"]),
    cat("remboursements", "Remboursements", "in", &["remboursement*", "rbt", "avoir", "refund", "annulation", "mutuelle"]),
];

pub static FIXED: &[Category] = &[
    cat("credit", "Remboursement d'emprunt", "out", &[]),
    cat("frais", "Frais bancaires", "out", &[]),
    cat("retraits", "Retraits d'espèces", "out", &[]),
    cat("proches", "Virements à des personnes", "out", &[]),
    cat("prelevements", "Autres prélèvements", "out", &[]),
    cat("autres", "Autres dépenses", "out", &[]),
    cat("interets", "Intérêts", "in", &[]),
    cat("recus", "Virements reçus", "in", &[]),
    cat("autres_entrees", "Autres entrées", "in", &[]),
    cat("emprunt", "Emprunt (fonds reçus)", "in", &[]),
    cat("interne", "Transfert compte à compte", "both", &[]),
];

pub fn fixed(id: &str) -> &'static Category {
    FIXED.iter().find(|c| c.id == id).unwrap()
}

/// Every category by id: OUT, INCOME then FIXED, in that order.
pub fn all() -> impl Iterator<Item = &'static Category> {
    OUT.iter().chain(INCOME.iter()).chain(FIXED.iter())
}

pub fn get(id: &str) -> Option<&'static Category> {
    all().find(|c| c.id == id)
}

const WORD: &str = r"[\p{L}\p{N}_]";

fn pattern(words: &[&str]) -> Option<Regex> {
    let mut parts = vec![];
    for word in words {
        let prefix = word.ends_with('*');
        let w = py::strip(&norm(word.trim_end_matches('*'))).to_string();
        let first = w.chars().next().unwrap();
        let last = w.chars().next_back().unwrap();
        let before = if py::is_alnum_char(first) { format!("(?<!{WORD})") } else { String::new() };
        let after = if py::is_alnum_char(last) && !prefix { format!("(?!{WORD})") } else { String::new() };
        parts.push(format!("{before}{}{after}", fancy_regex::escape(&w)));
    }
    if parts.is_empty() {
        None
    } else {
        Some(Regex::new(&parts.join("|")).unwrap())
    }
}

fn patterns() -> &'static HashMap<&'static str, Option<Regex>> {
    static P: OnceLock<HashMap<&'static str, Option<Regex>>> = OnceLock::new();
    P.get_or_init(|| OUT.iter().chain(INCOME.iter()).map(|c| (c.id, pattern(c.keywords))).collect())
}

/// The categories one can choose for an operation going `direction` ("out" or "in").
pub fn options(direction: &str) -> Vec<&'static Category> {
    let mut out: Vec<&'static Category> = if direction == "out" {
        OUT.iter().chain(["credit", "frais", "retraits", "proches", "prelevements", "autres"].iter().map(|k| fixed(k))).collect()
    } else {
        INCOME.iter().chain(["interets", "recus", "emprunt", "autres_entrees"].iter().map(|k| fixed(k))).collect()
    };
    out.push(fixed("interne"));
    out
}

/// (rule key, label to show, contact id) of the other party of an operation. `contact` is the linked contact's name.
pub fn merchant(t: &Transaction, contact: Option<&str>) -> (String, String, Option<i64>) {
    if let (Some(name), Some(id)) = (contact, t.counterparty_id) {
        return (format!("c{id}"), name.to_string(), Some(id));
    }
    let raw = t.counterparty_name.clone().filter(|s| !s.is_empty()).or_else(|| Some(t.label.clone()).filter(|s| !s.is_empty())).unwrap_or_else(|| "?".into());
    let label = py::strip(&raw).to_string();
    let key = name_key(&label);
    (if key.is_empty() { py::lower(&label) } else { key }, label, None)
}

fn owner() -> Option<i64> {
    access::current().map(|me| me.id)
}

fn owner_clause() -> &'static str {
    if owner().is_some() {
        "owner_id = ?"
    } else {
        "owner_id IS NULL"
    }
}

pub fn load_rules(c: &Connection) -> AppResult<HashMap<String, String>> {
    let rules = match owner() {
        Some(o) => CategoryRule::query(c, "WHERE owner_id = ?", [o])?,
        None => CategoryRule::query(c, "WHERE owner_id IS NULL", [])?,
    };
    let mut rules: HashMap<String, String> = rules.into_iter().map(|r| (r.key, r.category)).collect();
    // a contact marked "Moi (autre compte)" is one of my own accounts: what goes to or comes from it is a transfer between
    // accounts, unless a rule of mine says otherwise
    for me in Counterparty::query(c, "WHERE kind = 'self'", [])? {
        rules.entry(format!("c{}", me.id)).or_insert_with(|| "interne".to_string());
    }
    Ok(rules)
}

/// Remember where a merchant goes (`category` "auto" forgets the rule).
pub fn set_rule(c: &Connection, key: &str, category: &str) -> AppResult<()> {
    if category != "auto" && get(category).is_none() {
        return value_err("Catégorie inconnue");
    }
    let tail = format!("WHERE {} AND \"key\" = ?", owner_clause());
    let row = match owner() {
        Some(o) => CategoryRule::query_one(c, &tail, rusqlite::params![o, key])?,
        None => CategoryRule::query_one(c, &tail, [key])?,
    };
    match (category, row) {
        ("auto", Some(r)) => CategoryRule::delete(c, r.id)?,
        ("auto", None) => {}
        (_, None) => {
            CategoryRule { owner_id: owner(), key: key.into(), category: category.into(), ..Default::default() }.insert(c)?;
        }
        (_, Some(mut r)) => {
            r.category = category.into();
            r.update(c)?;
        }
    }
    Ok(())
}

/// `categorize` for a whole batch: operations that read the same (the same merchant month after month) are decided
/// once.
pub struct Categorizer<'a> {
    rules: &'a HashMap<String, String>,
    seen: HashMap<(bool, Option<i64>, Option<String>, String, Option<String>, Option<String>, bool, Option<String>, Option<String>), &'static str>,
}

impl<'a> Categorizer<'a> {
    pub fn new(rules: &'a HashMap<String, String>) -> Self {
        Categorizer { rules, seen: HashMap::new() }
    }

    pub fn decide(&mut self, t: &Transaction, contact: Option<&str>) -> &'static str {
        let key = (
            t.internal(),
            t.counterparty_id,
            t.counterparty_name.clone(),
            t.label.clone(),
            t.memo.clone(),
            t.kind.clone(),
            t.amount >= Decimal::ZERO,
            contact.map(|s| s.to_string()),
            t.category.clone(),
        );
        if let Some(c) = self.seen.get(&key) {
            return c;
        }
        let c = categorize(t, contact, self.rules);
        self.seen.insert(key, c);
        c
    }
}

/// The id of the category of one operation.
pub fn categorize(t: &Transaction, contact: Option<&str>, rules: &HashMap<String, String>) -> &'static str {
    // 0. the category chosen for this very operation
    if let Some(own) = t.category.as_deref().and_then(get) {
        return own.id;
    }
    if t.internal() {
        return "interne";
    }
    let (key, _, _) = merchant(t, contact);
    if let Some(chosen) = rules.get(&key).and_then(|c| get(c)) {
        return chosen.id;
    }
    let income = t.amount >= Decimal::ZERO;
    let kind = t.kind.as_deref().unwrap_or("");
    if kind == "interest" {
        return "interets";
    }
    if income && kind == "loan_payment" {
        return "emprunt"; // funds paid out to the account by a loan
    }
    if !income {
        match kind {
            "loan_payment" => return "credit",
            "fee" => return "frais",
            "cash" => return "retraits",
            _ => {}
        }
    }
    let parts: Vec<&str> = [contact, t.counterparty_name.as_deref(), Some(t.label.as_str()), t.memo.as_deref()].into_iter().flatten().filter(|x| !x.is_empty()).collect();
    let text = format!(" {} ", norm(&parts.join(" ").replace('*', " ")));
    for category in if income { INCOME } else { OUT } {
        if let Some(Some(p)) = patterns().get(category.id) {
            if p.is_match(&text).unwrap_or(false) {
                return category.id;
            }
        }
    }
    let transfer = kind == "transfer" || kind == "instant_transfer";
    if income {
        return if transfer { "recus" } else { "autres_entrees" };
    }
    if transfer {
        return "proches";
    }
    if kind == "direct_debit" {
        return "prelevements";
    }
    "autres"
}
