//! python/tests/test_loans.py: a loan from its documents (offer, early repayment notice, dated amortization table).

use chrono::NaiveDate;
use regex::Regex;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::adapters::base::{ParsedStatement, SourceDocument};
use crate::adapters::registry::{identify, Identification};
use crate::app::MyMoney;
use crate::error::AppError;
use crate::models::{Account, Loan, LoanInstallment, Transaction};
use crate::services::loans::{self, Plan};
use crate::testkit::env::{client, core, Client, Core, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::fortuneo::{fentry, fortuneo_statement_pdf, FSpec};
use crate::testkit::loan::*;
use crate::testkit::{db, drop_file, form, num};

fn d(s: &str) -> Decimal {
    dec(s)
}

fn today() -> NaiveDate {
    ymd(2026, 10, 24)
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn read(data: Vec<u8>) -> (Identification, Option<ParsedStatement>) {
    let doc = SourceDocument::new("doc.pdf", data);
    let found = identify(&doc);
    let st = found.chosen.as_ref().map(|c| c.adapter.parse(&doc).unwrap().remove(0));
    (found, st)
}

fn drop(core: &Core, data: &[u8]) -> i64 {
    drop_file(core, "doc.pdf", data)
}

/// Drop a document like the user would: imported on its own when it can be, else attached to `account_id` (or a new loan).
fn feed(core: &Core, data: &[u8], account_id: Option<i64>) -> (i64, serde_json::Map<String, Value>) {
    let doc_id = drop(core, data);
    let result = match core.auto_import(doc_id).unwrap() {
        Some(r) => r,
        None => core.import_statement(doc_id, 0, &form(&[("account_id", &account_id.map(|a| a.to_string()).unwrap_or_else(|| "new".into()))])).unwrap(),
    };
    (doc_id, result)
}

fn plan_of(core: &MyMoney, account_id: i64) -> (Loan, Plan) {
    db(core, |c| {
        let mut loan = Loan::query_one(c, "WHERE account_id = ?", [account_id])?.unwrap();
        let plan = loans::rebuild(c, &mut loan, Some(today()))?;
        Ok((loan, plan))
    })
}

fn full_loan(core: &Core, spec: &LoanSpec, order: &[&str]) -> i64 {
    let mut account_id = None;
    for kind in order {
        let data = match *kind {
            "offer" => offer_pdf(spec),
            "event" => prepayment_pdf(spec, None),
            _ => schedule_pdf(spec, None),
        };
        let (_, result) = feed(core, &data, account_id);
        account_id = account_id.or(Some(num(&result, "account_id")));
    }
    account_id.unwrap()
}

fn expect_value<T: std::fmt::Debug>(result: Result<T, AppError>, needle: &str) {
    match result {
        Err(AppError::Value(m)) => assert!(m.contains(needle), "{m:?} does not contain {needle:?}"),
        other => panic!("expected a ValueError containing {needle:?}, got {other:?}"),
    }
}

fn loan_extra(st: &ParsedStatement) -> Value {
    st.extra.get("loan").cloned().unwrap_or(Value::Null)
}

// ------------------------------------------------------------------------------------------ detection
#[test]
fn test_each_document_goes_to_its_own_adapter() {
    let spec = LoanSpec::new();
    for (data, adapter, kind) in [
        (offer_pdf(&spec), "fortuneo.loan_offer_pdf", "loan_offer"),
        (prepayment_pdf(&spec, None), "fortuneo.loan_prepayment_pdf", "loan_event"),
        (schedule_pdf(&spec, None), "fortuneo.loan_schedule_pdf", "loan_schedule"),
    ] {
        let (found, st) = read(data);
        let st = st.unwrap();
        assert_eq!(found.decision, "auto");
        assert_eq!(found.chosen.as_ref().unwrap().adapter.id(), adapter);
        assert_eq!(st.document_kind, s(kind));
        assert_eq!((st.institution.clone(), st.account_type.clone(), st.confidence.clone()), (s("Fortuneo"), s("mortgage"), "high".to_string()));
        assert!(st.warnings.is_empty());
    }
}

#[test]
fn test_the_brand_is_required() {
    let other = || LoanSpec { brand: "Autre".into(), ..LoanSpec::new() };
    for data in [offer_pdf(&other()), prepayment_pdf(&other(), None), schedule_pdf(&other(), None)] {
        let found = identify(&SourceDocument::new("x.pdf", data));
        assert!(!(found.decision == "auto" && found.chosen.as_ref().unwrap().adapter.id().starts_with("fortuneo.loan")));
    }
}

// ----------------------------------------------------------------------------------------------- offer
#[test]
fn test_offer_is_read_and_its_own_figures_are_checked() {
    let spec = LoanSpec::new();
    let st = read(offer_pdf(&spec)).1.unwrap();
    let loan = loan_extra(&st);
    assert_eq!((loan["reference"].clone(), loan["project"].clone(), loan["product"].clone()), (json!(L_REF), json!(spec.project), json!("Fiximmo")));
    assert_eq!((loan["principal"].clone(), loan["duration"].clone(), loan["rate"].clone(), loan["taeg"].clone()), (json!("60000.00"), json!(60), json!("2.40"), json!("2.95")));
    assert_eq!(loan["object"], json!("Achat ancien"));
    assert_eq!(loan["destination"], json!("Résidence principale"));
    assert_eq!(st.holder_name, s(L_BORROWER));
    assert_eq!(st.account_label, s("Prêt Fiximmo …0002"));
    let rows = loan["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 60);
    assert_eq!(rows[0]["n"], json!(1));
    assert_eq!(rows[59]["remaining"], json!("0.00"));
    let text = loan.to_string();
    assert!(!text.contains("12345678") && !text.contains("1980")); // the borrower's id and birth date are not kept
}

#[test]
fn test_offer_with_figures_that_do_not_add_up_is_flagged() {
    let st = read(offer_pdf(&LoanSpec { wrong_interest_total: true, ..LoanSpec::new() })).1.unwrap();
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("intérêts")));
    let st = read(offer_pdf(&LoanSpec { broken_row: Some(7), ..LoanSpec::new() })).1.unwrap();
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("total à payer")));
}

