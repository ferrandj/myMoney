//! How a document is called once it is imported: ETABLISSEMENT_TYPECOMPTE_TYPEFICHIER_NOM_PRENOM_DATE.ext, upper case,
//! no accent, words joined by "-" (the date too: 2026-09-30). When the name would not fit in a file name, the first
//! names go first, then the names; the rest always fits. The name the file had when it was dropped is kept apart.

use chrono::NaiveDate;
use rusqlite::Connection;

use crate::adapters::base::ParsedStatement;
use crate::adapters::common::strip_accents;
use crate::error::AppResult;
use crate::models::{Account, Document, PersonCard};
use crate::py;
use crate::services::holders::{self, Person};
use crate::services::people;

/// The longest file name the usual file systems take, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

pub const DOC_KINDS: &[(&str, &str)] = &[
    ("statement", "Relevé de compte"),
    ("savings_statement", "Relevé d'épargne"),
    ("fees_summary", "Récapitulatif des frais"),
    ("info", "Information"),
    ("loan_offer", "Offre de prêt"),
    ("loan_event", "Remboursement anticipé"),
    ("loan_schedule", "Tableau d'amortissement"),
    ("pdf", "Relevé (lecture générique)"),
    ("csv", "Relevé (CSV)"),
    ("ofx", "Relevé (OFX)"),
    ("qif", "Relevé (QIF)"),
];

fn parse_statements(d: &Document) -> Vec<serde_json::Value> {
    d.parse_result.as_ref().and_then(|r| r.get("statements")).and_then(|s| s.as_array()).cloned().unwrap_or_default()
}

/// The line naming the holder / addressee / borrower in the reading of a document ("M DUPONT JEAN"), if it has one.
pub fn reading_holder(d: &Document) -> String {
    parse_statements(d).iter().filter_map(|st| st.get("holder_name").and_then(|h| h.as_str())).map(|h| h.trim().to_string()).find(|h| !h.is_empty()).unwrap_or_default()
}

/// (institution, type of document) as the library shows them.
pub fn document_group(d: &Document) -> (String, String) {
    let institution = d
        .parse_result
        .as_ref()
        .and_then(|r| r.get("institution"))
        .filter(|v| crate::models::truthy(v))
        .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))
        .unwrap_or_else(|| "Autres documents".into());
    let category = parse_statements(d)
        .iter()
        .find(|st| st.get("extra").map(crate::models::truthy).unwrap_or(false))
        .and_then(|st| st.get("extra").and_then(|e| e.get("category")).cloned())
        .filter(crate::models::truthy);
    let kind = if d.adapter_id.as_deref().map(|a| !a.is_empty()).unwrap_or(false) {
        match category {
            Some(c) => c.as_str().map(|s| s.to_string()).unwrap_or_else(|| c.to_string()),
            None => DOC_KINDS.iter().find(|(k, _)| *k == d.kind).map(|(_, l)| l.to_string()).unwrap_or_else(|| "Document".into()),
        }
    } else {
        "Non reconnu".into()
    };
    (institution, kind)
}

/// The day a document is about: the end of its period, else its balance date, else the day it was dropped.
pub fn document_day(d: &Document) -> NaiveDate {
    for st in parse_statements(d) {
        for key in ["period_end", "balance_date"] {
            if let Some(v) = st.get(key).filter(|v| crate::models::truthy(v)) {
                if let Some(day) = v.as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
                    return day;
                }
            }
        }
    }
    d.uploaded_at.date()
}

/// "Relevé de compte" -> "RELEVE-DE-COMPTE": upper case, no accent, anything that is not a letter or a digit becomes one
/// "-".
pub fn token(text: &str) -> String {
    let plain = py::upper(&strip_accents(text));
    let mut out = String::new();
    for c in plain.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

pub struct NameParts<'a> {
    pub institution: &'a str,
    pub account_type: &'a str,
    pub kind: &'a str,
    pub people: &'a [Person],
    pub day: NaiveDate,
    /// With its dot, lower case ("" when the file has none).
    pub extension: &'a str,
}

