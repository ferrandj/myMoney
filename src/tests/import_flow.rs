//! python/tests/test_import_flow.py: the drop-a-document flow end to end, on synthetic statements only.

use std::collections::{BTreeSet, HashMap};

use chrono::Duration;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::adapters::base::{Adapter, Detection, ParsedStatement, SourceDocument};
use crate::adapters::registry::{identify, OVERRIDE};
use crate::error::AppError;
use crate::models::{utcnow, Account, Counterparty, Document, Statement, Transaction};
use crate::services::counterparties as cps;
use crate::testkit::env::{client, core, Client, Core, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::{db, form, num};

fn d(s: &str) -> Decimal {
    dec(s)
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn september_with(f: impl FnOnce(&mut Spec)) -> Spec {
    let mut spec = september();
    f(&mut spec);
    spec
}

fn october_entries() -> Vec<Entry> {
    vec![
        entry(ymd(2026, 10, 3), &["CARTE 01/10/26 BOULANGERIE CB*1234"], "-4.20"),
        entry(ymd(2026, 10, 8), &["VIR SEPA Pauline MARTIN", "remboursement", "Rèf : SCT123450000000000000000009"], "-30.00"),
    ]
}

fn october(after: &Spec, entries: Option<Vec<Entry>>, title: &str) -> Spec {
    Spec {
        opening: after.closing(),
        opening_date: ymd(2026, 9, 30),
        period: (ymd(2026, 10, 1), ymd(2026, 10, 31)),
        title: title.into(),
        ..Spec::new(entries.unwrap_or_else(october_entries))
    }
}

/// (document id, already stored, status)
fn drop(core: &Core, spec: &Spec) -> (i64, bool, String) {
    drop_pdf(core, "releve.pdf", &boursobank_statement_pdf(spec))
}

fn drop_pdf(core: &Core, name: &str, data: &[u8]) -> (i64, bool, String) {
    let (id, existing) = core.store_document(name, data).unwrap();
    let status = core.analyze_document(id, None, Some(data.to_vec())).unwrap();
    (id, existing, status)
}

fn first_import(core: &Core, spec: Option<Spec>) -> (i64, i64) {
    let spec = spec.unwrap_or_else(september);
    let (doc_id, _, _) = drop(core, &spec);
    let result = core
        .import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte courant"), ("import_transactions", "on"), ("update_balance", "on"), ("balance", &spec.closing().to_string())]))
        .unwrap();
    (doc_id, num(&result, "account_id"))
}

fn transactions(core: &Core, account_id: Option<i64>) -> Vec<Transaction> {
    db(core, |c| match account_id {
        Some(a) => Ok(Transaction::query(c, "WHERE account_id = ? ORDER BY date, id", [a])?),
        None => Ok(Transaction::query(c, "ORDER BY date, id", [])?),
    })
}

fn expect_value<T: std::fmt::Debug>(result: Result<T, AppError>, needle: &str) {
    match result {
        Err(AppError::Value(m)) => assert!(m.contains(needle), "{m:?} does not contain {needle:?}"),
        other => panic!("expected a ValueError containing {needle:?}, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------ first import
#[test]
fn test_first_statement_creates_the_account_with_its_identity() {
    let core = core();
    let spec = september();
    let (doc_id, existing, status) = drop(&core, &spec);
    assert_eq!((existing, status.as_str()), (false, "parsed"));
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(plan.account.is_none() && plan.classified.exact.is_empty() && plan.classified.new.len() == spec.entries.len());
    assert!(core.auto_import(doc_id).unwrap().is_none()); // an account to create: the user is asked

    let (_, account_id) = first_import(&core, Some(spec.clone()));
    db(&core, |c| {
        let account = Account::get(c, account_id)?.unwrap();
        assert_eq!((account.institution.clone(), account.account_type.clone()), (s("BoursoBank"), "checking".to_string()));
        assert_eq!(account.holder_name, s("M DUPONT JEAN"));
        assert_eq!(account.holder_address, s("12 RUE DES LILAS\n75011 PARIS"));
        assert_eq!((account.iban.clone(), account.bic.clone()), (Some(iban()), s("BOUSFRPPXXX")));
        assert_eq!(account.rib(), Some(format!("{BANK} {BRANCH} {ACCOUNT} {}", account.rib_key.clone().unwrap())));
        assert_eq!(account.balance, spec.closing());
        assert_eq!(account.balance_date.unwrap().date(), ymd(2026, 9, 30));
        let statements = Statement::query(c, "", [])?;
        let [statement] = &statements[..] else { panic!("one statement expected") };
        assert_eq!((statement.period_start, statement.period_end), (Some(ymd(2026, 9, 1)), Some(ymd(2026, 9, 30))));
        assert_eq!((statement.opening_balance, statement.closing_balance, statement.added_count), (Some(d("1000.00")), Some(spec.closing()), spec.entries.len() as i64));
        Ok(())
    });
    let rows = transactions(&core, None);
    assert_eq!(rows.len(), spec.entries.len());
    let card = rows.iter().find(|t| t.kind.as_deref() == Some("card")).unwrap();
    assert_eq!(card.counterparty_name, s("TRAINS DE FRANCE"));
    assert_eq!(card.details.as_ref().unwrap()["card_last4"], json!("1234"));
    let transfer = rows.iter().find(|t| t.reference.as_deref() == Some("SCT123450000000000000000001")).unwrap();
    assert_eq!(transfer.counterparty_name, s("MARTIN Pauline"));
    assert_eq!(transfer.memo, s("cadeau anniversaire"));
    assert!(transfer.raw_label.as_deref().map(|r| !r.is_empty()).unwrap_or(false));
}

#[test]
fn test_the_original_file_is_encrypted_on_disk_and_downloadable() {
    let core = core();
    let data = boursobank_statement_pdf(&september());
    let (doc_id, _) = core.store_document("releve.pdf", &data).unwrap();
    let blobs: Vec<_> = std::fs::read_dir(core.settings.documents_dir()).unwrap().map(|e| e.unwrap().path()).collect();
    let [blob] = &blobs[..] else { panic!("one blob expected") };
    let stored = std::fs::read(blob).unwrap();
    let has = |needle: &[u8]| stored.windows(needle.len()).any(|w| w == needle);
    assert!(!has(b"%PDF") && !has(b"BOURSOBANK") && !has(b"DUPONT"));
    let name = blob.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name != "releve.pdf" && !name.contains("releve"));
    assert_eq!(core.read_document(doc_id).unwrap(), ("releve.pdf".to_string(), data));
}

#[test]
fn test_the_same_file_dropped_twice_is_reported_and_not_stored_twice() {
    let core = core();
    let data = boursobank_statement_pdf(&september());
    let (first, existing) = core.store_document("a.pdf", &data).unwrap();
    let (again, existing_again) = core.store_document("b.pdf", &data).unwrap();
    assert_eq!((existing, existing_again), (false, true));
    assert_eq!(first, again);
    db(&core, |c| {
        assert_eq!(Document::query(c, "", [])?.len(), 1);
        Ok(())
    });
}

// --------------------------------------------------------------------------------- following statements
#[test]
fn test_next_month_is_imported_by_itself_into_the_right_account() {
    let core = core();
    let spec = september();
    let (_, account_id) = first_import(&core, Some(spec.clone()));
    let nxt = october(&spec, None, "");
    let (doc_id, _, status) = drop(&core, &nxt);
    assert_eq!(status, "parsed");
    let result = core.auto_import(doc_id).unwrap().unwrap();
    assert_eq!(Value::Object(result), json!({"account_id": account_id, "added": 2, "skipped_exact": 0, "skipped_probable": 0}));
    db(&core, |c| {
        let account = Account::get(c, account_id)?.unwrap();
        assert_eq!(account.balance, nxt.closing());
        assert_eq!(account.balance_date.unwrap().date(), ymd(2026, 10, 31));
        assert_eq!(Statement::query(c, "", [])?.len(), 2);
        Ok(())
    });
}

#[test]
fn test_same_statement_downloaded_again_is_flagged_as_a_duplicate() {
    let core = core();
    let spec = september();
    let (_, account_id) = first_import(&core, Some(spec.clone()));
    let (doc_id, existing, _) = drop(&core, &september_with(|sp| sp.title = "downloaded a second time".into()));
    assert!(!existing); // another file (another hash) with the same content
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert_eq!(plan.account.as_ref().unwrap().id, account_id);
    assert!(plan.already_imported.is_some());
    assert_eq!(plan.classified.exact.len(), spec.entries.len());
    assert!(plan.classified.new.is_empty());
    assert!(core.auto_import(doc_id).unwrap().is_none());
    expect_value(core.import_statement(doc_id, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on")])), "déjà été importé");
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on"), ("force", "on")])).unwrap();
    assert_eq!(num(&result, "added"), 0);
    assert_eq!(num(&result, "skipped_exact"), spec.entries.len() as i64);
    db(&core, |c| {
        assert_eq!(Statement::query(c, "", [])?.len(), 1);
        Ok(())
    });
    assert_eq!(transactions(&core, None).len(), spec.entries.len());
}

#[test]
fn test_overlapping_statement_only_adds_what_is_new() {
    let core = core();
    let spec = september();
    let (_, account_id) = first_import(&core, Some(spec));
    let mut later: Vec<Entry> = sample_entries().into_iter().filter(|e| e.day >= ymd(2026, 9, 12)).collect();
    later.push(entry(ymd(2026, 10, 5), &["CARTE 03/10/26 CAFE DU COIN CB*1234"], "-3.00"));
    let overlap = Spec { opening: d("500.00"), opening_date: ymd(2026, 9, 11), period: (ymd(2026, 9, 12), ymd(2026, 10, 15)), ..Spec::new(later.clone()) };
    let (doc_id, _, _) = drop(&core, &overlap);
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(plan.overlaps.len() == 1 && plan.already_imported.is_none());
    assert!(plan.classified.exact.len() == later.len() - 1 && plan.classified.new.len() == 1);
    assert!(core.auto_import(doc_id).unwrap().is_none());
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on")])).unwrap();
    assert_eq!(num(&result, "added"), 1);
    assert_eq!(num(&result, "skipped_exact"), later.len() as i64 - 1);
}

#[test]
fn test_a_missing_period_is_reported() {
    let core = core();
    first_import(&core, None);
    let december = Spec {
        opening: d("100.00"),
        opening_date: ymd(2026, 11, 30),
        period: (ymd(2026, 12, 1), ymd(2026, 12, 31)),
        ..Spec::new(vec![entry(ymd(2026, 12, 2), &["CARTE 01/12/26 CAFE CB*1234"], "-3.00")])
    };
    let (doc_id, _, _) = drop(&core, &december);
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(plan.issues.iter().any(|i| i.contains("Il manque une période") && i.contains("01/10/2026") && i.contains("30/11/2026")));
    assert!(core.auto_import(doc_id).unwrap().is_none());
}

#[test]
fn test_balances_must_chain_from_one_statement_to_the_next() {
    let core = core();
    let spec = september();
    first_import(&core, Some(spec.clone()));
    let mut nxt = october(&spec, None, "");
    nxt.opening = spec.closing() + d("12.00"); // does not continue the previous statement
    let (doc_id, _, _) = drop(&core, &nxt);
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(plan.issues.iter().any(|i| i.contains("solde initial")));
    assert!(core.auto_import(doc_id).unwrap().is_none());
}

#[test]
fn test_an_older_statement_does_not_overwrite_a_newer_balance() {
    let core = core();
    let spec = september();
    let (_, account_id) = first_import(&core, Some(spec.clone()));
    let nxt = october(&spec, None, "");
    core.auto_import(drop(&core, &nxt).0).unwrap();
    let august = Spec {
        opening: d("1003.00"),
        opening_date: ymd(2026, 7, 31),
        period: (ymd(2026, 8, 1), ymd(2026, 8, 31)),
        ..Spec::new(vec![entry(ymd(2026, 8, 5), &["CARTE 04/08/26 CAFE CB*1234"], "-3.00")])
    };
    let (doc_id, _, _) = drop(&core, &august);
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(!plan.newer_balance);
    core.import_statement(doc_id, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on")])).unwrap();
    db(&core, |c| {
        assert_eq!(Account::get(c, account_id)?.unwrap().balance, nxt.closing());
        Ok(())
    });
}

#[test]
fn test_lines_already_known_from_another_source_are_flagged_as_probable_duplicates() {
    let core = core();
    let (_, account_id) = first_import(&core, None);
    let csv = format!("IBAN;{}\nDate;Libellé;Montant\n14/09/2026;CB CARREFOUR 12/09;-14,08\n20/09/2026;AUTRE LIGNE;-1,00\n", iban());
    let bytes: Vec<u8> = csv.chars().map(crate::testkit::cp1252_byte).collect();
    let (doc_id, _) = core.store_document("export.csv", &bytes).unwrap();
    assert_eq!(core.analyze_document(doc_id, None, Some(bytes.clone())).unwrap(), "parsed");
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert_eq!(plan.account.as_ref().unwrap().id, account_id);
    let probable: Vec<Decimal> = plan.classified.probable.iter().map(|(i, _)| plan.statement.transactions[*i].amount).collect();
    assert_eq!(probable, vec![d("-14.08")]);
    assert_eq!(plan.statuses(), vec!["probable", "new"]);
    assert!(core.auto_import(doc_id).unwrap().is_none()); // generic reading is never imported unseen
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on")])).unwrap();
    assert_eq!((num(&result, "added"), num(&result, "skipped_probable")), (1, 1));
    let mut bytes2 = bytes.clone();
    bytes2.push(b'\n');
    let (doc_id2, _) = core.store_document("export2.csv", &bytes2).unwrap();
    core.analyze_document(doc_id2, None, Some(bytes2)).unwrap();
    let result = core.import_statement(doc_id2, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on"), ("import_probable", "on"), ("force", "on")])).unwrap();
    assert_eq!(num(&result, "added"), 1);
    assert_eq!(num(&result, "skipped_probable"), 0); // the other probable line, explicitly kept
}

#[test]
fn test_undoing_an_import_removes_what_it_brought() {
    let core = core();
    let (doc_id, account_id) = first_import(&core, None);
    assert_eq!(core.undo_import(doc_id).unwrap(), sample_entries().len() as i64);
    assert!(transactions(&core, None).is_empty());
    db(&core, |c| {
        assert!(Statement::query(c, "", [])?.is_empty());
        assert_eq!(Document::get(c, doc_id)?.unwrap().status, "parsed");
        assert!(Account::get(c, account_id)?.is_some());
        Ok(())
    });
}

// ---------------------------------------------------------------------------------- choosing an adapter
struct Two;
struct Three;

fn acme(label: &str) -> Result<Vec<ParsedStatement>, String> {
    let mut st = ParsedStatement::new("pdf");
    st.institution = s("Acme");
    st.account_label = s(label);
    Ok(vec![st])
}

impl Adapter for Two {
    fn id(&self) -> &'static str {
        "test.a"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Acme")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Acme A"
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn detect(&self, _: &SourceDocument) -> Result<Detection, String> {
        Ok(Detection { score: 0.85, reasons: vec!["looks like A".into()] })
    }
    fn parse(&self, _: &SourceDocument) -> Result<Vec<ParsedStatement>, String> {
        acme("A")
    }
}

impl Adapter for Three {
    fn id(&self) -> &'static str {
        "test.b"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Acme")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Acme B"
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn detect(&self, _: &SourceDocument) -> Result<Detection, String> {
        Ok(Detection { score: 0.8, reasons: vec!["looks like B".into()] })
    }
    fn parse(&self, _: &SourceDocument) -> Result<Vec<ParsedStatement>, String> {
        acme("B")
    }
}

static TWO: Two = Two;
static THREE: Three = Three;

/// `monkeypatch.setattr("mymoney.adapters.registry.ADAPTERS", [_Two, _Three])`, undone when dropped.
struct TwoAdapters;

impl TwoAdapters {
    fn new() -> TwoAdapters {
        OVERRIDE.with(|o| *o.borrow_mut() = Some(vec![&TWO, &THREE]));
        TwoAdapters
    }
}

impl Drop for TwoAdapters {
    fn drop(&mut self) {
        OVERRIDE.with(|o| *o.borrow_mut() = None);
    }
}

#[test]
fn test_two_plausible_adapters_means_the_user_is_asked() {
    let core = core();
    let _patched = TwoAdapters::new();
    let data = boursobank_statement_pdf(&september());
    let found = identify(&SourceDocument::new("x.pdf", data.clone()));
    assert!(found.decision == "ask" && found.chosen.is_none() && found.candidates.len() == 2);

    let (doc_id, _) = core.store_document("x.pdf", &data).unwrap();
    assert_eq!(core.analyze_document(doc_id, None, Some(data.clone())).unwrap(), "needs_choice");
    assert!(core.auto_import(doc_id).unwrap().is_none());
    assert_eq!(core.analyze_document(doc_id, Some("test.b"), Some(data)).unwrap(), "parsed");
    db(&core, |c| {
        let doc = Document::get(c, doc_id)?.unwrap();
        assert_eq!(doc.adapter_id, s("test.b"));
        assert_eq!(doc.parse_result.unwrap()["statements"][0]["account_label"], json!("B"));
        Ok(())
    });
}

#[test]
fn test_unknown_file_is_kept_but_reported() {
    let core = core();
    let data = b"\x00\x01 not a statement".to_vec();
    let (doc_id, _) = core.store_document("mystery.bin", &data).unwrap();
    assert_eq!(core.analyze_document(doc_id, None, Some(data.clone())).unwrap(), "failed");
    db(&core, |c| {
        let error = Document::get(c, doc_id)?.unwrap().parse_result.unwrap()["error"].as_str().unwrap().to_string();
        assert!(error.contains("Format non reconnu"));
        Ok(())
    });
    assert_eq!(core.read_document(doc_id).unwrap().1, data);
}

// --------------------------------------------------------------------------------------------- contacts
fn pending(core: &Core, shops: bool) -> Vec<cps::Unresolved> {
    db(core, |c| cps::unresolved(c, shops, None, None))
}

fn identify_contact(core: &Core, pairs: &[(&str, &str)]) -> Result<Counterparty, AppError> {
    core.session(|c| cps::identify(c, &form(pairs)))
}

#[test]
fn test_people_and_organisations_are_listed_but_not_shops_nor_own_accounts() {
    let core = core();
    first_import(&core, None);
    let groups = pending(&core, false);
    let names: BTreeSet<String> = groups.iter().filter_map(|g| g.name.clone()).collect();
    for expected in ["MARTIN Pauline", "ACME CONSULTING SAS", "EAUX DU NORD", "Revolut"] {
        assert!(names.contains(expected), "{expected} in {names:?}");
    }
    // "LEROY CLAIRE" and "MME LEROY CLAIRE" are one person printed two ways: one line to identify, not two.
    let claire: Vec<_> = groups.iter().filter(|g| g.name.as_deref().unwrap_or("").contains("LEROY")).collect();
    let [claire] = &claire[..] else { panic!("one group expected") };
    assert_eq!(claire.count, 2);
    assert!(!names.contains("TRAINS DE FRANCE") && !names.contains("DUPONT JEAN"));
    assert!(pending(&core, true).iter().any(|g| g.name.as_deref() == Some("TRAINS DE FRANCE")));
}

#[test]
fn test_a_named_contact_is_recognised_in_the_next_imports_whatever_the_spelling() {
    let core = core();
    let spec = september();
    first_import(&core, Some(spec.clone()));
    let contact_id = identify_contact(&core, &[("name", "Pauline"), ("kind", "person"), ("phone", "0600000000"), ("printed_name", "MARTIN Pauline")]).unwrap().id;
    let linked = |core: &Core| transactions(core, None).into_iter().filter(|t| t.counterparty_id == Some(contact_id)).filter_map(|t| t.counterparty_name).collect::<Vec<_>>();
    assert_eq!(linked(&core), vec!["MARTIN Pauline".to_string()]);

    let (doc_id, _, _) = drop(&core, &october(&spec, None, "")); // prints the same person as "Pauline MARTIN"
    core.auto_import(doc_id).unwrap();
    let mut names = linked(&core);
    names.sort();
    assert_eq!(names, vec!["MARTIN Pauline".to_string(), "Pauline MARTIN".to_string()]);
    db(&core, |c| {
        assert_eq!(Counterparty::get(c, contact_id)?.unwrap().phone, s("0600000000"));
        Ok(())
    });
    assert!(!pending(&core, false).iter().any(|g| g.name.as_deref() == Some("Pauline MARTIN")));
}

#[test]
fn test_an_iban_is_remembered_even_when_the_printed_name_changes() {
    let core = core();
    let garage = fake_iban(7);
    let mut spec = september();
    spec.entries.push(entry(ymd(2026, 9, 20), &["VIR SEPA GARAGE DURAND", &format!("facture 123 {garage}"), "Rèf : SCT123450000000000000000005"], "-300.00"));
    first_import(&core, Some(spec.clone()));
    let groups = pending(&core, false);
    let group: Vec<_> = groups.iter().filter(|g| g.iban.as_deref() == Some(garage.as_str())).collect();
    let [group] = &group[..] else { panic!("one group expected") };
    assert_eq!((group.kind.as_str(), group.name.as_deref(), group.count), ("iban", Some("GARAGE DURAND"), 1));

    let contact_id = identify_contact(&core, &[("name", "Garage Durand"), ("kind", "company"), ("iban", &garage), ("printed_name", "GARAGE DURAND")]).unwrap().id;
    let other_spelling = vec![entry(ymd(2026, 10, 9), &["VIR SEPA DURAND SARL", &format!("{garage} entretien")], "-120.00")];
    let (doc_id, _, _) = drop(&core, &october(&spec, Some(other_spelling), ""));
    core.auto_import(doc_id).unwrap();
    let linked: BTreeSet<String> = transactions(&core, None).into_iter().filter(|t| t.counterparty_id == Some(contact_id)).filter_map(|t| t.counterparty_name).collect();
    assert_eq!(linked, ["GARAGE DURAND".to_string(), "DURAND SARL".to_string()].into_iter().collect());
}

#[test]
fn test_an_iban_cannot_be_stolen_from_another_contact() {
    let core = core();
    first_import(&core, None);
    let iban = fake_iban(3);
    identify_contact(&core, &[("name", "Marie"), ("iban", &iban)]).unwrap();
    expect_value(identify_contact(&core, &[("name", "Autre"), ("iban", &iban)]), "Déjà associé");
    expect_value(identify_contact(&core, &[("name", "Autre"), ("iban", "FR7612345678901234567890123")]), "IBAN invalide");
    db(&core, |c| {
        // refused identifiers leave no half-made contact
        assert_eq!(Counterparty::query(c, "", [])?.into_iter().map(|c| c.name).collect::<Vec<_>>(), vec!["Marie".to_string()]);
        Ok(())
    });
}

#[test]
fn test_merging_and_deleting_contacts_keeps_the_operations() {
    let core = core();
    first_import(&core, None);
    let iban = fake_iban(5);
    let a = identify_contact(&core, &[("name", "Claire"), ("printed_name", "LEROY CLAIRE")]).unwrap().id;
    let b = identify_contact(&core, &[("name", "Claire L."), ("iban", &iban), ("phone", "0700000000")]).unwrap().id;
    // "LEROY CLAIRE" and "MME LEROY CLAIRE" are the same key: both printed names land on the first contact.
    let leroy = |core: &Core| transactions(core, None).into_iter().filter(|t| t.counterparty_name.as_deref().map(|n| n.contains("LEROY")).unwrap_or(false)).collect::<Vec<_>>();
    let rows = leroy(&core);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().map(|t| t.counterparty_id).collect::<BTreeSet<_>>(), [Some(a)].into_iter().collect());
    db(&core, |c| cps::merge(c, b, a));
    db(&core, |c| {
        let merged = Counterparty::get(c, a)?.unwrap();
        assert!(Counterparty::get(c, b)?.is_none());
        let kinds: BTreeSet<String> = cps::identifiers_of(c, a)?.into_iter().map(|i| i.kind).collect();
        assert_eq!(kinds, ["name".to_string(), "iban".to_string()].into_iter().collect());
        assert_eq!(merged.phone, s("0700000000"));
        Ok(())
    });
    db(&core, |c| cps::delete_contact(c, a));
    let rows = leroy(&core);
    assert!(rows.len() == 2 && rows.iter().all(|t| t.counterparty_id.is_none()));
}

// ------------------------------------------------------------------------------------------------- web
fn upload(client: &mut Client, csrf: &str, data: &[u8], name: &str) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

fn capture(page: &crate::testkit::env::Page, pattern: &str) -> String {
    page.find(pattern).unwrap_or_else(|| panic!("{pattern} not found in {}", page.text()))
}

fn import_url_doc(page: &crate::testkit::env::Page) -> i64 {
    capture(page, r"/documents/(\d+)/import/0").parse().unwrap()
}

fn import_new(client: &mut Client, csrf: &str, doc_id: i64, name: &str) -> crate::testkit::env::Page {
    client.post(&format!("/documents/{doc_id}/import/0"), &[("csrf", csrf), ("account_id", "new"), ("name", name), ("import_transactions", "on")])
}

#[test]
fn test_web_drop_review_import_then_next_month_automatically() {
    let mut client = client();
    let csrf = client.login();
    let spec = september();
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&spec), "releve.pdf");
    assert_eq!(page.status, 200);
    let text = page.text();
    let rib = format!("{BANK} {BRANCH} {ACCOUNT}");
    for expected in ["M DUPONT JEAN", "12 RUE DES LILAS", &iban(), "BOUSFRPPXXX", &rib, "Valider l'import", "BoursoBank · extrait de compte mensuel"] {
        assert!(text.contains(expected), "{expected}");
    }
    assert!(text.contains("les 14 opérations lues sont complètes"));
    let doc_id = import_url_doc(&page);

    let done = client.post(
        &format!("/documents/{doc_id}/import/0"),
        &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte courant"), ("institution", "BoursoBank"), ("account_type", "checking"), ("update_balance", "on"), ("balance", &spec.closing().to_string()), ("import_transactions", "on")],
    );
    assert!(done.status == 200 && done.text().contains("Import terminé"));
    for expected in ["Titulaire et coordonnées", "M DUPONT JEAN", &rib, "Relevés importés", "TRAINS DE FRANCE"] {
        assert!(done.text().contains(expected), "{expected}");
    }

    let auto = upload(&mut client, &csrf, &boursobank_statement_pdf(&october(&spec, None, "")), "releve.pdf");
    assert!(auto.text().contains("Importé automatiquement") && auto.text().contains("2 opération(s) ajoutée(s)"));

    let same_bytes = upload(&mut client, &csrf, &boursobank_statement_pdf(&october(&spec, None, "")), "copie.pdf"); // the very same file again
    assert!(same_bytes.text().contains("strictement identique"));
    let again = upload(&mut client, &csrf, &boursobank_statement_pdf(&october(&spec, None, "second download")), "copie.pdf"); // another download
    assert!(again.text().contains("Doublon : aucune différence"));
}

#[test]
fn test_web_same_file_twice_and_download() {
    let mut client = client();
    let csrf = client.login();
    let data = boursobank_statement_pdf(&september());
    let first = upload(&mut client, &csrf, &data, "releve.pdf");
    let doc_id = import_url_doc(&first);
    let second = upload(&mut client, &csrf, &data, "autre-nom.pdf");
    assert!(second.text().contains("strictement identique") && second.text().contains(&format!("/documents/{doc_id}/import/0")));
    let download = client.get(&format!("/documents/{doc_id}/download"));
    assert_eq!(download.body, data);
    assert!(download.header("content-disposition").unwrap().contains("attachment"));
}

#[test]
fn test_web_contacts_flow() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&september()), "releve.pdf");
    let doc_id = import_url_doc(&page);
    let done = import_new(&mut client, &csrf, doc_id, "CC");
    let account_url = format!("/accounts/{}", capture(&done, r"/accounts/(\d+)").parse::<i64>().unwrap());

    let contacts = client.get("/contacts").text();
    assert!(contacts.contains("À identifier") && contacts.contains("MARTIN Pauline"));
    let r = client.post("/contacts/identify", &[("csrf", &csrf), ("next", &account_url), ("name", "Pauline"), ("kind", "person"), ("phone", "0600000000"), ("printed_name", "MARTIN Pauline"), ("iban", "")]);
    assert!(r.status == 200 && r.text().contains("les prochains imports le reconnaîtront"));
    assert!(r.text().contains("href=\"/contacts/") && r.text().contains(">Pauline<"));

    let contact_id = capture(&r, r#"href="/contacts/(\d+)""#);
    let detail = client.get(&format!("/contacts/{contact_id}")).text();
    assert!(detail.contains("0600000000") && detail.contains("MARTIN Pauline") && detail.contains("cadeau anniversaire")); // the linked operation
    let page = client.get("/contacts/identify?name=MARTIN%20Pauline&iban=&next=//evil.example");
    assert!(page.status == 200 && page.text().contains("MARTIN Pauline") && !page.text().contains("evil.example")); // only local return addresses
}

#[test]
fn test_web_unknown_file_and_choice_page() {
    let mut client = client();
    let csrf = client.login();
    let r = upload(&mut client, &csrf, b"nothing useful here", "notes.bin");
    assert!(r.status == 200 && r.text().contains("Format non reconnu") && r.text().contains("Changer de lecture"));
    let doc_id = capture(&r, r"/documents/(\d+)/analyze");
    let r = client.post(&format!("/documents/{doc_id}/analyze"), &[("csrf", &csrf), ("adapter", "generic.csv")]);
    assert!(r.status == 200 && r.text().contains("Lecture impossible"));
}

#[test]
fn test_web_requires_login_for_contacts_and_documents() {
    let mut client = client();
    for path in ["/contacts", "/documents", "/contacts/identify"] {
        assert_eq!(client.get_raw(path).status, 303, "{path}");
    }
}

#[test]
fn test_web_account_identity_can_be_corrected_by_hand() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&september()), "releve.pdf");
    let doc_id = import_url_doc(&page);
    let done = import_new(&mut client, &csrf, doc_id, "CC");
    let account_id = capture(&done, r"/accounts/(\d+)");
    let form_page = client.get(&format!("/accounts/{account_id}/edit"));
    let text = form_page.text();
    // the holder is offered person by person, read from the statement: civility, last name, first name
    assert!(form_page.status == 200 && text.contains("75011 PARIS") && text.contains("name=\"rib_key\""));
    assert!(text.contains("name=\"holder_0_last_0\" value=\"DUPONT\"") && text.contains("name=\"holder_0_first_0\" value=\"JEAN\""));
    let r = client.post(
        &format!("/accounts/{account_id}/edit"),
        &[
            ("csrf", &csrf), ("name", "Compte courant"), ("account_type", "checking"), ("holder_address", "3 PLACE DU MARCHE\n69001 LYON"), ("iban", &iban()), ("bic", "BOUSFRPPXXX"),
            ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN"),
            ("holder_1_civility", "MME"), ("holder_1_last_0", "DUPONT"), ("holder_1_birth_0", "MARTIN"), ("holder_1_first_0", "ANNE"),
        ],
    );
    assert!(r.status == 200 && r.text().contains("M DUPONT JEAN OU MME DUPONT ANNE") && r.text().contains("69001 LYON"));
}

