"""Fortuneo: monthly statement, yearly fees recap and the FGDR letter, on synthetic documents only (see fixtures_fortuneo.py)."""

import re
from datetime import date
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from mymoney.adapters import SourceDocument, identify
from mymoney.core import MyMoney
from mymoney.models import Account, Document, FeeSummary, Transaction
from mymoney.web.app import create_app

from .conftest import ADMIN_EMAIL, PASSWORD
from .fixtures import Entry, Spec, boursobank_statement_pdf
from .fixtures_fortuneo import (
    F_ACCOUNT, F_ADDRESS, F_BANK, F_BRANCH, F_HOLDER, F_IBAN, FeesSpec, FEntry, FSpec, fortuneo_entries, fortuneo_fees_pdf,
    fortuneo_info_pdf, fortuneo_september, fortuneo_statement_pdf,
)

D = Decimal


@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    return app


def parse(data: bytes, name: str = "doc.pdf"):
    doc = SourceDocument(name, data)
    found = identify(doc)
    return found, found.chosen.adapter().parse(doc) if found.chosen else []


def drop(core: MyMoney, data: bytes, name: str = "doc.pdf") -> int:
    doc_id, _ = core.store_document(name, data)
    core.analyze_document(doc_id, None, data)
    return doc_id


def import_statement(core: MyMoney, spec: FSpec) -> tuple[int, int]:
    doc_id = drop(core, fortuneo_statement_pdf(spec))
    result = core.import_statement(doc_id, 0, {"account_id": "new", "name": "Compte Fortuneo", "import_transactions": "on"})
    return doc_id, result["account_id"]


# ------------------------------------------------------------------------------------------ detection
def test_each_document_goes_to_its_own_adapter():
    for data, adapter in (
        (fortuneo_statement_pdf(fortuneo_september()), "fortuneo.statement_pdf"),
        (fortuneo_fees_pdf(FeesSpec()), "fortuneo.fees_pdf"),
        (fortuneo_info_pdf(), "fortuneo.deposit_guarantee_pdf"),
    ):
        found = identify(SourceDocument("x.pdf", data))
        assert found.decision == "auto" and found.chosen.adapter.id == adapter and found.institution == "Fortuneo"


def test_the_brand_is_required_so_another_banks_look_alike_is_not_taken():
    for data in (
        fortuneo_statement_pdf(FSpec(fortuneo_entries(), brand="AUTREBANQUE")),
        fortuneo_fees_pdf(FeesSpec(brand="AutreBanque")),
        fortuneo_info_pdf(brand="AutreBanque"),
    ):
        found = identify(SourceDocument("x.pdf", data))
        assert not (found.decision == "auto" and found.chosen.adapter.id.startswith("fortuneo."))


# ------------------------------------------------------------------------------------------ statement
def test_statement_is_read_and_reconciled():
    spec = fortuneo_september()
    _, [st] = parse(fortuneo_statement_pdf(spec))
    assert (st.institution, st.document_kind, st.account_type) == ("Fortuneo", "statement", "checking")
    assert (st.period_start, st.period_end) == (date(2026, 9, 1), date(2026, 9, 30))
    assert (st.opening_balance, st.balance) == (spec.opening, spec.closing)
    assert st.confidence == "high" and not st.warnings
    assert st.extra["declared_totals"] == {"debit": "433.50", "credit": "2400.00"}
    assert st.holder_name == F_HOLDER and st.holder_address == "\n".join(F_ADDRESS)
    assert (st.bank_code, st.branch_code, st.account_number) == (F_BANK, F_BRANCH, F_ACCOUNT)
    assert st.iban == F_IBAN and st.bic == "FTNOFRP1XXX"
    assert len(st.transactions) == len(spec.entries)


