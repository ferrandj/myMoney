//! python/tests/test_boursobank.py

use rust_decimal::Decimal;
use serde_json::json;

use crate::adapters::base::{ParsedStatement, SourceDocument, TransactionData};
use crate::adapters::common::{iban_valid, name_key, rib_valid};
use crate::adapters::pdftools::fix_text;
use crate::adapters::registry::identify;
use crate::testkit::fixtures::*;
use crate::testkit::{parse_one, rows_pdf};

const ADAPTER: &str = "boursobank.statement_pdf";
const SAVINGS: &str = "boursobank.savings_pdf";

fn parse(spec: &Spec) -> ParsedStatement {
    parse_one(ADAPTER, "releve.pdf", boursobank_statement_pdf(spec))
}

fn by_label<'a>(st: &'a ParsedStatement, text: &str) -> &'a TransactionData {
    st.transactions.iter().find(|t| t.label.contains(text) || t.raw.as_deref().unwrap_or("").contains(text)).unwrap()
}

fn details(t: &TransactionData) -> serde_json::Value {
    t.details.clone().map(serde_json::Value::Object).unwrap_or(serde_json::Value::Null)
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn test_detected_and_chosen_without_asking() {
    let found = identify(&SourceDocument::new("releve.pdf", boursobank_statement_pdf(&september())));
    assert_eq!(found.decision, "auto");
    let chosen = found.chosen.unwrap();
    assert_eq!(chosen.adapter.id(), ADAPTER);
    assert!(chosen.score >= 0.9);
    assert_eq!(found.institution.as_deref(), Some("BoursoBank"));
}

#[test]
fn test_identity_of_the_account_and_its_holder() {
    let st = parse(&september());
    assert_eq!(st.holder_name, s("M DUPONT JEAN"));
    assert_eq!(st.holder_address, Some(address().join("\n")));
    assert_eq!((st.bank_code.clone(), st.branch_code.clone(), st.account_number.clone()), (s(BANK), s(BRANCH), s(ACCOUNT)));
    assert!(rib_valid(st.bank_code.as_deref().unwrap(), st.branch_code.as_deref().unwrap(), st.account_number.as_deref().unwrap(), st.rib_key.as_deref().unwrap()));
    assert_eq!(st.iban, Some(iban()));
    assert!(iban_valid(st.iban.as_deref().unwrap()));
    assert_eq!(st.bic, s("BOUSFRPPXXX"));
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 9, 1)), Some(ymd(2026, 9, 30))));
    assert_eq!(st.currency, "EUR");
    assert_eq!(st.account_type, s("checking"));
}

#[test]
fn test_balances_reconcile_with_the_operations() {
    let spec = september();
    let st = parse(&spec);
    assert_eq!(st.opening_balance, Some(dec("1000.00")));
    assert_eq!(st.opening_date, Some(ymd(2026, 8, 31)));
    assert_eq!(st.balance, Some(spec.closing()));
    assert_eq!(st.balance_date, Some(ymd(2026, 9, 30)));
    assert_eq!(st.transactions.len(), spec.entries.len());
    assert_eq!(st.transactions.iter().map(|t| t.amount).sum::<Decimal>(), spec.closing() - spec.opening);
    assert_eq!(st.confidence, "high");
    assert!(st.warnings.is_empty());
}

