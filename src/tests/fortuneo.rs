//! python/tests/test_fortuneo.py: monthly statement, yearly fees recap and the FGDR letter, on synthetic documents.

use regex::Regex;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::adapters::base::{ParsedStatement, SourceDocument};
use crate::adapters::registry::{identify, Identification};
use crate::error::AppError;
use crate::models::{Account, Document, FeeSummary, Holding, Transaction};
use crate::testkit::env::{client, core, Client, Core, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::fortuneo::*;
use crate::testkit::{db, drop_file, form, num};

fn d(s: &str) -> Decimal {
    dec(s)
}

fn parse(data: Vec<u8>) -> (Identification, Vec<ParsedStatement>) {
    let doc = SourceDocument::new("doc.pdf", data);
    let found = identify(&doc);
    let parsed = match &found.chosen {
        Some(c) => c.adapter.parse(&doc).unwrap(),
        None => vec![],
    };
    (found, parsed)
}

fn parse_one(data: Vec<u8>) -> ParsedStatement {
    let (_, mut parsed) = parse(data);
    assert_eq!(parsed.len(), 1);
    parsed.remove(0)
}

fn drop(core: &Core, data: &[u8]) -> i64 {
    drop_file(core, "doc.pdf", data)
}

fn import_statement(core: &Core, spec: &FSpec) -> (i64, i64) {
    let doc_id = drop(core, &fortuneo_statement_pdf(spec));
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte Fortuneo"), ("import_transactions", "on")])).unwrap();
    (doc_id, num(&result, "account_id"))
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn f_address_text() -> String {
    f_address().join("\n")
}

fn extra<'a>(st: &'a ParsedStatement, key: &str) -> &'a Value {
    st.extra.get(key).unwrap_or(&Value::Null)
}

fn details(t: &crate::adapters::base::TransactionData) -> Value {
    t.details.clone().map(Value::Object).unwrap_or(Value::Null)
}

// ------------------------------------------------------------------------------------------ detection
#[test]
fn test_each_document_goes_to_its_own_adapter() {
    for (data, adapter) in [
        (fortuneo_statement_pdf(&fortuneo_september()), "fortuneo.statement_pdf"),
        (fortuneo_fees_pdf(&FeesSpec::new()), "fortuneo.fees_pdf"),
        (fortuneo_info(), "fortuneo.deposit_guarantee_pdf"),
    ] {
        let found = identify(&SourceDocument::new("x.pdf", data));
        assert_eq!(found.decision, "auto");
        assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), adapter);
        assert_eq!(found.institution.as_deref(), Some("Fortuneo"));
    }
}

#[test]
fn test_the_brand_is_required_so_another_banks_look_alike_is_not_taken() {
    for data in [
        fortuneo_statement_pdf(&FSpec { brand: "AUTREBANQUE".into(), ..FSpec::new(fortuneo_entries()) }),
        fortuneo_fees_pdf(&FeesSpec { brand: "AutreBanque".into(), ..FeesSpec::new() }),
        fortuneo_info_pdf(2026, F_HOLDER, None, "AutreBanque", "", "100 000 €"),
    ] {
        let found = identify(&SourceDocument::new("x.pdf", data));
        assert!(!(found.decision == "auto" && found.chosen.as_ref().unwrap().adapter.id().starts_with("fortuneo.")));
    }
}

// ------------------------------------------------------------------------------------------ statement
#[test]
fn test_statement_is_read_and_reconciled() {
    let spec = fortuneo_september();
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!((st.institution.clone(), st.document_kind.clone(), st.account_type.clone()), (s("Fortuneo"), s("statement"), s("checking")));
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 9, 1)), Some(ymd(2026, 9, 30))));
    assert_eq!((st.opening_balance, st.balance), (Some(spec.opening), Some(spec.closing())));
    assert_eq!(st.confidence, "high");
    assert!(st.warnings.is_empty());
    assert_eq!(extra(&st, "declared_totals"), &json!({"debit": "433.50", "credit": "2400.00"}));
    assert_eq!(st.holder_name, s(F_HOLDER));
    assert_eq!(st.holder_address, Some(f_address_text()));
    assert_eq!((st.bank_code.clone(), st.branch_code.clone(), st.account_number.clone()), (s(F_BANK), s(F_BRANCH), s(F_ACCOUNT)));
    assert_eq!(st.iban, Some(f_iban()));
    assert_eq!(st.bic, s("FTNOFRP1XXX"));
    assert_eq!(st.transactions.len(), spec.entries.len());
}

#[test]
fn test_statement_operations_are_classified() {
    let st = parse_one(fortuneo_statement_pdf(&fortuneo_september()));
    let by_amount = |a: &str| st.transactions.iter().find(|t| t.amount == d(a)).unwrap();
    let own = by_amount("-60.00");
    assert_eq!(own.kind, s("transfer"));
    assert_eq!(details(own)["internal"], json!(true));
    let card = by_amount("-3.50");
    assert_eq!(card.kind, s("card"));
    assert_eq!(card.counterparty, s("Boulangerie Test Lyon"));
    assert_eq!(details(card)["purchase_date"], json!("2026-09-12"));
    assert_eq!(card.date, ymd(2026, 9, 14));
    assert_eq!(card.value_date, Some(ymd(2026, 9, 13)));
    let loan = by_amount("-250.00");
    assert_eq!(loan.kind, s("loan_payment"));
    assert_eq!(details(loan)["loan_number"], json!("0000TEST00000001"));
    let debit = by_amount("-80.00");
    assert_eq!(debit.kind, s("direct_debit"));
    assert_eq!(debit.counterparty, s("ASSURANCE EXEMPLE"));
    assert_eq!(details(debit)["ics"], json!("FR00ZZZ000000"));
    assert_eq!(details(debit)["ids"], json!(["ABCD25139000000003485321", "V260918007125"]));
    let salary = by_amount("1500.00");
    assert_eq!(salary.kind, s("transfer"));
    assert_eq!(salary.counterparty, s("ACME CONSULTING SAS"));
    assert_eq!(salary.reference, s("SCT000000000000000000000001"));
    assert!(salary.label.contains("Salaire septembre"));
    assert!(!crate::models::truthy(&details(&salary.clone()).get("internal").cloned().unwrap_or(Value::Null)));
}

#[test]
fn test_a_wrong_closing_balance_or_wrong_declared_totals_are_flagged() {
    let st = parse_one(fortuneo_statement_pdf(&FSpec { wrong_closing: Some(d("1.00")), ..FSpec::new(fortuneo_entries()) }));
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("ne tombent pas juste")));
    let st = parse_one(fortuneo_statement_pdf(&FSpec { wrong_totals: true, ..FSpec::new(fortuneo_entries()) }));
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("totaux imprimés")));
}

#[test]
fn test_an_overdrawn_account_and_a_statement_with_no_operation() {
    let spec = FSpec { opening: d("20.00"), ..FSpec::new(vec![fentry(ymd(2026, 9, 5), &["CARTE 04/09 SHOP"], "-100.00")]) };
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!(st.balance, Some(d("-80.00")));
    assert_eq!(st.confidence, "high");
    let empty = parse_one(fortuneo_statement_pdf(&FSpec { opening: d("20.00"), ..FSpec::new(vec![]) }));
    assert_eq!(empty.balance, Some(d("20.00")));
    assert!(empty.transactions.is_empty());
    assert_eq!(empty.confidence, "high");
}

#[test]
fn test_dates_without_a_year_follow_the_period_across_new_year() {
    let spec = FSpec {
        period_end: ymd(2027, 1, 31),
        opening: d("100.00"),
        ..FSpec::new(vec![fentry(ymd(2026, 12, 30), &["CARTE 29/12 SHOP"], "-5.00"), fentry(ymd(2027, 1, 2), &["CARTE 02/01 SHOP"], "-6.00")])
    };
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!(st.transactions.iter().map(|t| t.date).collect::<Vec<_>>(), vec![ymd(2026, 12, 30), ymd(2027, 1, 2)]);
}

// ---------------------------------------------------------------------------------------------- import
#[test]
fn test_first_statement_creates_the_account_with_its_identity() {
    let core = core();
    let spec = fortuneo_september();
    let doc_id = drop(&core, &fortuneo_statement_pdf(&spec));
    assert!(core.auto_import(doc_id).unwrap().is_none()); // an account to create: the user is asked
    let (_, account_id) = import_statement(&core, &FSpec { title: "again".into(), ..FSpec::new(fortuneo_entries()) });
    db(&core, |c| {
        let account = Account::get(c, account_id)?.unwrap();
        assert_eq!(account.institution, s("Fortuneo"));
        assert_eq!(account.holder_name, s(F_HOLDER));
        assert_eq!((account.iban.clone(), account.bic.clone()), (Some(f_iban()), s("FTNOFRP1XXX")));
        assert_eq!(account.holder_address, Some(f_address_text()));
        assert_eq!(Transaction::query(c, "WHERE account_id = ?", [account_id])?.len(), spec.entries.len());
        Ok(())
    });
}

#[test]
fn test_the_next_statement_of_the_account_is_imported_automatically() {
    let core = core();
    let first = fortuneo_september();
    let (_, account_id) = import_statement(&core, &first);
    let october = FSpec { opening: first.closing(), period_end: ymd(2026, 10, 31), ..FSpec::new(vec![fentry(ymd(2026, 10, 5), &["CARTE 04/10 SHOP"], "-9.00")]) };
    let doc_id = drop(&core, &fortuneo_statement_pdf(&october));
    let result = core.auto_import(doc_id).unwrap().expect("imported");
    assert_eq!(num(&result, "account_id"), account_id);
    assert_eq!(num(&result, "added"), 1);
}