#[test]
fn test_web_other_pages_render() {
    let mut client = client();
    let csrf = client.login();
    assert_eq!(client.get("/").status, 200);
    assert!(client.get("/").text().contains("Déposer un document"));
    assert_eq!(client.get("/settings").status, 200);
    let documents = client.get("/documents").text();
    assert!(documents.contains("Documents pris en charge") && !documents.contains("boursobank.statement_pdf"));
    assert_eq!(client.get("/contacts/new").status, 200);
    let created = client.post("/contacts/new", &[("csrf", &csrf), ("name", "Claire"), ("kind", "person"), ("phone", "0700000000"), ("iban", &fake_iban(9))]);
    assert!(created.status == 200 && created.text().contains("Contact créé") && created.text().contains(&fake_iban(9)));
    assert_eq!(client.get_raw("/connections").status, 404); // connectors are gone
}

#[test]
fn test_web_asks_which_reading_to_use_when_two_fit() {
    let mut client = client();
    let _patched = TwoAdapters::new();
    let csrf = client.login();
    let r = upload(&mut client, &csrf, &boursobank_statement_pdf(&september()), "releve.pdf");
    let text = r.text();
    assert!(text.contains("Quelle lecture choisir ?") && text.contains("Acme A") && text.contains("Acme B") && text.contains("looks like B"));
    let doc_id = capture(&r, r"/documents/(\d+)/analyze");
    let r = client.post(&format!("/documents/{doc_id}/analyze"), &[("csrf", &csrf), ("adapter", "test.b")]);
    let text = r.text();
    assert!(r.status == 200 && text.contains("Valider l'import") && text.contains("Lu avec") && text.contains("Acme B"));
}

