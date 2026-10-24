"""A loan from its documents: offer, early repayment notice, dated amortization table (synthetic, see fixtures_loan.py)."""

import re
from datetime import date
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from mymoney.adapters import SourceDocument, identify
from mymoney.core import MyMoney
from mymoney.models import Account, Loan, LoanEvent, LoanInstallment
from mymoney.services import loans
from mymoney.web.app import create_app

from .conftest import PASSWORD
from .fixtures_fortuneo import FEntry, FSpec, fortuneo_statement_pdf
from .fixtures_loan import L_BORROWER, L_REF, LoanSpec, offer_pdf, prepayment_pdf, schedule_pdf

D = Decimal
TODAY = date(2026, 10, 24)


@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    return app


def read(data: bytes):
    doc = SourceDocument("doc.pdf", data)
    found = identify(doc)
    return found, found.chosen.adapter().parse(doc)[0] if found.chosen else None


def drop(core: MyMoney, data: bytes) -> int:
    doc_id, _ = core.store_document("doc.pdf", data)
    core.analyze_document(doc_id, None, data)
    return doc_id


def feed(core: MyMoney, data: bytes, account_id: int | None = None) -> tuple[int, dict | None]:
    """Drop a document like the user would: imported on its own when it can be, else attached to `account_id` (or a new loan)."""
    doc_id = drop(core, data)
    result = core.auto_import(doc_id)
    if result is None:
        result = core.import_statement(doc_id, 0, {"account_id": str(account_id) if account_id else "new"})
    return doc_id, result


def plan_of(core: MyMoney, account_id: int):
    with core.db.session() as s:
        loan = s.scalar(select(Loan).where(Loan.account_id == account_id))
        return loan, loans.rebuild(s, loan, TODAY)


def full_loan(core: MyMoney, spec: LoanSpec, order=("offer", "event", "schedule")) -> int:
    account_id = None
    data = {"offer": offer_pdf(spec), "event": prepayment_pdf(spec), "schedule": schedule_pdf(spec)}
    for kind in order:
        _, result = feed(core, data[kind], account_id)
        account_id = account_id or result["account_id"]
    return account_id


# ------------------------------------------------------------------------------------------ detection
def test_each_document_goes_to_its_own_adapter():
    spec = LoanSpec()
    for data, adapter, kind in (
        (offer_pdf(spec), "fortuneo.loan_offer_pdf", "loan_offer"),
        (prepayment_pdf(spec), "fortuneo.loan_prepayment_pdf", "loan_event"),
        (schedule_pdf(spec), "fortuneo.loan_schedule_pdf", "loan_schedule"),
    ):
        found, st = read(data)
        assert found.decision == "auto" and found.chosen.adapter.id == adapter and st.document_kind == kind
        assert st.institution == "Fortuneo" and st.account_type == "mortgage" and st.confidence == "high" and not st.warnings


def test_the_brand_is_required():
    for data in (offer_pdf(LoanSpec(brand="Autre")), prepayment_pdf(LoanSpec(brand="Autre")), schedule_pdf(LoanSpec(brand="Autre"))):
        found = identify(SourceDocument("x.pdf", data))
        assert not (found.decision == "auto" and found.chosen.adapter.id.startswith("fortuneo.loan"))


# ----------------------------------------------------------------------------------------------- offer
def test_offer_is_read_and_its_own_figures_are_checked():
    spec = LoanSpec()
    _, st = read(offer_pdf(spec))
    loan = st.extra["loan"]
    assert (loan["reference"], loan["project"], loan["product"]) == (L_REF, spec.project, "Fiximmo")
    assert (loan["principal"], loan["duration"], loan["rate"], loan["taeg"]) == ("60000.00", 60, "2.40", "2.95")
    assert loan["object"] == "Achat ancien" and loan["destination"] == "Résidence principale"
    assert st.holder_name == L_BORROWER and st.account_label == "Prêt Fiximmo …0002"
    assert len(loan["rows"]) == 60 and loan["rows"][0]["n"] == 1 and loan["rows"][-1]["remaining"] == "0.00"
    assert "12345678" not in str(loan) and "1980" not in str(loan)  # the borrower's id and birth date are not kept


def test_offer_with_figures_that_do_not_add_up_is_flagged():
    _, st = read(offer_pdf(LoanSpec(wrong_interest_total=True)))
    assert st.confidence == "low" and any("intérêts" in w for w in st.warnings)
    _, st = read(offer_pdf(LoanSpec(broken_row=7)))
    assert st.confidence == "low" and any("total à payer" in w for w in st.warnings)