#[test]
fn test_other_documents_read() {
    let spec = LoanSpec::new();
    let event = read(prepayment_pdf(&spec, None)).1.unwrap();
    let loan = loan_extra(&event);
    let ev = &loan["event"];
    assert_eq!(ev["on_date"], json!("2026-06-06"));
    assert_eq!(ev["before"]["capital"], json!(spec.capital_before().to_string()));
    assert_eq!(ev["after"]["capital"], json!(spec.capital_after().to_string()));
    assert_eq!(ev["before"]["end"], json!(spec.end_before().to_string()));
    assert_eq!(ev["after"]["end"], json!(spec.end_after().to_string()));
    assert_eq!(loan["borrower"], json!(L_BORROWER));
    let sched = read(schedule_pdf(&spec, None)).1.unwrap();
    let loan = loan_extra(&sched);
    let schedule = spec.schedule();
    assert_eq!(loan["reference"], json!(L_REF));
    assert_eq!(loan["rate"], json!("2.40"));
    assert_eq!(loan["balance"], json!((schedule[0].remaining + schedule[0].capital).to_string()));
    assert_eq!(sched.period_start, schedule[0].day);
    assert_eq!(sched.period_end, Some(spec.end_after()));
    assert_eq!(loan["rows"].as_array().unwrap().len(), schedule.len());
}