#[test]
fn test_transfers_between_fortuneo_and_boursobank_are_paired() {
    let core = core();
    let (_, fortuneo_id) = import_statement(&core, &fortuneo_september());
    let bourso = Spec { opening: d("1000.00"), ..Spec::new(vec![entry(ymd(2026, 9, 9), &["VIR INST MARTIN PAUL", "vers Fortuneo"], "-200.00")]) };
    let doc_id = drop_file(&core, "bourso.pdf", &boursobank_statement_pdf(&bourso));
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte courant"), ("import_transactions", "on")])).unwrap();
    db(&core, |c| {
        let sent = Transaction::query_one(c, "WHERE account_id = ?", [num(&result, "account_id")])?.unwrap();
        let received = Transaction::query_one(c, "WHERE account_id = ? AND amount = '200.00'", [fortuneo_id])?.unwrap();
        assert!(sent.internal() && received.internal());
        assert_eq!(sent.details.as_ref().unwrap()["paired_with"], json!(received.id));
        Ok(())
    });
}

// ----------------------------------------------------------------------------------------- fees recap
#[test]
fn test_fees_recap_is_read_with_the_services_and_checked() {
    let spec = FeesSpec { services: vec![service("Cartes", "COTISATION CARTE", 1, "9,00", 1, "9,00")], ..FeesSpec::new() };
    let st = parse_one(fortuneo_fees_pdf(&spec));
    assert_eq!((st.document_kind.clone(), st.institution.clone()), (s("fees_summary"), s("Fortuneo")));
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2023, 1, 1)), Some(ymd(2023, 12, 31))));
    assert_eq!(extra(&st, "fees_total"), &json!("9.00"));
    assert_eq!(extra(&st, "interest_paid"), &json!("0.00"));
    assert_eq!(extra(&st, "interest_earned"), &Value::Null);
    assert_eq!(extra(&st, "services"), &json!([{"category": "Cartes", "name": "COTISATION CARTE", "used": 1, "unit_price": "9.00", "billed": 1, "total": "9.00"}]));
    assert_eq!(st.iban, Some(f_iban()));
    assert_eq!(st.holder_name, s(F_HOLDER));
    assert_eq!(st.holder_address, Some(f_address_text()));
    assert_eq!(st.confidence, "high");
    assert!(st.transactions.is_empty());
    let bad = parse_one(fortuneo_fees_pdf(&FeesSpec { total: d("12.00"), services: vec![service("Cartes", "COTISATION CARTE", 1, "9,00", 1, "9,00")], ..FeesSpec::new() }));
    assert_eq!(bad.confidence, "low");
    assert!(bad.warnings.iter().any(|w| w.contains("somme des services")));
}

#[test]
fn test_fees_recap_is_attached_to_the_account_known_by_its_iban() {
    let core = core();
    let (_, account_id) = import_statement(&core, &fortuneo_september());
    let doc_id = drop(&core, &fortuneo_fees_pdf(&FeesSpec::new()));
    let result = core.auto_import(doc_id).unwrap().expect("imported");
    assert_eq!(result["kind"], json!("fees_summary"));
    assert_eq!(num(&result, "account_id"), account_id);
    assert_eq!(num(&result, "year"), 2023);
    db(&core, |c| {
        let all = FeeSummary::query(c, "", [])?;
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].account_id, all[0].fees_total, all[0].interest_earned), (account_id, d("9.00"), None));
        assert_eq!(Document::get(c, doc_id)?.unwrap().status, "imported");
        assert!(Transaction::query_one(c, "WHERE kind = 'fee'", [])?.is_none()); // no operation is created by a recap
        Ok(())
    });

    // The same recap in another file: refused for the same period unless forced.
    let other = drop(&core, &fortuneo_fees_pdf(&FeesSpec { title: "copy".into(), ..FeesSpec::new() }));
    match core.import_statement(other, 0, &form(&[("account_id", &account_id.to_string())])) {
        Err(AppError::Value(m)) => assert!(m.contains("déjà rattaché"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    core.import_statement(other, 0, &form(&[("account_id", &account_id.to_string()), ("force", "on")])).unwrap();
    db(&core, |c| {
        assert_eq!(FeeSummary::query(c, "", [])?.len(), 1);
        Ok(())
    });

    core.undo_import(other).unwrap();
    db(&core, |c| {
        assert!(FeeSummary::query(c, "", [])?.is_empty());
        Ok(())
    });
}

#[test]
fn test_fees_recap_without_a_known_account_asks_and_can_create_it() {
    let core = core();
    let doc_id = drop(&core, &fortuneo_fees_pdf(&FeesSpec::new()));
    assert!(core.auto_import(doc_id).unwrap().is_none());
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte Fortuneo")])).unwrap();
    db(&core, |c| {
        let account = Account::get(c, num(&result, "account_id"))?.unwrap();
        assert_eq!(account.iban, Some(f_iban()));
        assert_eq!(account.holder_name, s(F_HOLDER));
        assert_eq!(account.institution, s("Fortuneo"));
        Ok(())
    });
}

#[test]
fn test_the_fees_recap_is_compared_with_the_fee_lines_of_the_statements() {
    let core = core();
    let spec = FSpec { opening: d("100.00"), ..FSpec::new(vec![fentry(ymd(2026, 9, 5), &["COTISATION CARTE GOLD"], "-9.00")]) };
    let (_, account_id) = import_statement(&core, &spec);
    db(&core, |c| {
        for (start, end, total) in [(ymd(2026, 9, 1), ymd(2026, 9, 30), "9.00"), (ymd(2026, 8, 1), ymd(2026, 8, 31), "3.00")] {
            let mut fs = FeeSummary::default();
            fs.account_id = account_id;
            fs.period_start = start;
            fs.period_end = end;
            fs.fees_total = d(total);
            fs.services = Some(json!([]));
            fs.insert(c)?;
        }
        let account = Account::get(c, account_id)?.unwrap();
        let rows = crate::web::views::fee_summaries(c, &account)?;
        let text = format!("{rows:?}");
        assert!(text.contains("seen_count") || !rows.is_empty());
        let spent = Transaction::query(c, "WHERE account_id = ? AND kind = 'fee' AND date >= '2026-09-01' AND date <= '2026-09-30'", [account_id])?;
        assert_eq!(spent.iter().map(|t| -t.amount).sum::<Decimal>(), d("9.00"));
        assert_eq!(spent.len(), 1);
        assert!(crate::services::coverage::covers(c, account_id, ymd(2026, 9, 1), ymd(2026, 9, 30))?);
        assert!(!crate::services::coverage::covers(c, account_id, ymd(2026, 8, 1), ymd(2026, 8, 31))?); // no statement for August
        Ok(())
    });
}

// ----------------------------------------------------------------------------------------- FGDR letter
#[test]
fn test_the_deposit_guarantee_letter_is_filed_automatically() {
    let core = core();
    let st = parse_one(fortuneo_info());
    assert_eq!(st.document_kind, s("info"));
    assert_eq!(extra(&st, "year"), &json!(2026));
    assert_eq!(extra(&st, "ceiling"), &json!("100 000 €"));
    assert_eq!(st.holder_name, s(F_HOLDER));
    assert_eq!(st.holder_address, Some(f_address_text()));
    let doc_id = drop(&core, &fortuneo_info());
    let result = core.auto_import(doc_id).unwrap().expect("filed");
    assert_eq!(result["kind"], json!("info"));
    assert_eq!(result["account_id"], Value::Null);
    db(&core, |c| {
        assert_eq!(Document::get(c, doc_id)?.unwrap().status, "imported");
        assert!(Transaction::query_one(c, "", [])?.is_none());
        Ok(())
    });
    assert!(!core.read_document(doc_id).unwrap().1.is_empty()); // still downloadable (decrypted from the encrypted store)
}

#[test]
fn test_the_same_letter_dropped_twice_is_recognised_as_the_same_file() {
    let core = core();
    let data = fortuneo_info();
    let (first, existing) = core.store_document("a.pdf", &data).unwrap();
    let (again, existing_again) = core.store_document("b.pdf", &data).unwrap();
    assert!(!existing);
    assert!(existing_again);
    assert_eq!(first, again);
}

// ------------------------------------------------------------------------------------------------- web
fn upload(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

fn doc_of(url: &str) -> Option<String> {
    Regex::new(r"/documents/(\d+)").unwrap().captures(url).map(|c| c[1].to_string())
}

#[test]
fn test_web_the_library_shows_type_and_institution_and_the_account_checks_the_fees() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve.pdf", &fortuneo_statement_pdf(&fortuneo_september()));
    let doc = doc_of(&page.url).unwrap();
    assert!(page.text().contains("Fortuneo · relevé de compte ou de livret (PDF)"));
    let form = client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte Fortuneo"), ("import_transactions", "on")]);
    let account_id = Regex::new(r"/accounts/(\d+)").unwrap().captures(&form.url).unwrap()[1].to_string();

    let done = upload(&mut client, &csrf, "frais.pdf", &fortuneo_fees_pdf(&FeesSpec::new()));
    assert!(done.text().contains("récapitulatif des frais 2023 rattaché au compte"));

    let info = upload(&mut client, &csrf, "fgdr.pdf", &fortuneo_info());
    assert!(info.text().contains("classé avec tes papiers"));

    let library = client.get("/documents").text();
    assert!(library.contains("Récapitulatif des frais") && library.contains("Information") && library.contains("Relevé de compte"));

    let account = client.get(&format!("/accounts/{account_id}")).text();
    assert!(account.contains("Frais et intérêts") && account.contains("9,00"));
    assert!(account.contains("Relevés incomplets sur cette période")); // one statement of 2026 cannot confirm the fees of 2023
}

// ------------------------------------------------------------------------ savings book, first statement, joint account
#[test]
fn test_a_savings_book_statement_is_read_without_rib_or_iban() {
    let spec = FSpec {
        opening: d("10.32"),
        period_end: ymd(2026, 12, 31),
        opening_on: Some(ymd(2025, 12, 31)),
        livret: true,
        number: "1 - 2026".into(),
        ..FSpec::new(vec![FEntry { value_day: Some(ymd(2027, 1, 1)), ..fentry(ymd(2026, 12, 31), &["INTERETS 2026"], "0.13") }])
    };
    let (found, parsed) = parse(fortuneo_statement_pdf(&spec));
    let st = &parsed[0];
    assert_eq!(found.decision, "auto");
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.statement_pdf");
    assert_eq!((st.account_type.clone(), st.extra_str("product")), (s("savings"), s("Livret +")));
    assert_eq!(st.account_label, s("Livret + Fortuneo …6501"));
    assert_eq!(st.account_number, Some(format!("{}01", &F_ACCOUNT[..8])));
    assert!(st.iban.is_none() && st.bank_code.is_none());
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 1, 1)), Some(ymd(2026, 12, 31)))); // one statement a year
    assert_eq!((st.opening_balance, st.balance), (Some(d("10.32")), Some(d("10.45"))));
    assert_eq!(st.confidence, "high");
    assert!(st.warnings.is_empty());
    let [t] = &st.transactions[..] else { panic!("one operation expected") };
    assert_eq!(t.kind, s("interest"));
    assert_eq!(t.date, ymd(2026, 12, 31));
    assert_eq!(t.amount, d("0.13")); // the value date is in January: the operation is not
}