def test_statement_operations_are_classified():
    _, [st] = parse(fortuneo_statement_pdf(fortuneo_september()))
    by_amount = {t.amount: t for t in st.transactions}
    own = by_amount[D("-60.00")]
    assert own.kind == "transfer" and own.details["internal"]
    card = by_amount[D("-3.50")]
    assert card.kind == "card" and card.counterparty == "Boulangerie Test Lyon" and card.details["purchase_date"] == "2026-09-12"
    assert card.date == date(2026, 9, 14) and card.value_date == date(2026, 9, 13)
    loan = by_amount[D("-250.00")]
    assert loan.kind == "loan_payment" and loan.details["loan_number"] == "0000TEST00000001"
    debit = by_amount[D("-80.00")]
    assert debit.kind == "direct_debit" and debit.counterparty == "ASSURANCE EXEMPLE"
    assert debit.details["ics"] == "FR00ZZZ000000" and debit.details["ids"] == ["ABCD25139000000003485321", "V260918007125"]
    salary = by_amount[D("1500.00")]
    assert salary.kind == "transfer" and salary.counterparty == "ACME CONSULTING SAS" and salary.reference == "SCT000000000000000000000001"
    assert "Salaire septembre" in salary.label and not (salary.details or {}).get("internal")


def test_a_wrong_closing_balance_or_wrong_declared_totals_are_flagged():
    _, [st] = parse(fortuneo_statement_pdf(FSpec(fortuneo_entries(), wrong_closing=D("1.00"))))
    assert st.confidence == "low" and any("ne tombent pas juste" in w for w in st.warnings)
    _, [st] = parse(fortuneo_statement_pdf(FSpec(fortuneo_entries(), wrong_totals=True)))
    assert st.confidence == "low" and any("totaux imprimés" in w for w in st.warnings)


def test_an_overdrawn_account_and_a_statement_with_no_operation():
    spec = FSpec([FEntry(date(2026, 9, 5), ["CARTE 04/09 SHOP"], D("-100.00"))], opening=D("20.00"))
    _, [st] = parse(fortuneo_statement_pdf(spec))
    assert st.balance == D("-80.00") and st.confidence == "high"
    _, [empty] = parse(fortuneo_statement_pdf(FSpec([], opening=D("20.00"))))
    assert empty.balance == D("20.00") and empty.transactions == [] and empty.confidence == "high"


def test_dates_without_a_year_follow_the_period_across_new_year():
    spec = FSpec([FEntry(date(2026, 12, 30), ["CARTE 29/12 SHOP"], D("-5.00")), FEntry(date(2027, 1, 2), ["CARTE 02/01 SHOP"], D("-6.00"))],
                 period_end=date(2027, 1, 31), opening=D("100.00"))
    spec.period_end = date(2027, 1, 31)
    _, [st] = parse(fortuneo_statement_pdf(spec))
    assert [t.date for t in st.transactions] == [date(2026, 12, 30), date(2027, 1, 2)]


# ---------------------------------------------------------------------------------------------- import
def test_first_statement_creates_the_account_with_its_identity(core):
    spec = fortuneo_september()
    doc_id = drop(core, fortuneo_statement_pdf(spec))
    assert core.auto_import(doc_id) is None  # an account to create: the user is asked
    _, account_id = import_statement(core, FSpec(fortuneo_entries(), title="again"))
    with core.db.session() as s:
        account = s.get(Account, account_id)
        assert account.institution == "Fortuneo" and account.holder_name == F_HOLDER
        assert (account.iban, account.bic) == (F_IBAN, "FTNOFRP1XXX")
        assert account.holder_address == "\n".join(F_ADDRESS)
        assert s.scalars(select(Transaction).where(Transaction.account_id == account_id)).all().__len__() == len(spec.entries)


def test_the_next_statement_of_the_account_is_imported_automatically(core):
    first = fortuneo_september()
    _, account_id = import_statement(core, first)
    october = FSpec([FEntry(date(2026, 10, 5), ["CARTE 04/10 SHOP"], D("-9.00"))], opening=first.closing, period_end=date(2026, 10, 31))
    doc_id = drop(core, fortuneo_statement_pdf(october))
    result = core.auto_import(doc_id)
    assert result and result["account_id"] == account_id and result["added"] == 1


