from datetime import date
from decimal import Decimal

import pytest

from mymoney.adapters import SourceDocument, identify, parse_with
from mymoney.adapters.common import iban_valid, name_key, rib_valid
from mymoney.adapters.pdftools import fix_text

from .fixtures import ACCOUNT, ADDRESS, BANK, BRANCH, IBAN, JOINT_ADDRESS, JOINT_HOLDER, Entry, SavingsSpec, Spec, boursobank_savings_pdf, boursobank_statement_pdf, sample_entries

ADAPTER = "boursobank.statement_pdf"


def parse(spec: Spec):
    doc = SourceDocument("releve.pdf", boursobank_statement_pdf(spec))
    [st] = parse_with(ADAPTER, doc)
    return st


def by_label(st, text):
    return next(t for t in st.transactions if text in t.label or text in (t.raw or ""))


def test_detected_and_chosen_without_asking():
    found = identify(SourceDocument("releve.pdf", boursobank_statement_pdf(Spec(sample_entries()))))
    assert found.decision == "auto" and found.chosen.adapter.id == ADAPTER
    assert found.chosen.score >= 0.9
    assert found.institution == "BoursoBank"


def test_identity_of_the_account_and_its_holder():
    st = parse(Spec(sample_entries()))
    assert st.holder_name == "M DUPONT JEAN"
    assert st.holder_address == "\n".join(ADDRESS)
    assert (st.bank_code, st.branch_code, st.account_number) == (BANK, BRANCH, ACCOUNT)
    assert rib_valid(st.bank_code, st.branch_code, st.account_number, st.rib_key)
    assert st.iban == IBAN and iban_valid(st.iban) and st.bic == "BOUSFRPPXXX"
    assert (st.period_start, st.period_end) == (date(2026, 9, 1), date(2026, 9, 30))
    assert st.currency == "EUR" and st.account_type == "checking"


def test_balances_reconcile_with_the_operations():
    spec = Spec(sample_entries())
    st = parse(spec)
    assert st.opening_balance == Decimal("1000.00") and st.opening_date == date(2026, 8, 31)
    assert st.balance == spec.closing and st.balance_date == date(2026, 9, 30)
    assert len(st.transactions) == len(spec.entries)
    assert sum(t.amount for t in st.transactions) == spec.closing - spec.opening
    assert st.confidence == "high" and not st.warnings


def test_each_kind_of_line_is_understood():
    st = parse(Spec(sample_entries()))

    card = by_label(st, "TRAINS DE FRANCE")
    assert (card.kind, card.counterparty, card.amount) == ("card", "TRAINS DE FRANCE", Decimal("-71.00"))
    assert card.details == {"purchase_date": "2026-08-31", "card_last4": "1234"}

    debit = by_label(st, "EAUX DU NORD")
    assert debit.kind == "direct_debit" and debit.counterparty == "EAUX DU NORD"
    assert debit.details["rum"] == "S-1-0000000001-000000000-01" and "Facture EAUX" in debit.label

    tax = by_label(st, "DIRECTION GENERALE")
    assert "IMPOT TF" in tax.label  # the descriptive line, not the identifier line

    transfer = by_label(st, "MARTIN Pauline")
    assert transfer.kind == "transfer" and transfer.counterparty == "MARTIN Pauline"
    assert transfer.reference == "SCT123450000000000000000001" == transfer.external_id
    assert transfer.memo == "cadeau anniversaire"

    salary = by_label(st, "ACME CONSULTING")
    assert salary.amount == Decimal("2400.00") and salary.counterparty == "ACME CONSULTING SAS"
    assert salary.reference == "03620N0200000000000000" and salary.external_id is None

    instant = next(t for t in st.transactions if t.kind == "instant_transfer")
    assert instant.counterparty == "DUPONT JEAN" and instant.details == {"internal": True}

    loan = next(t for t in st.transactions if t.kind == "loan_payment")
    assert loan.details["loan_number"] == "80000000000000000001" and loan.amount == Decimal("-166.67")

    fee = next(t for t in st.transactions if t.kind == "fee")
    assert fee.amount == Decimal("-2.00") and "COTIS" in fee.label