#[test]
fn test_a_savings_book_is_its_own_account_and_matched_again_by_its_number() {
    let core = core();
    let spec = FSpec {
        opening: d("10.32"),
        period_end: ymd(2026, 12, 31),
        opening_on: Some(ymd(2025, 12, 31)),
        livret: true,
        ..FSpec::new(vec![FEntry { value_day: Some(ymd(2027, 1, 1)), ..fentry(ymd(2026, 12, 31), &["INTERETS 2026"], "0.13") }])
    };
    let (_, courant) = import_statement(&core, &fortuneo_september());
    let doc_id = drop(&core, &fortuneo_statement_pdf(&spec));
    assert!(core.auto_import(doc_id).unwrap().is_none()); // a new account to create: the user decides
    let saved = num(&core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Livret +"), ("import_transactions", "on")])).unwrap(), "account_id");
    assert_ne!(saved, courant);
    let next_year = FSpec {
        opening: d("10.45"),
        period_end: ymd(2027, 12, 31),
        opening_on: Some(ymd(2026, 12, 31)),
        livret: true,
        ..FSpec::new(vec![fentry(ymd(2027, 12, 31), &["INTERETS 2027"], "0.20")])
    };
    let again = drop(&core, &fortuneo_statement_pdf(&next_year));
    assert_eq!(num(&core.auto_import(again).unwrap().unwrap(), "account_id"), saved); // recognised by its number, no IBAN needed
    db(&core, |c| {
        assert_eq!(Account::get(c, saved)?.unwrap().account_type, "savings");
        Ok(())
    });
}

#[test]
fn test_the_first_statement_of_an_account_has_no_opening_date() {
    let spec = FSpec {
        opening: d("0.00"),
        period_end: ymd(2026, 10, 31),
        first: true,
        ..FSpec::new(vec![
            fentry(ymd(2026, 10, 8), &["REM CHQ 4085312 0034 001 CHQ", "24293IP1RT038729693"], "645.11"),
            fentry(ymd(2026, 10, 19), &["VIR INST M MARTIN PAUL"], "-645.11"),
        ])
    };
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 10, 1)), Some(ymd(2026, 10, 31))));
    assert!(st.extra_truthy("first_statement"));
    assert_eq!((st.opening_balance, st.balance), (Some(d("0.00")), Some(d("0.00"))));
    assert_eq!(st.confidence, "high");
    assert!(st.warnings.is_empty());
    let [cheque, transfer] = &st.transactions[..] else { panic!("two operations expected") };
    assert_eq!(cheque.kind, s("cheque"));
    assert!(cheque.label.contains("Remise de chèque") && cheque.label.contains("4085312"));
    assert_eq!(transfer.kind, s("instant_transfer"));
}

#[test]
fn test_a_joint_account_holder_line_cut_by_the_bank_is_glued_back_and_confirmed_once() {
    let core = core();
    let holder = "M MARTIN PAUL OU MME DUPUIS-LEBLANC ANNE"; // cut at 30 characters, in the middle of a word
    assert!(holder.chars().count() > 30 && holder.chars().nth(29) != Some(' '));
    let spec = FSpec { holder: holder.into(), ..FSpec::new(fortuneo_entries()) };
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!(st.holder_name, s(holder));
    assert!(st.extra_truthy("holder_maybe_truncated"));
    assert_eq!(st.confidence, "high");
    let doc_id = drop(&core, &fortuneo_statement_pdf(&spec));
    assert!(core.auto_import(doc_id).unwrap().is_none()); // new account, and the names are to be confirmed
    let review = core.review_document(doc_id).unwrap().unwrap();
    assert!(review.plans[0].ask_holder);
    core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte joint"), ("holder_name", holder), ("import_transactions", "on")])).unwrap();
    db(&core, |c| {
        assert_eq!(Account::query_one(c, "", [])?.unwrap().holder_name, s(holder));
        Ok(())
    });
}

#[test]
fn test_the_guarantee_ceiling_is_shown_the_same_way_whatever_the_spacing() {
    for printed in ["100 000 €", "100000 €", "100\u{a0}000 €"] {
        let st = parse_one(fortuneo_info_pdf(2026, F_HOLDER, None, "Fortuneo", "", printed));
        assert_eq!(extra(&st, "ceiling"), &json!("100 000 €"), "{printed}");
    }
}

#[test]
fn test_a_savings_statement_with_an_operation_the_balances_do_not_explain_is_flagged() {
    let spec = FSpec {
        opening: d("10.32"),
        period_end: ymd(2026, 12, 31),
        opening_on: Some(ymd(2025, 12, 31)),
        livret: true,
        wrong_closing: Some(d("11.00")),
        ..FSpec::new(vec![fentry(ymd(2026, 12, 31), &["INTERETS 2026"], "0.13")])
    };
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("ne tombent pas juste")));
}

fn import_by_hand(client: &mut Client, csrf: &str, name: &str, data: &[u8], account: &str, balance: Option<Decimal>) -> Option<String> {
    let page = upload(client, csrf, name, data);
    let doc = doc_of(&page.url)?; // None: the account was known and the document clean, so it was imported on arrival
    let balance_text = balance.map(|b| b.to_string());
    let mut fields: Vec<(&str, &str)> = vec![("csrf", csrf), ("account_id", account), ("import_transactions", "on")];
    if account == "new" {
        fields.push(("name", "Compte"));
    }
    if let Some(b) = &balance_text {
        // the account then has a balance date, which is what its history is drawn from
        fields.push(("update_balance", "on"));
        fields.push(("balance", b));
    }
    client.post(&format!("/documents/{doc}/import/0"), &fields);
    Some(doc)
}

fn statuses(client: &Client) -> std::collections::BTreeMap<String, String> {
    db(&client.core, |c| Ok(Document::query(c, "", [])?.into_iter().map(|d| (d.original_filename.clone().unwrap_or(d.filename), d.status)).collect()))
}

#[test]
fn test_web_there_is_no_validate_all_button() {
    let mut client = client();
    let csrf = client.login();
    upload(&mut client, &csrf, "releve.pdf", &fortuneo_statement_pdf(&fortuneo_september()));
    let page = client.get("/documents").text();
    assert!(page.contains("À valider") && !page.contains("Tout valider") && page.contains("Passer en revue"));
    let status = client.post("/documents/validate-all", &[("csrf", &csrf)]).status;
    assert!(status == 404 || status == 405);
}

#[test]
fn test_web_invalidate_everything_one_institution_or_one_type() {
    let mut client = client();
    let csrf = client.login();
    import_by_hand(&mut client, &csrf, "septembre.pdf", &fortuneo_statement_pdf(&fortuneo_september()), "new", None);
    import_by_hand(&mut client, &csrf, "frais.pdf", &fortuneo_fees_pdf(&FeesSpec::new()), "1", None);
    upload(&mut client, &csrf, "fgdr.pdf", &fortuneo_info());
    let page = client.get("/documents").text();
    assert!(page.contains("Tout invalider (3)") && page.contains("Invalider la sélection"));
    assert!(page.contains("Invalider (3)")); // the institution

    let expect = |pairs: &[(&str, &str)]| pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<std::collections::BTreeMap<_, _>>();

    // One type of document: only the fees recap goes back to "à valider".
    client.post("/documents/invalidate", &[("csrf", &csrf), ("scope", "kind:Fortuneo::Récapitulatif des frais")]);
    assert_eq!(statuses(&client), expect(&[("septembre.pdf", "imported"), ("frais.pdf", "parsed"), ("fgdr.pdf", "imported")]));
    db(&client.core, |c| {
        assert!(FeeSummary::query_one(c, "", [])?.is_none());
        assert_eq!(Transaction::query(c, "", [])?.len(), fortuneo_entries().len());
        Ok(())
    });

    // The ticked documents.
    let fgdr = db(&client.core, |c| Ok(Document::query_one(c, "WHERE filename = 'fgdr.pdf' OR original_filename = 'fgdr.pdf'", [])?.unwrap().id)).to_string();
    client.post("/documents/invalidate", &[("csrf", &csrf), ("ids", &fgdr)]);
    assert_eq!(statuses(&client)["fgdr.pdf"], "parsed");
    assert_eq!(statuses(&client)["septembre.pdf"], "imported");

    // Everything left: the operations leave, the files stay, and the account an import made goes with its last document.
    let done = client.post("/documents/invalidate", &[("csrf", &csrf), ("scope", "all")]);
    assert!(done.text().contains("1 import(s) invalidé(s)"));
    assert_eq!(statuses(&client).values().cloned().collect::<std::collections::BTreeSet<_>>(), ["parsed".to_string()].into_iter().collect());
    db(&client.core, |c| {
        assert_eq!(Transaction::query(c, "", [])?.len(), 0);
        assert_eq!(Account::query(c, "", [])?.len(), 0);
        Ok(())
    });
    assert!(done.text().contains("1 compte(s) devenu(s) vide(s) supprimé(s)"), "{}", done.text());
    assert!(!client.get("/documents").text().contains("Tout invalider"));
}