#[test]
fn test_each_kind_of_line_is_understood() {
    let st = parse(&september());

    let card = by_label(&st, "TRAINS DE FRANCE");
    assert_eq!((card.kind.clone(), card.counterparty.clone(), card.amount), (s("card"), s("TRAINS DE FRANCE"), dec("-71.00")));
    assert_eq!(details(card), json!({"purchase_date": "2026-08-31", "card_last4": "1234"}));

    let debit = by_label(&st, "EAUX DU NORD");
    assert_eq!(debit.kind, s("direct_debit"));
    assert_eq!(debit.counterparty, s("EAUX DU NORD"));
    assert_eq!(details(debit)["rum"], json!("S-1-0000000001-000000000-01"));
    assert!(debit.label.contains("Facture EAUX"));

    let tax = by_label(&st, "DIRECTION GENERALE");
    assert!(tax.label.contains("IMPOT TF")); // the descriptive line, not the identifier line

    let transfer = by_label(&st, "MARTIN Pauline");
    assert_eq!(transfer.kind, s("transfer"));
    assert_eq!(transfer.counterparty, s("MARTIN Pauline"));
    assert_eq!(transfer.reference, s("SCT123450000000000000000001"));
    assert_eq!(transfer.external_id, s("SCT123450000000000000000001"));
    assert_eq!(transfer.memo, s("cadeau anniversaire"));

    let salary = by_label(&st, "ACME CONSULTING");
    assert_eq!(salary.amount, dec("2400.00"));
    assert_eq!(salary.counterparty, s("ACME CONSULTING SAS"));
    assert_eq!(salary.reference, s("03620N0200000000000000"));
    assert_eq!(salary.external_id, None);

    let instant = st.transactions.iter().find(|t| t.kind.as_deref() == Some("instant_transfer")).unwrap();
    assert_eq!(instant.counterparty, s("DUPONT JEAN"));
    assert_eq!(details(instant), json!({"internal": true}));

    let loan = st.transactions.iter().find(|t| t.kind.as_deref() == Some("loan_payment")).unwrap();
    assert_eq!(details(loan)["loan_number"], json!("80000000000000000001"));
    assert_eq!(loan.amount, dec("-166.67"));

    let fee = st.transactions.iter().find(|t| t.kind.as_deref() == Some("fee")).unwrap();
    assert_eq!(fee.amount, dec("-2.00"));
    assert!(fee.label.contains("COTIS"));
}

#[test]
fn test_transfers_between_own_accounts_are_marked_internal() {
    let st = parse(&september());
    let from_savings = by_label(&st, "LIVRET DEVELOPPE");
    assert_eq!(details(from_savings), json!({"internal": true}));
    assert_eq!(from_savings.counterparty, s("DUPONT JEAN"));
    let to_partner = by_label(&st, "LEROY CLAIRE");
    assert_ne!(details(to_partner).get("internal"), Some(&json!(true)));
}

#[test]
fn test_credit_and_debit_columns_and_value_dates() {
    let st = parse(&september());
    let incoming = st.transactions.iter().find(|t| t.counterparty.as_deref() == Some("LEROY CLAIRE") && t.amount > Decimal::ZERO).unwrap();
    assert_eq!(incoming.amount, dec("60.00"));
    assert_eq!(incoming.date, ymd(2026, 9, 12));
    assert_eq!(incoming.value_date, Some(ymd(2026, 9, 13)));
    let outgoing = st.transactions.iter().find(|t| t.counterparty.as_deref() == Some("MME LEROY CLAIRE")).unwrap();
    assert_eq!(outgoing.amount, dec("-35.00"));
}

#[test]
fn test_many_pages_still_reconcile() {
    let mut entries: Vec<Entry> = (0..70)
        .map(|i| Entry { day: ymd(2026, 9, 1 + (i % 28) as u32), lines: vec![format!("CARTE 01/09/26 SHOP {i} CB*1234")], amount: dec(&format!("-{}.50", i + 1)), value_day: None })
        .collect();
    entries.sort_by_key(|e| e.day);
    let spec = Spec { opening: dec("5000.00"), ..Spec::new(entries) };
    let data = boursobank_statement_pdf(&spec);
    assert!(SourceDocument::new("releve.pdf", data.clone()).words(3.0).unwrap().len() > 2);
    let st = parse_one(ADAPTER, "releve.pdf", data);
    assert_eq!(st.transactions.len(), 70);
    assert_eq!(st.confidence, "high");
    assert_eq!(st.balance, Some(spec.closing()));
}

#[test]
fn test_accents_mangled_by_the_pdf_font_do_not_break_the_reading() {
    let clean = parse(&september());
    let garbled = parse(&Spec { garble: true, ..september() });
    assert_eq!(garbled.confidence, "high");
    assert!(garbled.warnings.is_empty());
    let key = |st: &ParsedStatement| st.transactions.iter().map(|t| (t.date, t.amount, t.kind.clone())).collect::<Vec<_>>();
    assert_eq!(key(&garbled), key(&clean));
    assert_eq!(garbled.holder_name, clean.holder_name);
    assert_eq!(garbled.iban, clean.iban);
}