// --------------------------------------------------------------------------------------- the whole loan
fn check_the_three_documents_rebuild_the_loan(order: &[&str]) {
    let core = core();
    let spec = LoanSpec::new();
    let account_id = full_loan(&core, &spec, order);
    let (loan, plan) = plan_of(&core, account_id);
    assert!(plan.problems().is_empty(), "{:?}", plan.problems().iter().map(|c| c.text.clone()).collect::<Vec<_>>());
    assert_eq!((loan.reference.clone(), loan.principal, loan.rate), (s(L_REF), Some(spec.principal), Some(spec.rate)));
    assert_eq!(loan.first_due_date, Some(spec.first_due)); // dated from the repayment notice, which points at instalment n° 15
    let sm = plan.summary.as_ref().unwrap();
    assert_eq!(sm.end_date, Some(spec.end_after()));
    assert_eq!(plan.installments().last().unwrap().remaining, Decimal::ZERO);
    // Capital owed today: what the last instalment up to today leaves.
    let expected = spec.new_plan().into_iter().filter(|x| x.day.unwrap() <= today()).last().unwrap().remaining;
    assert_eq!(sm.owed, Some(expected));
    let total = spec.repaid_after as usize + spec.new_plan().len();
    assert_eq!(sm.paid_count + sm.remaining_count, sm.total_count);
    assert_eq!(sm.total_count, total);
    assert_eq!(sm.repaid_early, spec.repaid);
    let saved = sm.saved.as_ref().unwrap();
    assert!(saved.interest > Decimal::ZERO);
    assert_eq!(saved.months, spec.months as i64 - sm.total_count as i64);
    let next = sm.next.as_ref().unwrap();
    assert_eq!((next.date, next.total), (spec.schedule()[0].day, spec.schedule()[0].total));
    assert!(plan.checks.iter().any(|c| c.text.contains("retombe sur le tableau daté")) && plan.checks.iter().any(|c| c.text.contains("entièrement remboursé")));
    db(&core, |c| {
        let account = Account::get(c, account_id)?.unwrap();
        assert_eq!((account.account_type.as_str(), account.holder_name.clone(), account.balance), ("mortgage", s(L_BORROWER), expected));
        Ok(())
    });
}

#[test]
fn test_the_three_documents_rebuild_the_loan_in_any_order_offer_event_schedule() {
    check_the_three_documents_rebuild_the_loan(&["offer", "event", "schedule"]);
}

#[test]
fn test_the_three_documents_rebuild_the_loan_in_any_order_schedule_event_offer() {
    check_the_three_documents_rebuild_the_loan(&["schedule", "event", "offer"]);
}

#[test]
fn test_the_three_documents_rebuild_the_loan_in_any_order_event_schedule_offer() {
    check_the_three_documents_rebuild_the_loan(&["event", "schedule", "offer"]);
}

#[test]
fn test_the_three_documents_rebuild_the_loan_in_any_order_schedule_offer_event() {
    check_the_three_documents_rebuild_the_loan(&["schedule", "offer", "event"]);
}

#[test]
fn test_rows_are_in_order_with_the_repayment_between_the_contract_and_the_new_plan() {
    let core = core();
    let spec = LoanSpec::new();
    let (_, plan) = plan_of(&core, full_loan(&core, &spec, &["offer", "event", "schedule"]));
    let kinds: Vec<(&str, &str)> = plan.rows.iter().map(|r| (r.kind, r.source)).collect();
    let first_new = plan.rows.iter().position(|r| r.kind == "prepayment").unwrap();
    assert_eq!(kinds[0], ("installment", "contrat"));
    assert_eq!(kinds[first_new - 1], ("installment", "contrat"));
    assert_eq!(plan.rows[first_new - 1].number, Some(spec.repaid_after as i64));
    assert_eq!(plan.rows[first_new].remaining, spec.capital_after());
    assert_eq!(plan.rows[first_new].capital, spec.repaid);
    assert_eq!(kinds[first_new + 1], ("installment", "calculé"));
    assert_eq!(*kinds.last().unwrap(), ("installment", "échéancier"));
    let dates: Vec<_> = plan.rows.iter().map(|r| r.date).collect();
    let mut sorted = dates.clone();
    sorted.sort();
    assert_eq!(dates, sorted);
    assert_eq!(plan.rows[0].date, Some(spec.first_due));
}

#[test]
fn test_without_the_repayment_notice_the_missing_repayment_is_noticed() {
    let core = core();
    let spec = LoanSpec::new();
    let account_id = full_loan(&core, &spec, &["offer", "schedule"]);
    let (loan, plan) = plan_of(&core, account_id);
    assert_eq!(loan.first_due_date, None); // nothing dates the contract's instalments
    let problems = plan.problems();
    let warning = problems.iter().find(|c| c.text.contains("aucune échéance du contrat ne mène à ce capital")).expect("a warning");
    assert!(warning.text.contains("remboursement anticipé"));
    // The notice arrives: everything is explained.
    feed(&core, &prepayment_pdf(&spec, None), Some(account_id));
    let (loan, plan) = plan_of(&core, account_id);
    assert!(plan.problems().is_empty());
    assert_eq!(loan.first_due_date, Some(spec.first_due));
}