#[test]
fn test_web_the_documents_show_their_reliability_and_can_be_reviewed_one_after_the_other() {
    let mut client = client();
    let csrf = client.login();
    upload(&mut client, &csrf, "releve.pdf", &fortuneo_statement_pdf(&fortuneo_september()));
    let joint = FSpec { holder: "M PRENOMLONG NOMDEFAMILLE OU MME AUTRE NOM".into(), account: "0001234500".into(), title: "b".into(), ..fortuneo_september() };
    upload(&mut client, &csrf, "joint.pdf", &fortuneo_statement_pdf(&joint));
    let page = client.get("/documents").text();
    assert!(page.matches("fiabilité élevée").count() >= 2 && page.contains("Passer en revue un par un"));

    let first = client.get("/documents/review");
    assert!(first.url.contains("review=1"));
    assert!(first.text().contains("Revue des documents à valider") && first.text().contains("2 en attente"));
    let doc = doc_of(&first.url).unwrap();

    let skipped = client.get(&format!("/documents/review?after={doc}"));
    let other = doc_of(&skipped.url).unwrap();
    assert_ne!(other, doc);
    assert!(skipped.text().contains("Revue des documents à valider"));

    // Validating inside the review leads to the next waiting document, not to the account page.
    let done = client.post(&format!("/documents/{other}/import/0"), &[("csrf", &csrf), ("review", "1"), ("account_id", "new"), ("name", "Compte"), ("holder_name", "M A OU MME B"), ("import_transactions", "on")]);
    assert!(!done.url.contains("/accounts/"));
    let text = done.text();
    assert!(text.contains("Revue des documents à valider") || text.contains("Revue terminée"));
}

#[test]
fn test_amounts_with_a_thousands_space_are_read_whole() {
    let entries = vec![
        fentry(ymd(2026, 9, 2), &["VIR INST PAUL MARTIN", "Virement depuis une autre banque", "26138IP1RT393925364"], "2000.00"),
        fentry(ymd(2026, 9, 5), &["VIR INST PAUL MARTIN", "Virement depuis une autre banque", "26138IP1RT393926934"], "1234.56"),
        fentry(ymd(2026, 9, 9), &["PRLV SEPA ASSURANCE EXEMPLE"], "-1000.50"),
    ];
    let spec = FSpec { opening: d("1320.92"), ..FSpec::new(entries) }; // 4 digits already in the opening balance
    let st = parse_one(fortuneo_statement_pdf(&spec));
    assert_eq!(st.opening_balance, Some(d("1320.92")));
    assert_eq!(st.transactions.iter().map(|t| t.amount).collect::<Vec<_>>(), vec![d("2000.00"), d("1234.56"), d("-1000.50")]);
    assert_eq!(st.balance, Some(spec.closing()));
    assert_eq!(extra(&st, "declared_totals"), &json!({"debit": "1000.50", "credit": "3234.56"}));
    assert!(st.warnings.is_empty());
    assert_eq!(st.confidence, "high");
}

#[test]
fn test_life_insurance_arbitrage_is_read_and_filed() {
    let (found, parsed) = parse(fortuneo_arbitrage_pdf("100,00", "100,00", ""));
    assert_eq!(found.decision, "auto");
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.life_arbitrage_pdf");
    let st = &parsed[0];
    assert_eq!((st.document_kind.clone(), st.institution.clone(), st.confidence.clone()), (s("info"), s("Fortuneo"), "high".to_string()));
    assert!(st.warnings.is_empty());
    assert_eq!(st.balance_date, Some(ymd(2026, 9, 21)));
    assert_eq!(extra(st, "arbitrage_date"), &json!("2026-09-16"));
    assert_eq!((extra(st, "gross"), extra(st, "fees"), extra(st, "net")), (&json!("100.00"), &json!("0.00"), &json!("100.00")));
    assert_eq!(extra(st, "contract"), &json!("FORTUNEO VIE"));
    assert_eq!(extra(st, "contract_number"), &json!("000123456789012345"));
    let isins = |k: &str| extra(st, k).as_array().unwrap().iter().map(|f| f["isin"].clone()).collect::<Vec<_>>();
    assert_eq!(isins("sold"), vec![json!("FR0000000001"), json!("LU0000000002")]);
    assert_eq!(isins("bought"), vec![Value::Null, json!("IE0000000003")]);
    let total: Decimal = extra(st, "bought").as_array().unwrap().iter().map(|f| d(f["amount"].as_str().unwrap())).sum();
    assert_eq!(total, d("100.00"));
}

#[test]
fn test_life_insurance_arbitrage_that_does_not_add_up_is_flagged() {
    let (_, parsed) = parse(fortuneo_arbitrage_pdf("100,00", "70,00", ""));
    let st = &parsed[0];
    assert_eq!(st.confidence, "medium");
    assert!(st.warnings.iter().any(|w| w.contains("achetés")));
}

#[test]
fn test_web_the_arbitrage_is_filed_under_life_insurance() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "arbitrage.pdf", &fortuneo_arbitrage_pdf("100,00", "100,00", ""));
    assert!(page.text().to_lowercase().contains("classé"));
    assert!(client.get("/documents").text().contains("Assurance-vie"));
    let doc = doc_of(&page.url).unwrap();
    let review = client.get(&format!("/documents/{doc}")).text();
    assert!(review.contains("Fonds vendus") && review.contains("FR0000000001") && review.contains("16/09/2026"));
}

#[test]
fn test_a_first_statement_covering_a_whole_year_keeps_all_its_operations() {
    let mut entries: Vec<FEntry> = [1, 3, 6, 9].iter().map(|m| fentry(ymd(2026, *m, 3), &["VIR Virement avec Fortuneo Livre"], "25.00")).collect();
    entries.push(FEntry { value_day: Some(ymd(2027, 1, 1)), ..fentry(ymd(2026, 12, 31), &["INTERETS 2026"], "1.12") });
    let spec = FSpec { opening: d("0.00"), period_end: ymd(2026, 12, 31), first: true, livret: true, ..FSpec::new(entries) };
    let (_, parsed) = parse(fortuneo_statement_pdf(&spec));
    let st = &parsed[0];
    assert_eq!(st.transactions.len(), 5);
    assert!(st.warnings.is_empty());
    assert_eq!(st.confidence, "high");
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 1, 1)), Some(ymd(2026, 12, 31))));
}

#[test]
fn test_web_an_imported_document_does_not_warn_about_being_imported_twice() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve.pdf", &fortuneo_statement_pdf(&fortuneo_september()));
    let doc = doc_of(&page.url).unwrap();
    client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte"), ("import_transactions", "on")]);
    let review = client.get(&format!("/documents/{doc}")).text();
    assert!(!review.contains("Relevé déjà importé") && !review.contains("doublon(s) exact(s)"));
    assert!(!review.contains("Valider l'import"));
    assert!(review.contains("Importé") && review.contains("Invalider l'import"));
}

#[test]
fn test_web_rebuild_cancels_every_import_and_reads_the_files_again() {
    let mut client = client();
    let csrf = client.login();
    import_by_hand(&mut client, &csrf, "septembre.pdf", &fortuneo_statement_pdf(&fortuneo_september()), "new", None);
    import_by_hand(&mut client, &csrf, "frais.pdf", &fortuneo_fees_pdf(&FeesSpec::new()), "1", None);
    // October does not pick up where September ended: imported by hand, it will not pass the checks a second time.
    let october = FSpec { opening: d("999.00"), period_end: ymd(2026, 10, 31), number: "10 - 2026".into(), title: "octobre".into(), ..FSpec::new(vec![fentry(ymd(2026, 10, 5), &["VIR M MARTIN PAUL"], "10.00")]) };
    let page = upload(&mut client, &csrf, "octobre.pdf", &fortuneo_statement_pdf(&october));
    let doc = doc_of(&page.url).unwrap();
    client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "1"), ("import_transactions", "on")]);
    db(&client.core, |c| {
        assert_eq!(Transaction::query(c, "", [])?.len(), fortuneo_entries().len() + 1);
        Ok(())
    });

    assert!(client.get("/documents").text().contains("Tout relire depuis les fichiers"));
    let result = client.post("/documents/rebuild", &[("csrf", &csrf)]).text();
    assert!(result.contains("2</strong> document(s) sur 3"));
    assert!(result.contains("octobre.pdf") && result.contains("pas</strong> été validés"));
    db(&client.core, |c| {
        assert_eq!(Account::query(c, "", [])?.len(), 1); // the account keeps existing
        assert_eq!(Transaction::query(c, "", [])?.len(), fortuneo_entries().len()); // October is not back
        assert!(FeeSummary::query_one(c, "", [])?.is_some());
        assert_eq!(Document::query_one(c, "WHERE filename = 'octobre.pdf'", [])?.unwrap().status, "parsed");
        Ok(())
    });
    let docs = client.get("/documents").text();
    assert!(docs.contains("octobre.pdf") && docs.contains("À regarder"));
}

#[test]
fn test_web_a_period_waiting_for_validation_is_not_reported_missing() {
    let mut client = client();
    let csrf = client.login();
    let sep = fortuneo_september();
    let october = FSpec { opening: sep.closing(), period_end: ymd(2026, 10, 31), number: "10 - 2026".into(), title: "octobre".into(), ..FSpec::new(vec![fentry(ymd(2026, 10, 5), &["VIR M MARTIN PAUL"], "10.00")]) };
    let november = FSpec { opening: october.closing(), period_end: ymd(2026, 11, 30), number: "11 - 2026".into(), title: "novembre".into(), ..FSpec::new(vec![fentry(ymd(2026, 11, 5), &["VIR M MARTIN PAUL"], "20.00")]) };
    let first = upload(&mut client, &csrf, "septembre.pdf", &fortuneo_statement_pdf(&sep));
    upload(&mut client, &csrf, "octobre.pdf", &fortuneo_statement_pdf(&october));
    let nov = upload(&mut client, &csrf, "novembre.pdf", &fortuneo_statement_pdf(&november));
    let sep_id = doc_of(&first.url).unwrap();
    let nov_id = doc_of(&nov.url).unwrap();
    client.post(&format!("/documents/{sep_id}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte"), ("import_transactions", "on")]);

    // October is read but not validated: it is waiting, not missing.
    assert!(!client.get(&format!("/documents/{nov_id}")).text().contains("Il manque une période"));

    // Without it, the hole is real and is reported.
    let oct_id = db(&client.core, |c| Ok(Document::query_one(c, "WHERE filename = 'octobre.pdf'", [])?.unwrap().id));
    client.post(&format!("/documents/{oct_id}/delete"), &[("csrf", &csrf)]);
    let page = client.get(&format!("/documents/{nov_id}")).text();
    assert!(page.contains("Il manque une période : rien entre le 01/10/2026 et le 31/10/2026"));
}