def test_transfers_between_fortuneo_and_boursobank_are_paired(core):
    _, fortuneo_id = import_statement(core, fortuneo_september())
    bourso = Spec([Entry(date(2026, 9, 9), ["VIR INST MARTIN PAUL", "vers Fortuneo"], D("-200.00"))], opening=D("1000.00"))
    data = boursobank_statement_pdf(bourso)
    doc_id, _ = core.store_document("bourso.pdf", data)
    core.analyze_document(doc_id, None, data)
    result = core.import_statement(doc_id, 0, {"account_id": "new", "name": "Compte courant", "import_transactions": "on"})
    with core.db.session() as s:
        sent = s.scalar(select(Transaction).where(Transaction.account_id == result["account_id"]))
        received = s.scalar(select(Transaction).where(Transaction.account_id == fortuneo_id, Transaction.amount == D("200.00")))
        assert sent.internal and received.internal and sent.details["paired_with"] == received.id


# ----------------------------------------------------------------------------------------- fees recap
def test_fees_recap_is_read_with_the_services_and_checked():
    spec = FeesSpec(services=[("Cartes", "COTISATION CARTE", 1, "9,00", 1, "9,00")])
    _, [st] = parse(fortuneo_fees_pdf(spec))
    assert (st.document_kind, st.institution) == ("fees_summary", "Fortuneo")
    assert (st.period_start, st.period_end) == (date(2023, 1, 1), date(2023, 12, 31))
    assert st.extra["fees_total"] == "9.00" and st.extra["interest_paid"] == "0.00" and st.extra["interest_earned"] is None
    assert st.extra["services"] == [{"category": "Cartes", "name": "COTISATION CARTE", "used": 1, "unit_price": "9.00", "billed": 1, "total": "9.00"}]
    assert st.iban == F_IBAN and st.holder_name == F_HOLDER and st.holder_address == "\n".join(F_ADDRESS)
    assert st.confidence == "high" and st.transactions == []
    _, [bad] = parse(fortuneo_fees_pdf(FeesSpec(total=D("12.00"), services=[("Cartes", "COTISATION CARTE", 1, "9,00", 1, "9,00")])))
    assert bad.confidence == "low" and any("somme des services" in w for w in bad.warnings)


def test_fees_recap_is_attached_to_the_account_known_by_its_iban(core):
    _, account_id = import_statement(core, fortuneo_september())
    doc_id = drop(core, fortuneo_fees_pdf(FeesSpec()))
    result = core.auto_import(doc_id)
    assert result and result["kind"] == "fees_summary" and result["account_id"] == account_id and result["year"] == 2023
    with core.db.session() as s:
        [summary] = s.scalars(select(FeeSummary)).all()
        assert (summary.account_id, summary.fees_total, summary.interest_earned) == (account_id, D("9.00"), None)
        assert s.get(Document, doc_id).status == "imported"
        assert s.scalar(select(Transaction).where(Transaction.kind == "fee")) is None  # no operation is created by a recap

    # The same recap in another file: refused for the same period unless forced.
    other = drop(core, fortuneo_fees_pdf(FeesSpec(title="copy")))
    with pytest.raises(ValueError, match="déjà rattaché"):
        core.import_statement(other, 0, {"account_id": str(account_id)})
    core.import_statement(other, 0, {"account_id": str(account_id), "force": "on"})
    with core.db.session() as s:
        assert len(s.scalars(select(FeeSummary)).all()) == 1

    core.undo_import(other)
    with core.db.session() as s:
        assert s.scalars(select(FeeSummary)).all() == []


def test_fees_recap_without_a_known_account_asks_and_can_create_it(core):
    doc_id = drop(core, fortuneo_fees_pdf(FeesSpec()))
    assert core.auto_import(doc_id) is None
    result = core.import_statement(doc_id, 0, {"account_id": "new", "name": "Compte Fortuneo"})
    with core.db.session() as s:
        account = s.get(Account, result["account_id"])
        assert account.iban == F_IBAN and account.holder_name == F_HOLDER and account.institution == "Fortuneo"