#[test]
fn test_a_repayment_that_is_not_in_the_contract_is_flagged() {
    let core = core();
    let account_id = full_loan(&core, &LoanSpec::new(), &["offer"]);
    let st = read(prepayment_pdf(&LoanSpec { principal: d("61000.00"), ..LoanSpec::new() }, None)).1.unwrap(); // the capital owed before is not one of this contract's
    db(&core, |c| {
        let account = Account::get(c, account_id)?.unwrap();
        assert_eq!(loans::find_accounts(c, &st)?.iter().map(|a| a.id).collect::<Vec<_>>(), vec![account.id]); // the borrower's name is the only link
        assert!(loans::unverified(c, &st, Some(&account))?.unwrap().contains("ne figure dans aucun tableau"));
        Ok(())
    });
}

#[test]
fn test_the_notice_alone_asks_and_the_offer_then_claims_it() {
    let core = core();
    let spec = LoanSpec::new();
    let doc_id = drop(&core, &prepayment_pdf(&spec, None));
    assert!(core.auto_import(doc_id).unwrap().is_none()); // no loan yet: the user is asked
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Mon prêt")])).unwrap();
    let account_id = num(&result, "account_id");
    db(&core, |c| {
        let loan = Loan::query_one(c, "", [])?.unwrap();
        assert_eq!(loan.reference, None);
        assert_eq!(loans::events(c, loan.id)?.len(), 1);
        Ok(())
    });
    let offer_id = drop(&core, &offer_pdf(&spec));
    let review = core.review_document(offer_id).unwrap().unwrap();
    assert_eq!(review.plans[0].candidates.iter().map(|a| a.id).collect::<Vec<_>>(), vec![account_id]); // matched by its figures
    assert_eq!(num(&core.auto_import(offer_id).unwrap().unwrap(), "account_id"), account_id);
    let (loan, _) = plan_of(&core, account_id);
    assert_eq!(loan.reference, s(L_REF));
    assert_eq!(loan.first_due_date, Some(spec.first_due));
}

// ------------------------------------------------------------------------------- duplicates and undo
#[test]
fn test_a_document_already_imported_is_refused_unless_forced_and_can_be_undone() {
    let core = core();
    let spec = LoanSpec::new();
    let account_id = full_loan(&core, &spec, &["offer", "event", "schedule"]);
    let again = drop(&core, &offer_pdf(&LoanSpec { title: "another download".into(), ..LoanSpec::new() }));
    let review = core.review_document(again).unwrap().unwrap();
    assert!(review.plans[0].loan_existing.is_some() && !review.plans[0].clean());
    assert!(core.auto_import(again).unwrap().is_none());
    expect_value(core.import_statement(again, 0, &form(&[("account_id", &account_id.to_string())])), "déjà son offre");
    core.import_statement(again, 0, &form(&[("account_id", &account_id.to_string()), ("force", "on")])).unwrap();
    db(&core, |c| {
        assert_eq!(LoanInstallment::query(c, "WHERE source = 'offer'", [])?.len(), spec.months as usize);
        assert_eq!(Loan::query_one(c, "", [])?.unwrap().offer_document_id, Some(again));
        Ok(())
    });

    core.undo_import(again).unwrap();
    db(&core, |c| {
        let loan = Loan::query_one(c, "", [])?.unwrap();
        assert!(loan.offer_document_id.is_none() && loan.principal.is_none());
        assert!(!loans::installments(c, loan.id)?.iter().any(|i| i.source == "offer"));
        assert!(!loans::events(c, loan.id)?.is_empty() && !loans::installments(c, loan.id)?.is_empty()); // the rest stays
        Ok(())
    });

    // A repayment notice twice is the same event.
    let notice = drop(&core, &prepayment_pdf(&LoanSpec { title: "copy".into(), ..LoanSpec::new() }, None));
    expect_value(core.import_statement(notice, 0, &form(&[("account_id", &account_id.to_string())])), "déjà enregistré");
}

#[test]
fn test_undoing_the_only_document_removes_the_loan_but_not_the_account() {
    let core = core();
    let (doc_id, result) = feed(&core, &offer_pdf(&LoanSpec::new()), None);
    core.undo_import(doc_id).unwrap();
    db(&core, |c| {
        assert!(Loan::query_one(c, "", [])?.is_none());
        assert!(Account::get(c, num(&result, "account_id"))?.is_some());
        Ok(())
    });
}