#[test]
fn test_a_savings_account_without_iban_is_recognised_by_its_account_number() {
    let core = core();
    first_import(&core, None); // a current account already exists: the savings plan must not be mistaken for it
    let spec = SavingsSpec::new(vec![entry(ymd(2026, 9, 28), &["VIR DUPONT JEAN"], "45.00")]);
    let data = boursobank_savings_pdf(&spec);
    let (doc_id, _) = core.store_document("pel.pdf", &data).unwrap();
    assert_eq!(core.analyze_document(doc_id, None, Some(data)).unwrap(), "parsed");
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(plan.account.is_none() && plan.candidates.is_empty()); // first time: a new account is proposed
    assert!(core.auto_import(doc_id).unwrap().is_none());
    let result = core
        .import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "PEL"), ("account_type", "pel"), ("import_transactions", "on"), ("update_balance", "on"), ("balance", &spec.closing().to_string())]))
        .unwrap();
    let pel_id = num(&result, "account_id");
    db(&core, |c| {
        let pel = Account::get(c, pel_id)?.unwrap();
        assert_eq!((pel.account_type.clone(), pel.account_number.clone(), pel.iban.clone(), pel.holder_name.clone()), ("pel".to_string(), s("00099999999"), None, s("DUPONT JEAN")));
        assert_eq!(pel.institution, s("BoursoBank"));
        assert_eq!(pel.balance, d("3045.00"));
        Ok(())
    });

    let nxt = SavingsSpec { opening: spec.closing(), period: (ymd(2026, 10, 1), ymd(2026, 10, 31)), ..SavingsSpec::new(vec![entry(ymd(2026, 10, 28), &["VIR DUPONT JEAN"], "45.00")]) };
    let data2 = boursobank_savings_pdf(&nxt);
    let (doc2, _) = core.store_document("pel2.pdf", &data2).unwrap();
    core.analyze_document(doc2, None, Some(data2)).unwrap();
    assert_eq!(Value::Object(core.auto_import(doc2).unwrap().unwrap()), json!({"account_id": pel_id, "added": 1, "skipped_exact": 0, "skipped_probable": 0}));
    db(&core, |c| {
        assert_eq!(Account::get(c, pel_id)?.unwrap().balance, nxt.closing());
        assert_eq!(Account::query(c, "", [])?.len(), 2);
        Ok(())
    });
}