fn set_reader(client: &Client, doc: i64, reader: &str, warning: bool) {
    db(&client.core, |c| {
        let mut row = Document::get(c, doc)?.unwrap();
        let before = row.clone();
        let mut result = row.parse_result.clone().unwrap();
        result["reader"] = json!(reader);
        if warning {
            let mut st = result["statements"][0].clone();
            st["warnings"] = json!(["Les totaux imprimés par la banque (débits 1.00, crédits 2.00) ne correspondent pas"]);
            st["confidence"] = json!("medium");
            result["statements"] = json!([st]);
        }
        row.parse_result = Some(result);
        row.save(c, &before)?;
        Ok(())
    });
}

#[test]
fn test_web_a_waiting_document_is_read_again_when_the_reading_code_changed() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve.pdf", &fortuneo_statement_pdf(&fortuneo_september()));
    let doc = doc_of(&page.url).unwrap();
    // Simulate a reading made by an older version of the application, with a wrong warning.
    set_reader(&client, doc.parse().unwrap(), "old00000", true);
    let review = client.get(&format!("/documents/{doc}")).text();
    assert!(!review.contains("débits 1.00") && review.contains("fiabilité élevée"));
    db(&client.core, |c| {
        assert_ne!(Document::get(c, doc.parse().unwrap())?.unwrap().parse_result.unwrap()["reader"], json!("old00000"));
        Ok(())
    });
}

#[test]
fn test_web_an_imported_document_read_by_an_older_version_says_so() {
    let mut client = client();
    let csrf = client.login();
    let doc = import_by_hand(&mut client, &csrf, "releve.pdf", &fortuneo_statement_pdf(&fortuneo_september()), "new", None).unwrap();
    assert!(!client.get(&format!("/documents/{doc}")).text().contains("version plus ancienne"));
    set_reader(&client, doc.parse().unwrap(), "old00000", false);
    let review = client.get(&format!("/documents/{doc}")).text();
    assert!(review.contains("version plus ancienne") && review.contains("myMoney dev · lecture "));
    db(&client.core, |c| {
        assert_eq!(Document::get(c, doc.parse().unwrap())?.unwrap().parse_result.unwrap()["reader"], json!("old00000")); // an import is never rewritten silently
        Ok(())
    });
}

#[test]
fn test_the_margin_watermark_does_not_hide_the_closing_balance_or_an_operation() {
    // The fixtures print the mirrored "Émis le" watermark on the closing row and on one operation row.
    let st = parse_one(fortuneo_statement_pdf(&fortuneo_september()));
    assert_eq!(st.balance, Some(fortuneo_september().closing()));
    assert_eq!(st.transactions.len(), fortuneo_entries().len());
    assert!(st.warnings.is_empty());
    assert_eq!(st.confidence, "high");
}

#[test]
fn test_a_home_loan_payment_line_is_a_loan_payment() {
    let entries = vec![fentry(ymd(2026, 9, 26), &["CREDIT LOGT 0000TEST00000001"], "-1931.81")];
    let st = parse_one(fortuneo_statement_pdf(&FSpec { opening: d("5000.00"), ..FSpec::new(entries) }));
    let [t] = &st.transactions[..] else { panic!("one operation expected") };
    assert_eq!(t.kind, s("loan_payment"));
    assert_eq!(t.amount, d("-1931.81"));
    assert_eq!(details(t)["loan_number"], json!("0000TEST00000001"));
    assert!(st.warnings.is_empty());
}

#[test]
fn test_fees_summary_with_a_refund_whose_name_wraps_around_its_figures() {
    let spec = FeesSpec {
        total: d("9.00"),
        services: vec![
            service("Cartes et espèces", "INACTIVITE CARTE EXEMPLE", 2, "9,00", 2, "18,00"),
            service("Cartes et espèces", "REMBOURSEMENT(S)\nEXCEPTIONNEL(S)", 1, "9,00", 1, "-9,00"),
        ],
        ..FeesSpec::new()
    };
    let st = parse_one(fortuneo_fees_pdf(&spec));
    let services = extra(&st, "services").as_array().unwrap().clone();
    assert_eq!(
        services.iter().map(|s| (s["name"].as_str().unwrap().to_string(), s["total"].as_str().unwrap().to_string())).collect::<Vec<_>>(),
        vec![("INACTIVITE CARTE EXEMPLE".to_string(), "18.00".to_string()), ("REMBOURSEMENT(S) EXCEPTIONNEL(S)".to_string(), "-9.00".to_string())]
    );
    assert_eq!(extra(&st, "fees_total"), &json!("9.00"));
    assert!(st.warnings.is_empty());
    assert_eq!(st.confidence, "high");
    assert_eq!(services.iter().map(|s| s["category"].clone()).collect::<Vec<_>>(), vec![json!("Cartes et espèces"); 2]);
}

#[test]
fn test_web_the_bank_fees_are_shown_next_to_the_spending() {
    let mut client = client();
    let csrf = client.login();
    let mut entries = fortuneo_entries();
    entries.push(fentry(ymd(2026, 9, 28), &["FRAIS TENUE DE COMPTE"], "-9.00"));
    let spec = FSpec::new(entries);
    import_by_hand(&mut client, &csrf, "septembre.pdf", &fortuneo_statement_pdf(&spec), "new", Some(spec.closing()));
    let recap = FeesSpec {
        year: 2026,
        total: d("9.00"),
        services: vec![
            service("Cartes et espèces", "INACTIVITE CARTE EXEMPLE", 2, "9,00", 2, "18,00"),
            service("Cartes et espèces", "REMBOURSEMENT(S) EXCEPTIONNEL(S)", 1, "9,00", 1, "-9,00"),
        ],
        ..FeesSpec::new()
    };
    import_by_hand(&mut client, &csrf, "frais.pdf", &fortuneo_fees_pdf(&recap), "1", None);
    let page = client.get("/analytics?vue=depenses&periode=tout").text();
    assert!(page.contains("id=\"frais\"") && page.contains("Frais bancaires"));
    assert!(page.contains("Frais d'après les récapitulatifs") && page.contains("Frais dans les opérations"));
    assert!(page.contains("1 opération(s)")); // the fee line of the statement
    assert!(page.contains("Détail des services (2)") && page.contains("INACTIVITE CARTE EXEMPLE"));
    // No fees to show inside one category.
    assert!(!client.get("/analytics?vue=depenses&periode=tout&cat=courses").text().contains("id=\"frais\""));
}

#[test]
fn test_web_a_fee_refund_is_taken_off_the_fees_not_added_to_them() {
    let mut client = client();
    let csrf = client.login();
    let mut entries = fortuneo_entries();
    entries.push(fentry(ymd(2026, 9, 10), &["FRAIS TENUE DE COMPTE"], "-9.00"));
    entries.push(fentry(ymd(2026, 9, 20), &["FRAIS TENUE DE COMPTE"], "-9.00"));
    entries.push(fentry(ymd(2026, 9, 25), &["REMBOURSEMENT EXCEPTIONNEL FRAIS"], "9.00"));
    let spec = FSpec::new(entries);
    import_by_hand(&mut client, &csrf, "septembre.pdf", &fortuneo_statement_pdf(&spec), "new", Some(spec.closing()));
    let recap = FeesSpec { year: 2026, total: d("9.00"), ..FeesSpec::new() };
    import_by_hand(&mut client, &csrf, "frais.pdf", &fortuneo_fees_pdf(&recap), "1", None);
    let page = client.get("/analytics?vue=depenses&periode=tout").text();
    let tile = Regex::new(r"Frais dans les opérations.*?</div>").unwrap().find(&page.replace('\n', " ")).map(|m| m.as_str().to_string()).unwrap();
    assert!(tile.contains("9,00") && !tile.contains("18,00"), "{tile}");
    assert!(tile.contains("2 opération(s)") && tile.contains("remboursés déjà déduits"), "{tile}");
}

fn annual_spec_pdf(spec: &LifeAnnualSpec) -> (Identification, ParsedStatement) {
    let (found, mut parsed) = parse(fortuneo_life_annual_pdf(spec));
    assert_eq!(parsed.len(), 1);
    (found, parsed.remove(0))
}

#[test]
fn test_life_insurance_annual_statement_is_read() {
    let spec = LifeAnnualSpec::new(2025);
    let (found, st) = annual_spec_pdf(&spec);
    assert_eq!(found.decision, "auto");
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.life_annual_pdf");
    assert_eq!((st.document_kind.clone(), st.account_type.clone(), st.confidence.clone()), (s("statement"), s("life_insurance"), "high".to_string()));
    assert!(st.warnings.is_empty(), "{:?}", st.warnings);
    assert_eq!(st.account_number, s("000123456789012345"));
    assert_eq!(st.account_label, s("Fortuneo Vie …2345"));
    assert_eq!(st.balance, Some(d("600.00")));
    assert_eq!((st.balance_date, st.period_start, st.period_end), (Some(ymd(2025, 12, 31)), Some(ymd(2025, 1, 1)), Some(ymd(2025, 12, 31))));
    assert_eq!((st.opening_balance, st.opening_date), (Some(d("440.00")), Some(ymd(2024, 12, 31))));
    assert!(st.transactions.is_empty() && st.holdings.is_none());
    assert_eq!(st.holder_name, s("M. MARTIN PAUL"));
    assert_eq!(st.holder_address, s("8 RUE DES TILLEULS\n69003 LYON"));
    assert_eq!(extra(&st, "category"), &json!("Relevé d'information annuel"));
    assert_eq!(extra(&st, "issued_on"), &json!("2026-02-19"));
    assert_eq!(extra(&st, "contract"), &json!("Fortuneo Vie"));
    assert_eq!(extra(&st, "payments"), &json!("100.00"));
    assert_eq!(extra(&st, "fees_total"), &json!("4.48"));
}