#[test]
fn test_fix_text() {
    assert_eq!(fix_text("LibellØ DØbit RŁf (cid:224) 20h 5(cid:176) 10(cid:128)"), "Libellé Débit Rèf à 20h 5° 10€");
}

#[test]
fn test_a_statement_that_does_not_add_up_is_flagged() {
    let st = parse(&Spec { wrong_closing: Some(dec("1.00")), ..september() });
    assert_eq!(st.confidence, "low");
    assert!(st.warnings.iter().any(|w| w.contains("écart")));
}

#[test]
fn test_other_documents_do_not_trigger_this_adapter() {
    let rows = vec![vec![(10.0, "BFORBANK - Releve de compte courant")], vec![(10.0, "Date"), (30.0, "Libelle"), (140.0, "Debit"), (175.0, "Credit")]];
    let found = identify(&SourceDocument::new("releve.pdf", rows_pdf(&rows)));
    assert_eq!(found.decision, "fallback");
    assert!(found.candidates.iter().all(|c| c.adapter.id() != ADAPTER || c.score < 0.4));
}

#[test]
fn test_same_brand_but_another_document_is_not_taken_for_a_statement() {
    let rows = vec![vec![(10.0, "BoursoBank - Avis d'opere")], vec![(10.0, "Detail des frais de tenue de compte")]];
    let found = identify(&SourceDocument::new("avis.pdf", rows_pdf(&rows)));
    assert_ne!(found.decision, "auto");
}

#[test]
fn test_name_key_matches_the_same_person_written_differently() {
    assert_eq!(name_key("M DUPONT Jean"), name_key("Jean DUPONT"));
    assert_eq!(name_key("Jean DUPONT"), name_key("MME dupont  jean"));
    assert_eq!(name_key("ACME CONSULTING SAS"), name_key("Acme Consulting"));
    assert_ne!(name_key("DUPONT JEAN"), name_key("DUPONT PAUL"));
}

// ------------------------------------------------------------------------------- savings extract (PEL...)
fn pel_spec() -> SavingsSpec {
    SavingsSpec::new(vec![
        entry(ymd(2026, 9, 28), &["VIR DUPONT JEAN"], "45.00"),
        entry(ymd(2026, 9, 30), &["INTERETS 2026", "Prime d'Etat"], "12.34"),
    ])
}

fn parse_pel(spec: &SavingsSpec) -> ParsedStatement {
    parse_one(SAVINGS, "pel.pdf", boursobank_savings_pdf(spec))
}

#[test]
fn test_a_savings_extract_picks_its_own_adapter_and_not_the_current_account_one() {
    let found = identify(&SourceDocument::new("pel.pdf", boursobank_savings_pdf(&pel_spec())));
    assert_eq!(found.decision, "auto");
    let chosen = found.chosen.as_ref().unwrap();
    assert_eq!(chosen.adapter.id(), SAVINGS);
    assert!(chosen.score >= 0.9);
    let score = |f: &crate::adapters::registry::Identification, id: &str| f.candidates.iter().find(|c| c.adapter.id() == id).unwrap().score;
    assert!(score(&found, ADAPTER) < 0.6); // same brand and table, but no RIB banner
    let current = identify(&SourceDocument::new("releve.pdf", boursobank_statement_pdf(&september())));
    assert_eq!(current.chosen.as_ref().unwrap().adapter.id(), ADAPTER);
    assert!(score(&current, SAVINGS) <= 0.3);
}

#[test]
fn test_savings_identity_has_an_account_number_but_no_iban() {
    let st = parse_pel(&pel_spec());
    assert_eq!(st.account_number, s("00099999999"));
    assert_eq!(st.currency, "EUR");
    assert_eq!((st.iban.clone(), st.bic.clone(), st.bank_code.clone(), st.rib_key.clone()), (None, None, None, None));
    assert_eq!(st.holder_name, s("DUPONT JEAN"));
    assert_eq!(st.holder_address, Some(address().join("\n")));
    assert_eq!((st.period_start, st.period_end), (Some(ymd(2026, 9, 1)), Some(ymd(2026, 9, 30))));
    assert_eq!(st.account_type, s("pel"));
    assert_eq!(st.extra_str("product"), s("Plan Epargne Logement"));
    assert_eq!(st.account_label, s("Plan Epargne Logement BoursoBank …9999"));
}

