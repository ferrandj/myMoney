//! Hello bank!: the "Relevé de compte chèques" PDF, on synthetic documents (the layout of a real statement, made-up
//! holder, address, IBAN and amounts).

use rust_decimal::Decimal;

use crate::adapters::base::{ParsedStatement, SourceDocument};
use crate::adapters::registry::{identify, Identification};
use crate::models::{Account, Document, Transaction};
use crate::testkit::env::{client, core};
use crate::testkit::fixtures::*;
use crate::testkit::hellobank::*;
use crate::testkit::{db, drop_file, form, num};

fn read(data: Vec<u8>) -> (Identification, ParsedStatement) {
    let doc = SourceDocument::new("releve.pdf", data);
    let found = identify(&doc);
    let st = found.chosen.as_ref().expect("an adapter").adapter.parse(&doc).unwrap().remove(0);
    (found, st)
}

fn september() -> HSpec {
    HSpec::new(hello_entries())
}

#[test]
fn the_statement_is_recognised_and_its_identity_read() {
    let (found, st) = read(hello_statement_pdf(&september()));
    assert_eq!(found.decision, "auto");
    assert_eq!(found.chosen.unwrap().adapter.id(), "hellobank.statement_pdf");
    assert_eq!(st.institution.as_deref(), Some("Hello bank!"));
    assert_eq!(st.account_type.as_deref(), Some("checking"));
    assert_eq!((st.bank_code.as_deref(), st.branch_code.as_deref(), st.account_number.as_deref()), (Some(H_BANK), Some(H_BRANCH), Some(H_ACCOUNT)));
    assert!(st.iban.as_deref().unwrap().starts_with("FR76"));
    assert_eq!(st.bic.as_deref(), Some("BNPAFRPPXXX"));
    assert_eq!(st.holder_address.as_deref(), Some("8 RUE DES TILLEULS\n69003 LYON"));
    assert_eq!(st.account_label.as_deref(), Some("Compte chèques Hello bank …5678"));
    assert_eq!(st.currency, "EUR");
}

#[test]
fn the_first_name_printed_first_is_put_after_the_last_name() {
    let (_, st) = read(hello_statement_pdf(&september()));
    assert_eq!(st.holder_name.as_deref(), Some(H_HOLDER));
    assert_eq!(st.extra_str("holder_printed").as_deref(), Some(H_PRINTED_HOLDER));
    // a joint line is left as printed (it is asked once)
    let joint = HSpec { holder: "M PAUL MARTIN OU MME LEA DURAND".into(), ..september() };
    let (_, st) = read(hello_statement_pdf(&joint));
    assert_eq!(st.holder_name.as_deref(), Some("M PAUL MARTIN OU MME LEA DURAND"));
}

#[test]
fn the_balances_the_period_and_the_operations_are_checked_against_each_other() {
    let spec = september();
    let (_, st) = read(hello_statement_pdf(&spec));
    assert_eq!((st.opening_balance, st.balance), (Some(d("500.00")), Some(spec.closing())));
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 9, 23)), Some(ymd(2026, 10, 15))));
    assert_eq!((st.opening_date, st.balance_date), (Some(ymd(2026, 9, 22)), Some(ymd(2026, 10, 15))));
    assert_eq!(st.transactions.len(), 8);
    assert_eq!(st.confidence, "high");
    assert!(st.warnings.is_empty(), "{:?}", st.warnings);
    assert_eq!(st.extra.get("overdraft_limit").and_then(|v| v.as_str()), Some("0.00"));
    // the sum of what was read is the movement of the account
    let movement: Decimal = st.transactions.iter().map(|t| t.amount).sum();
    assert_eq!(st.balance.unwrap() - st.opening_balance.unwrap(), movement);
}

fn d(s: &str) -> Decimal {
    dec(s)
}

#[test]
fn every_kind_of_line_is_understood() {
    let (_, st) = read(hello_statement_pdf(&september()));
    let by = |needle: &str| st.transactions.iter().find(|t| t.raw.as_deref().unwrap_or("").contains(needle)).unwrap_or_else(|| panic!("no {needle}"));

    let pay = by("SALAIRE");
    assert_eq!((pay.kind.as_deref(), pay.amount, pay.counterparty.as_deref(), pay.reference.as_deref()), (Some("transfer"), d("1800.00"), Some("SOCIETE EXEMPLE SAS"), Some("20260925A")));
    assert_eq!(pay.memo.as_deref(), Some("SALAIRE SEPTEMBRE"));

    let card = by("BOULANGERIE");
    assert_eq!((card.kind.as_deref(), card.amount, card.counterparty.as_deref()), (Some("card"), d("-3.50"), Some("BOULANGERIE TEST LYON")));
    assert_eq!(card.details.as_ref().unwrap()["purchase_date"], "2026-09-27");

    let debit = by("OPERATEUR MOBILE");
    assert_eq!((debit.kind.as_deref(), debit.amount, debit.counterparty.as_deref()), (Some("direct_debit"), d("-19.99"), Some("OPERATEUR MOBILE EXEMPLE")));
    assert_eq!(debit.reference.as_deref(), Some("000111"));
    assert_eq!(debit.details.as_ref().unwrap()["ics"], "FR12ZZZ123456");

    let sent = by("REMBOURSEMENT RESTAURANT");
    assert_eq!((sent.kind.as_deref(), sent.amount, sent.counterparty.as_deref()), (Some("transfer"), d("-25.00"), Some("LEA DURAND")));
    assert!(sent.details.is_none(), "a transfer to someone else is not internal");

    let own = by("PAUL MARTIN");
    assert_eq!((own.kind.as_deref(), own.amount), (Some("transfer"), d("200.00")));
    assert_eq!(own.details.as_ref().unwrap()["internal"], true, "from the holder's own account");

    let cheque = by("CHEQUE");
    assert_eq!((cheque.kind.as_deref(), cheque.amount, cheque.reference.as_deref()), (Some("cheque"), d("-60.00"), Some("0001234")));
    assert_eq!(by("COTISATION").kind.as_deref(), Some("fee"));
    assert_eq!((by("RETRAIT").kind.as_deref(), by("RETRAIT").amount), (Some("cash"), d("-40.00")));
}