#[test]
fn test_life_insurance_annual_statement_funds_and_movements() {
    let (_, st) = annual_spec_pdf(&LifeAnnualSpec::new(2025));
    let supports = extra(&st, "supports").as_array().unwrap();
    assert_eq!(supports.len(), 4); // the euro fund, then three funds over two pages
    assert_eq!(supports[0]["fund"], json!("FDS EUROS EXEMPLE RENDEMENT"));
    assert_eq!((supports[0]["isin"].clone(), supports[0]["capital_guaranteed"].clone(), supports[0]["amount"].clone()), (Value::Null, json!(true), json!("400.00")));
    assert_eq!((supports[1]["isin"].clone(), supports[1]["units"].clone(), supports[1]["unit_value"].clone(), supports[1]["amount"].clone()), (json!("FR0000000001"), json!("0.1500"), json!("400.00"), json!("60.00")));
    assert_eq!((supports[3]["fund"].clone(), supports[3]["unit_value"].clone(), supports[3]["amount"].clone()), (json!("FONDS DIVERS EXEMPLE C"), json!("1500.00"), json!("15.00")));
    let total: Decimal = supports.iter().map(|f| d(f["amount"].as_str().unwrap())).sum();
    assert_eq!(Some(total), st.balance);
    let movements = extra(&st, "movements").as_array().unwrap();
    assert_eq!(movements.len(), 4);
    assert_eq!((movements[0]["label"].clone(), movements[0]["credit"].clone(), movements[0]["debit"].clone()), (json!("Versements"), json!("100.00"), Value::Null));
    assert_eq!((movements[1]["debit"].clone(), movements[1]["credit"].clone()), (json!("50.00"), json!("50.00")));
    assert_eq!((movements[3]["label"].clone(), movements[3]["debit"].clone()), (json!("Prélèvements sociaux sur la revalorisation annuelle des fonds en euros"), json!("1.03")));
    let rates = extra(&st, "euro_rates").as_array().unwrap();
    assert_eq!((rates[0]["fund"].clone(), rates[0]["rate"].clone(), rates[0]["bonus"].clone()), (json!("FDS EUROS EXEMPLE RENDEMENT"), json!("2.50"), json!("0.40")));
    assert_eq!(rates.len(), 2);
}

#[test]
fn test_life_insurance_annual_first_year_has_no_opening_balance() {
    let spec = LifeAnnualSpec { previous: None, ..LifeAnnualSpec::new(2023) };
    let (_, st) = annual_spec_pdf(&spec);
    assert_eq!((st.opening_balance, st.opening_date), (None, None));
    assert_eq!(st.confidence, "high");
}

#[test]
fn test_life_insurance_annual_funds_that_do_not_add_up_are_flagged() {
    let spec = LifeAnnualSpec { declared: Some("700,00"), ..LifeAnnualSpec::new(2025) };
    let (_, st) = annual_spec_pdf(&spec);
    assert_eq!(st.balance, Some(d("700.00")));
    assert_eq!(st.confidence, "medium");
    assert!(st.warnings.iter().any(|w| w.contains("supports")));
}

#[test]
fn test_investor_profile_is_read_and_filed() {
    let (found, parsed) = parse(fortuneo_investor_profile_pdf("13 janvier 2026", "DYNAMIQUE"));
    assert_eq!(found.decision, "auto");
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.investor_profile_pdf");
    let st = &parsed[0];
    assert_eq!((st.document_kind.clone(), st.institution.clone(), st.confidence.clone()), (s("info"), s("Fortuneo"), "high".to_string()));
    assert!(st.warnings.is_empty());
    assert_eq!(st.balance_date, Some(ymd(2026, 1, 13)));
    assert_eq!(st.holder_name, s("MARTIN PAUL"));
    assert_eq!(extra(st, "investor_profile"), &json!("Dynamique"));
    assert_eq!(extra(st, "extra_financial_profile"), &json!("Sans préférence"));
    assert_eq!(extra(st, "horizon"), &json!("plus de 8 ans"));
    assert_eq!(extra(st, "category"), &json!("Profil investisseur"));
    // the answers of the questionnaire are not copied
    let dump = serde_json::to_string(&st.to_json()).unwrap();
    assert!(!dump.contains("Union libre") && !dump.contains("Profession exemple"));
}

#[test]
fn test_the_two_new_documents_do_not_get_mixed_up() {
    let (found, _) = parse(fortuneo_investor_profile_pdf("10 avril 2026", "PRUDENT"));
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.investor_profile_pdf");
    let (found, _) = parse(fortuneo_life_annual_pdf(&LifeAnnualSpec::new(2024)));
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.life_annual_pdf");
    let (found, _) = parse(fortuneo_arbitrage_pdf("100,00", "100,00", ""));
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.life_arbitrage_pdf");
}

fn life_account(core: &Core) -> i64 {
    let doc = drop(core, &fortuneo_life_annual_pdf(&LifeAnnualSpec { euro: "440,00", ..LifeAnnualSpec::new(2025) }));
    num(&core.import_statement(doc, 0, &form(&[("account_id", "new"), ("name", "Fortuneo Vie"), ("update_balance", "on"), ("balance", "640.00")])).unwrap(), "account_id")
}

#[test]
fn test_life_insurance_annual_statement_is_imported_as_a_balance() {
    let core = core();
    let spec = LifeAnnualSpec::new(2025);
    let doc = drop(&core, &fortuneo_life_annual_pdf(&spec));
    let id = num(&core.import_statement(doc, 0, &form(&[("account_id", "new"), ("name", "Fortuneo Vie"), ("update_balance", "on"), ("balance", "600.00")])).unwrap(), "account_id");
    db(&core, |c| {
        let account = Account::get(c, id)?.unwrap();
        assert_eq!((account.account_type.as_str(), account.account_number.clone(), account.institution.clone()), ("life_insurance", s("000123456789012345"), s("Fortuneo")));
        // the value is carried by the lines, once: the cash is zero, the four funds hold the 600
        assert_eq!(account.balance, Decimal::ZERO);
        let lines = Holding::query(c, "WHERE account_id = ?", [id])?;
        assert_eq!(lines.len(), 4);
        assert!(Transaction::query(c, "WHERE account_id = ?", [id])?.is_empty());
        let statements = crate::models::Statement::query(c, "WHERE account_id = ?", [id])?;
        assert_eq!(statements.len(), 1);
        let st = &statements[0];
        assert_eq!((st.period_start, st.period_end, st.opening_balance, st.closing_balance, st.tx_count), (Some(ymd(2025, 1, 1)), Some(ymd(2025, 12, 31)), Some(d("440.00")), Some(d("600.00")), 0));
        let snapshots = crate::models::Snapshot::query(c, "WHERE account_id = ? ORDER BY day", [id])?;
        assert_eq!(snapshots.iter().map(|x| (x.day, x.value)).collect::<Vec<_>>(), vec![(ymd(2024, 12, 31), d("440.00")), (ymd(2025, 12, 31), d("600.00"))]);
        Ok(())
    });
}

#[test]
fn test_life_insurance_annual_statements_chain_year_after_year() {
    let core = core();
    let id = life_account(&core);
    let older = drop(&core, &fortuneo_life_annual_pdf(&LifeAnnualSpec { euro: "240,00", previous: Some("311,00"), ..LifeAnnualSpec::new(2024) }));
    let result = core.auto_import(older).unwrap().expect("same contract, contiguous years: imported without asking");
    assert_eq!(num(&result, "account_id"), id);
    db(&core, |c| {
        assert_eq!(Account::query(c, "", [])?.len(), 1);
        assert_eq!(Account::get(c, id)?.unwrap().balance, Decimal::ZERO); // the lines come from the newest statement, not from the older one
        let statements = crate::models::Statement::query(c, "WHERE account_id = ? ORDER BY period_start", [id])?;
        assert_eq!(statements.len(), 2);
        assert_eq!(crate::services::statements::coverage(c, id)?.1, Vec::<String>::new());
        assert!(crate::services::coverage::covers(c, id, ymd(2024, 1, 1), ymd(2025, 12, 31))?);
        Ok(())
    });
    // undoing the older one forgets the day only it knew, and keeps the one the newer statement also gives
    core.undo_import(older).unwrap();
    db(&core, |c| {
        let days: Vec<_> = crate::models::Snapshot::query(c, "WHERE account_id = ? ORDER BY day", [id])?.iter().map(|x| x.day).collect();
        assert_eq!(days, vec![ymd(2024, 12, 31), ymd(2025, 12, 31)]);
        assert_eq!(crate::models::Statement::query(c, "WHERE account_id = ?", [id])?.len(), 1);
        Ok(())
    });
}

#[test]
fn test_life_insurance_annual_statement_hole_between_years_is_reported() {
    let core = core();
    let id = life_account(&core);
    let doc = drop(&core, &fortuneo_life_annual_pdf(&LifeAnnualSpec { euro: "240,00", previous: Some("311,00"), ..LifeAnnualSpec::new(2023) }));
    assert!(core.auto_import(doc).unwrap().is_none()); // 2024 is missing: it asks instead of importing blindly
    db(&core, |c| {
        assert_eq!(crate::models::Statement::query(c, "WHERE account_id = ?", [id])?.len(), 1);
        Ok(())
    });
}

#[test]
fn test_investor_profile_is_filed_without_an_account() {
    let core = core();
    let doc = drop(&core, &fortuneo_investor_profile_pdf("13 janvier 2026", "DYNAMIQUE"));
    let result = core.auto_import(doc).unwrap().expect("filed");
    assert_eq!(result["kind"], json!("info"));
    db(&core, |c| {
        let document = Document::get(c, doc)?.unwrap();
        assert_eq!(document.status, "imported");
        assert_eq!(crate::services::naming::document_group(&document), ("Fortuneo".to_string(), "Profil investisseur".to_string()));
        assert!(document.filename.starts_with("FORTUNEO_PROFIL-INVESTISSEUR_MARTIN_PAUL_2026-01-13"), "{}", document.filename);
        Ok(())
    });
}