#[test]
fn test_savings_balances_reconcile_and_lines_are_understood() {
    let spec = pel_spec();
    let st = parse_pel(&spec);
    assert_eq!(st.opening_balance, Some(dec("3000.00")));
    assert_eq!(st.opening_date, Some(ymd(2026, 9, 1)));
    assert_eq!(st.balance, Some(spec.closing()));
    assert_eq!(spec.closing(), dec("3057.34"));
    assert_eq!(st.balance_date, Some(ymd(2026, 9, 30)));
    assert_eq!(st.confidence, "high");
    assert!(st.warnings.is_empty());
    let [transfer, interest] = &st.transactions[..] else { panic!("two lines expected") };
    assert_eq!((transfer.kind.clone(), transfer.counterparty.clone(), transfer.amount), (s("transfer"), s("DUPONT JEAN"), dec("45.00")));
    assert_eq!(details(transfer), json!({"internal": true})); // a transfer from the holder to the holder
    assert_eq!((interest.kind.clone(), interest.amount), (s("interest"), dec("12.34")));
    assert!(interest.label.contains("Prime d'Etat"));
}

#[test]
fn test_savings_debits_are_read_from_the_debit_column() {
    let mut spec = pel_spec();
    spec.entries.push(entry(ymd(2026, 9, 29), &["VIR RETRAIT PARTIEL"], "-500.00"));
    let st = parse_pel(&spec);
    assert!(st.transactions.iter().any(|t| t.amount == dec("-500.00")));
    assert_eq!(st.balance, Some(spec.closing()));
    assert_eq!(st.confidence, "high");
}

// --------------------------------------------------------------- savings account and joint account (same layout)
#[test]
fn test_a_savings_account_in_the_current_account_layout_is_typed_from_its_product_line() {
    let three = sample_entries()[..3].to_vec();
    let st = parse(&Spec { product: "Livret Developpement Durable Solidaire".into(), ..Spec::new(three.clone()) });
    assert_eq!(st.account_type, s("ldds"));
    assert_eq!(st.extra_str("product"), s("Livret Developpement Durable Solidaire"));
    assert_eq!(st.account_label, s("Livret Developpement Durable Solidaire BoursoBank …5678"));
    assert_eq!(st.holder_name, s("M DUPONT JEAN"));
    assert_eq!(st.holder_address, Some(address().join("\n"))); // the extra title line does not leak into them
    assert_eq!(st.iban, Some(iban()));
    assert_eq!(st.confidence, "high");
    assert_eq!(parse(&Spec::new(three)).account_type, s("checking"));
}

#[test]
fn test_a_joint_account_is_labelled_and_its_cut_holder_line_is_flagged() {
    let three = sample_entries()[..3].to_vec();
    let st = parse(&Spec { holder: joint_holder(), address: joint_address(), ..Spec::new(three.clone()) });
    assert_eq!(st.holder_name, s("M DUPONT OU MME LEROY CLAIRE"));
    assert!(st.extra_truthy("holder_maybe_truncated"));
    assert_eq!(st.holder_address, s("12 RUE DES LILAS\nBAT B APPT 12\n75011 PARIS")); // the apartment line stays in the address
    assert_eq!(st.account_label, s("Compte joint BoursoBank …5678"));
    assert_eq!(st.account_type, s("checking"));
    assert!(!parse(&Spec::new(three)).extra_truthy("holder_maybe_truncated"));
}

#[test]
fn test_a_word_cut_by_the_bank_at_the_end_of_a_line_is_glued_back() {
    let e = entry(ymd(2026, 9, 24), &["VIR Virement depuis BOURSORAMA Mathi", "eu", "DUPONT JEAN"], "50.00");
    let st = parse(&Spec::new(vec![e]));
    let [t] = &st.transactions[..] else { panic!("one line expected") };
    assert_eq!(t.counterparty, s("DUPONT JEAN"));
    assert_eq!(t.memo, s("Virement depuis BOURSORAMA Mathieu"));
}