def test_other_documents_read():
    spec = LoanSpec()
    _, event = read(prepayment_pdf(spec))
    ev = event.extra["loan"]["event"]
    assert ev["on_date"] == "2026-06-06" and ev["before"]["capital"] == str(spec.capital_before) and ev["after"]["capital"] == str(spec.capital_after)
    assert ev["before"]["end"] == spec.end_before.isoformat() and ev["after"]["end"] == spec.end_after.isoformat()
    assert event.extra["loan"]["borrower"] == L_BORROWER
    _, sched = read(schedule_pdf(spec))
    loan = sched.extra["loan"]
    assert loan["reference"] == L_REF and loan["rate"] == "2.40" and loan["balance"] == str(spec.schedule[0].remaining + spec.schedule[0].capital)
    assert sched.period_start == spec.schedule[0].day and sched.period_end == spec.end_after and len(loan["rows"]) == len(spec.schedule)


# --------------------------------------------------------------------------------------- the whole loan
@pytest.mark.parametrize("order", [("offer", "event", "schedule"), ("schedule", "event", "offer"), ("event", "schedule", "offer"), ("schedule", "offer", "event")])
def test_the_three_documents_rebuild_the_loan_in_any_order(core, order):
    spec = LoanSpec()
    account_id = full_loan(core, spec, order)
    loan, plan = plan_of(core, account_id)
    assert not plan.problems, [c.text for c in plan.problems]
    assert loan.reference == L_REF and loan.principal == spec.principal and loan.rate == spec.rate
    assert loan.first_due_date == spec.first_due  # dated from the repayment notice, which points at instalment n° 15
    sm = plan.summary
    assert sm["end_date"] == spec.end_after and plan.installments[-1].remaining == 0
    # Capital owed today: what the last instalment up to today leaves.
    expected = [x for x in spec.new_plan if x.day <= TODAY][-1].remaining
    assert sm["owed"] == expected
    assert sm["paid_count"] + sm["remaining_count"] == sm["total_count"] == spec.repaid_after + len(spec.new_plan)
    assert sm["repaid_early"] == spec.repaid and sm["saved_interest"] > 0 and sm["saved_months"] == spec.months - sm["total_count"]
    assert sm["next"].date == spec.schedule[0].day and sm["next"].total == spec.schedule[0].total
    assert any("retombe sur le tableau daté" in c.text for c in plan.checks) and any("entièrement remboursé" in c.text for c in plan.checks)
    with core.db.session() as s:
        account = s.get(Account, account_id)
        assert account.account_type == "mortgage" and account.holder_name == L_BORROWER and account.balance == expected


def test_rows_are_in_order_with_the_repayment_between_the_contract_and_the_new_plan(core):
    spec = LoanSpec()
    _, plan = plan_of(core, full_loan(core, spec))
    kinds = [(r.kind, r.source) for r in plan.rows]
    first_new = next(i for i, r in enumerate(plan.rows) if r.kind == "prepayment")
    assert kinds[0] == ("installment", "contrat") and kinds[first_new - 1] == ("installment", "contrat") and plan.rows[first_new - 1].number == spec.repaid_after
    assert plan.rows[first_new].remaining == spec.capital_after and plan.rows[first_new].capital == spec.repaid
    assert kinds[first_new + 1] == ("installment", "calculé") and kinds[-1] == ("installment", "échéancier")
    dates = [r.date for r in plan.rows]
    assert dates == sorted(dates) and plan.rows[0].date == spec.first_due


def test_without_the_repayment_notice_the_missing_repayment_is_noticed(core):
    spec = LoanSpec()
    account_id = full_loan(core, spec, ("offer", "schedule"))
    loan, plan = plan_of(core, account_id)
    assert loan.first_due_date is None  # nothing dates the contract's instalments
    warning = next(c for c in plan.problems if "aucune échéance du contrat ne mène à ce capital" in c.text)
    assert "remboursement anticipé" in warning.text
    # The notice arrives: everything is explained.
    feed(core, prepayment_pdf(spec), account_id)
    loan, plan = plan_of(core, account_id)
    assert not plan.problems and loan.first_due_date == spec.first_due


def test_a_repayment_that_is_not_in_the_contract_is_flagged(core):
    account_id = full_loan(core, LoanSpec(), ("offer",))
    _, st = read(prepayment_pdf(LoanSpec(principal=D("61000.00"))))  # the capital owed before is not one of this contract's
    with core.db.session() as s:
        account = s.get(Account, account_id)
        assert loans.find_accounts(s, st) == [account]  # the borrower's name is the only link
        assert "ne figure dans aucun tableau" in loans.unverified(s, st, account)


