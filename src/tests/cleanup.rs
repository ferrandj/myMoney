//! Invalidating a document undoes everything it brought, and leaves nothing behind: no operation, statement, recap,
//! position, loan, history, card or account that an import made and nothing else rests on.

use rust_decimal::Decimal;

use crate::adapters::base::{HoldingData, ParsedStatement};
use crate::app::MyMoney;
use crate::models::{Account, Counterparty, Document, Holding, PersonCard, Statement, Transaction};
use crate::services::statements as statement_service;
use crate::testkit::env::{core, Core};
use crate::testkit::fixtures::*;
use crate::testkit::fortuneo::*;
use crate::testkit::loan::*;
use crate::testkit::{db, drop_file, form, num};

/// Every table an import can write to.
const TABLES: &[&str] = &[
    "accounts",
    "transactions",
    "statements",
    "fee_summaries",
    "holdings",
    "snapshots",
    "loans",
    "loan_installments",
    "loan_events",
    "people",
    "account_shares",
    "gap_acks",
    "counterparties",
    "counterparty_identifiers",
];

pub(super) fn counts(core: &MyMoney) -> Vec<(&'static str, i64)> {
    db(core, |c| {
        let mut out = vec![];
        for t in TABLES {
            out.push((*t, c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get::<_, i64>(0))?));
        }
        out.push(("settings:life_estimate", c.query_row("SELECT COUNT(*) FROM settings WHERE \"key\" LIKE 'life_estimate_%'", [], |r| r.get::<_, i64>(0))?));
        Ok(out)
    })
}