#[test]
fn test_joint_account_asks_for_the_full_names_once_and_recognises_transfers_between_my_accounts() {
    let core = core();
    let to_joint = entry(ymd(2026, 9, 1), &["VIR Payer les factures", "M DUPONT OU MME LEROY CLAIRE"], "-50.00"); // printed as the bank cuts it
    let to_partner = entry(ymd(2026, 9, 2), &["VIR SEPA LEROY CLAIRE", "cadeau"], "-20.00");
    let (_, checking_id) = first_import(&core, Some(Spec::new(vec![to_joint, to_partner])));
    assert!(!transactions(&core, None).iter().any(|t| t.internal())); // the joint account is not known yet

    let joint = Spec {
        holder: joint_holder(),
        address: joint_address(),
        bank: BANK.into(),
        branch: "80999".into(),
        account: "00077777777".into(),
        ..Spec::new(vec![
            entry(ymd(2026, 9, 1), &["VIR Payer les factures", "DUPONT JEAN"], "50.00"),
            entry(ymd(2026, 9, 3), &["VIR Virement depuis BoursoBank", "LEROY CLAIRE"], "200.00"),
        ])
    };
    let (doc_id, _, _) = drop(&core, &joint);
    let review = core.review_document(doc_id).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    assert!(plan.account.is_none() && plan.ask_holder && !plan.clean());
    let result = core
        .import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte joint"), ("import_transactions", "on"), ("update_balance", "on"), ("balance", &joint.closing().to_string()), ("holder_name", "M JEAN DUPONT OU MME CLAIRE LEROY")]))
        .unwrap();
    let joint_id = num(&result, "account_id");
    db(&core, |c| {
        let account = Account::get(c, joint_id)?.unwrap();
        assert_eq!(account.holder_name, s("M JEAN DUPONT OU MME CLAIRE LEROY")); // what the user typed
        assert_eq!(account.holder_alias, s("M DUPONT OU MME LEROY CLAIRE")); // what the bank prints, kept to recognise it elsewhere
        assert_eq!(account.holder_address, s("12 RUE DES LILAS\nBAT B APPT 12\n75011 PARIS"));
        Ok(())
    });

    let by_name: HashMap<(i64, String), Transaction> = transactions(&core, None).into_iter().map(|t| ((t.account_id, t.counterparty_name.clone().unwrap_or_default()), t)).collect();
    assert!(by_name[&(checking_id, "M DUPONT OU MME LEROY CLAIRE".to_string())].internal()); // the earlier transfer to the joint account is now "mine"
    assert!(!by_name[&(checking_id, "LEROY CLAIRE".to_string())].internal()); // the partner's name is not the user's
    assert!(by_name[&(joint_id, "DUPONT JEAN".to_string())].internal()); // "me" on the joint account, known from the other account's holder
    assert!(!by_name[&(joint_id, "LEROY CLAIRE".to_string())].internal());

    // Next month: no question any more, the full names survive, and it imports by itself.
    let nxt = Spec {
        opening: joint.closing(),
        opening_date: ymd(2026, 9, 30),
        period: (ymd(2026, 10, 1), ymd(2026, 10, 31)),
        holder: joint_holder(),
        address: joint_address(),
        bank: BANK.into(),
        branch: "80999".into(),
        account: "00077777777".into(),
        ..Spec::new(vec![entry(ymd(2026, 10, 2), &["VIR SEPA ACME CONSULTING SAS", "Salaire"], "100.00")])
    };
    let (doc2, _, _) = drop(&core, &nxt);
    let review2 = core.review_document(doc2).unwrap().unwrap();
    let [plan2] = &review2.plans[..] else { panic!("one plan expected") };
    assert!(plan2.account.as_ref().unwrap().id == joint_id && !plan2.ask_holder);
    assert_eq!(num(&core.auto_import(doc2).unwrap().unwrap(), "added"), 1);
    db(&core, |c| {
        assert_eq!(Account::get(c, joint_id)?.unwrap().holder_name, s("M JEAN DUPONT OU MME CLAIRE LEROY"));
        Ok(())
    });
}

