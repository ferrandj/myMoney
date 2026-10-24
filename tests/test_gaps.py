"""Missing periods between statements: found, shown, uploaded later or marked as normal."""

import calendar
import re
from datetime import date
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from mymoney.core import MyMoney
from mymoney.models import Account, GapAck, Statement
from mymoney.services import coverage
from mymoney.services import statements as statement_service
from mymoney.web.app import create_app
from mymoney.adapters.base import ParsedStatement

from .conftest import PASSWORD
from .fixtures import Entry, Spec, boursobank_statement_pdf

D = Decimal


@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    return app


def month(year: int, m: int, opening: str | None = None, **kw) -> Spec:
    """One monthly statement with a single operation, for the account every fixture statement belongs to.
    Each month opens where the previous one closed (1000 minus 10 per month), unless `opening` says otherwise."""
    opening = opening or f"{1000 - 10 * (m - 1)}.00"
    last = calendar.monthrange(year, m)[1]
    prev = date(year, m, 1).toordinal() - 1
    return Spec(
        [Entry(date(year, m, 5), [f"CARTE {5:02d}/{m:02d}/{year % 100} SHOP {m} CB*1234"], D("-10.00"))],
        opening=D(opening), opening_date=date.fromordinal(prev), period=(date(year, m, 1), date(year, m, last)), **kw,
    )


def load(core: MyMoney, spec: Spec, account_id: int | None = None) -> int:
    """Drop and import one statement; returns the account id."""
    data = boursobank_statement_pdf(spec)
    doc_id, _ = core.store_document(f"{spec.period[0]}.pdf", data)
    core.analyze_document(doc_id, None, data)
    form = {"account_id": str(account_id) if account_id else "new", "name": "Compte courant", "import_transactions": "on"}
    return core.import_statement(doc_id, 0, form)["account_id"]


def load_months(core: MyMoney, year: int, months: list[int], account_id: int | None = None) -> int:
    for m in months:
        account_id = load(core, month(year, m), account_id)
    return account_id


def gaps(core: MyMoney, **kw):
    with core.db.session() as s:
        return coverage.all_gaps(s, **kw)


# ------------------------------------------------------------------------------------------ detection
def test_a_missing_month_between_two_runs_of_statements_is_found(core):
    load_months(core, 2026, [1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12])  # January to April, then June to December
    [gap] = gaps(core)
    assert (gap.start, gap.end, gap.days) == (date(2026, 5, 1), date(2026, 5, 31), 31)
    assert gap.label == "mai 2026" and gap.account_name == "Compte courant" and not gap.acknowledged


def test_contiguous_overlapping_and_single_statements_leave_no_gap(core):
    load_months(core, 2026, [1, 2, 3])
    assert gaps(core) == []
    overlap = Spec([Entry(date(2026, 3, 20), ["CARTE 19/03/26 CAFE CB*1234"], D("-3.00"))], period=(date(2026, 3, 15), date(2026, 4, 15)), opening_date=date(2026, 3, 14))
    load(core, overlap, 1)
    assert gaps(core) == []  # a statement overlapping the others covers days, it does not open a hole
    assert coverage.gaps_between([(date(2026, 1, 1), date(2026, 1, 31))]) == []
    assert coverage.gaps_between([]) == []


def test_gaps_are_labelled_by_months_or_by_dates():
    assert coverage.describe(date(2026, 5, 1), date(2026, 5, 31)) == "mai 2026"
    assert coverage.describe(date(2026, 6, 1), date(2026, 8, 31)) == "juin → août 2026"
    assert coverage.describe(date(2025, 12, 1), date(2026, 2, 28)) == "décembre 2025 → février 2026"
    assert coverage.describe(date(2026, 2, 1), date(2026, 2, 28)) == "février 2026"
    assert coverage.describe(date(2026, 5, 5), date(2026, 5, 28)) == "du 05/05/2026 au 28/05/2026"
    assert coverage.gaps_between([(date(2026, 1, 1), date(2026, 1, 31)), (date(2026, 3, 1), date(2026, 3, 31)), (date(2026, 6, 1), date(2026, 6, 30))]) == [
        (date(2026, 2, 1), date(2026, 2, 28)), (date(2026, 4, 1), date(2026, 5, 31))]