#[test]
fn test_web_the_annual_statement_shows_funds_and_movements() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "releve-annuel.pdf", &fortuneo_life_annual_pdf(&LifeAnnualSpec::new(2025)));
    let text = page.text();
    for expected in ["Supports détenus au", "FONDS ACTIONS EXEMPLE A", "FR0000000001", "capital garanti", "Mouvements de l'année", "Versements", "Rendement des fonds en euros", "Les 4 supports totalisent bien la valeur du contrat"] {
        assert!(text.contains(expected), "{expected}");
    }
    assert!(!text.contains("opérations lues sont complètes"));
    // once imported, the library files it under its own type
    let id = doc_of(&page.url).unwrap();
    let done = client.post(&format!("/documents/{id}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Fortuneo Vie"), ("update_balance", "on"), ("balance", "600.00")]);
    assert!(done.text().contains("ce compte"), "{}", done.text());
    let library = client.get("/documents").text();
    assert!(library.contains("information annuel"), "{library}");
}

#[test]
fn test_web_the_investor_profile_is_shown_and_filed() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "profil.pdf", &fortuneo_investor_profile_pdf("10 avril 2026", "PRUDENT"));
    let text = page.text();
    for expected in ["Profil investisseur", "Prudent", "Sans préférence", "plus de 8 ans", "10/04/2026"] {
        assert!(text.contains(expected), "{expected}");
    }
    assert!(!text.contains("Union libre"));
    assert!(client.get("/documents").text().contains("Profil investisseur"));
}

// ---------------------------------------------------------------------- life insurance: letters and operations
#[test]
fn test_arbitrage_letter_in_the_other_wording_is_read_with_its_portfolio() {
    let (found, parsed) = parse(fortuneo_arbitrage_v2_pdf("100,00", "40,00"));
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.life_arbitrage_pdf");
    let st = &parsed[0];
    assert!(st.warnings.is_empty(), "{:?}", st.warnings);
    assert_eq!((st.account_number.clone(), st.account_type.clone()), (s("000123456789012345"), s("life_insurance")));
    assert_eq!((extra(st, "gross"), extra(st, "management_fees"), extra(st, "net")), (&json!("100.02"), &json!("0.02"), &json!("100.00")));
    let funds = |k: &str| extra(st, k).as_array().unwrap().iter().map(|f| f["fund"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(funds("sold"), vec!["FDS EUROS EXEMPLE RENDEMENT", "FONDS ACTIONS EXEMPLE A"]); // names keep their spaces
    assert_eq!(funds("bought"), vec!["FDS EUROS EXEMPLE RENDEMENT", "FONDS DIVERS EXEMPLE C"]);
    assert_eq!(extra(st, "portfolio_after").as_array().unwrap().len(), 2);
    // the unit value comes first in the letter, then the number of units
    let first = &extra(st, "sold")[1];
    assert_eq!((first["unit_value"].clone(), first["units"].clone(), first["amount"].clone()), (json!("400.00"), json!("0.1500"), json!("60.00")));
}

#[test]
fn test_arbitrage_letters_go_with_the_account_of_their_contract() {
    let core = core();
    let letter = drop(&core, &fortuneo_arbitrage_pdf("100,00", "100,00", "")); // filed before the contract is known: waits
    let result = core.auto_import(letter).unwrap().expect("filed");
    assert_eq!(result["account_id"], Value::Null);
    db(&core, |c| {
        assert_eq!(Document::get(c, letter)?.unwrap().account_id, None);
        Ok(())
    });
    // the annual statement creates the account: the loose letter joins it, and a later letter goes straight to it
    let id = life_account(&core);
    let later = drop(&core, &fortuneo_arbitrage_v2_pdf("100,00", "40,00"));
    let result = core.auto_import(later).unwrap().expect("filed");
    assert_eq!(num(&result, "account_id"), id);
    db(&core, |c| {
        assert_eq!(Document::get(c, letter)?.unwrap().account_id, Some(id));
        assert_eq!(Document::get(c, later)?.unwrap().account_id, Some(id));
        assert!(Document::get(c, later)?.unwrap().filename.contains("ASSURANCE-VIE"), "{}", Document::get(c, later)?.unwrap().filename);
        Ok(())
    });
}

#[test]
fn test_life_insurance_operations_are_read_and_the_arbitrages_set_aside() {
    let doc = SourceDocument::new("operations.csv", fortuneo_life_operations_csv());
    let found = identify(&doc);
    let parsed = found.chosen.as_ref().unwrap().adapter.parse(&doc).unwrap();
    assert_eq!(found.decision, "auto");
    assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), "fortuneo.life_operations_csv");
    let st = &parsed[0];
    assert_eq!((st.document_kind.clone(), st.institution.clone(), st.account_type.clone()), (s("statement"), s("Fortuneo"), s("life_insurance")));
    assert!(st.balance.is_none() && st.period_start.is_none());
    let lines: Vec<(String, Decimal, Option<String>)> = st.transactions.iter().map(|t| (t.date.to_string(), t.amount, t.kind.clone())).collect();
    assert_eq!(lines.len(), 8); // 11 lines: an arbitrage, a set-up line without an amount and the provisional fees are not operations
    assert!(lines.contains(&("2024-09-20".into(), d("300.00"), s("transfer"))));
    assert!(lines.contains(&("2026-01-24".into(), d("-1.03"), s("other"))));
    assert!(lines.contains(&("2025-12-31".into(), d("-3.01"), s("fee")))); // the fees of a closed year
    assert!(!lines.iter().any(|l| l.1 == d("-5.38"))); // the running year's fees are provisional
    assert!(!lines.iter().any(|l| l.1 == d("50.00"))); // an arbitrage moves money between funds
    assert_eq!(extra(st, "arbitrages").as_array().unwrap().len(), 1);
    assert_eq!(extra(st, "events").as_array().unwrap().len(), 1);
    assert!(st.warnings.is_empty());
    assert!(extra(st, "notes").as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("provisoire")));
    // payments are the user's own money: not income
    assert!(st.transactions.iter().filter(|t| t.amount == d("80.00")).all(|t| t.details.as_ref().unwrap().get("internal") == Some(&json!(true))));
    assert_eq!(st.balance_date, Some(ymd(2026, 9, 15)));
}

#[test]
fn test_life_insurance_operations_go_to_the_only_life_insurance_account_without_duplicates() {
    let core = core();
    let id = life_account(&core);
    let doc = drop_file(&core, "operations.csv", &fortuneo_life_operations_csv());
    let result = core.auto_import(doc).unwrap().expect("one life-insurance account: no question");
    assert_eq!(num(&result, "account_id"), id);
    assert_eq!(num(&result, "added"), 8);
    // the same export again is recognised line by line, a longer one adds only the new lines
    let longer = String::from_utf8(fortuneo_life_operations_csv()).unwrap().replace("2026-10-26;Frais", "2026-10-15;Versement programmé;Versement;Traité;80.00\n2026-10-26;Frais");
    let again = drop_file(&core, "operations-2.csv", longer.as_bytes());
    let result = core.auto_import(again).unwrap();
    db(&core, |c| {
        let count = Transaction::query(c, "WHERE account_id = ?", [id])?.len();
        assert_eq!(count, 9, "{result:?}");
        let fees: Vec<_> = Transaction::query(c, "WHERE account_id = ? AND kind = 'fee'", [id])?.into_iter().map(|t| t.amount).collect();
        assert_eq!(fees, vec![d("-3.01")]);
        // the yearly values still draw the curve of the contract
        let account = Account::get(c, id)?.unwrap();
        let history = crate::services::analytics::account_history(c, &account, &[], d("640.00"), ymd(2026, 9, 30))?;
        assert_eq!(history.get(&ymd(2025, 6, 1)).copied(), Some(d("440.00")));
        Ok(())
    });
}

#[test]
fn test_life_insurance_operations_ask_when_there_are_two_contracts() {
    let core = core();
    life_account(&core);
    let other = drop(&core, &fortuneo_life_annual_pdf(&LifeAnnualSpec { euro: "440,00", ..LifeAnnualSpec::new(2025) }.with_contract("000999888777666555")));
    core.import_statement(other, 0, &form(&[("account_id", "new"), ("name", "Autre contrat"), ("update_balance", "on"), ("balance", "640.00")])).unwrap();
    let doc = drop_file(&core, "operations.csv", &fortuneo_life_operations_csv());
    assert!(core.auto_import(doc).unwrap().is_none()); // two possible contracts: it asks
}

#[test]
fn test_web_life_insurance_pages_show_the_letter_and_the_operations() {
    let mut client = client();
    let csrf = client.login();
    let page = upload(&mut client, &csrf, "arbitrage.pdf", &fortuneo_arbitrage_v2_pdf("100,00", "40,00")).text();
    for expected in ["Contrat après l&#39;arbitrage", "frais de gestion", "aucun compte n"] {
        assert!(page.contains(expected) || page.contains(&expected.replace("&#39;", "'")), "{expected}");
    }
    let ops = client.post_files("/documents", &[("csrf", &csrf)], &[Upload { field: "files", filename: "operations.csv", data: &fortuneo_life_operations_csv(), content_type: "text/csv" }]).text();
    for expected in ["Importer les opérations nouvelles", "arbitrage(s) noté(s) à part", "provisoire"] {
        assert!(ops.contains(expected), "{expected}");
    }
}

#[test]
fn test_arbitrage_letter_that_lost_its_spaces_names_its_contract_properly() {
    let st = parse_one(fortuneo_arbitrage_v2_named_pdf("100,00", "0,00", "Contrat : FORTUNEOVIE", true));
    assert_eq!(extra(&st, "contract"), &json!("FORTUNEO VIE"));
    assert!(extra(&st, "title").as_str().unwrap().ends_with("FORTUNEO VIE"));
}