def test_the_fees_recap_is_compared_with_the_fee_lines_of_the_statements(core):
    from mymoney.web.app import fee_summaries

    spec = FSpec([FEntry(date(2026, 9, 5), ["COTISATION CARTE GOLD"], D("-9.00"))], opening=D("100.00"))
    _, account_id = import_statement(core, spec)
    with core.db.session() as s:
        s.add(FeeSummary(account_id=account_id, period_start=date(2026, 9, 1), period_end=date(2026, 9, 30), fees_total=D("9.00"), services=[]))
        s.add(FeeSummary(account_id=account_id, period_start=date(2026, 8, 1), period_end=date(2026, 8, 31), fees_total=D("3.00"), services=[]))
        s.flush()
        rows = {r["summary"].period_start: r for r in fee_summaries(s, s.get(Account, account_id))}
    ok = rows[date(2026, 9, 1)]
    assert ok["complete"] and ok["seen"] == D("9.00") and ok["seen_count"] == 1
    assert not rows[date(2026, 8, 1)]["complete"]  # no statement for August


# ----------------------------------------------------------------------------------------- FGDR letter
def test_the_deposit_guarantee_letter_is_filed_automatically(core):
    _, [st] = parse(fortuneo_info_pdf(year=2026))
    assert st.document_kind == "info" and st.extra["year"] == 2026 and st.extra["ceiling"] == "100 000 €"
    assert st.holder_name == F_HOLDER and st.holder_address == "\n".join(F_ADDRESS)
    doc_id = drop(core, fortuneo_info_pdf(year=2026))
    result = core.auto_import(doc_id)
    assert result and result["kind"] == "info" and result["account_id"] is None
    with core.db.session() as s:
        assert s.get(Document, doc_id).status == "imported" and s.scalar(select(Transaction)) is None
    assert core.read_document(doc_id)  # still downloadable (decrypted from the encrypted store)


def test_the_same_letter_dropped_twice_is_recognised_as_the_same_file(core):
    data = fortuneo_info_pdf()
    first, existing = core.store_document("a.pdf", data)
    again, existing_again = core.store_document("b.pdf", data)
    assert not existing and existing_again and first == again


# ------------------------------------------------------------------------------------------------- web
@pytest.fixture
def client(settings):
    app = create_app(settings)
    with TestClient(app) as c:
        c.core = app.state.core
        yield c


def csrf_of(html):
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def login(client):
    client.post("/setup", data={"setup_token": client.core.setup_token, "email": ADMIN_EMAIL, "password": PASSWORD, "confirm": PASSWORD})
    return csrf_of(client.get("/documents").text)


def upload(client, csrf, name, data):
    return client.post("/documents", data={"csrf": csrf}, files={"files": (name, data, "application/pdf")})


def test_web_the_library_shows_type_and_institution_and_the_account_checks_the_fees(client):
    csrf = login(client)
    page = upload(client, csrf, "releve.pdf", fortuneo_statement_pdf(fortuneo_september()))
    doc = re.search(r"/documents/(\d+)", str(page.url)).group(1)
    assert "Fortuneo · relevé de compte ou de livret (PDF)" in page.text
    form = client.post(f"/documents/{doc}/import/0", data={"csrf": csrf, "account_id": "new", "name": "Compte Fortuneo", "import_transactions": "on"})
    account_id = re.search(r"/accounts/(\d+)", str(form.url)).group(1)

    done = upload(client, csrf, "frais.pdf", fortuneo_fees_pdf(FeesSpec()))
    assert "récapitulatif des frais 2023 rattaché au compte" in done.text

    info = upload(client, csrf, "fgdr.pdf", fortuneo_info_pdf())
    assert "classé avec tes papiers" in info.text

    library = client.get("/documents")
    assert "Récapitulatif des frais" in library.text and "Information" in library.text and "Relevé de compte" in library.text

    account = client.get(f"/accounts/{account_id}")
    assert "Frais et intérêts" in account.text and "9,00" in account.text
    assert "Relevés incomplets sur cette période" in account.text  # one statement of 2026 cannot confirm the fees of 2023