#[test]
fn test_a_table_with_another_rate_than_the_loan_is_refused() {
    let core = core();
    let account_id = full_loan(&core, &LoanSpec::new(), &["offer"]);
    let other = drop(&core, &schedule_pdf(&LoanSpec { rate: d("3.10"), ..LoanSpec::new() }, None));
    expect_value(core.import_statement(other, 0, &form(&[("account_id", &account_id.to_string())])), "taux");
}

#[test]
fn test_a_loan_document_needs_a_credit_account() {
    let core = core();
    db(&core, |c| {
        let mut a = Account::default();
        a.name = "Courant".into();
        a.account_type = "checking".into();
        a.insert(c)?;
        Ok(())
    });
    let doc_id = drop(&core, &offer_pdf(&LoanSpec::new()));
    expect_value(core.import_statement(doc_id, 0, &form(&[("account_id", "1")])), "type crédit");
}

// ----------------------------------------------------------------------------- statements and dates
#[test]
fn test_instalments_found_in_the_account_statements_are_compared_with_the_plan() {
    let core = core();
    let spec = LoanSpec::new();
    let account_id = full_loan(&core, &spec, &["offer", "event", "schedule"]);
    let paid = spec.new_plan()[3].clone(); // an instalment of the new plan, debited the day before its due date
    let day = paid.day.unwrap();
    let statement = FSpec {
        opening: d("5000.00"),
        period_end: ymd(day.year_(), day.month_(), 28),
        ..FSpec::new(vec![crate::testkit::fortuneo::FEntry {
            value_day: Some(day),
            amount: -paid.total,
            ..fentry(ymd(day.year_(), day.month_(), 14), &[&format!("ECH PRET 0099{L_REF}")], "0")
        }])
    };
    let data = fortuneo_statement_pdf(&statement);
    let (doc_id, _) = core.store_document("releve.pdf", &data).unwrap();
    core.analyze_document(doc_id, None, Some(data)).unwrap();
    core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte courant"), ("import_transactions", "on")])).unwrap();
    let (_, plan) = plan_of(&core, account_id);
    assert!(plan.checks.iter().any(|c| c.text.contains("1 prélèvement(s) retrouvé(s)")));
    assert_eq!(plan.installments().iter().filter(|r| r.date == Some(day)).map(|r| r.confirmed).collect::<Vec<_>>(), vec![Some(paid.total)]);
    assert!(plan.problems().is_empty());

    db(&core, |c| {
        // replace the first statement's line by one with another amount
        let mut t = Transaction::query_one(c, "WHERE kind = 'loan_payment'", [])?.unwrap();
        let before = t.clone();
        t.amount -= d("1.00");
        t.save(c, &before)?;
        Ok(())
    });
    let (_, plan) = plan_of(&core, account_id);
    assert!(plan.problems().iter().any(|c| c.text.contains("débités") && c.text.contains("prévus au plan")));
}

trait Ymd {
    fn year_(&self) -> i32;
    fn month_(&self) -> u32;
}

impl Ymd for NaiveDate {
    fn year_(&self) -> i32 {
        chrono::Datelike::year(self)
    }
    fn month_(&self) -> u32 {
        chrono::Datelike::month(self)
    }
}

#[test]
fn test_first_due_date_can_be_given_by_hand_and_dates_the_contract() {
    let core = core();
    let spec = LoanSpec::new();
    let (_, result) = feed(&core, &offer_pdf(&spec), None);
    let (loan, plan) = plan_of(&core, num(&result, "account_id"));
    assert!(loan.first_due_date.is_none() && !plan.summary.as_ref().unwrap().dated && plan.checks.iter().any(|c| c.status == "info"));
    db(&core, |c| {
        let mut loan = Loan::query_one(c, "", [])?.unwrap();
        let plan = loans::set_first_due(c, &mut loan, ymd(2025, 3, 15))?;
        assert_eq!(plan.rows[0].date, Some(ymd(2025, 3, 15)));
        assert_eq!(plan.rows.last().unwrap().date, Some(spec.end_before()));
        assert!(plan.summary.as_ref().unwrap().dated);
        Ok(())
    });
}

#[test]
fn test_without_any_repayment_the_contract_is_matched_by_the_dated_table() {
    let core = core();
    let spec = LoanSpec { repaid: d("0.00"), new_payment_delta: d("0.00"), ..LoanSpec::new() };
    let account_id = full_loan(&core, &spec, &["offer", "schedule"]);
    let (loan, plan) = plan_of(&core, account_id);
    assert_eq!(loan.first_due_date, Some(spec.first_due));
    assert!(plan.problems().is_empty());
}

