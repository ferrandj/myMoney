//! python/tests/test_adapters_generic.py

use crate::adapters::base::SourceDocument;
use crate::adapters::common::{find_iban, parse_amount, parse_date};
use crate::testkit::fixtures::*;
use crate::testkit::{parse_document, rows_pdf};

fn detect_format(filename: &str, data: &[u8]) -> String {
    SourceDocument::new(filename, data.to_vec()).format().to_string()
}

#[test]
fn parse_amount_cases() {
    for (raw, expected) in [
        ("1 234,56", Some("1234.56")), ("-1,234.56", Some("-1234.56")), ("1.234,56 €", Some("1234.56")), ("12,00-", Some("-12.00")),
        ("(45.10)", Some("-45.10")), ("+3,5", Some("3.5")), ("1 000,00", Some("1000.00")), ("0.00012345", Some("0.00012345")), ("", None),
    ] {
        assert_eq!(parse_amount(Some(raw)), expected.map(dec), "{raw}");
    }
}

#[test]
fn parse_date_cases() {
    assert_eq!(parse_date(Some("03/02/2026"), None), Some(ymd(2026, 2, 3)));
    assert_eq!(parse_date(Some("2026-02-03"), None), Some(ymd(2026, 2, 3)));
    assert_eq!(parse_date(Some("03.02.26"), None), Some(ymd(2026, 2, 3)));
    assert_eq!(parse_date(Some("3 févr. 2026"), None), Some(ymd(2026, 2, 3)));
    assert_eq!(parse_date(Some("03/02"), Some(2025)), Some(ymd(2025, 2, 3)));
}

#[test]
fn find_iban_cases() {
    assert_eq!(find_iban("IBAN : FR76 3000 1007 9412 3456 7890 185 BIC XXX").as_deref(), Some("FR7630001007941234567890185"));
    assert_eq!(find_iban("FR76 3000 1007 9412 3456 7890 186"), None);
}

#[test]
fn csv_french_bank() {
    let text = "Compte courant;FR76 3000 1007 9412 3456 7890 185\n\nDate opération;Date valeur;Libellé;Débit;Crédit;Solde\n05/02/2026;05/02/2026;CARTE CARREFOUR;45,20;;1 954,80\n03/02/2026;03/02/2026;VIR SALAIRE;;2 000,00;2 000,00\n";
    let data: Vec<u8> = text.chars().map(|c| crate::testkit::cp1252_byte(c)).collect();
    let st = parse_document("export.csv", data).remove(0);
    assert_eq!(st.iban.as_deref(), Some("FR7630001007941234567890185"));
    assert_eq!(st.transactions.iter().map(|t| t.amount).collect::<Vec<_>>(), vec![dec("2000.00"), dec("-45.20")]);
    assert_eq!(st.balance, Some(dec("1954.80")));
    assert_eq!(st.balance_date, Some(ymd(2026, 2, 5)));
}

#[test]
fn csv_revolut() {
    let data = "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\nCARD_PAYMENT,Current,2026-02-01 10:00:00,2026-02-02 10:00:00,Amazon,-20.00,0.50,EUR,COMPLETED,79.50\nTOPUP,Current,2026-02-03 10:00:00,,Top-up,100.00,0.00,EUR,PENDING,\n";
    let st = parse_document("revolut.csv", data.as_bytes().to_vec()).remove(0);
    assert_eq!(st.institution.as_deref(), Some("Revolut"));
    assert_eq!(st.transactions.len(), 1);
    assert_eq!(st.transactions[0].amount, dec("-20.50"));
}

#[test]
fn csv_holdings() {
    let st = parse_document("nexo.csv", b"Symbol;Quantity\nBTC;0,5\nETH;2\n".to_vec()).remove(0);
    let holdings: std::collections::BTreeMap<String, rust_decimal::Decimal> = st.holdings.as_ref().unwrap().iter().map(|h| (h.symbol.clone().unwrap(), h.quantity)).collect();
    assert_eq!(holdings, [("BTC".to_string(), dec("0.5")), ("ETH".to_string(), dec("2"))].into_iter().collect());
    assert_eq!(st.account_type.as_deref(), Some("crypto_exchange"));
}

const OFX: &str = "OFXHEADER:100
DATA:OFXSGML
VERSION:102
SECURITY:NONE
ENCODING:USASCII
CHARSET:1252
COMPRESSION:NONE
OLDFILEUID:NONE
NEWFILEUID:NONE