#[test]
fn test_opposite_transfers_on_two_of_my_accounts_are_paired_whatever_the_names() {
    let core = core();
    let out = entry(ymd(2026, 9, 5), &["VIR Payer les factures", "COMPTE COMMUN"], "-50.00"); // the other account is named by its label
    let stranger = entry(ymd(2026, 9, 6), &["VIR SEPA PLOMBIER SARL", "facture"], "-90.00");
    let (_, checking_id) = first_import(&core, Some(Spec::new(vec![out, stranger])));
    let other = Spec {
        holder: vec!["MME LEROY CLAIRE".into()],
        bank: BANK.into(),
        branch: "80999".into(),
        account: "00077777777".into(),
        ..Spec::new(vec![
            entry(ymd(2026, 9, 6), &["VIR Virement depuis BoursoBank", "UN AUTRE NOM"], "50.00"), // a day later: still the same transfer
            entry(ymd(2026, 9, 7), &["VIR SEPA GARAGISTE", "pneus"], "-90.00"), // same amount as the plumber but outgoing: not a pair
        ])
    };
    let (doc_id, _, _) = drop(&core, &other);
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Autre"), ("import_transactions", "on")])).unwrap();
    let other_id = num(&result, "account_id");
    let rows: HashMap<(i64, Decimal), Transaction> = transactions(&core, None).into_iter().map(|t| ((t.account_id, t.amount), t)).collect();
    let (sent, received) = (&rows[&(checking_id, d("-50.00"))], &rows[&(other_id, d("50.00"))]);
    assert!(sent.internal() && received.internal());
    assert_eq!(sent.details.as_ref().unwrap()["paired_with"], json!(received.id));
    assert_eq!(received.details.as_ref().unwrap()["paired_with"], json!(sent.id));
    assert!(!rows[&(checking_id, d("-90.00"))].internal() && !rows[&(other_id, d("-90.00"))].internal());

    core.undo_import(doc_id).unwrap(); // the other half leaves: the transfer is no longer "internal" nor paired
    let freed = transactions(&core, None).into_iter().find(|t| t.id == sent.id).unwrap();
    assert!(!freed.internal());
    assert!(freed.details.as_ref().map(|d| d.get("paired_with").is_none()).unwrap_or(true));
}

