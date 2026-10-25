//! The standard name of an imported file: ETABLISSEMENT_TYPECOMPTE_TYPEFICHIER_NOM_PRENOM_DATE.ext

use chrono::NaiveDate;
use regex::Regex;

use crate::models::{Account, Document};
use crate::services::holders::Person;
use crate::services::naming::{standard_name, token, NameParts, MAX_NAME_BYTES};
use crate::testkit::env::{client, core, Client, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::{db, drop_file, form, num};

const STD: &str = "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf";

fn person(civility: &str, last: &str, first: &str) -> Person {
    Person { id: None, civility: civility.into(), last_names: vec![last.into()], birth_names: vec![], first_names: if first.is_empty() { vec![] } else { vec![first.into()] } }
}

fn parts<'a>(people: &'a [Person], day: NaiveDate) -> NameParts<'a> {
    NameParts { institution: "BoursoBank", account_type: "Compte courant", kind: "Relevé de compte", people, day, extension: ".pdf" }
}

#[test]
fn words_lose_their_accents_and_are_joined_by_dashes() {
    assert_eq!(token("Relevé d'épargne "), "RELEVE-D-EPARGNE");
    assert_eq!(token("Épargne salariale (PEE/PERCO)"), "EPARGNE-SALARIALE-PEE-PERCO");
    assert_eq!(token("  "), "");
    assert_eq!(token("Çà et là — œuvre"), "CA-ET-LA-UVRE");
}

#[test]
fn the_name_has_the_institution_the_account_the_type_the_names_and_the_date() {
    let people = [person("M", "DUPONT", "JEAN")];
    assert_eq!(standard_name(&parts(&people, ymd(2026, 9, 30)), 0), STD);
    // a joint account: both last names, then both first names
    let joint = [person("M", "DUPONT", "JEAN"), person("MME", "LEROY", "CLAIRE")];
    assert_eq!(standard_name(&parts(&joint, ymd(2026, 9, 30)), 0), "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT-LEROY_JEAN-CLAIRE_2026-09-30.pdf");
    // the same last name twice is written once; nobody: no name at all
    let couple = [person("M", "DUPONT", "JEAN"), person("MME", "DUPONT", "ANNE")];
    assert_eq!(standard_name(&parts(&couple, ymd(2026, 1, 5)), 0), "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN-ANNE_2026-01-05.pdf");
    assert_eq!(standard_name(&parts(&[], ymd(2026, 9, 30)), 0), "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_2026-09-30.pdf");
    // accents and spaces inside names
    let accented = [person("MME", "de la Fontaine", "Éloïse")];
    assert_eq!(standard_name(&parts(&accented, ymd(2026, 9, 30)), 0), "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DE-LA-FONTAINE_ELOISE_2026-09-30.pdf");
}

#[test]
fn a_name_too_long_for_a_file_loses_the_first_names_then_the_names() {
    let many = |last_len: usize, first_len: usize| -> Vec<Person> { (0..6).map(|i| person("M", &format!("{}{i}", "N".repeat(last_len)), &format!("{}{i}", "P".repeat(first_len)))).collect() };
    let day = ymd(2026, 9, 30);
    // everything fits
    let small = many(5, 5);
    let name = standard_name(&parts(&small, day), 0);
    assert!(name.contains("_N") && name.contains("_P") && name.len() <= MAX_NAME_BYTES);
    // the first names make it too long: only the names stay
    let long_first = many(10, 60);
    let name = standard_name(&parts(&long_first, day), 0);
    assert!(name.len() <= MAX_NAME_BYTES && name.contains("NNNNNNNNNN0") && !name.contains("PPPP"), "{name}");
    // even the names are too long: nothing of them stays
    let long_both = many(60, 60);
    let name = standard_name(&parts(&long_both, day), 0);
    assert_eq!(name, "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_2026-09-30.pdf");
    // room for a number is kept when asked
    let tight = many(36, 5);
    let free = standard_name(&parts(&tight, day), 0);
    let numbered = standard_name(&parts(&tight, day), 8);
    assert!(free.len() <= MAX_NAME_BYTES && numbered.len() + 8 <= MAX_NAME_BYTES);
}

