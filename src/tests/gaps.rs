//! python/tests/test_gaps.py: missing periods between statements: found, shown, uploaded later or marked as normal.

use regex::Regex;
use rust_decimal::Decimal;

use crate::adapters::base::ParsedStatement;
use crate::app::MyMoney;
use crate::error::AppError;
use crate::models::{Account, GapAck, Statement};
use crate::services::coverage::{self, Gap};
use crate::services::statements;
use crate::testkit::env::{client, core, Client, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::{db, drop_file, form, num};

fn d(s: &str) -> Decimal {
    dec(s)
}

/// One monthly statement with a single operation, for the account every fixture statement belongs to.
/// Each month opens where the previous one closed (1000 minus 10 per month), unless `opening` says otherwise.
fn month_with(year: i32, m: u32, opening: Option<&str>) -> Spec {
    let opening = opening.map(|s| s.to_string()).unwrap_or_else(|| format!("{}.00", 1000 - 10 * (m as i64 - 1)));
    let last = coverage::last_day(year, m);
    let prev = ymd(year, m, 1).pred_opt().unwrap();
    Spec {
        opening: d(&opening),
        opening_date: prev,
        period: (ymd(year, m, 1), ymd(year, m, last)),
        ..Spec::new(vec![entry(ymd(year, m, 5), &[&format!("CARTE {:02}/{:02}/{} SHOP {} CB*1234", 5, m, year % 100, m)], "-10.00")])
    }
}

fn month(year: i32, m: u32) -> Spec {
    month_with(year, m, None)
}

/// Drop and import one statement; returns the account id.
fn load(core: &MyMoney, spec: &Spec, account_id: Option<i64>) -> i64 {
    let data = boursobank_statement_pdf(spec);
    let doc_id = drop_file(core, &format!("{}.pdf", spec.period.0), &data);
    let account = account_id.map(|a| a.to_string()).unwrap_or_else(|| "new".into());
    num(&core.import_statement(doc_id, 0, &form(&[("account_id", &account), ("name", "Compte courant"), ("import_transactions", "on")])).unwrap(), "account_id")
}

fn load_months(core: &MyMoney, year: i32, months: &[u32], account_id: Option<i64>) -> i64 {
    let mut account_id = account_id;
    for m in months {
        account_id = Some(load(core, &month(year, *m), account_id));
    }
    account_id.unwrap()
}

fn gaps(core: &MyMoney, include_acknowledged: bool) -> Vec<Gap> {
    db(core, |c| coverage::all_gaps(c, include_acknowledged))
}

// ------------------------------------------------------------------------------------------ detection
#[test]
fn test_a_missing_month_between_two_runs_of_statements_is_found() {
    let core = core();
    load_months(&core, 2026, &[1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12], None); // January to April, then June to December
    let found = gaps(&core, false);
    let [gap] = &found[..] else { panic!("one gap expected") };
    assert_eq!((gap.start, gap.end, gap.days()), (ymd(2026, 5, 1), ymd(2026, 5, 31), 31));
    assert_eq!(gap.label(), "mai 2026");
    assert_eq!(gap.account_name, "Compte courant");
    assert!(!gap.acknowledged());
}

#[test]
fn test_contiguous_overlapping_and_single_statements_leave_no_gap() {
    let core = core();
    load_months(&core, 2026, &[1, 2, 3], None);
    assert!(gaps(&core, false).is_empty());
    let overlap = Spec { period: (ymd(2026, 3, 15), ymd(2026, 4, 15)), opening_date: ymd(2026, 3, 14), ..Spec::new(vec![entry(ymd(2026, 3, 20), &["CARTE 19/03/26 CAFE CB*1234"], "-3.00")]) };
    load(&core, &overlap, Some(1));
    assert!(gaps(&core, false).is_empty()); // a statement overlapping the others covers days, it does not open a hole
    assert!(coverage::gaps_between(&[(ymd(2026, 1, 1), ymd(2026, 1, 31))]).is_empty());
    assert!(coverage::gaps_between(&[]).is_empty());
}

#[test]
fn test_gaps_are_labelled_by_months_or_by_dates() {
    assert_eq!(coverage::describe(ymd(2026, 5, 1), ymd(2026, 5, 31)), "mai 2026");
    assert_eq!(coverage::describe(ymd(2026, 6, 1), ymd(2026, 8, 31)), "juin → août 2026");
    assert_eq!(coverage::describe(ymd(2025, 12, 1), ymd(2026, 2, 28)), "décembre 2025 → février 2026");
    assert_eq!(coverage::describe(ymd(2026, 2, 1), ymd(2026, 2, 28)), "février 2026");
    assert_eq!(coverage::describe(ymd(2026, 5, 5), ymd(2026, 5, 28)), "du 05/05/2026 au 28/05/2026");
    assert_eq!(
        coverage::gaps_between(&[(ymd(2026, 1, 1), ymd(2026, 1, 31)), (ymd(2026, 3, 1), ymd(2026, 3, 31)), (ymd(2026, 6, 1), ymd(2026, 6, 30))]),
        vec![(ymd(2026, 2, 1), ymd(2026, 2, 28)), (ymd(2026, 4, 1), ymd(2026, 5, 31))]
    );
}

#[test]
fn test_uploading_the_missing_statement_closes_the_gap() {
    let core = core();
    let account = load_months(&core, 2026, &[1, 2, 3, 4, 6, 7], None);
    assert_eq!(gaps(&core, false).len(), 1);
    load(&core, &month(2026, 5), Some(account));
    assert!(gaps(&core, false).is_empty());
}

fn mid_may() -> Spec {
    Spec { period: (ymd(2026, 5, 10), ymd(2026, 5, 20)), opening_date: ymd(2026, 5, 9), ..Spec::new(vec![entry(ymd(2026, 5, 12), &["CARTE 11/05/26 CAFE CB*1234"], "-3.00")]) }
}

#[test]
fn test_a_partial_statement_splits_the_gap_without_hiding_what_is_left() {
    let core = core();
    let account = load_months(&core, 2026, &[1, 2, 3, 4, 6], None);
    load(&core, &mid_may(), Some(account));
    assert_eq!(gaps(&core, false).iter().map(|g| (g.start, g.end)).collect::<Vec<_>>(), vec![(ymd(2026, 5, 1), ymd(2026, 5, 9)), (ymd(2026, 5, 21), ymd(2026, 5, 31))]);
}

#[test]
fn test_archived_accounts_are_not_reported_and_accounts_are_independent() {
    let core = core();
    let first = load_months(&core, 2026, &[1, 3], None);
    let other = load(&core, &Spec { bank: "12345".into(), branch: "80999".into(), account: "00077777777".into(), ..Spec::new(month(2026, 1).entries) }, None); // another account, no hole
    assert_eq!(gaps(&core, false).iter().map(|g| g.account_id).collect::<Vec<_>>(), vec![first]);
    assert_ne!(other, first);
    db(&core, |c| {
        let mut account = Account::get(c, first)?.unwrap();
        let before = account.clone();
        account.archived = true;
        account.save(c, &before)?;
        Ok(())
    });
    assert!(gaps(&core, false).is_empty());
}

// ---------------------------------------------------------------------------------- "c'est normal"
#[test]
fn test_marking_a_gap_as_normal_silences_it_until_it_is_restored() {
    let core = core();
    load_months(&core, 2026, &[1, 2, 3, 4, 6, 7], None);
    let found = gaps(&core, false);
    let [gap] = &found[..] else { panic!("one gap expected") };
    db(&core, |c| coverage::acknowledge(c, gap.account_id, gap.start, gap.end, Some(" pas de mouvement ")));
    assert!(gaps(&core, false).is_empty());
    let shown = gaps(&core, true);
    let [shown] = &shown[..] else { panic!("one gap expected") };
    assert!(shown.acknowledged());
    db(&core, |c| {
        let acks = GapAck::query(c, "", [])?;
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].note.as_deref(), Some("pas de mouvement"));
        coverage::restore(c, shown.ack_id.unwrap())
    });
    assert_eq!(gaps(&core, false).len(), 1); // reported again
}