#[test]
fn test_web_review_asks_for_the_full_names_of_a_joint_account() {
    let mut client = client();
    let csrf = client.login();
    let spec = Spec { holder: joint_holder(), address: joint_address(), ..Spec::new(sample_entries()[..3].to_vec()) };
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&spec), "releve.pdf");
    let text = page.text();
    assert!(text.contains("Titulaire(s) du compte") && text.contains("Titulaire(s) à confirmer") && text.contains("Compte joint BoursoBank"));
    // one block per person, prefilled from the statement: the two people of the joint account are both offered
    assert!(text.contains("name=\"holder_0_last_0\" value=\"DUPONT\"") && text.contains("name=\"holder_1_last_0\" value=\"LEROY\"") && text.contains("name=\"holder_1_first_0\" value=\"CLAIRE\""));
    let sole = upload(&mut client, &csrf, &boursobank_statement_pdf(&Spec { account: "00099999999".into(), ..Spec::new(sample_entries()[..3].to_vec()) }), "releve.pdf");
    assert!(!sole.text().contains("Titulaire(s) à confirmer") && sole.text().contains("name=\"holder_0_last_0\" value=\"DUPONT\"")); // a name that is not cut is not asked
}

// ---------------------------------------------------------------------------------- duplicate files
fn blobs(core: &Core) -> usize {
    std::fs::read_dir(core.settings.documents_dir()).unwrap().count()
}

