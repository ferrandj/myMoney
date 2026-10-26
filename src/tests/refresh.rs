//! "Actualiser à la dernière version" and the documents page: the library by account, the link of each file.

use regex::Regex;
use serde_json::json;

use crate::app::MyMoney;
use crate::error::AppError;
use crate::models::{Account, Document, Transaction};
use crate::testkit::env::{client, core, Client, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::{db, drop_file, form};

const STD: &str = "BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf";

fn upload(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

/// Make a document look like one read by an older version, with the name it had then.
fn age(core: &MyMoney, doc_id: i64, filename: Option<&str>) {
    db(core, |c| {
        let mut doc = Document::get(c, doc_id)?.unwrap();
        let before = doc.clone();
        let mut result = doc.parse_result.clone().unwrap();
        result["reader"] = json!("old00000");
        doc.parse_result = Some(result);
        if let Some(name) = filename {
            doc.filename = name.into();
            doc.original_filename = None;
        }
        doc.save(c, &before)?;
        Ok(())
    });
}

fn document(core: &MyMoney, doc_id: i64) -> Document {
    db(core, |c| Ok(Document::get(c, doc_id)?.unwrap()))
}

fn import(core: &MyMoney, name: &str, spec: &Spec) -> i64 {
    let id = drop_file(core, name, &boursobank_statement_pdf(spec));
    core.import_statement(id, 0, &form(&[("account_id", "new"), ("name", "Compte courant"), ("import_transactions", "on")])).unwrap();
    id
}

#[test]
fn nothing_is_stale_when_everything_was_read_by_the_current_version() {
    let core = core();
    import(&core, "releve.pdf", &september());
    assert!(core.stale_documents().unwrap().is_empty());
    assert_eq!(core.refresh_documents(None).unwrap(), (0, 0));
}

#[test]
fn a_validated_document_is_read_again_without_touching_its_operations() {
    let core = core();
    let id = import(&core, "releve.pdf", &september());
    let operations = db(&core, |c| Ok(Transaction::query(c, "ORDER BY id", [])?.into_iter().map(|t| (t.id, t.label, t.amount)).collect::<Vec<_>>()));
    // imported by a former version: its file keeps the name it came with, and was read by older code
    age(&core, id, Some("releve.pdf"));
    assert_eq!(core.stale_documents().unwrap().iter().map(|d| d.id).collect::<Vec<_>>(), vec![id]);
    assert_eq!(core.refresh_documents(None).unwrap(), (1, 0));
    let doc = document(&core, id);
    assert_eq!(doc.status, "imported");
    assert!(core.reading_is_current(&doc));
    assert_eq!((doc.filename.as_str(), doc.original_filename.as_deref()), (STD, Some("releve.pdf"))); // the standard name, the old one kept
    let after = db(&core, |c| Ok(Transaction::query(c, "ORDER BY id", [])?.into_iter().map(|t| (t.id, t.label, t.amount)).collect::<Vec<_>>()));
    assert_eq!(after, operations);
    assert!(core.stale_documents().unwrap().is_empty());
}

#[test]
fn a_waiting_document_is_read_again_and_keeps_its_name() {
    let core = core();
    let id = drop_file(&core, "attente.pdf", &boursobank_statement_pdf(&september()));
    age(&core, id, None);
    assert_eq!(core.refresh_documents(None).unwrap(), (1, 0));
    let doc = document(&core, id);
    assert_eq!((doc.status.as_str(), doc.filename.as_str()), ("parsed", "attente.pdf"));
    assert!(core.reading_is_current(&doc));
}

#[test]
fn a_reading_that_fails_leaves_the_old_one_in_place() {
    let core = core();
    let id = import(&core, "releve.pdf", &september());
    // the user once chose a reading that cannot read this file
    db(&core, |c| {
        let mut doc = Document::get(c, id)?.unwrap();
        let before = doc.clone();
        let mut result = doc.parse_result.clone().unwrap();
        result["reader"] = json!("old00000");
        result["decision"] = json!("chosen");
        result["adapter"] = json!("generic.csv");
        doc.parse_result = Some(result);
        doc.save(c, &before)?;
        Ok(())
    });
    match core.refresh_document(id) {
        Err(AppError::Value(m)) => assert!(m.contains("l'ancienne lecture est conservée"), "{m}"),
        other => panic!("{other:?}"),
    }
    let doc = document(&core, id);
    assert_eq!(doc.status, "imported");
    assert_eq!(doc.parse_result.unwrap()["reader"], json!("old00000"));
    assert_eq!(core.refresh_documents(None).unwrap(), (0, 1));
}

#[test]
fn the_documents_page_offers_the_button_and_it_works() {
    let mut client = client();
    let csrf = client.login();
    let core = client.core.clone();
    assert!(!client.get("/documents").text().contains("Actualiser à la dernière version"));
    let page = upload(&mut client, &csrf, "releve.pdf", &boursobank_statement_pdf(&september()));
    let doc: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&page.url).unwrap()[1].parse().unwrap();
    client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "CC"), ("import_transactions", "on")]);
    let waiting = drop_file(&core, "autre.pdf", &boursobank_statement_pdf(&Spec { title: "autre".into(), opening: d("2000.00"), period: (ymd(2027, 1, 1), ymd(2027, 1, 31)), opening_date: ymd(2026, 12, 31), ..Spec::new(vec![entry(ymd(2027, 1, 5), &["CARTE 04/01/27 SHOP CB*1234"], "-5.00")]) }));
    age(&core, doc, Some("releve.pdf"));
    age(&core, waiting, None);

    let listing = client.get("/documents").text();
    // the document still waiting is read again as the page opens; the validated one needs the button
    assert!(listing.contains("Actualiser à la dernière version (1)") && listing.contains("validés ou non"));
    assert!(core.reading_is_current(&document(&core, waiting)));
    let done = client.post("/documents/refresh", &[("csrf", &csrf)]);
    assert!(done.text().contains("1 document(s) actualisé(s) à la dernière version"));
    assert!(!client.get("/documents").text().contains("Actualiser à la dernière version"));
    assert_eq!(document(&core, doc).filename, STD);
    assert_eq!(client.post("/documents/refresh", &[]).status, 403); // CSRF
}