#[test]
fn test_a_normal_mark_covers_the_pieces_but_not_a_different_hole() {
    let core = core();
    let account = load_months(&core, 2026, &[1, 2, 3, 4, 6, 7, 9], None); // holes: May and August
    let found = gaps(&core, false);
    assert_eq!(found.len(), 2);
    db(&core, |c| coverage::acknowledge(c, account, found[0].start, found[0].end, None));
    assert_eq!(gaps(&core, false).iter().map(|g| g.label()).collect::<Vec<_>>(), vec!["août 2026"]); // only the hole that was marked is quiet

    load(&core, &mid_may(), Some(account));
    assert_eq!(gaps(&core, false).iter().map(|g| g.label()).collect::<Vec<_>>(), vec!["août 2026"]); // May is now two smaller holes, still inside what was marked
}

#[test]
fn test_only_an_existing_gap_can_be_marked() {
    let core = core();
    let account = load_months(&core, 2026, &[1, 2, 3], None);
    db(&core, |c| {
        match coverage::acknowledge(c, account, ymd(2026, 5, 1), ymd(2026, 5, 31), None) {
            Err(AppError::Value(m)) => assert!(m.contains("plus un trou"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(coverage::acknowledge(c, 999, ymd(2026, 5, 1), ymd(2026, 5, 31), None), Err(AppError::Value(_))));
        assert!(matches!(coverage::acknowledge(c, account, ymd(2026, 5, 31), ymd(2026, 5, 1), None), Err(AppError::Value(_))));
        Ok(())
    });
    db(&core, |c| {
        assert!(GapAck::query(c, "", [])?.is_empty());
        Ok(())
    });
}

#[test]
fn test_the_import_check_does_not_repeat_a_hole_marked_as_normal() {
    let mut st = ParsedStatement::new("pdf");
    st.period_start = Some(ymd(2026, 6, 1));
    st.period_end = Some(ymd(2026, 6, 30));
    st.opening_balance = Some(d("900"));
    st.balance = Some(d("890"));
    let april = Statement { account_id: 1, period_start: Some(ymd(2026, 4, 1)), period_end: Some(ymd(2026, 4, 30)), opening_balance: Some(d("1000")), closing_balance: Some(d("900")), ..Default::default() };
    let issues = statements::continuity(&st, &[april.clone()], &[], &[]);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("Il manque une période") && issues[0].contains("01/05/2026") && issues[0].contains("31/05/2026"));
    assert!(statements::continuity(&st, &[april.clone()], &[(ymd(2026, 5, 1), ymd(2026, 5, 31))], &[]).is_empty());
    assert!(!statements::continuity(&st, &[april], &[(ymd(2026, 5, 10), ymd(2026, 5, 31))], &[]).is_empty()); // a smaller mark does not cover it
}

#[test]
fn test_balances_that_do_not_chain_are_still_reported_on_the_account() {
    let core = core();
    let account = load_months(&core, 2026, &[1, 2], None);
    let broken = month_with(2026, 3, Some("5.00")); // March does not start where February ended
    load(&core, &broken, Some(account));
    let (_, chain) = db(&core, |c| statements::coverage(c, account));
    assert_eq!(chain.len(), 1);
    assert!(chain[0].contains("ne se suivent pas"));
}

// ------------------------------------------------------------------------------------------------- web
fn upload_pdf(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

#[test]
fn test_web_dashboard_flags_the_missing_month_and_offers_both_actions() {
    let mut client = client();
    let csrf = client.login();
    let account = load_months(&client.core, 2026, &[1, 2, 3, 4, 6, 7], None);
    let home = client.get("/").text();
    assert!(home.contains("Relevés manquants") && home.contains("mai 2026") && home.contains("31 jours sans relevé"));
    assert!(home.contains(&format!("/documents?account_id={account}&start=2026-05-01&end=2026-05-31")));
    assert!(home.contains("Déposer le relevé manquant") && home.contains("C'est normal"));

    let hint = client.get(&format!("/documents?account_id={account}&start=2026-05-01&end=2026-05-31"));
    let text = hint.text();
    assert!(hint.status == 200 && text.contains("Tu cherches le relevé") && text.contains("mai 2026") && text.contains("Compte courant"));
    assert_eq!(client.get("/documents?account_id=999&start=2026-05-01&end=2026-05-31").status, 200);
    assert_eq!(client.get("/documents?account_id=1&start=nope&end=nope").status, 200);

    let done = client.post("/gaps/acknowledge", &[("csrf", &csrf), ("account_id", &account.to_string()), ("start", "2026-05-01"), ("end", "2026-05-31"), ("next", "/")]);
    assert!(done.text().contains("plus signalé") && !done.text().contains("Relevés manquants"));

    let page = client.get(&format!("/accounts/{account}")).text();
    assert!(page.contains("mai 2026 : marqué comme normal") && page.contains("Remettre en alerte"));
    let ack_id = Regex::new(r"/gaps/(\d+)/restore").unwrap().captures(&page).unwrap()[1].to_string();
    let back = client.post(&format!("/gaps/{ack_id}/restore"), &[("csrf", &csrf), ("next", "/")]);
    assert!(back.text().contains("Relevés manquants") && back.text().contains("mai 2026"));
}

#[test]
fn test_web_uploading_the_missing_statement_makes_the_card_disappear() {
    let mut client = client();
    let csrf = client.login();
    let account = load_months(&client.core, 2026, &[1, 2, 3, 4, 6], None);
    assert!(client.get("/").text().contains("Relevés manquants"));
    let r = upload_pdf(&mut client, &csrf, "mai.pdf", &boursobank_statement_pdf(&month(2026, 5)));
    assert_eq!(r.status, 200);
    let text = r.text();
    if text.contains("/import/0") {
        // reviewed because of the gap around it: confirm
        let doc_id = Regex::new(r"/documents/(\d+)/import/0").unwrap().captures(&text).unwrap()[1].to_string();
        client.post(&format!("/documents/{doc_id}/import/0"), &[("csrf", &csrf), ("account_id", &account.to_string()), ("import_transactions", "on")]);
    }
    assert!(!client.get("/").text().contains("Relevés manquants"));
}

#[test]
fn test_web_stale_or_forged_gap_forms_change_nothing() {
    let mut client = client();
    let csrf = client.login();
    let account = load_months(&client.core, 2026, &[1, 2, 3], None).to_string();
    let r = client.post("/gaps/acknowledge", &[("csrf", &csrf), ("account_id", &account), ("start", "2026-05-01"), ("end", "2026-05-31"), ("next", "//evil.example")]);
    assert!(r.status == 200 && !r.url.contains("evil.example") && r.text().contains("plus un trou"));
    assert_eq!(client.post("/gaps/acknowledge", &[("csrf", &csrf), ("account_id", &account), ("start", "x"), ("end", "y")]).status, 200);
    assert_eq!(client.post("/gaps/acknowledge", &[("account_id", &account), ("start", "2026-05-01"), ("end", "2026-05-31")]).status, 403); // no CSRF token
    db(&client.core, |c| {
        assert!(GapAck::query(c, "", [])?.is_empty());
        Ok(())
    });
}