def test_uploading_the_missing_statement_closes_the_gap(core):
    account = load_months(core, 2026, [1, 2, 3, 4, 6, 7])
    assert len(gaps(core)) == 1
    load(core, month(2026, 5), account)
    assert gaps(core) == []


def test_a_partial_statement_splits_the_gap_without_hiding_what_is_left(core):
    account = load_months(core, 2026, [1, 2, 3, 4, 6])
    mid_may = Spec([Entry(date(2026, 5, 12), ["CARTE 11/05/26 CAFE CB*1234"], D("-3.00"))], period=(date(2026, 5, 10), date(2026, 5, 20)), opening_date=date(2026, 5, 9))
    load(core, mid_may, account)
    assert [(g.start, g.end) for g in gaps(core)] == [(date(2026, 5, 1), date(2026, 5, 9)), (date(2026, 5, 21), date(2026, 5, 31))]


def test_archived_accounts_are_not_reported_and_accounts_are_independent(core):
    first = load_months(core, 2026, [1, 3])
    other = load(core, Spec(month(2026, 1).entries, bank="12345", branch="80999", account="00077777777"))  # another account, no hole
    assert [g.account_id for g in gaps(core)] == [first] and other != first
    with core.db.session() as s:
        s.get(Account, first).archived = True
    assert gaps(core) == []


# ---------------------------------------------------------------------------------- "c'est normal"
def test_marking_a_gap_as_normal_silences_it_until_it_is_restored(core):
    load_months(core, 2026, [1, 2, 3, 4, 6, 7])
    [gap] = gaps(core)
    with core.db.session() as s:
        coverage.acknowledge(s, gap.account_id, gap.start, gap.end, " pas de mouvement ")
    assert gaps(core) == []
    [shown] = gaps(core, include_acknowledged=True)
    assert shown.acknowledged
    with core.db.session() as s:
        assert s.scalars(select(GapAck)).one().note == "pas de mouvement"
        coverage.restore(s, shown.ack_id)
    assert len(gaps(core)) == 1  # reported again


def test_a_normal_mark_covers_the_pieces_but_not_a_different_hole(core):
    account = load_months(core, 2026, [1, 2, 3, 4, 6, 7, 9])  # holes: May and August
    [may, august] = gaps(core)
    with core.db.session() as s:
        coverage.acknowledge(s, account, may.start, may.end)
    assert [g.label for g in gaps(core)] == ["août 2026"]  # only the hole that was marked is quiet

    mid_may = Spec([Entry(date(2026, 5, 12), ["CARTE 11/05/26 CAFE CB*1234"], D("-3.00"))], period=(date(2026, 5, 10), date(2026, 5, 20)), opening_date=date(2026, 5, 9))
    load(core, mid_may, account)
    assert [g.label for g in gaps(core)] == ["août 2026"]  # May is now two smaller holes, still inside what was marked


def test_only_an_existing_gap_can_be_marked(core):
    account = load_months(core, 2026, [1, 2, 3])
    with core.db.session() as s:
        with pytest.raises(ValueError, match="plus un trou"):
            coverage.acknowledge(s, account, date(2026, 5, 1), date(2026, 5, 31))
        with pytest.raises(ValueError):
            coverage.acknowledge(s, 999, date(2026, 5, 1), date(2026, 5, 31))
        with pytest.raises(ValueError):
            coverage.acknowledge(s, account, date(2026, 5, 31), date(2026, 5, 1))
    with core.db.session() as s:
        assert s.scalars(select(GapAck)).all() == []


def test_the_import_check_does_not_repeat_a_hole_marked_as_normal():
    st = ParsedStatement(source_format="pdf", period_start=date(2026, 6, 1), period_end=date(2026, 6, 30), opening_balance=D("900"), balance=D("890"))
    april = Statement(account_id=1, period_start=date(2026, 4, 1), period_end=date(2026, 4, 30), opening_balance=D("1000"), closing_balance=D("900"))
    issues = statement_service._continuity(st, [april], [])
    assert len(issues) == 1 and "Il manque une période" in issues[0] and "01/05/2026" in issues[0] and "31/05/2026" in issues[0]
    assert statement_service._continuity(st, [april], [(date(2026, 5, 1), date(2026, 5, 31))]) == []
    assert statement_service._continuity(st, [april], [(date(2026, 5, 10), date(2026, 5, 31))]) != []  # a smaller mark does not cover it