#[test]
fn test_letters_read_again_are_filed_and_join_their_account() {
    let core = core();
    let id = life_account(&core);
    let letter = drop(&core, &fortuneo_arbitrage_v2_pdf("100,00", "40,00"));
    core.auto_import(letter).unwrap().expect("filed");
    // a letter filed without an account (before the contract existed) joins it when it is read again
    db(&core, |c| {
        let mut doc = Document::get(c, letter)?.unwrap();
        let before = doc.clone();
        doc.account_id = None;
        doc.save(c, &before)?;
        Ok(())
    });
    core.refresh_document(letter).unwrap();
    db(&core, |c| {
        assert_eq!(Document::get(c, letter)?.unwrap().account_id, Some(id));
        Ok(())
    });
    // a letter still waiting (invalidated) is filed again, with its account, when it is read again
    core.undo_import(letter).unwrap();
    db(&core, |c| {
        assert_eq!(Document::get(c, letter)?.unwrap().status, "parsed");
        Ok(())
    });
    core.refresh_document(letter).unwrap();
    db(&core, |c| {
        let doc = Document::get(c, letter)?.unwrap();
        assert_eq!((doc.status.as_str(), doc.account_id), ("imported", Some(id)));
        Ok(())
    });
}

#[test]
fn test_web_a_waiting_letter_says_which_account_it_will_join() {
    let mut client = client();
    let csrf = client.login();
    let annual = upload(&mut client, &csrf, "annuel.pdf", &fortuneo_life_annual_pdf(&LifeAnnualSpec::new(2025)));
    let id = doc_of(&annual.url).unwrap();
    client.post(&format!("/documents/{id}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Mon assurance-vie"), ("update_balance", "on"), ("balance", "600.00")]);
    upload(&mut client, &csrf, "arbitrage.pdf", &fortuneo_arbitrage_v2_pdf("100,00", "40,00"));
    let (letter_id, account) = {
        let mut found = (0, None);
        db(&client.core, |c| {
            let doc = Document::query_one(c, "WHERE adapter_id = 'fortuneo.life_arbitrage_pdf'", [])?.unwrap();
            found = (doc.id, doc.account_id);
            Ok(())
        });
        found
    };
    assert!(account.is_some(), "filed on upload, with the account of the contract");
    assert!(client.get(&format!("/documents/{letter_id}")).text().contains("le compte du contrat"));
    client.post(&format!("/documents/{letter_id}/undo"), &[("csrf", &csrf)]);
    let waiting = client.get(&format!("/documents/{letter_id}")).text();
    assert!(waiting.contains("Il sera rattaché au compte") && waiting.contains("Mon assurance-vie"), "{waiting}");
}

// -------------------------------------------------------------------------- the lines of a life-insurance contract
fn contract_value(core: &Core, id: i64) -> Decimal {
    core.portfolio(false).unwrap().accounts.iter().find(|v| v.account.id == id).map(|v| v.total()).unwrap()
}

fn line_units(core: &Core, id: i64) -> Vec<(String, Decimal)> {
    let mut lines: Vec<(String, Decimal)> = vec![];
    db(core, |c| {
        lines = Holding::query(c, "WHERE account_id = ? ORDER BY name", [id])?.into_iter().map(|h| (h.name.unwrap_or_default(), h.quantity)).collect();
        Ok(())
    });
    lines
}

#[test]
fn test_the_lines_of_a_contract_follow_its_papers() {
    let core = core();
    // the yearly statement gives the funds held on 31 December: the contract is worth the sum of its lines, once
    let id = life_account(&core);
    assert_eq!(contract_value(&core, id), d("640.00"));
    assert_eq!(line_units(&core, id).len(), 4);

    // a letter without the whole portfolio: units sold leave, units bought come in (fund A sold out, fund C up)
    let letter = drop(&core, &fortuneo_arbitrage_v2_plain_pdf("100,00", "40,00"));
    core.auto_import(letter).unwrap().expect("filed");
    let lines = line_units(&core, id);
    assert!(!lines.iter().any(|(n, _)| n == "FONDS ACTIONS EXEMPLE A"), "{lines:?}");
    let units = |name: &str| lines.iter().find(|(n, _)| n == name).map(|l| l.1);
    assert_eq!(units("FONDS DIVERS EXEMPLE C"), Some(d("0.5100")));
    assert_eq!(units("FDS EUROS EXEMPLE RENDEMENT"), Some(d("440.00"))); // 440 - 40 sold + 40 bought
    // 440 (euro fund) + 125 (fund B) + 0.51 x 120 (fund C, last unit value of the letter)
    assert_eq!(contract_value(&core, id), d("626.20"));
    db(&core, |c| {
        let summary = crate::services::life_insurance::summary(c, id).unwrap();
        assert_eq!((summary["letters"].clone(), summary["base_from"].clone()), (json!(1), json!("relevé annuel")));
        Ok(())
    });

    // the payments of the operations export since the statement go to the euro fund, and an arbitrage without its letter is said
    let csv = drop_file(&core, "operations.csv", &fortuneo_life_operations_csv());
    core.auto_import(csv).unwrap().expect("one life-insurance account");
    assert_eq!(contract_value(&core, id), d("786.20")); // + 2 payments of 80
    db(&core, |c| {
        let warnings = crate::services::life_insurance::summary(c, id).unwrap()["warnings"].clone();
        assert!(warnings.as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("n'a pas sa lettre")), "{warnings}");
        Ok(())
    });

    // taking a letter back out puts its lines back
    core.undo_import(letter).unwrap();
    assert!(line_units(&core, id).iter().any(|(n, _)| n == "FONDS ACTIONS EXEMPLE A"));
}

#[test]
fn test_a_letter_with_the_whole_portfolio_becomes_the_base() {
    let core = core();
    let id = life_account(&core);
    let letter = drop(&core, &fortuneo_arbitrage_v2_pdf("100,00", "40,00")); // lists the portfolio afterwards: 340 euros and fund C
    core.auto_import(letter).unwrap().expect("filed");
    let lines = line_units(&core, id);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(contract_value(&core, id), d("400.00")); // 340 + 0.5 x 120
    db(&core, |c| {
        assert_eq!(crate::services::life_insurance::summary(c, id).unwrap()["base_from"], json!("lettre d'arbitrage"));
        Ok(())
    });
}

#[test]
fn test_web_the_account_shows_the_lines_of_the_contract() {
    let mut client = client();
    let csrf = client.login();
    let annual = upload(&mut client, &csrf, "annuel.pdf", &fortuneo_life_annual_pdf(&LifeAnnualSpec::new(2025)));
    let id = doc_of(&annual.url).unwrap();
    client.post(&format!("/documents/{id}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Mon assurance-vie"), ("update_balance", "on"), ("balance", "600.00")]);
    let page = client.get("/accounts/1").text();
    for expected in ["FONDS ACTIONS EXEMPLE A", "FR0000000001", "FDS EUROS EXEMPLE RENDEMENT", "Lignes reconstituées d'après le relevé annuel du 31/12/2025", "le relevé annuel de l"] {
        assert!(page.contains(expected) || page.replace("&#39;", "'").contains(expected), "{expected}");
    }
    upload(&mut client, &csrf, "arbitrage.pdf", &fortuneo_arbitrage_v2_plain_pdf("100,00", "40,00"));
    let after = client.get("/accounts/1").text();
    assert!(!after.contains("FONDS ACTIONS EXEMPLE A") && after.contains("une lettre d'arbitrage") || after.contains("1 lettre(s) d&#39;arbitrage") || after.contains("1 lettre(s) d'arbitrage"), "{after}");
}

#[test]
fn test_the_statements_of_an_account_are_validated_oldest_first() {
    let mut client = client();
    client.login();
    let core = client.core.clone();
    let core = &*core;
    let august = FSpec { period_end: ymd(2026, 8, 31), title: "août".into(), ..FSpec::new(vec![fentry(ymd(2026, 8, 5), &["CARTE 04/08 SHOP"], "-9.00")]) };
    let september = FSpec { opening: august.closing(), period_end: ymd(2026, 9, 30), title: "sept".into(), ..FSpec::new(vec![fentry(ymd(2026, 9, 5), &["CARTE 04/09 SHOP"], "-10.00")]) };
    let october = FSpec { opening: september.closing(), period_end: ymd(2026, 10, 31), title: "oct".into(), ..FSpec::new(vec![fentry(ymd(2026, 10, 5), &["CARTE 04/10 SHOP"], "-11.00")]) };
    let other = FSpec { account: "00011112222".into(), title: "autre".into(), ..FSpec::new(vec![fentry(ymd(2026, 10, 6), &["CARTE 05/10 SHOP"], "-1.00")]) };
    // dropped in the wrong order
    let (oct, sep, aug) = (drop_file(core, "doc.pdf", &fortuneo_statement_pdf(&october)), drop_file(core, "doc.pdf", &fortuneo_statement_pdf(&september)), drop_file(core, "doc.pdf", &fortuneo_statement_pdf(&august)));
    let by_hand = form(&[("account_id", "new"), ("name", "Compte Fortuneo"), ("import_transactions", "on")]);

    let page = client.get(&format!("/documents/{oct}")).text();
    assert!(page.contains("À valider dans l'ordre"), "{page}");
    let err = core.import_statement(oct, 0, &by_hand).unwrap_err().to_string();
    assert!(err.contains("Valide d'abord"), "{err}");
    assert!(core.import_statement(sep, 0, &by_hand).is_err(), "September waits for August too");
    assert!(core.pending_reasons().unwrap().iter().any(|(id, why)| *id == oct && why.contains("Valide d'abord")));
    // the oldest one is never blocked
    assert!(!client.get(&format!("/documents/{aug}")).text().contains("À valider dans l'ordre"));
    let imported = core.import_statement(aug, 0, &by_hand).unwrap();
    let account = num(&imported, "account_id");
    assert!(core.import_statement(oct, 0, &by_hand).is_err(), "October still waits for September");
    let account = account.to_string();
    core.import_statement(sep, 0, &form(&[("account_id", &account), ("import_transactions", "on")])).unwrap();
    core.import_statement(oct, 0, &form(&[("account_id", &account), ("import_transactions", "on")])).unwrap();
    // another account does not wait for these
    let doc = drop_file(core, "doc.pdf", &fortuneo_statement_pdf(&other));
    assert!(core.earlier_waiting(doc).unwrap().is_empty());
}