fn with_extra_line(title: &str) -> Spec {
    let mut entries = sample_entries();
    entries.push(entry(ymd(2026, 9, 20), &["CARTE 18/09/26 LIBRAIRIE DU COIN CB*1234"], "-12.50"));
    Spec { title: title.into(), ..Spec::new(entries) }
}

#[test]
fn test_a_file_with_no_difference_is_set_aside_and_can_be_kept_or_dropped() {
    let core = core();
    let (doc1, _) = first_import(&core, None);
    let (doc2, existing, status) = drop(&core, &september_with(|sp| sp.title = "downloaded again".into()));
    assert_eq!((existing, status.as_str()), (false, "parsed")); // another file (another hash), the same statement
    assert_eq!(core.check_duplicate(doc2).unwrap(), Some("identical"));
    db(&core, |c| {
        let doc = Document::get(c, doc2)?.unwrap();
        assert_eq!((doc.status.as_str(), doc.duplicate_of), ("duplicate", Some(doc1)));
        Ok(())
    });
    assert!(core.auto_import(doc2).unwrap().is_none());
    assert_eq!(transactions(&core, None).len(), sample_entries().len());

    core.keep_duplicate(doc2).unwrap(); // "keep it anyway": a copy next to the original, nothing imported
    db(&core, |c| {
        assert_eq!(Document::get(c, doc2)?.unwrap().status, "copy");
        Ok(())
    });
    assert_eq!(transactions(&core, None).len(), sample_entries().len());

    let (doc3, _, _) = drop(&core, &september_with(|sp| sp.title = "and again".into()));
    assert_eq!(core.check_duplicate(doc3).unwrap(), Some("identical"));
    let before = blobs(&core);
    core.delete_document(doc3).unwrap(); // dropped
    db(&core, |c| {
        assert!(Document::get(c, doc3)?.is_none());
        Ok(())
    });
    assert_eq!(blobs(&core), before - 1);
}

#[test]
fn test_duplicates_set_aside_are_deleted_after_a_day_unless_kept() {
    let core = core();
    first_import(&core, None);
    let old = drop(&core, &september_with(|sp| sp.title = "old".into())).0;
    let recent = drop(&core, &september_with(|sp| sp.title = "recent".into())).0;
    let kept = drop(&core, &september_with(|sp| sp.title = "kept".into())).0;
    for doc_id in [old, recent, kept] {
        assert_eq!(core.check_duplicate(doc_id).unwrap(), Some("identical"));
    }
    core.keep_duplicate(kept).unwrap();
    db(&core, |c| {
        for doc_id in [old, kept] {
            let mut doc = Document::get(c, doc_id)?.unwrap();
            let before = doc.clone();
            doc.uploaded_at = utcnow() - Duration::hours(25);
            doc.save(c, &before)?;
        }
        Ok(())
    });
    let files = blobs(&core);
    assert_eq!(core.purge_quarantine().unwrap(), 1); // only the old one that nobody kept
    db(&core, |c| {
        assert!(Document::get(c, old)?.is_none());
        assert_eq!(Document::get(c, recent)?.unwrap().status, "duplicate");
        assert_eq!(Document::get(c, kept)?.unwrap().status, "copy");
        Ok(())
    });
    assert_eq!(blobs(&core), files - 1);
}

#[test]
fn test_a_file_with_extra_lines_is_reported_and_can_be_stored_with_the_old_one() {
    let core = core();
    let (first, account_id) = first_import(&core, None);
    let n = sample_entries().len();
    let new_spec = with_extra_line("");
    let (doc2, _, _) = drop(&core, &new_spec);
    assert_eq!(core.check_duplicate(doc2).unwrap(), Some("differs"));
    let review = core.review_document(doc2).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    let dd = plan.dedup.as_ref().unwrap();
    assert!(dd.document.as_ref().unwrap().id == first && dd.can_replace() && !dd.identical());
    assert_eq!(dd.extra.iter().map(|i| plan.statement.transactions[*i].label.clone()).collect::<Vec<_>>(), vec!["LIBRAIRIE DU COIN".to_string()]);
    assert!(dd.new_count == 1 && dd.probable_count == 0);
    assert!(dd.missing.is_empty() && dd.opening.is_none());
    assert_eq!(dd.closing, Some((september().closing(), new_spec.closing())));
    assert!(core.auto_import(doc2).unwrap().is_none());
    db(&core, |c| {
        let doc = Document::get(c, doc2)?.unwrap();
        assert!(doc.status == "parsed" && doc.duplicate_of == Some(first)); // kept for the user's decision
        Ok(())
    });

    let result = core.import_default(doc2, 0, true).unwrap(); // "store with the old one": both files stay, only the new line is imported
    assert_eq!((num(&result, "added"), num(&result, "skipped_exact")), (1, n as i64));
    db(&core, |c| {
        assert!(Document::get(c, first)?.is_some() && Document::get(c, doc2)?.is_some());
        assert_eq!(Statement::query(c, "", [])?.len(), 1); // still one statement for the period
        assert_eq!(Account::get(c, account_id)?.unwrap().balance, new_spec.closing());
        Ok(())
    });
    assert_eq!(transactions(&core, None).len(), n + 1);
}

#[test]
fn test_replacing_the_old_file_swaps_it_and_what_it_brought() {
    let core = core();
    let (first, account_id) = first_import(&core, None);
    let n = sample_entries().len();
    let new_spec = with_extra_line("");
    let (doc2, _, _) = drop(&core, &new_spec);
    core.check_duplicate(doc2).unwrap();
    let files = blobs(&core);
    let result = core.replace_document(doc2, 0).unwrap();
    assert_eq!((result["replaced"].clone(), num(&result, "removed"), num(&result, "added")), (json!("BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf"), n as i64, n as i64 + 1));
    db(&core, |c| {
        assert!(Document::get(c, first)?.is_none()); // the old file is gone...
        let doc = Document::get(c, doc2)?.unwrap();
        assert!(doc.status == "imported" && doc.duplicate_of.is_none());
        assert_eq!(Statement::query(c, "", [])?.into_iter().map(|st| st.document_id).collect::<Vec<_>>(), vec![Some(doc2)]); // ...and so is its statement record
        assert_eq!(Account::get(c, account_id)?.unwrap().balance, new_spec.closing());
        Ok(())
    });
    assert_eq!(blobs(&core), files - 1);
    let rows = transactions(&core, None);
    assert!(rows.len() == n + 1 && rows.iter().all(|t| t.document_id == Some(doc2)));
}