def test_balances_that_do_not_chain_are_still_reported_on_the_account(core):
    account = load_months(core, 2026, [1, 2])
    broken = month(2026, 3, opening="5.00")  # March does not start where February ended
    load(core, broken, account)
    with core.db.session() as s:
        _, chain = statement_service.coverage(s, account)
    assert len(chain) == 1 and "ne se suivent pas" in chain[0]


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


def test_web_dashboard_flags_the_missing_month_and_offers_both_actions(client):
    csrf = login(client)
    account = load_months(client.core, 2026, [1, 2, 3, 4, 6, 7])
    home = client.get("/")
    assert "Relevés manquants" in home.text and "mai 2026" in home.text and "31 jours sans relevé" in home.text
    assert f"/documents?account_id={account}&start=2026-05-01&end=2026-05-31" in home.text
    assert "Déposer le relevé manquant" in home.text and "C'est normal" in home.text

    hint = client.get(f"/documents?account_id={account}&start=2026-05-01&end=2026-05-31")
    assert hint.status_code == 200 and "Tu cherches le relevé" in hint.text and "mai 2026" in hint.text and "Compte courant" in hint.text
    assert client.get("/documents?account_id=999&start=2026-05-01&end=2026-05-31").status_code == 200
    assert client.get("/documents?account_id=1&start=nope&end=nope").status_code == 200

    done = client.post("/gaps/acknowledge", data={"csrf": csrf, "account_id": account, "start": "2026-05-01", "end": "2026-05-31", "next": "/"})
    assert "plus signalé" in done.text and "Relevés manquants" not in done.text

    page = client.get(f"/accounts/{account}")
    assert "mai 2026 : marqué comme normal" in page.text and "Remettre en alerte" in page.text
    ack_id = int(re.search(r"/gaps/(\d+)/restore", page.text).group(1))
    back = client.post(f"/gaps/{ack_id}/restore", data={"csrf": csrf, "next": "/"})
    assert "Relevés manquants" in back.text and "mai 2026" in back.text


def test_web_uploading_the_missing_statement_makes_the_card_disappear(client):
    csrf = login(client)
    account = load_months(client.core, 2026, [1, 2, 3, 4, 6])
    assert "Relevés manquants" in client.get("/").text
    r = client.post("/documents", data={"csrf": csrf}, files={"files": ("mai.pdf", boursobank_statement_pdf(month(2026, 5)), "application/pdf")})
    assert r.status_code == 200
    doc_id = int(re.search(r"/documents/(\d+)/import/0", r.text).group(1)) if "/import/0" in r.text else None
    if doc_id:  # reviewed because of the gap around it: confirm
        client.post(f"/documents/{doc_id}/import/0", data={"csrf": csrf, "account_id": str(account), "import_transactions": "on"})
    assert "Relevés manquants" not in client.get("/").text


def test_web_stale_or_forged_gap_forms_change_nothing(client):
    csrf = login(client)
    account = load_months(client.core, 2026, [1, 2, 3])
    r = client.post("/gaps/acknowledge", data={"csrf": csrf, "account_id": account, "start": "2026-05-01", "end": "2026-05-31", "next": "//evil.example"})
    assert r.status_code == 200 and "evil.example" not in str(r.url) and "plus un trou" in r.text
    assert client.post("/gaps/acknowledge", data={"csrf": csrf, "account_id": account, "start": "x", "end": "y"}).status_code == 200
    assert client.post("/gaps/acknowledge", data={"account_id": account, "start": "2026-05-01", "end": "2026-05-31"}).status_code == 403  # no CSRF token
    with client.core.db.session() as s:
        assert s.scalars(select(GapAck)).all() == []