def test_the_notice_alone_asks_and_the_offer_then_claims_it(core):
    spec = LoanSpec()
    doc_id = drop(core, prepayment_pdf(spec))
    assert core.auto_import(doc_id) is None  # no loan yet: the user is asked
    result = core.import_statement(doc_id, 0, {"account_id": "new", "name": "Mon prêt"})
    with core.db.session() as s:
        loan = s.scalar(select(Loan))
        assert loan.reference is None and len(loan.events) == 1
    offer_id = drop(core, offer_pdf(spec))
    review = core.review_document(offer_id)
    assert [a.id for a in review["plans"][0].candidates] == [result["account_id"]]  # matched by its figures
    assert core.auto_import(offer_id)["account_id"] == result["account_id"]
    loan, plan = plan_of(core, result["account_id"])
    assert loan.reference == L_REF and loan.first_due_date == spec.first_due


# ------------------------------------------------------------------------------- duplicates and undo
def test_a_document_already_imported_is_refused_unless_forced_and_can_be_undone(core):
    spec = LoanSpec()
    account_id = full_loan(core, spec)
    again = drop(core, offer_pdf(LoanSpec(title="another download")))
    review = core.review_document(again)["plans"][0]
    assert review.loan_existing and not review.clean
    assert core.auto_import(again) is None
    with pytest.raises(ValueError, match="déjà son offre"):
        core.import_statement(again, 0, {"account_id": str(account_id)})
    core.import_statement(again, 0, {"account_id": str(account_id), "force": "on"})
    with core.db.session() as s:
        assert len(s.scalars(select(LoanInstallment).where(LoanInstallment.source == "offer")).all()) == spec.months
        assert s.scalar(select(Loan)).offer_document_id == again

    core.undo_import(again)
    with core.db.session() as s:
        loan = s.scalar(select(Loan))
        assert loan.offer_document_id is None and loan.principal is None and not [i for i in loan.installments if i.source == "offer"]
        assert loan.events and loan.installments  # the rest stays

    # A repayment notice twice is the same event.
    notice = drop(core, prepayment_pdf(LoanSpec(title="copy")))
    with pytest.raises(ValueError, match="déjà enregistré"):
        core.import_statement(notice, 0, {"account_id": str(account_id)})


def test_undoing_the_only_document_removes_the_loan_but_not_the_account(core):
    doc_id, result = feed(core, offer_pdf(LoanSpec()))
    core.undo_import(doc_id)
    with core.db.session() as s:
        assert s.scalar(select(Loan)) is None and s.get(Account, result["account_id"]) is not None


def test_a_table_with_another_rate_than_the_loan_is_refused(core):
    spec = LoanSpec()
    account_id = full_loan(core, spec, ("offer",))
    other = drop(core, schedule_pdf(LoanSpec(rate=D("3.10"))))
    with pytest.raises(ValueError, match="taux"):
        core.import_statement(other, 0, {"account_id": str(account_id)})


def test_a_loan_document_needs_a_credit_account(core):
    with core.db.session() as s:
        s.add(Account(name="Courant", account_type="checking"))
    doc_id = drop(core, offer_pdf(LoanSpec()))
    with pytest.raises(ValueError, match="type crédit"):
        core.import_statement(doc_id, 0, {"account_id": "1"})


# ----------------------------------------------------------------------------- statements and dates
def test_instalments_found_in_the_account_statements_are_compared_with_the_plan(core):
    spec = LoanSpec()
    account_id = full_loan(core, spec)
    paid = spec.new_plan[3]  # an instalment of the new plan, debited the day before its due date
    statement = FSpec([FEntry(paid.day.replace(day=14), [f"ECH PRET 0099{L_REF}"], -paid.total, value_day=paid.day)],
                      opening=D("5000.00"), period_end=date(paid.day.year, paid.day.month, 28))
    data = fortuneo_statement_pdf(statement)
    doc_id, _ = core.store_document("releve.pdf", data)
    core.analyze_document(doc_id, None, data)
    core.import_statement(doc_id, 0, {"account_id": "new", "name": "Compte courant", "import_transactions": "on"})
    _, plan = plan_of(core, account_id)
    assert any("1 prélèvement(s) retrouvé(s)" in c.text for c in plan.checks)
    assert [r.confirmed for r in plan.installments if r.date == paid.day] == [paid.total]
    assert not plan.problems

    with core.db.session() as s:  # replace the first statement's line by one with another amount
        from mymoney.models import Transaction
        t = s.scalar(select(Transaction).where(Transaction.kind == "loan_payment"))
        t.amount -= D("1.00")
    _, plan = plan_of(core, account_id)
    assert any("débités" in c.text and "prévus au plan" in c.text for c in plan.problems)