fn joined(values: impl Iterator<Item = String>) -> String {
    let mut seen: Vec<String> = vec![];
    for v in values.map(|v| token(&v)).filter(|v| !v.is_empty()) {
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.join("-")
}

/// The standard name of a document, short enough for a file name (`reserve` bytes are left for a number).
pub fn standard_name(p: &NameParts, reserve: usize) -> String {
    let last = joined(p.people.iter().map(|x| x.last_names.join(" ")));
    let first = joined(p.people.iter().filter_map(|x| x.first_names.first().cloned()));
    let date = p.day.format("%Y-%m-%d").to_string();
    let build = |names: &[&str]| -> String {
        let mut fields: Vec<String> = vec![token(p.institution), token(p.account_type), token(p.kind)];
        fields.extend(names.iter().map(|n| n.to_string()));
        fields.push(date.clone());
        let mut name = fields.into_iter().filter(|f| !f.is_empty()).collect::<Vec<_>>().join("_");
        name.push_str(p.extension);
        name
    };
    // everything, then the names without the first names, then no name at all
    for names in [vec![last.as_str(), first.as_str()], vec![last.as_str()], vec![]] {
        let name = build(&names);
        if name.len() + reserve <= MAX_NAME_BYTES {
            return name;
        }
    }
    build(&[])
}

fn extension_of(filename: &str) -> String {
    match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => format!(".{}", ext.to_lowercase()),
        _ => String::new(),
    }
}

/// The people a document is about: its account's, else the cards it is linked to, else the ones its own reading names.
fn people_of(c: &Connection, account: Option<&Account>, doc: &Document, st: Option<&ParsedStatement>) -> AppResult<Vec<Person>> {
    if let Some(a) = account {
        let people = holders::of_account(a);
        if !people.is_empty() {
            return Ok(people);
        }
    }
    let mut linked = vec![];
    for id in doc.people() {
        if let Some(card) = PersonCard::get(c, id)? {
            linked.push(people::person_of(&card));
        }
    }
    if !linked.is_empty() {
        return Ok(linked);
    }
    Ok(holders::parse_printed(st.and_then(|s| s.holder_name.as_deref()).unwrap_or("")))
}

/// Give an imported document its standard name (keeping the name it was dropped with). Documents with the same name
/// get a number. Returns the new name.
pub fn rename_document(c: &Connection, doc: &mut Document) -> AppResult<String> {
    people::attach_known(c, doc)?; // a document nobody linked yet gets the people it clearly is about
    let statements = parse_statements(doc);
    let st = statements.first().map(ParsedStatement::from_json);
    let account = match doc.account_id {
        Some(id) => Account::get(c, id)?,
        None => None,
    };
    let (read_institution, kind) = document_group(doc);
    let institution = account
        .as_ref()
        .and_then(|a| a.institution.clone())
        .filter(|i| !i.is_empty())
        .unwrap_or(if read_institution == "Autres documents" { String::new() } else { read_institution });
    let account_type = account.as_ref().map(|a| a.type_label()).unwrap_or_default();
    let people = people_of(c, account.as_ref(), doc, st.as_ref())?;
    let source = doc.original_filename.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| doc.filename.clone());
    let parts = NameParts { institution: &institution, account_type: &account_type, kind: &kind, people: &people, day: document_day(doc), extension: &extension_of(&source) };
    let mut name = standard_name(&parts, 0);
    let mut number = 1;
    loop {
        let taken = Document::query_one(c, "WHERE filename = ? AND id != ?", rusqlite::params![name, doc.id])?.is_some();
        if !taken {
            break;
        }
        number += 1;
        let suffix = format!("_{number}");
        let stem = standard_name(&parts, suffix.len());
        name = match stem.rsplit_once('.').filter(|(_, e)| format!(".{e}") == parts.extension) {
            Some((s, e)) => format!("{s}{suffix}.{e}"),
            None => format!("{stem}{suffix}"),
        };
    }
    let before = doc.clone();
    if doc.original_filename.as_deref().map(|n| n.is_empty()).unwrap_or(true) {
        doc.original_filename = Some(doc.filename.clone());
    }
    doc.filename = name.clone();
    doc.save(c, &before)?;
    Ok(name)
}

/// The document goes back to waiting: it takes back the name it was dropped with.
pub fn restore_name(c: &Connection, doc: &mut Document) -> AppResult<()> {
    if let Some(original) = doc.original_filename.clone().filter(|n| !n.is_empty()) {
        let before = doc.clone();
        doc.filename = original;
        doc.original_filename = None;
        doc.save(c, &before)?;
    }
    Ok(())
}

/// The institution, type or holders of an account changed: the names of its imported documents follow.
pub fn rename_account_documents(c: &Connection, account_id: i64) -> AppResult<()> {
    for mut doc in Document::query(c, "WHERE account_id = ? AND status = 'imported' ORDER BY id", [account_id])? {
        rename_document(c, &mut doc)?;
    }
    Ok(())
}