<OFX><SIGNONMSGSRSV1><SONRS><STATUS><CODE>0<SEVERITY>INFO</STATUS><DTSERVER>20260210<LANGUAGE>FRA</SONRS></SIGNONMSGSRSV1>
<BANKMSGSRSV1><STMTTRNRS><TRNUID>1<STATUS><CODE>0<SEVERITY>INFO</STATUS>
<STMTRS><CURDEF>EUR<BANKACCTFROM><BANKID>14505<BRANCHID>00001<ACCTID>12345678901<ACCTTYPE>CHECKING</BANKACCTFROM>
<BANKTRANLIST><DTSTART>20260101<DTEND>20260210>
<STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20260105<TRNAMT>-12.50<FITID>A1<NAME>BOULANGERIE</STMTTRN>
<STMTTRN><TRNTYPE>CREDIT<DTPOSTED>20260107<TRNAMT>1500.00<FITID>A2<NAME>SALAIRE</STMTTRN>
</BANKTRANLIST><LEDGERBAL><BALAMT>3210.55<DTASOF>20260210</LEDGERBAL></STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>
";

#[test]
fn ofx() {
    assert_eq!(detect_format("x.ofx", OFX.as_bytes()), "ofx");
    let st = parse_document("fortuneo.ofx", OFX.as_bytes().to_vec()).remove(0);
    assert_eq!(st.balance, Some(dec("3210.55")));
    assert_eq!(st.account_number.as_deref(), Some("12345678901"));
    assert_eq!(st.transactions.iter().map(|t| t.amount).collect::<Vec<_>>(), vec![dec("-12.50"), dec("1500.00")]);
    assert_eq!(st.transactions[0].external_id.as_deref(), Some("A1"));
}

#[test]
fn pdf_statement_with_debit_credit_columns() {
    let rows = vec![
        vec![(10.0, "BFORBANK - Releve de compte courant")],
        vec![(10.0, "Compte courant joint")],
        vec![(10.0, "IBAN : FR76 3000 1007 9412 3456 7890 185")],
        vec![(10.0, "Releve du 01/02/2026 au 28/02/2026")],
        vec![(10.0, "Date"), (30.0, "Libelle"), (140.0, "Debit"), (175.0, "Credit")],
        vec![(10.0, "ANCIEN SOLDE AU 31/01/2026"), (173.0, "1 000,00")],
        vec![(10.0, "03/02"), (30.0, "VIR SEPA SALAIRE ACME"), (173.0, "2 500,00")],
        vec![(10.0, "05/02"), (30.0, "CARTE 04/02 CARREFOUR"), (140.0, "84,30")],
        vec![(30.0, "PARIS 15")],
        vec![(10.0, "10/02"), (30.0, "PRLV SEPA EDF"), (140.0, "1 200,00")],
        vec![(10.0, "NOUVEAU SOLDE AU 28/02/2026"), (173.0, "2 215,70")],
    ];
    let st = parse_document("releve.pdf", rows_pdf(&rows)).remove(0);
    assert_eq!(st.institution.as_deref(), Some("BforBank"));
    assert_eq!(st.iban.as_deref(), Some("FR7630001007941234567890185"));
    assert_eq!(st.opening_balance, Some(dec("1000.00")));
    assert_eq!(st.balance, Some(dec("2215.70")));
    assert_eq!(
        st.transactions.iter().map(|t| (t.date, t.amount)).collect::<Vec<_>>(),
        vec![(ymd(2026, 2, 3), dec("2500.00")), (ymd(2026, 2, 5), dec("-84.30")), (ymd(2026, 2, 10), dec("-1200.00"))]
    );
    assert!(st.transactions[1].label.contains("PARIS 15"));
    assert_eq!(st.confidence, "high");
}

#[test]
fn pdf_other_document_offers_candidate_amounts() {
    let rows = vec![
        vec![(10.0, "Spirica - Releve de situation annuel")],
        vec![(10.0, "Contrat Linxea Avenir")],
        vec![(10.0, "Valeur de rachat au 31/12/2025"), (150.0, "25 431,12")],
    ];
    let st = parse_document("situation.pdf", rows_pdf(&rows)).remove(0);
    assert!(st.transactions.is_empty());
    assert_eq!(st.candidate_balances[0].1, dec("25431.12"));
}
