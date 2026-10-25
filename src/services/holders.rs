//! The people behind an account: civility, last name(s), birth name(s) and first name(s), one block per person (a
//! joint account has several). Documents ask for these in different ways (one last name or both, the birth name, the
//! first names), so each piece is kept apart; `compose` writes the single line the rest of the application matches
//! transfers with (`Account.holder_name`).

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::adapters::re::re;
use crate::models::Account;
use crate::py;

/// What the forms offer in the civility list.
pub const CIVILITIES: [&str; 3] = ["M", "MME", "MLLE"];
/// Boxes shown per person (more are kept if the person already has more).
pub const LAST_SLOTS: usize = 2;
pub const BIRTH_SLOTS: usize = 2;
pub const FIRST_SLOTS: usize = 3;
/// The most people a form can carry.
pub const MAX_PEOPLE: usize = 6;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Person {
    /// The card (`people` table) this person is linked to; None for names only typed on an account.
    pub id: Option<i64>,
    pub civility: String,
    /// Last name(s) as used: some documents want one, some both.
    pub last_names: Vec<String>,
    /// Birth name(s), when different.
    pub birth_names: Vec<String>,
    pub first_names: Vec<String>,
}

impl Person {
    pub fn is_empty(&self) -> bool {
        self.last_names.is_empty() && self.birth_names.is_empty() && self.first_names.is_empty()
    }
}

fn clean(text: &str) -> String {
    py::split_ws(text).join(" ")
}

fn civility_of(token: &str) -> Option<&'static str> {
    match py::upper(token.trim_end_matches('.')).as_str() {
        "M" | "MR" | "MONSIEUR" => Some("M"),
        "MME" | "MADAME" => Some("MME"),
        "MLLE" | "MADEMOISELLE" => Some("MLLE"),
        _ => None,
    }
}

fn shouted(token: &str) -> bool {
    let letters: Vec<char> = token.chars().filter(|c| c.is_alphabetic()).collect();
    !letters.is_empty() && letters.iter().all(|c| c.is_uppercase())
}

/// A name as a bank prints it ("M DUPONT JEAN", "MME LEROY CLAIRE", "M DUPONT OU MME LEROY CLAIRE", "Jean DUPONT") read
/// as people. It is a guess meant to prefill the form: banks print the last name first, or the first name first in
/// mixed case; the user corrects whatever is wrong.
pub fn parse_printed(line: &str) -> Vec<Person> {
    let line = clean(line);
    if line.is_empty() {
        return vec![];
    }
    let mut people = vec![];
    for part in re(r"(?i)\s+(?:OU|ET)\s+").split(&line) {
        let mut tokens: Vec<&str> = part.split_whitespace().collect();
        let mut person = Person::default();
        if let Some(c) = tokens.first().and_then(|t| civility_of(t)) {
            person.civility = c.to_string();
            tokens.remove(0);
        }
        match tokens.len() {
            0 => {}
            1 => person.last_names.push(tokens[0].to_string()),
            n => {
                let (first, last) = if !shouted(tokens[0]) && shouted(tokens[n - 1]) {
                    // "Jean DUPONT": first name first
                    (tokens[..1].join(" "), tokens[1..].join(" "))
                } else {
                    // "DUPONT JEAN": last name first
                    (tokens[n - 1].to_string(), tokens[..n - 1].join(" "))
                };
                person.last_names.push(last);
                person.first_names.push(first);
            }
        }
        if !person.is_empty() {
            people.push(person);
        }
    }
    people
}

/// A printed name that cannot be trusted as read: several people (OU, ET, a second civility) or more
/// words than a last name and a first name. Such a line is shown for confirmation before an account is created.
pub fn is_unclear(line: &str) -> bool {
    let line = clean(line);
    if line.is_empty() {
        return false;
    }
    let parts: Vec<&str> = re(r"(?i)\s+(?:OU|ET)\s+").split(&line).collect();
    if parts.len() > 1 {
        return true;
    }
    let mut tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.first().and_then(|t| civility_of(t)).is_some() {
        tokens.remove(0);
    }
    tokens.len() > 2 || tokens.iter().any(|t| civility_of(t).is_some())
}