def test_first_due_date_can_be_given_by_hand_and_dates_the_contract(core):
    spec = LoanSpec()
    _, result = feed(core, offer_pdf(spec))
    loan, plan = plan_of(core, result["account_id"])
    assert loan.first_due_date is None and not plan.summary["dated"] and any(c.status == "info" for c in plan.checks)
    with core.db.session() as s:
        loan = s.scalar(select(Loan))
        plan = loans.set_first_due(s, loan, date(2025, 3, 15))
        assert plan.rows[0].date == date(2025, 3, 15) and plan.rows[-1].date == spec.end_before and plan.summary["dated"]


def test_without_any_repayment_the_contract_is_matched_by_the_dated_table(core):
    spec = LoanSpec(repaid=D("0.00"), new_payment_delta=D("0.00"))
    account_id = full_loan(core, spec, ("offer", "schedule"))
    loan, plan = plan_of(core, account_id)
    assert loan.first_due_date == spec.first_due and not plan.problems


# ---------------------------------------------------------------------------------------- unit pieces
def test_dates_and_projection():
    assert loans.add_months(date(2025, 1, 31), 1) == date(2025, 2, 28) and loans.add_months(date(2025, 11, 15), 3) == date(2026, 2, 15)
    rows = loans.project(D("1000.00"), D("12.00"), D("100.00"), D("0"), date(2026, 1, 15))
    assert rows[0].interest == D("10.00") and rows[0].capital == D("90.00") and rows[-1].remaining == 0 and rows[-1].date == date(2026, 11, 15)
    assert loans.project(D("1000.00"), D("12.00"), D("5.00"), D("0"), date(2026, 1, 15)) == []  # the payment does not even cover the interest


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
    client.post("/setup", data={"setup_token": client.core.setup_token, "password": PASSWORD, "confirm": PASSWORD})
    return csrf_of(client.get("/documents").text)


def upload(client, csrf, name, data):
    return client.post("/documents", data={"csrf": csrf}, files={"files": (name, data, "application/pdf")})


def test_web_the_loan_page_shows_the_plan_and_the_checks(client):
    csrf = login(client)
    spec = LoanSpec()
    review = upload(client, csrf, "offre.pdf", offer_pdf(spec))
    assert "Offre de prêt" in review.text and "Fortuneo · offre de prêt immobilier (PDF)" in review.text and "Créer le prêt à partir de ce document" in review.text
    doc = re.search(r"/documents/(\d+)", str(review.url)).group(1)
    done = client.post(f"/documents/{doc}/import/0", data={"csrf": csrf, "account_id": "new", "name": "Prêt Fiximmo", "account_type": "mortgage"})
    account_id = re.search(r"/accounts/(\d+)", str(done.url)).group(1)
    assert "offre de prêt enregistrée" in done.text and "Dater les échéances du contrat" in done.text

    for data in (prepayment_pdf(spec), schedule_pdf(spec)):
        assert "plan du prêt mis à jour" in upload(client, csrf, "doc.pdf", data).text
    page = client.get(f"/accounts/{account_id}")
    assert "tout est cohérent" in page.text and "Dater les échéances du contrat" not in page.text
    assert L_REF in page.text and "Capital restant dû" in page.text and "Remboursement anticipé" in page.text
    assert "lu dans les documents du prêt" in page.text and f"{spec.end_after.strftime('%d/%m/%Y')}" in page.text

    library = client.get("/documents")
    assert "Offre de prêt" in library.text and "Tableau d&#39;amortissement" in library.text or "Tableau d'amortissement" in library.text
    assert "Remboursement anticipé" in library.text

    home = client.get("/")
    assert home.status_code == 200 and "Prêt Fiximmo" in home.text


def test_web_first_due_form(client):
    csrf = login(client)
    review = upload(client, csrf, "offre.pdf", offer_pdf(LoanSpec()))
    doc = re.search(r"/documents/(\d+)", str(review.url)).group(1)
    done = client.post(f"/documents/{doc}/import/0", data={"csrf": csrf, "account_id": "new", "account_type": "mortgage"})
    account_id = re.search(r"/accounts/(\d+)", str(done.url)).group(1)
    bad = client.post(f"/accounts/{account_id}/loan/first-due", data={"csrf": csrf, "first_due": "nope"})
    assert "Date invalide" in bad.text
    ok = client.post(f"/accounts/{account_id}/loan/first-due", data={"csrf": csrf, "first_due": "2025-03-15"})
    assert "Date de la première échéance enregistrée" in ok.text and "15/03/2025" in ok.text
    assert client.post("/accounts/999/loan/first-due", data={"csrf": csrf, "first_due": "2025-03-15"}).status_code == 404