pub(super) fn assert_nothing_left(core: &MyMoney) {
    let left: Vec<_> = counts(core).into_iter().filter(|(_, n)| *n != 0).collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

pub(super) fn import_new(core: &Core, name: &str, data: &[u8], extra: &[(&str, &str)]) -> (i64, i64) {
    let doc = drop_file(core, name, data);
    let mut pairs = vec![("account_id", "new"), ("import_transactions", "on")];
    pairs.extend_from_slice(extra);
    let result = core.import_statement(doc, 0, &form(&pairs)).unwrap();
    (doc, num(&result, "account_id"))
}

pub(super) fn feed(core: &Core, name: &str, data: &[u8], account_id: Option<i64>) -> (i64, i64) {
    let doc = drop_file(core, name, data);
    let result = match core.auto_import(doc).unwrap() {
        Some(r) => r,
        None => core.import_statement(doc, 0, &form(&[("account_id", &account_id.map(|a| a.to_string()).unwrap_or_else(|| "new".into())), ("import_transactions", "on")])).unwrap(),
    };
    (doc, result.get("account_id").and_then(|a| a.as_i64()).unwrap_or(0))
}

pub(super) fn imported_ids(core: &MyMoney) -> Vec<i64> {
    db(core, |c| Ok(Document::query(c, "WHERE status = 'imported'", [])?.into_iter().map(|d| d.id).collect()))
}

#[test]
fn test_invalidating_every_document_leaves_nothing_behind() {
    let core = core();
    let holders = [("holder_0_civility", "M"), ("holder_0_last_0", "MARTIN"), ("holder_0_first_0", "PAUL")];

    // a bank statement whose holder gets a card, a second bank's statement with its fees recap and a letter
    let spec = Spec::new(sample_entries());
    let mut extra = vec![("name", "Compte courant"), ("update_balance", "on")];
    let closing = spec.closing().to_string();
    extra.push(("balance", &closing));
    extra.extend_from_slice(&holders);
    import_new(&core, "septembre.pdf", &boursobank_statement_pdf(&spec), &extra);
    let (_, fortuneo_account) = import_new(&core, "fortuneo.pdf", &fortuneo_statement_pdf(&FSpec::new(fortuneo_entries())), &[("name", "Compte Fortuneo")]);
    feed(&core, "frais.pdf", &fortuneo_fees_pdf(&FeesSpec::new()), Some(fortuneo_account));
    feed(&core, "fgdr.pdf", &fortuneo_info(), None);

    // a loan from its documents
    let loan = LoanSpec::new();
    let (_, loan_account) = feed(&core, "offre.pdf", &offer_pdf(&loan), None);
    feed(&core, "tableau.pdf", &schedule_pdf(&loan, None), Some(loan_account));
    feed(&core, "remboursement.pdf", &prepayment_pdf(&loan, None), Some(loan_account));

    // a life-insurance contract: yearly statement, letters, operations export
    let (_, life) = import_new(&core, "annuel.pdf", &fortuneo_life_annual_pdf(&LifeAnnualSpec::new(2025)), &[("name", "Assurance-vie"), ("update_balance", "on"), ("balance", "600.00")]);
    feed(&core, "arbitrage.pdf", &fortuneo_arbitrage_v2_plain_pdf("100,00", "40,00"), None);
    feed(&core, "operations.csv", &fortuneo_life_operations_csv(), None);
    feed(&core, "profil.pdf", &fortuneo_investor_profile_pdf("13 janvier 2026", "DYNAMIQUE"), None);

    // positions brought by a document (no adapter reads them yet: given straight to the import)
    let doc = drop_file(&core, "titres.pdf", &fortuneo_info_pdf(2024, F_HOLDER, None, "Fortuneo", "", "100 000 €"));
    db(&core, |c| {
        let mut st = ParsedStatement::new("pdf");
        st.institution = Some("Courtier".into());
        st.account_type = Some("pea".into());
        st.document_kind = Some("statement".into());
        st.period_start = Some(ymd(2026, 9, 1));
        st.period_end = Some(ymd(2026, 9, 30));
        st.balance = Some(Decimal::ZERO);
        st.holdings = Some(vec![HoldingData { symbol: None, quantity: dec("3"), name: Some("ACTION EXEMPLE".into()), isin: Some("FR0000000009".into()), asset_class: "stock".into(), unit_price: None, value: Some(dec("300")), value_currency: Some("EUR".into()), cost_basis: None, extra: None }]);
        let mut d = Document::get(c, doc)?.unwrap();
        statement_service::import_statement(c, &mut d, &st, &form(&[("account_id", "new"), ("name", "PEA"), ("import_holdings", "on")]))?;
        Ok(())
    });

    // the app has seen the accounts for a few days: histories were written
    core.portfolio(false).unwrap();
    let before = counts(&core);
    for (table, n) in &before {
        if ["counterparties", "counterparty_identifiers", "gap_acks", "account_shares"].contains(table) {
            continue; // not something an import makes
        }
        assert!(*n > 0, "the scenario should have filled {table}: {before:?}");
    }
    assert_eq!(Account::get_all_for_test(&core), 5); // bank, second bank, loan, life insurance, securities
    let _ = life;

    let ids = imported_ids(&core);
    assert!(ids.len() >= 12, "{ids:?}");
    let (done, accounts_gone) = core.invalidate_documents_report(&ids).unwrap();
    assert_eq!(done, ids.len());
    assert_eq!(accounts_gone, 5);
    assert_nothing_left(&core);
    db(&core, |c| {
        for d in Document::query(c, "", [])? {
            assert_ne!(d.status, "imported", "{}", d.filename);
            assert_eq!(d.account_id, None, "{}", d.filename);
        }
        Ok(())
    });
    // and the files can be imported again like new
    let again = drop_file(&core, "septembre-bis.pdf", &boursobank_statement_pdf(&Spec { title: "bis".into(), ..Spec::new(sample_entries()) }));
    core.import_statement(again, 0, &form(&[("account_id", "new"), ("name", "Compte"), ("import_transactions", "on")])).unwrap();
    assert!(db(&core, |c| Ok(Transaction::query(c, "", [])?.len())) > 0);
}

trait CountAccounts {
    fn get_all_for_test(core: &MyMoney) -> usize;
}
impl CountAccounts for Account {
    fn get_all_for_test(core: &MyMoney) -> usize {
        db(core, |c| Ok(Account::query(c, "", [])?.len()))
    }
}

#[test]
fn test_invalidating_the_newest_statement_gives_the_previous_balance_back_and_the_last_one_removes_the_account() {
    let core = core();
    let september = Spec::new(sample_entries());
    let october = Spec { opening: september.closing(), opening_date: ymd(2026, 9, 30), period: (ymd(2026, 10, 1), ymd(2026, 10, 31)), ..Spec::new(vec![entry(ymd(2026, 10, 3), &["CARTE 01/10/26 BOULANGERIE CB*1234"], "-4.20")]) };
    let (sept_doc, account) = import_new(&core, "septembre.pdf", &boursobank_statement_pdf(&september), &[("name", "Compte courant"), ("update_balance", "on"), ("balance", &september.closing().to_string())]);
    let oct_doc = drop_file(&core, "octobre.pdf", &boursobank_statement_pdf(&october));
    core.import_statement(oct_doc, 0, &form(&[("account_id", &account.to_string()), ("import_transactions", "on"), ("update_balance", "on"), ("balance", &october.closing().to_string())])).unwrap();
    assert_eq!(db(&core, |c| Ok(Account::get(c, account)?.unwrap().balance)), october.closing());

    core.invalidate_documents(&[oct_doc]).unwrap();
    db(&core, |c| {
        let a = Account::get(c, account)?.unwrap();
        assert_eq!(a.balance, september.closing(), "the balance is the one of the statement that is left");
        assert_eq!(Transaction::query(c, "WHERE account_id = ?", [account])?.len(), sample_entries().len());
        assert_eq!(Statement::query(c, "WHERE account_id = ?", [account])?.len(), 1);
        Ok(())
    });
    core.invalidate_documents(&[sept_doc]).unwrap();
    assert_nothing_left(&core);
}

#[test]
fn test_what_the_user_made_by_hand_is_never_removed() {
    let core = core();
    // typed by hand before any import: an account with a balance, a card, a contact
    let manual = core.save_account(None, &form(&[("name", "Livret à la main"), ("account_type", "livret_a"), ("balance", "500")])).unwrap();
    let card = db(&core, |c| {
        let mut card = PersonCard { last_names: Some(serde_json::json!(["MARTIN"])), first_names: Some(serde_json::json!(["PAUL"])), ..Default::default() };
        card.insert(c)?;
        Counterparty { name: "Pauline Martin".into(), ..Default::default() }.insert(c)?;
        Ok(card.id)
    });
    // an import into the manual account, then another into a new account that reuses the card
    let spec = Spec::new(sample_entries());
    let doc = drop_file(&core, "septembre.pdf", &boursobank_statement_pdf(&spec));
    core.import_statement(doc, 0, &form(&[("account_id", &manual.to_string()), ("import_transactions", "on")])).unwrap();
    let (other_doc, other) = import_new(&core, "fortuneo.pdf", &fortuneo_statement_pdf(&FSpec::new(fortuneo_entries())), &[("name", "Compte Fortuneo"), ("holder_0_civility", "M"), ("holder_0_last_0", "MARTIN"), ("holder_0_first_0", "PAUL")]);
    // a position typed by hand in the imported account keeps it alive
    core.add_holding(other, &form(&[("symbol", "BTC"), ("quantity", "1")]).into_iter().collect()).unwrap();

    core.invalidate_documents(&[doc, other_doc]).unwrap();
    db(&core, |c| {
        assert!(Account::get(c, manual)?.is_some(), "an account typed by hand stays");
        assert!(Account::get(c, other)?.is_some(), "a typed position keeps the account");
        assert_eq!(Holding::query(c, "WHERE account_id = ?", [other])?.len(), 1);
        assert!(PersonCard::get(c, card)?.is_some(), "a card that was there before stays");
        assert_eq!(Counterparty::query(c, "", [])?.len(), 1);
        assert_eq!(Transaction::query(c, "", [])?.len(), 0);
        Ok(())
    });
}

#[test]
fn test_accounts_left_empty_by_older_versions_are_listed_and_can_be_deleted_but_never_a_manual_one() {
    let mut client = crate::testkit::env::client();
    let csrf = client.login();
    let manual = client.core.save_account(None, &form(&[("name", "Livret à la main"), ("account_type", "livret_a"), ("balance", "500")])).unwrap();
    // what an older version left: an account an import filled in (the bank's printed holder name), with nothing on it
    let left = client.core.save_account(None, &form(&[("name", "Reste d'un import"), ("account_type", "checking"), ("balance", "10")])).unwrap();
    db(&client.core, |c| {
        let mut a = Account::get(c, left)?.unwrap();
        let before = a.clone();
        a.holder_alias = Some("M MARTIN PAUL".into());
        a.manual_at = None; // an older version left no mark at all
        a.save(c, &before)?;
        Ok(())
    });
    assert_eq!(client.core.empty_accounts().unwrap().len(), 1);
    let page = client.get("/accounts").text();
    assert!(page.contains("Comptes sans document ni opération") && (page.contains("Reste d&#39;un import") || page.contains("Reste d'un import")), "{page}");
    assert!(!page.split("Comptes sans document ni opération").nth(1).unwrap_or("").split("</section>").next().unwrap_or("").contains("Livret à la main"));
    let done = client.post("/accounts/delete-empty", &[("csrf", &csrf)]);
    assert!(done.text().contains("1 compte(s) sans document ni opération supprimé(s)"), "{}", done.text());
    db(&client.core, |c| {
        assert!(Account::get(c, left)?.is_none());
        assert!(Account::get(c, manual)?.is_some());
        Ok(())
    });
}

#[test]
fn test_deleting_an_account_sends_its_documents_back_to_waiting() {
    let core = core();
    let (doc, account) = import_new(&core, "septembre.pdf", &boursobank_statement_pdf(&Spec::new(sample_entries())), &[("name", "Compte courant")]);
    core.delete_account(account).unwrap();
    db(&core, |c| {
        let d = Document::get(c, doc)?.unwrap();
        assert_eq!((d.status.as_str(), d.account_id), ("parsed", None), "not left « imported » with nothing behind it");
        Ok(())
    });
    assert_nothing_left(&core);
}