# ------------------------------------------------------------------------ savings book, first statement, joint account
def test_a_savings_book_statement_is_read_without_rib_or_iban():
    spec = FSpec([FEntry(date(2026, 12, 31), ["INTERETS 2026"], D("0.13"), value_day=date(2027, 1, 1))], opening=D("10.32"), period_end=date(2026, 12, 31),
                 opening_on=date(2025, 12, 31), livret=True, number="1 - 2026")
    found, [st] = parse(fortuneo_statement_pdf(spec))
    assert found.decision == "auto" and found.chosen.adapter.id == "fortuneo.statement_pdf"
    assert (st.account_type, st.extra["product"]) == ("savings", "Livret +") and st.account_label == "Livret + Fortuneo …6501"
    assert st.account_number == F_ACCOUNT[:8] + "01" and st.iban is None and st.bank_code is None
    assert (st.period_start, st.period_end) == (date(2026, 1, 1), date(2026, 12, 31))  # one statement a year
    assert (st.opening_balance, st.balance) == (D("10.32"), D("10.45")) and st.confidence == "high" and not st.warnings
    [t] = st.transactions
    assert t.kind == "interest" and t.date == date(2026, 12, 31) and t.amount == D("0.13")  # the value date is in January: the operation is not


def test_a_savings_book_is_its_own_account_and_matched_again_by_its_number(core):
    spec = FSpec([FEntry(date(2026, 12, 31), ["INTERETS 2026"], D("0.13"), value_day=date(2027, 1, 1))], opening=D("10.32"), period_end=date(2026, 12, 31),
                 opening_on=date(2025, 12, 31), livret=True)
    _, courant = import_statement(core, fortuneo_september())
    doc_id = drop(core, fortuneo_statement_pdf(spec))
    assert core.auto_import(doc_id) is None  # a new account to create: the user decides
    saved = core.import_statement(doc_id, 0, {"account_id": "new", "name": "Livret +", "import_transactions": "on"})["account_id"]
    assert saved != courant
    next_year = FSpec([FEntry(date(2027, 12, 31), ["INTERETS 2027"], D("0.20"))], opening=D("10.45"), period_end=date(2027, 12, 31), opening_on=date(2026, 12, 31), livret=True)
    again = drop(core, fortuneo_statement_pdf(next_year))
    assert core.auto_import(again)["account_id"] == saved  # recognised by its number, no IBAN needed
    with core.db.session() as s:
        assert s.get(Account, saved).account_type == "savings"


def test_the_first_statement_of_an_account_has_no_opening_date():
    spec = FSpec([FEntry(date(2026, 10, 8), ["REM CHQ 4085312 0034 001 CHQ", "24293IP1RT038729693"], D("645.11")), FEntry(date(2026, 10, 19), ["VIR INST M MARTIN PAUL"], D("-645.11"))],
                 opening=D("0.00"), period_end=date(2026, 10, 31), first=True)
    _, [st] = parse(fortuneo_statement_pdf(spec))
    assert (st.period_start, st.period_end) == (date(2026, 10, 1), date(2026, 10, 31)) and st.extra["first_statement"]
    assert st.opening_balance == D("0.00") and st.balance == D("0.00") and st.confidence == "high" and not st.warnings
    cheque, transfer = st.transactions
    assert cheque.kind == "cheque" and "Remise de chèque" in cheque.label and "4085312" in cheque.label
    assert transfer.kind == "instant_transfer"


def test_a_joint_account_holder_line_cut_by_the_bank_is_glued_back_and_confirmed_once(core):
    holder = "M MARTIN PAUL OU MME DUPUIS-LEBLANC ANNE"  # cut at 30 characters, in the middle of a word
    assert len(holder) > 30 and holder[29] != " "
    spec = FSpec(fortuneo_entries(), holder=holder)
    _, [st] = parse(fortuneo_statement_pdf(spec))
    assert st.holder_name == holder and st.extra["holder_maybe_truncated"]
    assert st.confidence == "high"
    doc_id = drop(core, fortuneo_statement_pdf(spec))
    assert core.auto_import(doc_id) is None  # new account, and the names are to be confirmed
    plan = core.review_document(doc_id)["plans"][0]
    assert plan.ask_holder
    core.import_statement(doc_id, 0, {"account_id": "new", "name": "Compte joint", "holder_name": holder, "import_transactions": "on"})
    with core.db.session() as s:
        assert s.scalar(select(Account)).holder_name == holder