// ---------------------------------------------------------------------------------------- unit pieces
#[test]
fn test_dates_and_projection() {
    assert_eq!(loans::add_months(ymd(2025, 1, 31), 1), ymd(2025, 2, 28));
    assert_eq!(loans::add_months(ymd(2025, 11, 15), 3), ymd(2026, 2, 15));
    let rows = loans::project(d("1000.00"), d("12.00"), d("100.00"), d("0"), ymd(2026, 1, 15), "calculé");
    assert_eq!(rows[0].interest, d("10.00"));
    assert_eq!(rows[0].capital, d("90.00"));
    assert_eq!(rows.last().unwrap().remaining, Decimal::ZERO);
    assert_eq!(rows.last().unwrap().date, Some(ymd(2026, 11, 15)));
    assert!(loans::project(d("1000.00"), d("12.00"), d("5.00"), d("0"), ymd(2026, 1, 15), "calculé").is_empty()); // the payment does not even cover the interest
}

// ------------------------------------------------------------------------------------------------- web
fn upload(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

#[test]
fn test_web_the_loan_page_shows_the_plan_and_the_checks() {
    let mut client = client();
    let csrf = client.login();
    let spec = LoanSpec::new();
    let review = upload(&mut client, &csrf, "offre.pdf", &offer_pdf(&spec));
    let text = review.text();
    assert!(text.contains("Offre de prêt") && text.contains("Fortuneo · offre de prêt immobilier (PDF)") && text.contains("Créer le prêt à partir de ce document"));
    let doc = Regex::new(r"/documents/(\d+)").unwrap().captures(&review.url).unwrap()[1].to_string();
    let done = client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Prêt Fiximmo"), ("account_type", "mortgage")]);
    let account_id = Regex::new(r"/accounts/(\d+)").unwrap().captures(&done.url).unwrap()[1].to_string();
    assert!(done.text().contains("offre de prêt enregistrée") && done.text().contains("Dater les échéances du contrat"));

    for data in [prepayment_pdf(&spec, None), schedule_pdf(&spec, None)] {
        assert!(upload(&mut client, &csrf, "doc.pdf", &data).text().contains("plan du prêt mis à jour"));
    }
    let page = client.get(&format!("/accounts/{account_id}")).text();
    assert!(page.contains("tout est cohérent") && !page.contains("Dater les échéances du contrat"));
    assert!(page.contains(L_REF) && page.contains("Capital restant dû") && page.contains("Remboursement anticipé"));
    assert!(page.contains("lu dans les documents du prêt") && page.contains(&spec.end_after().format("%d/%m/%Y").to_string()));

    let library = client.get("/documents").text();
    assert!(library.contains("Offre de prêt") && (library.contains("Tableau d&#39;amortissement") || library.contains("Tableau d'amortissement")));
    assert!(library.contains("Remboursement anticipé"));

    let home = client.get("/");
    assert!(home.status == 200 && home.text().contains("Prêt Fiximmo"));
}

#[test]
fn test_web_first_due_form() {
    let mut client = client();
    let csrf = client.login();
    let review = upload(&mut client, &csrf, "offre.pdf", &offer_pdf(&LoanSpec::new()));
    let doc = Regex::new(r"/documents/(\d+)").unwrap().captures(&review.url).unwrap()[1].to_string();
    let done = client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("account_type", "mortgage")]);
    let account_id = Regex::new(r"/accounts/(\d+)").unwrap().captures(&done.url).unwrap()[1].to_string();
    let bad = client.post(&format!("/accounts/{account_id}/loan/first-due"), &[("csrf", &csrf), ("first_due", "nope")]);
    assert!(bad.text().contains("Date invalide"));
    let ok = client.post(&format!("/accounts/{account_id}/loan/first-due"), &[("csrf", &csrf), ("first_due", "2025-03-15")]);
    assert!(ok.text().contains("Date de la première échéance enregistrée") && ok.text().contains("15/03/2025"));
    assert_eq!(client.post("/accounts/999/loan/first-due", &[("csrf", &csrf), ("first_due", "2025-03-15")]).status, 404);
}