def test_transfers_between_own_accounts_are_marked_internal():
    st = parse(Spec(sample_entries()))
    from_savings = by_label(st, "LIVRET DEVELOPPE")
    assert from_savings.details == {"internal": True} and from_savings.counterparty == "DUPONT JEAN"
    to_partner = by_label(st, "LEROY CLAIRE")
    assert not (to_partner.details or {}).get("internal")


def test_credit_and_debit_columns_and_value_dates():
    st = parse(Spec(sample_entries()))
    incoming = next(t for t in st.transactions if t.counterparty == "LEROY CLAIRE" and t.amount > 0)
    assert incoming.amount == Decimal("60.00") and incoming.date == date(2026, 9, 12) and incoming.value_date == date(2026, 9, 13)
    outgoing = next(t for t in st.transactions if t.counterparty == "MME LEROY CLAIRE")
    assert outgoing.amount == Decimal("-35.00")


def test_many_pages_still_reconcile():
    entries = [Entry(date(2026, 9, 1 + i % 28), [f"CARTE 01/09/26 SHOP {i} CB*1234"], Decimal(f"-{i + 1}.50")) for i in range(70)]
    entries.sort(key=lambda e: e.day)
    spec = Spec(entries, opening=Decimal("5000.00"))
    doc = SourceDocument("releve.pdf", boursobank_statement_pdf(spec))
    assert len(doc.words()) > 2
    [st] = parse_with(ADAPTER, doc)
    assert len(st.transactions) == 70 and st.confidence == "high" and st.balance == spec.closing


def test_accents_mangled_by_the_pdf_font_do_not_break_the_reading():
    clean = parse(Spec(sample_entries()))
    garbled = parse(Spec(sample_entries(), garble=True))
    assert garbled.confidence == "high" and not garbled.warnings
    assert [(t.date, t.amount, t.kind) for t in garbled.transactions] == [(t.date, t.amount, t.kind) for t in clean.transactions]
    assert garbled.holder_name == clean.holder_name and garbled.iban == clean.iban


def test_fix_text():
    assert fix_text("LibellØ DØbit RŁf (cid:224) 20h 5(cid:176) 10(cid:128)") == "Libellé Débit Rèf à 20h 5° 10€"


def test_a_statement_that_does_not_add_up_is_flagged():
    spec = Spec(sample_entries(), wrong_closing=Decimal("1.00"))
    st = parse(spec)
    assert st.confidence == "low"
    assert any("écart" in w for w in st.warnings)


def test_other_documents_do_not_trigger_this_adapter():
    from .test_adapters_generic import _pdf

    rows = [[(10, "FORTUNEO - Releve de compte courant")], [(10, "Date"), (30, "Libelle"), (140, "Debit"), (175, "Credit")]]
    found = identify(SourceDocument("releve.pdf", _pdf(rows)))
    assert found.decision == "fallback"
    assert all(c.adapter.id != ADAPTER or c.score < 0.4 for c in found.candidates)


def test_same_brand_but_another_document_is_not_taken_for_a_statement():
    from .test_adapters_generic import _pdf

    rows = [[(10, "BoursoBank - Avis d'opere")], [(10, "Detail des frais de tenue de compte")]]
    found = identify(SourceDocument("avis.pdf", _pdf(rows)))
    assert found.decision != "auto"


def test_name_key_matches_the_same_person_written_differently():
    assert name_key("M DUPONT Jean") == name_key("Jean DUPONT") == name_key("MME dupont  jean")
    assert name_key("ACME CONSULTING SAS") == name_key("Acme Consulting")
    assert name_key("DUPONT JEAN") != name_key("DUPONT PAUL")


# ------------------------------------------------------------------------------- savings extract (PEL...)
SAVINGS = "boursobank.savings_pdf"


def pel_spec(**kw) -> SavingsSpec:
    return SavingsSpec([
        Entry(date(2026, 9, 28), ["VIR DUPONT JEAN"], Decimal("45.00")),
        Entry(date(2026, 9, 30), ["INTERETS 2026", "Prime d'Etat"], Decimal("12.34")),
    ], **kw)


def parse_pel(spec: SavingsSpec):
    doc = SourceDocument("pel.pdf", boursobank_savings_pdf(spec))
    [st] = parse_with(SAVINGS, doc)
    return st