#[test]
fn a_total_that_does_not_add_up_is_reported() {
    let (_, st) = read(hello_statement_pdf(&HSpec { wrong_totals: true, ..september() }));
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("totaux imprimés")), "{:?}", st.warnings);
}

#[test]
fn an_overdrawn_account_has_negative_balances() {
    let spec = HSpec { opening: d("-120.00"), ..HSpec::new(vec![hentry(ymd(2026, 9, 30), &["VIR SEPA RECU /FRM SOCIETE EXEMPLE SAS"], "50.00")]) };
    let (_, st) = read(hello_statement_pdf(&spec));
    assert_eq!((st.opening_balance, st.balance), (Some(d("-120.00")), Some(d("-70.00"))));
    assert_eq!(st.confidence, "high", "{:?}", st.warnings);
}

#[test]
fn a_statement_across_new_year_puts_each_operation_in_the_right_year() {
    let spec = HSpec {
        opening_on: ymd(2026, 12, 22),
        period_end: ymd(2027, 1, 15),
        ..HSpec::new(vec![hentry(ymd(2026, 12, 28), &["CARTE X1234 27/12 SHOP"], "-10.00"), hentry(ymd(2027, 1, 3), &["CARTE X1234 02/01 SHOP"], "-5.00")])
    };
    let (_, st) = read(hello_statement_pdf(&spec));
    let days: Vec<_> = st.transactions.iter().map(|t| t.date).collect();
    assert_eq!(days, vec![ymd(2026, 12, 28), ymd(2027, 1, 3)]);
    assert_eq!(st.period_start, Some(ymd(2026, 12, 23)));
    assert_eq!(st.confidence, "high", "{:?}", st.warnings);
}

#[test]
fn an_empty_month_is_read_too() {
    let (_, st) = read(hello_statement_pdf(&HSpec::new(vec![])));
    assert_eq!((st.transactions.len(), st.opening_balance, st.balance), (0, Some(d("500.00")), Some(d("500.00"))));
    assert_eq!(st.confidence, "high");
}

#[test]
fn importing_creates_the_account_with_its_holder_and_a_standard_file_name() {
    let core = core();
    let doc = drop_file(&core, "releve_XYZ.pdf", &hello_statement_pdf(&september()));
    let closing = september().closing().to_string();
    let result = core.import_statement(doc, 0, &form(&[("account_id", "new"), ("name", "Compte Hello"), ("import_transactions", "on"), ("update_balance", "on"), ("balance", &closing)])).unwrap();
    let account = num(&result, "account_id");
    assert_eq!(num(&result, "added"), 8);
    db(&core, |c| {
        let a = Account::get(c, account)?.unwrap();
        assert_eq!((a.institution.as_deref(), a.holder_name.as_deref()), (Some("Hello bank!"), Some(H_HOLDER)));
        assert_eq!(a.balance, september().closing());
        assert_eq!(Transaction::query(c, "WHERE account_id = ?", [account])?.len(), 8);
        let name = Document::get(c, doc)?.unwrap().filename;
        assert!(name.starts_with("HELLO-BANK_") && name.contains("_MARTIN_PAUL_2026-10-15"), "{name}");
        Ok(())
    });
}

#[test]
fn the_next_month_goes_to_the_same_account() {
    let core = core();
    let first = september();
    let doc = drop_file(&core, "a.pdf", &hello_statement_pdf(&first));
    let account = num(&core.import_statement(doc, 0, &form(&[("account_id", "new"), ("name", "Compte Hello"), ("import_transactions", "on")])).unwrap(), "account_id");
    let next = HSpec { opening: first.closing(), opening_on: first.period_end, period_end: ymd(2026, 11, 14), title: "novembre".into(), ..HSpec::new(vec![hentry(ymd(2026, 10, 20), &["CARTE X1234 19/10 SHOP"], "-9.00")]) };
    let doc = drop_file(&core, "b.pdf", &hello_statement_pdf(&next));
    let result = core.auto_import(doc).unwrap().expect("imported without asking");
    assert_eq!(num(&result, "account_id"), account);
    assert_eq!(num(&result, "added"), 1);
}

#[test]
fn the_other_banks_are_not_taken_for_hello_bank() {
    let (found, _) = {
        let doc = SourceDocument::new("f.pdf", crate::testkit::fortuneo::fortuneo_statement_pdf(&crate::testkit::fortuneo::fortuneo_september()));
        let found = identify(&doc);
        (found, ())
    };
    assert_eq!(found.chosen.unwrap().adapter.id(), "fortuneo.statement_pdf");
    let doc = SourceDocument::new("h.pdf", hello_statement_pdf(&september()));
    assert!(identify(&doc).candidates.iter().find(|c| c.adapter.id() == "fortuneo.statement_pdf").map(|c| c.score).unwrap_or(0.0) < 0.5);
    let _ = client; // the web client is not needed here
}