/// "M DUPONT JEAN OU MME LEROY CLAIRE": civility, last names, first names; people joined by OU.
pub fn compose(people: &[Person]) -> String {
    people
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut words: Vec<&str> = vec![];
            if !p.civility.is_empty() {
                words.push(&p.civility);
            }
            words.extend(p.last_names.iter().map(|s| s.as_str()));
            words.extend(p.first_names.iter().map(|s| s.as_str()));
            words.join(" ")
        })
        .collect::<Vec<_>>()
        .join(" OU ")
}

pub fn to_json(people: &[Person]) -> Value {
    Value::Array(
        people
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| {
                let mut v = json!({"civility": p.civility, "last_names": p.last_names, "birth_names": p.birth_names, "first_names": p.first_names});
                if let Some(id) = p.id {
                    v["id"] = json!(id);
                }
                v
            })
            .collect(),
    )
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(clean).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

pub fn from_json(v: &Value) -> Vec<Person> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| Person {
                    id: p.get("id").and_then(|i| i.as_i64()),
                    civility: p.get("civility").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    last_names: strings(p.get("last_names")),
                    birth_names: strings(p.get("birth_names")),
                    first_names: strings(p.get("first_names")),
                })
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The people of an account: the ones that were entered, else a reading of the printed holder line.
pub fn of_account(account: &Account) -> Vec<Person> {
    let stored = account.holders.as_ref().map(from_json).unwrap_or_default();
    if !stored.is_empty() {
        return stored;
    }
    parse_printed(account.holder_name.as_deref().unwrap_or(""))
}

/// The people typed in a form (`holder_0_civility`, `holder_0_last_0`, `holder_0_birth_0`, `holder_0_first_0`...).
/// None when the form carries no such field at all (an older form that only has `holder_name`).
pub fn from_form(form: &HashMap<String, String>) -> Option<Vec<Person>> {
    if !form.keys().any(|k| k.starts_with("holder_") && (k.ends_with("_civility") || k.contains("_last_") || k.contains("_first_"))) {
        return None;
    }
    let mut people = vec![];
    for i in 0..MAX_PEOPLE {
        let boxes = |kind: &str| -> Vec<String> {
            let mut out = vec![];
            for j in 0..12 {
                if let Some(v) = form.get(&format!("holder_{i}_{kind}_{j}")) {
                    let v = clean(v);
                    if !v.is_empty() {
                        out.push(v);
                    }
                }
            }
            out
        };
        let civility = civility_of(form.get(&format!("holder_{i}_civility")).map(|s| s.as_str()).unwrap_or("")).unwrap_or("").to_string();
        let id = form.get(&format!("holder_{i}_person")).and_then(|v| v.trim().parse::<i64>().ok());
        let person = Person { id, civility, last_names: boxes("last"), birth_names: boxes("birth"), first_names: boxes("first") };
        if !person.is_empty() || person.id.is_some() {
            people.push(person);
        }
    }
    Some(people)
}

fn padded(values: &[String], slots: usize) -> Value {
    let n = slots.max(values.len());
    Value::Array((0..n).map(|i| Value::String(values.get(i).cloned().unwrap_or_default())).collect())
}

/// What a form needs to draw its boxes: one entry per person to show, each with its boxes padded to the usual number.
/// `minimum` people are always shown (a joint account asks for two).
pub fn form_blocks(people: &[Person], minimum: usize) -> Value {
    let count = people.len().max(minimum).max(1);
    Value::Array(
        (0..count)
            .map(|i| {
                let p = people.get(i).cloned().unwrap_or_default();
                json!({
                    "index": i,
                    "person": p.id.map(|i| i.to_string()).unwrap_or_default(),
                    "civility": p.civility,
                    "last": padded(&p.last_names, LAST_SLOTS),
                    "birth": padded(&p.birth_names, BIRTH_SLOTS),
                    "first": padded(&p.first_names, FIRST_SLOTS),
                })
            })
            .collect(),
    )
}