def test_a_savings_extract_picks_its_own_adapter_and_not_the_current_account_one():
    found = identify(SourceDocument("pel.pdf", boursobank_savings_pdf(pel_spec())))
    assert found.decision == "auto" and found.chosen.adapter.id == SAVINGS and found.chosen.score >= 0.9
    scores = {c.adapter.id: c.score for c in found.candidates}
    assert scores[ADAPTER] < 0.6  # same brand and table, but no RIB banner
    current = identify(SourceDocument("releve.pdf", boursobank_statement_pdf(Spec(sample_entries()))))
    assert current.chosen.adapter.id == ADAPTER
    assert {c.adapter.id: c.score for c in current.candidates}[SAVINGS] <= 0.3


def test_savings_identity_has_an_account_number_but_no_iban():
    st = parse_pel(pel_spec())
    assert st.account_number == "00099999999" and st.currency == "EUR"
    assert (st.iban, st.bic, st.bank_code, st.rib_key) == (None, None, None, None)
    assert st.holder_name == "DUPONT JEAN" and st.holder_address == "\n".join(ADDRESS)
    assert (st.period_start, st.period_end) == (date(2026, 9, 1), date(2026, 9, 30))
    assert st.account_type == "pel" and st.extra["product"] == "Plan Epargne Logement"
    assert st.account_label == "Plan Epargne Logement BoursoBank …9999"


def test_savings_balances_reconcile_and_lines_are_understood():
    spec = pel_spec()
    st = parse_pel(spec)
    assert st.opening_balance == Decimal("3000.00") and st.opening_date == date(2026, 9, 1)
    assert st.balance == spec.closing == Decimal("3057.34") and st.balance_date == date(2026, 9, 30)
    assert st.confidence == "high" and not st.warnings
    transfer, interest = st.transactions
    assert (transfer.kind, transfer.counterparty, transfer.amount) == ("transfer", "DUPONT JEAN", Decimal("45.00"))
    assert transfer.details == {"internal": True}  # a transfer from the holder to the holder
    assert (interest.kind, interest.amount) == ("interest", Decimal("12.34")) and "Prime d'Etat" in interest.label


def test_savings_debits_are_read_from_the_debit_column():
    spec = pel_spec()
    spec.entries.append(Entry(date(2026, 9, 29), ["VIR RETRAIT PARTIEL"], Decimal("-500.00")))
    st = parse_pel(spec)
    assert any(t.amount == Decimal("-500.00") for t in st.transactions)
    assert st.balance == spec.closing and st.confidence == "high"


# --------------------------------------------------------------- savings account and joint account (same layout)
def test_a_savings_account_in_the_current_account_layout_is_typed_from_its_product_line():
    st = parse(Spec(sample_entries()[:3], product="Livret Developpement Durable Solidaire"))
    assert st.account_type == "ldds" and st.extra["product"] == "Livret Developpement Durable Solidaire"
    assert st.account_label == "Livret Developpement Durable Solidaire BoursoBank …5678"
    assert st.holder_name == "M DUPONT JEAN" and st.holder_address == "\n".join(ADDRESS)  # the extra title line does not leak into them
    assert st.iban == IBAN and st.confidence == "high"
    assert parse(Spec(sample_entries()[:3])).account_type == "checking"


def test_a_joint_account_is_labelled_and_its_cut_holder_line_is_flagged():
    st = parse(Spec(sample_entries()[:3], holder=list(JOINT_HOLDER), address=list(JOINT_ADDRESS)))
    assert st.holder_name == "M DUPONT OU MME LEROY CLAIRE" and st.extra["holder_maybe_truncated"] is True
    assert st.holder_address == "12 RUE DES LILAS\nBAT B APPT 12\n75011 PARIS"  # the apartment line stays in the address
    assert st.account_label == "Compte joint BoursoBank …5678" and st.account_type == "checking"
    assert not parse(Spec(sample_entries()[:3])).extra.get("holder_maybe_truncated")


def test_a_word_cut_by_the_bank_at_the_end_of_a_line_is_glued_back():
    entry = Entry(date(2026, 9, 24), ["VIR Virement depuis BOURSORAMA Mathi", "eu", "DUPONT JEAN"], Decimal("50.00"))
    [t] = parse(Spec([entry])).transactions
    assert t.counterparty == "DUPONT JEAN" and t.memo == "Virement depuis BOURSORAMA Mathieu"