#[test]
fn test_a_corrected_file_that_lost_a_line_shows_it_as_missing_and_replacing_drops_it() {
    let core = core();
    first_import(&core, None);
    let n = sample_entries().len();
    let corrected = Spec::new(sample_entries()[..n - 1].to_vec()); // the bank removed the last line
    let (doc2, _, _) = drop(&core, &corrected);
    assert_eq!(core.check_duplicate(doc2).unwrap(), Some("differs"));
    let review = core.review_document(doc2).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    let dd = plan.dedup.as_ref().unwrap();
    assert_eq!(dd.missing.iter().map(|t| t.label.clone()).collect::<Vec<_>>(), vec!["CARREFOUR CITY".to_string()]);
    assert!(dd.extra.is_empty());
    core.replace_document(doc2, 0).unwrap();
    let rows = transactions(&core, None);
    assert!(rows.len() == n - 1 && !rows.iter().any(|t| t.label == "CARREFOUR CITY"));
}

#[test]
fn test_nothing_to_replace_when_the_old_file_was_deleted_but_both_can_still_be_kept() {
    let core = core();
    let (first, _) = first_import(&core, None);
    core.delete_document(first).unwrap(); // the statement record stays, its file does not
    let (doc2, _, _) = drop(&core, &with_extra_line(""));
    assert_eq!(core.check_duplicate(doc2).unwrap(), Some("differs"));
    let review = core.review_document(doc2).unwrap().unwrap();
    let [plan] = &review.plans[..] else { panic!("one plan expected") };
    let dd = plan.dedup.as_ref().unwrap();
    assert!(dd.document.is_none() && !dd.can_replace());
    expect_value(core.replace_document(doc2, 0), "n'existe plus");
    assert_eq!(num(&core.import_default(doc2, 0, true).unwrap(), "added"), 1);
}

#[test]
fn test_a_new_period_or_another_account_is_not_a_duplicate() {
    let core = core();
    let spec = september();
    first_import(&core, Some(spec.clone()));
    let (nxt, _, _) = drop(&core, &october(&spec, None, ""));
    assert_eq!(core.check_duplicate(nxt).unwrap(), None);
    let other_account = Spec { bank: BANK.into(), branch: "80999".into(), account: "00077777777".into(), ..Spec::new(sample_entries()) };
    let (doc, _, _) = drop(&core, &other_account);
    assert_eq!(core.check_duplicate(doc).unwrap(), None); // same period but another account: nothing to compare with
}

#[test]
fn test_web_identical_download_is_set_aside_and_listed() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&september()), "releve.pdf");
    let doc_id = import_url_doc(&page);
    import_new(&mut client, &csrf, doc_id, "CC");

    let again = upload(&mut client, &csrf, &boursobank_statement_pdf(&september_with(|sp| sp.title = "second download".into())), "copie.pdf");
    let text = again.text();
    assert!(text.contains("Doublon : aucune différence") && text.contains("Le garder quand même") && !text.contains("Valider l'import"));
    let dup_id = capture(&again, r"/documents/(\d+)/keep");
    let listing = client.get("/documents").text();
    assert!(listing.contains("Doublons écartés") && listing.contains("copie.pdf"));
    assert_eq!(listing.matches("copie.pdf").count(), 1); // shown in the set-aside section, not in the library table

    let kept = client.post(&format!("/documents/{dup_id}/keep"), &[("csrf", &csrf)]);
    assert!(kept.text().contains("Copie conservée"));
    assert!(!client.get("/documents").text().contains("Doublons écartés"));
    let third = upload(&mut client, &csrf, &boursobank_statement_pdf(&september_with(|sp| sp.title = "third download".into())), "troisieme.pdf");
    let third_id = capture(&third, r"/documents/(\d+)/keep");
    let dropped = client.post(&format!("/documents/{third_id}/delete"), &[("csrf", &csrf)]);
    assert!(!dropped.text().contains("troisieme.pdf"));
}

#[test]
fn test_web_file_with_differences_offers_replace_keep_or_discard() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&september()), "releve.pdf");
    let doc_id = import_url_doc(&page);
    import_new(&mut client, &csrf, doc_id, "CC");

    let diff = upload(&mut client, &csrf, &boursobank_statement_pdf(&with_extra_line("")), "releve.pdf");
    for expected in ["diffère", "1 opération(s) en plus", "LIBRAIRIE DU COIN", "Remplacer l'ancien", "Stocker avec l'ancien", "Écarter ce fichier", "Solde final"] {
        assert!(diff.text().contains(expected), "{expected}");
    }
    let new_id = capture(&diff, r"/documents/(\d+)/dedup/0");

    let replaced = client.post(&format!("/documents/{new_id}/dedup/0"), &[("csrf", &csrf), ("action", "replace")]);
    assert!(replaced.text().contains("remplacé") && replaced.text().contains("LIBRAIRIE DU COIN"));
    let names = client.get("/documents").text();
    assert_eq!(names.matches("BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf").count(), 1); // one file left: the new one took the old one's place

    // Discarding a differing file leaves the old one untouched.
    let n = sample_entries().len();
    let other = upload(&mut client, &csrf, &boursobank_statement_pdf(&Spec::new(sample_entries()[..n - 1].to_vec())), "releve.pdf");
    let discard_id = capture(&other, r"/documents/(\d+)/dedup/0");
    let out = client.post(&format!("/documents/{discard_id}/dedup/0"), &[("csrf", &csrf), ("action", "discard")]);
    let account_id = capture(&replaced, r"/accounts/(\d+)");
    assert!(out.text().contains("Nouveau fichier écarté") && client.get(&format!("/accounts/{account_id}")).text().contains("LIBRAIRIE DU COIN"));
}

#[test]
fn test_web_upload_summary_counts_duplicates() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, &boursobank_statement_pdf(&september()), "releve.pdf");
    let doc_id = import_url_doc(&page);
    import_new(&mut client, &csrf, doc_id, "CC");
    let a = boursobank_statement_pdf(&september_with(|sp| sp.title = "a".into()));
    let b = boursobank_statement_pdf(&with_extra_line("b"));
    let r = client.post_files(
        "/documents",
        &[("csrf", &csrf)],
        &[
            Upload { field: "files", filename: "a.pdf", data: &a, content_type: "application/pdf" },
            Upload { field: "files", filename: "b.pdf", data: &b, content_type: "application/pdf" },
        ],
    );
    assert!(r.text().contains("1 doublon(s) sans différence écarté(s)") && r.text().contains("1 déjà importé(s) mais différent(s)"));
}