def test_the_guarantee_ceiling_is_shown_the_same_way_whatever_the_spacing():
    for printed in ("100 000 €", "100000 €", "100\u00a0000 €"):
        _, [st] = parse(fortuneo_info_pdf(ceiling=printed))
        assert st.extra["ceiling"] == "100 000 €", printed


def test_a_savings_statement_with_an_operation_the_balances_do_not_explain_is_flagged():
    spec = FSpec([FEntry(date(2026, 12, 31), ["INTERETS 2026"], D("0.13"))], opening=D("10.32"), period_end=date(2026, 12, 31), opening_on=date(2025, 12, 31), livret=True,
                 wrong_closing=D("11.00"))
    _, [st] = parse(fortuneo_statement_pdf(spec))
    assert st.confidence == "low" and any("ne tombent pas juste" in w for w in st.warnings)


def test_web_validate_all_imports_what_is_obvious_in_one_click(client):
    csrf = login(client)
    # The recap comes first and the account does not exist yet: both wait for a green light.
    upload(client, csrf, "frais.pdf", fortuneo_fees_pdf(FeesSpec()))
    upload(client, csrf, "releve.pdf", fortuneo_statement_pdf(fortuneo_september()))
    page = client.get("/documents")
    assert "À valider" in page.text and "Tout valider (2)" in page.text

    done = client.post("/documents/validate-all", data={"csrf": csrf})
    assert "2 document(s) validé(s)" in done.text
    assert "Tout valider" not in done.text
    with client.core.db.session() as s:
        assert len(list(s.scalars(select(Account)))) == 1
        assert s.scalar(select(FeeSummary)) is not None
        assert len(list(s.scalars(select(Transaction)))) == len(fortuneo_entries())


def test_web_validate_all_leaves_a_cut_joint_holder_name_to_the_user(client):
    csrf = login(client)
    long_name = "M PRENOMLONG NOMDEFAMILLE OU MME AUTRE NOM"
    upload(client, csrf, "joint.pdf", fortuneo_statement_pdf(fortuneo_september(holder=long_name)))
    done = client.post("/documents/validate-all", data={"csrf": csrf})
    assert "0 document(s) validé(s)" in done.text and "1 à regarder un par un" in done.text
    with client.core.db.session() as s:
        assert s.scalar(select(Account)) is None


def test_web_the_documents_show_their_reliability_and_can_be_reviewed_one_after_the_other(client):
    csrf = login(client)
    upload(client, csrf, "releve.pdf", fortuneo_statement_pdf(fortuneo_september()))
    upload(client, csrf, "joint.pdf", fortuneo_statement_pdf(fortuneo_september(holder="M PRENOMLONG NOMDEFAMILLE OU MME AUTRE NOM", account="0001234500", title="b")))
    page = client.get("/documents")
    assert page.text.count("fiabilité élevée") >= 2 and "Passer en revue un par un" in page.text

    first = client.get("/documents/review")
    assert str(first.url).endswith("?review=1") or "review=1" in str(first.url)
    assert "Revue des documents à valider" in first.text and "2 en attente" in first.text
    doc = re.search(r"/documents/(\d+)", str(first.url)).group(1)

    skipped = client.get(f"/documents/review?after={doc}")
    other = re.search(r"/documents/(\d+)", str(skipped.url)).group(1)
    assert other != doc and "Revue des documents à valider" in skipped.text

    # Validating inside the review leads to the next waiting document, not to the account page.
    done = client.post(f"/documents/{other}/import/0", data={"csrf": csrf, "review": "1", "account_id": "new", "name": "Compte", "holder_name": "M A OU MME B", "import_transactions": "on"})
    assert "/accounts/" not in str(done.url)
    assert "Revue des documents à valider" in done.text or "Revue terminée" in done.text