fn upload(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

fn doc_named(core: &crate::testkit::env::Core, doc_id: i64) -> Document {
    db(core, |c| Ok(Document::get(c, doc_id)?.unwrap()))
}

#[test]
fn an_imported_file_takes_its_standard_name_and_gets_its_own_back_when_invalidated() {
    let core = core();
    let data = boursobank_statement_pdf(&september());
    let doc_id = drop_file(&core, "Mon relevé (1).PDF", &data);
    assert_eq!(doc_named(&core, doc_id).filename, "Mon relevé (1).PDF"); // waiting: the name it came with
    core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "CC"), ("import_transactions", "on")])).unwrap();
    let doc = doc_named(&core, doc_id);
    assert_eq!(doc.filename, STD.replace(".pdf", ".pdf")); // the extension is kept, in lower case
    assert_eq!(doc.original_filename.as_deref(), Some("Mon relevé (1).PDF"));
    assert_eq!(core.read_document(doc_id).unwrap().0, STD); // and it is what a download is called
    core.undo_import(doc_id).unwrap();
    let doc = doc_named(&core, doc_id);
    assert_eq!((doc.filename.as_str(), doc.original_filename), ("Mon relevé (1).PDF", None));
}

#[test]
fn files_of_the_same_period_do_not_share_a_name() {
    let core = core();
    let first = drop_file(&core, "a.pdf", &boursobank_statement_pdf(&september()));
    let (_, account_id) = {
        let r = core.import_statement(first, 0, &form(&[("account_id", "new"), ("name", "CC"), ("import_transactions", "on")])).unwrap();
        (first, num(&r, "account_id"))
    };
    // the same period again with an extra line, stored next to the first one
    let mut entries = sample_entries();
    entries.push(entry(ymd(2026, 9, 20), &["CARTE 18/09/26 LIBRAIRIE DU COIN CB*1234"], "-12.50"));
    let second = drop_file(&core, "b.pdf", &boursobank_statement_pdf(&Spec::new(entries)));
    core.check_duplicate(second).unwrap();
    core.import_default(second, 0, true).unwrap();
    assert_eq!(doc_named(&core, first).filename, STD);
    assert_eq!(doc_named(&core, second).filename, STD.replace(".pdf", "_2.pdf"));
    assert_eq!(db(&core, |c| Ok(Account::get(c, account_id)?.unwrap().name)), "CC");
}

#[test]
fn changing_the_account_renames_its_documents() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve.pdf", &boursobank_statement_pdf(&september()));
    let doc = Regex::new(r"/documents/(\d+)").unwrap().captures(&page.url).unwrap()[1].to_string();
    let done = client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "CC"), ("import_transactions", "on")]);
    let account_id = Regex::new(r"/accounts/(\d+)").unwrap().captures(&done.url).unwrap()[1].to_string();
    assert!(client.get("/documents").text().contains(STD));
    // the owner types the couple's names and says the account is a savings plan
    client.post(
        &format!("/accounts/{account_id}/edit"),
        &[
            ("csrf", &csrf), ("name", "Mon PEL"), ("account_type", "pel"),
            ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN"),
            ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE"),
        ],
    );
    let listing = client.get("/documents").text();
    assert!(listing.contains("BOURSOBANK_PEL_RELEVE-DE-COMPTE_DUPONT-LEROY_JEAN-CLAIRE_2026-09-30.pdf"), "{listing}");
    assert!(!listing.contains(STD));
}

#[test]
fn a_letter_without_an_account_is_named_without_the_account_type() {
    let core = core();
    let id = drop_file(&core, "fgdr.pdf", &crate::testkit::fortuneo::fortuneo_info());
    core.auto_import(id).unwrap();
    assert_eq!(doc_named(&core, id).filename, "FORTUNEO_INFORMATION_MARTIN_PAUL_2026-01-01.pdf");
}

#[test]
fn a_csv_keeps_its_extension() {
    let core = core();
    let csv = format!("IBAN;{}\nDate;Libellé;Montant\n14/09/2026;CB CARREFOUR 12/09;-14,08\n", iban());
    let bytes: Vec<u8> = csv.chars().map(crate::testkit::cp1252_byte).collect();
    let id = drop_file(&core, "Export.CSV", &bytes);
    core.import_statement(id, 0, &form(&[("account_id", "new"), ("name", "CC"), ("import_transactions", "on")])).unwrap();
    assert!(doc_named(&core, id).filename.ends_with(".csv"));
}
