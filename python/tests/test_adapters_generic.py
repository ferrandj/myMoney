from datetime import date
from decimal import Decimal

import pytest
from fpdf import FPDF

from mymoney.adapters import SourceDocument, identify, parse_with
from mymoney.adapters.common import find_iban, parse_amount, parse_date


def parse_document(filename: str, data: bytes):
    """What the app does: pick the adapter by itself, then read."""
    doc = SourceDocument(filename, data)
    found = identify(doc)
    assert found.chosen is not None, found.message
    return parse_with(found.chosen.adapter.id, doc)


def detect_format(filename: str, data: bytes) -> str:
    return SourceDocument(filename, data).format


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("1 234,56", "1234.56"), ("-1,234.56", "-1234.56"), ("1.234,56 €", "1234.56"), ("12,00-", "-12.00"),
        ("(45.10)", "-45.10"), ("+3,5", "3.5"), ("1 000,00", "1000.00"), ("0.00012345", "0.00012345"), ("", None),
    ],
)
def test_parse_amount(raw, expected):
    assert parse_amount(raw) == (Decimal(expected) if expected else None)


def test_parse_date():
    assert parse_date("03/02/2026") == date(2026, 2, 3)
    assert parse_date("2026-02-03") == date(2026, 2, 3)
    assert parse_date("03.02.26") == date(2026, 2, 3)
    assert parse_date("3 févr. 2026") == date(2026, 2, 3)
    assert parse_date("03/02", default_year=2025) == date(2025, 2, 3)


def test_find_iban():
    assert find_iban("IBAN : FR76 3000 1007 9412 3456 7890 185 BIC XXX") == "FR7630001007941234567890185"
    assert find_iban("FR76 3000 1007 9412 3456 7890 186") is None


def test_csv_french_bank():
    data = (
        "Compte courant;FR76 3000 1007 9412 3456 7890 185\n\n"
        "Date opération;Date valeur;Libellé;Débit;Crédit;Solde\n"
        "05/02/2026;05/02/2026;CARTE CARREFOUR;45,20;;1 954,80\n"
        "03/02/2026;03/02/2026;VIR SALAIRE;;2 000,00;2 000,00\n"
    ).encode("cp1252")
    [st] = parse_document("export.csv", data)
    assert st.iban == "FR7630001007941234567890185"
    assert [t.amount for t in st.transactions] == [Decimal("2000.00"), Decimal("-45.20")]
    assert st.balance == Decimal("1954.80") and st.balance_date == date(2026, 2, 5)


def test_csv_revolut():
    data = (
        "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n"
        "CARD_PAYMENT,Current,2026-02-01 10:00:00,2026-02-02 10:00:00,Amazon,-20.00,0.50,EUR,COMPLETED,79.50\n"
        "TOPUP,Current,2026-02-03 10:00:00,,Top-up,100.00,0.00,EUR,PENDING,\n"
    ).encode()
    [st] = parse_document("revolut.csv", data)
    assert st.institution == "Revolut"
    assert len(st.transactions) == 1 and st.transactions[0].amount == Decimal("-20.50")


def test_csv_holdings():
    data = b"Symbol;Quantity\nBTC;0,5\nETH;2\n"
    [st] = parse_document("nexo.csv", data)
    assert {h.symbol: h.quantity for h in st.holdings} == {"BTC": Decimal("0.5"), "ETH": Decimal("2")}
    assert st.account_type == "crypto_exchange"


OFX = b"""OFXHEADER:100
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
"""


def test_ofx():
    assert detect_format("x.ofx", OFX) == "ofx"
    [st] = parse_document("fortuneo.ofx", OFX)
    assert st.balance == Decimal("3210.55")
    assert st.account_number == "12345678901"
    assert [t.amount for t in st.transactions] == [Decimal("-12.50"), Decimal("1500.00")]
    assert st.transactions[0].external_id == "A1"


def _pdf(lines: list[tuple[float, str]]) -> bytes:
    """Lines of (x, text) blocks: each tuple list is one row."""
    pdf = FPDF()
    pdf.add_page()
    pdf.set_font("Helvetica", size=9)
    y = 20
    for row in lines:
        for x, text in row:
            pdf.text(x, y, text)
        y += 6
    return bytes(pdf.output())


def test_pdf_statement_with_debit_credit_columns():
    rows = [
        [(10, "BFORBANK - Releve de compte courant")],
        [(10, "Compte courant joint")],
        [(10, "IBAN : FR76 3000 1007 9412 3456 7890 185")],
        [(10, "Releve du 01/02/2026 au 28/02/2026")],
        [(10, "Date"), (30, "Libelle"), (140, "Debit"), (175, "Credit")],
        [(10, "ANCIEN SOLDE AU 31/01/2026"), (173, "1 000,00")],
        [(10, "03/02"), (30, "VIR SEPA SALAIRE ACME"), (173, "2 500,00")],
        [(10, "05/02"), (30, "CARTE 04/02 CARREFOUR"), (140, "84,30")],
        [(30, "PARIS 15")],
        [(10, "10/02"), (30, "PRLV SEPA EDF"), (140, "1 200,00")],
        [(10, "NOUVEAU SOLDE AU 28/02/2026"), (173, "2 215,70")],
    ]
    [st] = parse_document("releve.pdf", _pdf(rows))
    assert st.institution == "BforBank"
    assert st.iban == "FR7630001007941234567890185"
    assert st.opening_balance == Decimal("1000.00")
    assert st.balance == Decimal("2215.70")
    assert [(t.date, t.amount) for t in st.transactions] == [
        (date(2026, 2, 3), Decimal("2500.00")), (date(2026, 2, 5), Decimal("-84.30")), (date(2026, 2, 10), Decimal("-1200.00")),
    ]
    assert "PARIS 15" in st.transactions[1].label
    assert st.confidence == "high"


def test_pdf_other_document_offers_candidate_amounts():
    rows = [
        [(10, "Spirica - Releve de situation annuel")],
        [(10, "Contrat Linxea Avenir")],
        [(10, "Valeur de rachat au 31/12/2025"), (150, "25 431,12")],
    ]
    [st] = parse_document("situation.pdf", _pdf(rows))
    assert not st.transactions
    assert st.candidate_balances[0][1] == Decimal("25431.12")