#[test]
fn a_document_page_offers_its_own_button() {
    let mut client = client();
    let csrf = client.login();
    let core = client.core.clone();
    let page = upload(&mut client, &csrf, "releve.pdf", &boursobank_statement_pdf(&september()));
    let doc: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&page.url).unwrap()[1].parse().unwrap();
    client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "CC"), ("import_transactions", "on")]);
    assert!(!client.get(&format!("/documents/{doc}")).text().contains("Actualiser à la dernière version"));
    age(&core, doc, Some("releve.pdf"));
    let text = client.get(&format!("/documents/{doc}")).text();
    assert!(text.contains("version plus ancienne") && text.contains("Actualiser à la dernière version") && text.contains("nom d'origine") == false);
    let done = client.post(&format!("/documents/{doc}/refresh"), &[("csrf", &csrf)]);
    let text = done.text();
    assert!(text.contains("Document actualisé à la dernière version") && !text.contains("version plus ancienne"));
    assert!(text.contains("nom d'origine : <span class=\"sensitive\">releve.pdf</span>"));
    assert_eq!(client.post("/documents/999/refresh", &[("csrf", &csrf)]).status, 404);
}

#[test]
fn the_library_is_sorted_by_account_with_a_blue_link_per_file() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve.pdf", &boursobank_statement_pdf(&september()));
    let doc: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&page.url).unwrap()[1].parse().unwrap();
    client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Mon compte courant"), ("import_transactions", "on")]);
    // a second account of the same bank
    let savings = SavingsSpec::new(vec![entry(ymd(2026, 9, 28), &["VIR DUPONT JEAN"], "45.00")]);
    let up = upload(&mut client, &csrf, "pel.pdf", &boursobank_savings_pdf(&savings));
    let pel: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&up.url).unwrap()[1].parse().unwrap();
    client.post(&format!("/documents/{pel}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Mon PEL"), ("account_type", "pel"), ("import_transactions", "on")]);

    let listing = client.get("/documents").text();
    // bank > account > type of file
    assert!(listing.contains("class=\"doc-account\""));
    let account_a = listing.find("Mon compte courant").unwrap();
    let account_b = listing.find("Mon PEL").unwrap();
    assert!(account_a < account_b); // sorted by account
    // "Type de fichier - JJ/MM/AAAA" is the link to the document, the file name and the names come next to it
    assert!(listing.contains(&format!("<a class=\"doc-link\" href=\"/documents/{doc}\">Relevé de compte - 30/09/2026</a>")));
    let pel_link = Regex::new(&format!(r#"<a class="doc-link" href="/documents/{pel}">([^<]*)</a>"#)).unwrap().captures(&listing).unwrap()[1].to_string();
    assert_eq!(pel_link, "Relevé d&#39;épargne - 30/09/2026"); // (the apostrophe is escaped in the page)
    assert!(listing.contains(&format!("<span class=\"sensitive\">{STD}</span>")));
    assert!(listing.contains("<span class=\"sensitive\">M DUPONT JEAN</span>"));
}

#[test]
fn one_account_can_be_invalidated_on_its_own() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve.pdf", &boursobank_statement_pdf(&september()));
    let doc: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&page.url).unwrap()[1].parse().unwrap();
    let done = client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "CC"), ("import_transactions", "on")]);
    let account: i64 = Regex::new(r"/accounts/(\d+)").unwrap().captures(&done.url).unwrap()[1].parse().unwrap();
    let savings = SavingsSpec::new(vec![entry(ymd(2026, 9, 28), &["VIR DUPONT JEAN"], "45.00")]);
    let up = upload(&mut client, &csrf, "pel.pdf", &boursobank_savings_pdf(&savings));
    let pel: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&up.url).unwrap()[1].parse().unwrap();
    client.post(&format!("/documents/{pel}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "PEL"), ("account_type", "pel"), ("import_transactions", "on")]);
    assert!(client.get("/documents").text().contains(&format!("value=\"account:{account}\"")));

    let out = client.post("/documents/invalidate", &[("csrf", &csrf), ("scope", &format!("account:{account}"))]);
    assert!(out.text().contains("1 import(s) invalidé(s)"));
    let status = |id: i64| document(&client.core, id).status;
    assert_eq!((status(doc).as_str(), status(pel).as_str()), ("parsed", "imported")); // only that account's file went back
    assert_eq!(document(&client.core, doc).filename, "releve.pdf"); // and it took its own name back
    let _ = Account::default;
}

fn d(s: &str) -> rust_decimal::Decimal {
    dec(s)
}

#[test]
fn every_invalidate_button_is_red_and_needs_three_clicks() {
    let mut client = client();
    let csrf = client.login();
    let data = boursobank_statement_pdf(&Spec::new(sample_entries()[..3].to_vec()));
    let page = upload(&mut client, &csrf, "releve.pdf", &data);
    let doc = Regex::new(r"/documents/(\d+)").unwrap().captures(&page.url).unwrap()[1].to_string();
    client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte"), ("import_transactions", "on")]);

    let library = client.get("/documents").text();
    let buttons: Vec<&str> = Regex::new(r"<button[^>]*>(?:Invalider|Tout invalider|Annuler tous les imports)[^<]*</button>").unwrap().find_iter(&library).map(|m| m.as_str()).collect();
    assert!(buttons.len() >= 5, "selection, all, bank, account, type, rebuild: {buttons:?}");
    for b in &buttons {
        assert!(b.contains("danger") && b.contains("data-clicks=\"3\""), "{b}");
    }
    let one = client.get(&format!("/documents/{doc}")).text();
    let undo = Regex::new(r"<button[^>]*>Invalider l'import</button>").unwrap().find(&one).unwrap().as_str().to_string();
    assert!(undo.contains("danger") && undo.contains("data-clicks=\"3\""), "{undo}");

    let js = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/static/app.js")).unwrap();
    assert!(js.contains("data-clicks") && js.contains("encore"));
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/static/style.css")).unwrap();
    assert!(css.contains(".btn.danger { background: var(--neg)"));
}
