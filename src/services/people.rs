//! People's cards: the individuals behind accounts. An account keeps the people it is linked to in its `holders` JSON
//! (each entry carries the card's id), so everything that reads holders (file names, own-name transfer detection, the
//! single holder line) keeps working; a change to a card is copied into every account that points to it.

use std::collections::HashSet;

use rusqlite::Connection;
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::models::{Account, Document, PersonCard};
use crate::py;
use crate::services::{access, holders, naming};
use holders::Person;

fn strings(v: &Option<Value>) -> Vec<String> {
    v.as_ref().and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn list(items: &[String]) -> Option<Value> {
    Some(Value::Array(items.iter().map(|s| Value::String(s.clone())).collect())).filter(|v| !v.as_array().unwrap().is_empty())
}

/// The card as a person of a form or of an account.
pub fn person_of(card: &PersonCard) -> Person {
    Person { id: Some(card.id), civility: card.civility.clone(), last_names: strings(&card.last_names), birth_names: strings(&card.birth_names), first_names: strings(&card.first_names) }
}

pub fn apply(card: &mut PersonCard, p: &Person) {
    card.civility = p.civility.clone();
    card.last_names = list(&p.last_names);
    card.birth_names = list(&p.birth_names);
    card.first_names = list(&p.first_names);
}

/// "M DUPONT JEAN": what the card is called.
pub fn label(card: &PersonCard) -> String {
    let p = person_of(card);
    holders::compose(&[p])
}

fn same(a: &str, b: &str) -> bool {
    py::upper(a.trim()) == py::upper(b.trim())
}

fn same_names(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
}

fn all_cards(c: &Connection) -> AppResult<Vec<PersonCard>> {
    Ok(PersonCard::query(c, "ORDER BY id", [])?)
}

/// The ids of the cards the accounts in `accounts` point to.
fn linked_ids(accounts: &[Account]) -> HashSet<i64> {
    accounts.iter().filter_map(|a| a.holders.as_ref()).flat_map(|h| holders::from_json(h)).filter_map(|p| p.id).collect()
}

/// Cards the user may see: their own, and those of the accounts shared with them.
pub fn visible(c: &Connection) -> AppResult<Vec<PersonCard>> {
    let cards = all_cards(c)?;
    let Some(me) = access::current() else { return Ok(cards) };
    let linked = linked_ids(&access::only_readable(c, Account::query(c, "", [])?)?);
    Ok(cards.into_iter().filter(|k| k.owner_id == Some(me.id) || linked.contains(&k.id)).collect())
}

pub fn get_visible(c: &Connection, id: i64) -> AppResult<PersonCard> {
    visible(c)?.into_iter().find(|k| k.id == id).ok_or_else(|| AppError::Denied("Personne introuvable".into()))
}

/// May the current user change this card: it is theirs, or it belongs to an account they can edit.
pub fn can_edit(c: &Connection, card: &PersonCard) -> AppResult<bool> {
    let Some(me) = access::current() else { return Ok(true) };
    if card.owner_id == Some(me.id) {
        return Ok(true);
    }
    Ok(linked_ids(&access::only_editable(c, Account::query(c, "", [])?)?).contains(&card.id))
}

/// Accounts the current user can see that point to this card, best first.
pub fn accounts_of(c: &Connection, id: i64) -> AppResult<Vec<Account>> {
    let accounts = access::only_readable(c, Account::query(c, "ORDER BY archived, institution, name", [])?)?;
    Ok(accounts.into_iter().filter(|a| a.holders.as_ref().map(|h| holders::from_json(h).iter().any(|p| p.id == Some(id))).unwrap_or(false)).collect())
}

/// The card with exactly these names, if there is one.
pub fn find_exact(c: &Connection, p: &Person) -> AppResult<Option<PersonCard>> {
    for card in visible(c)? {
        let q = person_of(&card);
        if same(&q.civility, &p.civility) && same_names(&q.last_names, &p.last_names) && same_names(&q.birth_names, &p.birth_names) && same_names(&q.first_names, &p.first_names) {
            return Ok(Some(card));
        }
    }
    Ok(None)
}

/// A guess read from a printed line, replaced by the card it designates (same first last name and first first name).
pub fn match_guess(cards: &[PersonCard], guess: &[Person]) -> Vec<Person> {
    guess
        .iter()
        .map(|g| {
            let found = cards.iter().find(|k| {
                let q = person_of(k);
                (g.civility.is_empty() || q.civility.is_empty() || same(&g.civility, &q.civility))
                    && !g.last_names.is_empty()
                    && q.last_names.first().map(|n| same(n, &g.last_names[0])).unwrap_or(false)
                    && (g.first_names.is_empty() || q.first_names.first().map(|n| same(n, &g.first_names[0])).unwrap_or(false))
            });
            found.map(person_of).unwrap_or_else(|| g.clone())
        })
        .collect()
}

/// Turn what a form carries into cards: a person with a card keeps it (the card's names win), a typed person reuses the
/// card with the same names or gets a new one.
pub fn resolve(c: &Connection, typed: Vec<Person>) -> AppResult<Vec<Person>> {
    let mut out: Vec<Person> = vec![];
    for p in typed {
        let linked = match p.id {
            Some(id) => get_visible(c, id).ok(),
            None => None,
        };
        let card = match linked {
            Some(card) => card,
            None if p.is_empty() => continue,
            None => match find_exact(c, &p)? {
                Some(card) => card,
                None => {
                    let mut card = PersonCard { owner_id: access::owner_for_new(c)?, ..Default::default() };
                    apply(&mut card, &p);
                    card.insert(c)?;
                    card
                }
            },
        };
        let resolved = person_of(&card);
        if !out.iter().any(|o| o.id == resolved.id) {
            out.push(resolved);
        }
    }
    Ok(out)
}

/// Point an account at these people (the cards keep the names; the account keeps a copy for fast reading).
pub fn link(account: &mut Account, people: &[Person]) {
    account.holders = if people.is_empty() { None } else { Some(holders::to_json(people)) };
    account.holder_name = Some(holders::compose(people)).filter(|s| !s.is_empty());
}

/// Copy a changed card into every account that points to it, and rename their files.
pub fn sync(c: &Connection, id: i64) -> AppResult<()> {
    let Some(card) = PersonCard::get(c, id)? else { return Ok(()) };
    let fresh = person_of(&card);
    for mut account in Account::query(c, "", [])? {
        let Some(h) = account.holders.clone() else { continue };
        let mut people = holders::from_json(&h);
        if !people.iter().any(|p| p.id == Some(id)) {
            continue;
        }
        for p in people.iter_mut().filter(|p| p.id == Some(id)) {
            *p = fresh.clone();
        }
        let before = account.clone();
        link(&mut account, &people);
        account.save(c, &before)?;
        naming::rename_account_documents(c, account.id)?;
    }
    for mut doc in documents_linked_to(c, id)? {
        if doc.status == "imported" {
            naming::rename_document(c, &mut doc)?;
        }
    }
    Ok(())
}

/// Delete a card: the accounts keep the names as plain text, no longer linked.
pub fn delete(c: &Connection, id: i64) -> AppResult<()> {
    for mut account in Account::query(c, "", [])? {
        let Some(h) = account.holders.clone() else { continue };
        let mut people = holders::from_json(&h);
        if !people.iter().any(|p| p.id == Some(id)) {
            continue;
        }
        for p in people.iter_mut().filter(|p| p.id == Some(id)) {
            p.id = None;
        }
        let before = account.clone();
        link(&mut account, &people);
        account.save(c, &before)?;
    }
    for mut doc in documents_linked_to(c, id)? {
        let ids: Vec<i64> = doc.people().into_iter().filter(|i| *i != id).collect();
        set_document_people(c, &mut doc, &ids)?;
    }
    PersonCard::delete(c, id)?;
    Ok(())
}

/// Cards for the accounts that predate them: holders the user confirmed (or a clear single name) get a card, and the
/// account is linked to it. Safe to run again: accounts already linked are left alone.
pub fn backfill(c: &Connection) -> AppResult<usize> {
    let mut changed = 0;
    for mut account in access::only_editable(c, Account::query(c, "", [])?)? {
        let stored = account.holders.as_ref().map(holders::from_json).unwrap_or_default();
        let candidates: Vec<Person> = if !stored.is_empty() {
            stored
        } else if account.holder_name.as_deref().map(|n| !n.trim().is_empty() && !holders::is_unclear(n)).unwrap_or(false) {
            holders::parse_printed(account.holder_name.as_deref().unwrap())
        } else {
            continue;
        };
        if candidates.iter().all(|p| p.id.is_some()) {
            continue;
        }
        let linked = resolve(c, candidates)?;
        let before = account.clone();
        link(&mut account, &linked);
        account.save(c, &before)?;
        changed += 1;
    }
    Ok(changed)
}

// ------------------------------------------------------------------------------------------------ documents
// A document can be about one or several people (its addressee, its holder, its borrower): the cards it is linked to are
// in `documents.person_ids`. The reading of the document proposes them; a person can always link or unlink by hand.

/// The documents (all of them, whoever may see them) linked to this card.
fn documents_linked_to(c: &Connection, id: i64) -> AppResult<Vec<Document>> {
    Ok(Document::query(c, "WHERE person_ids IS NOT NULL ORDER BY id", [])?.into_iter().filter(|d| d.people().contains(&id)).collect())
}

/// The people the reading of a document names, a known card standing in for each one it designates (`id` set).
pub fn read_people(c: &Connection, doc: &Document) -> AppResult<Vec<Person>> {
    let guess = holders::parse_printed(&naming::reading_holder(doc));
    Ok(match_guess(&visible(c)?, &guess))
}

/// The cards a document is linked to (the ones the current user may see), in the order they were linked.
pub fn of_document(c: &Connection, doc: &Document) -> AppResult<Vec<PersonCard>> {
    let cards = visible(c)?;
    Ok(doc.people().into_iter().filter_map(|id| cards.iter().find(|k| k.id == id).cloned()).collect())
}

/// Remember who a document is about (an empty list is a decision too: nobody). The name of an imported file follows.
pub fn set_document_people(c: &Connection, doc: &mut Document, ids: &[i64]) -> AppResult<()> {
    let mut unique: Vec<i64> = vec![];
    for id in ids {
        if !unique.contains(id) {
            unique.push(*id);
        }
    }
    let before = doc.clone();
    doc.person_ids = Some(Value::Array(unique.into_iter().map(Value::from).collect()));
    doc.save(c, &before)?;
    if doc.status == "imported" && doc.person_ids != before.person_ids {
        naming::rename_document(c, doc)?;
    }
    Ok(())
}

/// A document nobody has looked at yet gets the people it clearly is about: the holders of its account, else the cards its
/// reading designates. Nothing known = left undecided, so a card made later can still be picked up.
pub fn attach_known(c: &Connection, doc: &mut Document) -> AppResult<()> {
    if doc.person_ids.is_some() {
        return Ok(());
    }
    let mut ids: Vec<i64> = match doc.account_id {
        Some(a) => Account::get(c, a)?.map(|a| holders::of_account(&a).iter().filter_map(|p| p.id).collect()).unwrap_or_default(),
        None => vec![],
    };
    if ids.is_empty() {
        ids = read_people(c, doc)?.iter().filter_map(|p| p.id).collect();
    }
    if ids.is_empty() {
        return Ok(());
    }
    let before = doc.clone();
    ids.dedup();
    doc.person_ids = Some(Value::Array(ids.into_iter().map(Value::from).collect()));
    doc.save(c, &before)?;
    Ok(())
}

/// Link the people the reading names, creating the cards that do not exist yet. Returns how many people were linked.
pub fn accept_reading(c: &Connection, doc: &mut Document) -> AppResult<usize> {
    let named = resolve(c, read_people(c, doc)?)?;
    let mut ids = doc.people();
    ids.extend(named.iter().filter_map(|p| p.id));
    set_document_people(c, doc, &ids)?;
    Ok(named.len())
}

/// One identity read from documents that no card stands for yet.
pub struct Identity {
    /// The line as the documents print it.
    pub line: String,
    pub label: String,
    pub documents: usize,
}

/// A holder of accounts, as the Contacts page lists them.
pub struct Holder {
    pub card: PersonCard,
    pub accounts: usize,
    pub documents: usize,
}

/// The people behind the accounts and documents the user can see: the cards (with what points to each), and the identities
/// the documents name that have no card yet.
pub fn holders_overview(c: &Connection) -> AppResult<(Vec<Holder>, Vec<Identity>)> {
    let cards = visible(c)?;
    let docs = access::visible_documents(c)?;
    let accounts = access::only_readable(c, Account::query(c, "", [])?)?;
    let mut documents: std::collections::HashMap<i64, HashSet<i64>> = std::collections::HashMap::new();
    let mut unknown: Vec<Identity> = vec![];
    for d in &docs {
        let mut ids: HashSet<i64> = d.people().into_iter().collect();
        if let Some(a) = d.account_id.and_then(|a| accounts.iter().find(|x| x.id == a)) {
            ids.extend(holders::of_account(a).iter().filter_map(|p| p.id));
        }
        if d.person_ids.is_none() {
            // nobody looked at this document yet: what it names counts, with or without a card
            let line = naming::reading_holder(d);
            let guess = holders::parse_printed(&line);
            let matched = match_guess(&cards, &guess);
            ids.extend(matched.iter().filter_map(|p| p.id));
            if matched.iter().any(|p| p.id.is_none()) {
                match unknown.iter_mut().find(|u| same(&u.line, &line)) {
                    Some(u) => u.documents += 1,
                    None => unknown.push(Identity { label: holders::compose(&guess), line, documents: 1 }),
                }
            }
        }
        for id in ids {
            documents.entry(id).or_default().insert(d.id);
        }
    }
    let mut out = vec![];
    for card in cards {
        let accounts = accounts_of(c, card.id)?.len();
        let documents = documents.get(&card.id).map(|s| s.len()).unwrap_or(0);
        out.push(Holder { card, accounts, documents });
    }
    Ok((out, unknown))
}

/// "Create the card" for an identity read from documents: every document that names it (and was not looked at yet) is
/// linked to the card, which is created unless it exists. Returns the number of documents linked.
pub fn adopt_identity(c: &Connection, line: &str) -> AppResult<usize> {
    let mut linked = 0;
    for mut d in access::visible_documents(c)? {
        if d.person_ids.is_some() || !same(&naming::reading_holder(&d), line) {
            continue;
        }
        accept_reading(c, &mut d)?;
        linked += 1;
    }
    if linked == 0 {
        // no document to start from: the line alone still gives a card
        resolve(c, holders::parse_printed(line))?;
    }
    Ok(linked)
}
