import re
from datetime import date, datetime
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient

from mymoney.core import MyMoney
from mymoney.models import Account, Counterparty, Snapshot, Transaction
from mymoney.services import analytics
from mymoney.web import charts
from mymoney.web.app import create_app

from .conftest import ADMIN_EMAIL, PASSWORD
from .fixtures import Spec, boursobank_statement_pdf, sample_entries

D = Decimal
TODAY = date(2026, 10, 15)


@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    return app


def add_account(s, name, balance, balance_day, account_type="checking", txs=()):
    a = Account(name=name, account_type=account_type, balance=D(balance), balance_date=datetime.combine(balance_day, datetime.min.time()))
    s.add(a)
    s.flush()
    for i, (day, amount, extra) in enumerate(txs):
        s.add(Transaction(account_id=a.id, fingerprint=f"{a.id}-{i}", date=day, label=extra.pop("label", "op"), amount=D(amount), **extra))
    s.flush()
    return a


def test_history_is_rebuilt_from_the_balance_and_the_operations(core):
    with core.db.session() as s:
        a = add_account(s, "CC", "1000", date(2026, 9, 30), txs=[
            (date(2026, 9, 10), "-100", {}), (date(2026, 9, 20), "50", {}), (date(2026, 9, 30), "-10", {}),
        ])
        history = analytics.account_history(s, a, D("1000"), TODAY)
    assert history[date(2026, 9, 30)] == D("1000")  # end of the anchor day: includes that day's operation
    assert history[date(2026, 9, 29)] == D("1010") and history[date(2026, 9, 20)] == D("1010")
    assert history[date(2026, 9, 19)] == D("960") and history[date(2026, 9, 10)] == D("960")
    assert history[date(2026, 9, 9)] == D("1060")  # balance before the first operation
    assert min(history) == date(2026, 9, 9) and history[TODAY] == D("1000")  # carried to today


def test_operations_after_the_known_balance_move_it_forward(core):
    with core.db.session() as s:
        a = add_account(s, "CC", "500", date(2026, 9, 30), txs=[(date(2026, 9, 5), "-5", {}), (date(2026, 10, 3), "-20", {})])
        history = analytics.account_history(s, a, D("500"), TODAY)
    assert history[date(2026, 10, 2)] == D("500") and history[date(2026, 10, 3)] == D("480") and history[TODAY] == D("480")


def test_debts_and_foreign_accounts_keep_their_sign_and_rate(core):
    with core.db.session() as s:
        loan = add_account(s, "Prêt", "1000", date(2026, 9, 30), "mortgage", txs=[(date(2026, 9, 15), "-100", {})])
        history = analytics.account_history(s, loan, D("-1000"), TODAY)  # portfolio value of a debt is negative
    assert history[date(2026, 9, 30)] == D("-1000") and history[date(2026, 9, 14)] == D("-1100")


def test_accounts_without_operations_use_snapshots(core):
    with core.db.session() as s:
        a = add_account(s, "Livret", "100", date(2026, 10, 15), "livret_a")
        s.add_all([Snapshot(account_id=a.id, day=date(2026, 10, 1), value=D("90")), Snapshot(account_id=a.id, day=date(2026, 10, 10), value=D("95"))])
        s.flush()
        history = analytics.account_history(s, a, D("100"), TODAY)
    assert history[date(2026, 10, 1)] == D("90") and history[date(2026, 10, 9)] == D("90")
    assert history[date(2026, 10, 10)] == D("95") and history[TODAY] == D("100")


def test_series_total_per_account_and_per_category(core):
    with core.db.session() as s:
        cc = add_account(s, "CC", "1000", date(2026, 10, 15), txs=[(date(2026, 10, 1), "100", {})])
        liv = add_account(s, "Livret", "200", date(2026, 10, 15), "livret_a", txs=[(date(2026, 10, 1), "10", {})])
        cur = {cc.id: D("1000"), liv.id: D("200")}
        histories = {a.id: analytics.account_history(s, a, cur[a.id], TODAY) for a in (cc, liv)}
    accounts = {cc.id: cc, liv.id: liv}
    ids = [cc.id, liv.id]
    [total] = analytics.build_series(histories, accounts, ids, "total", date(2026, 9, 30), TODAY)
    assert dict(total.points)[date(2026, 10, 1)] == D("1200") and dict(total.points)[date(2026, 9, 30)] == D("1090")  # 900 + 190 before
    assert [s.label for s in analytics.build_series(histories, accounts, ids, "compte", date(2026, 9, 30), TODAY)] == ["CC", "Livret"]
    assert [s.key for s in analytics.build_series(histories, accounts, ids, "categorie", date(2026, 9, 30), TODAY)] == ["liquidites", "epargne"]
    only = analytics.build_series(histories, accounts, [liv.id], "total", date(2026, 9, 30), TODAY)[0]
    assert only.points[-1][1] == D("200")
    stats = analytics.stats(total.points)
    assert (stats.first, stats.last, stats.delta) == (D("1090"), D("1200"), D("110")) and round(stats.pct, 2) == D("10.09")


def test_period_start_and_sampling():
    assert analytics.period_start("1m", TODAY, date(2020, 1, 1)) == date(2026, 9, 15)
    assert analytics.period_start("1m", TODAY, date(2026, 10, 1)) == date(2026, 10, 1)  # never before the first data
    assert analytics.period_start("tout", TODAY, date(2020, 1, 1)) == date(2020, 1, 1)
    days = analytics._days(date(2016, 1, 1), TODAY)
    assert len(days) <= analytics.MAX_POINTS + 1 and days[-1] == TODAY


def test_flows_leave_out_transfers_between_own_accounts_unless_asked(core):
    with core.db.session() as s:
        a = add_account(s, "CC", "0", date(2026, 10, 15), txs=[
            (date(2026, 9, 5), "2000", {}), (date(2026, 9, 6), "-300", {}), (date(2026, 9, 7), "-500", {"details": {"internal": True}}),
            (date(2026, 10, 2), "-40", {}),
        ])
        flows = analytics.monthly_flows(s, [a.id], date(2026, 9, 1), TODAY, False)
        with_internal = analytics.monthly_flows(s, [a.id], date(2026, 9, 1), TODAY, True)
    assert [(f.month, f.income, f.expense, f.net) for f in flows] == [
        (date(2026, 9, 1), D("2000"), D("300"), D("1700")), (date(2026, 10, 1), D(0), D("40"), D("-40"))]
    assert with_internal[0].expense == D("800")


def test_spending_groups_by_kind_and_by_contact(core):
    with core.db.session() as s:
        cp = Counterparty(name="Pauline")
        s.add(cp)
        s.flush()
        a = add_account(s, "CC", "0", date(2026, 10, 15), txs=[
            (date(2026, 9, 5), "-30", {"kind": "transfer", "counterparty_name": "MARTIN Pauline", "counterparty_id": cp.id}),
            (date(2026, 9, 6), "-20", {"kind": "transfer", "counterparty_name": "Pauline MARTIN", "counterparty_id": cp.id}),
            (date(2026, 9, 7), "-15", {"kind": "card", "counterparty_name": "UBER"}), (date(2026, 9, 8), "-5", {"kind": "card", "counterparty_name": "UBER"}),
            (date(2026, 9, 9), "2400", {"kind": "transfer", "counterparty_name": "ACME"}),
        ])
        result = analytics.spending(s, [a.id], date(2026, 9, 1), TODAY, False)
    assert result.total_out == D("70") and result.total_in == D("2400")
    assert [(r.label, r.total) for r in result.by_kind] == [("Virements", D("50")), ("Cartes", D("20"))]
    assert [(r.label, r.total, r.count) for r in result.top_out] == [("Pauline", D("50"), 2), ("UBER", D("20"), 2)]
    assert result.top_out[0].contact_id == cp.id and result.top_in[0].label == "ACME"


def test_repartition_by_category_and_by_account(core):
    class V:
        def __init__(self, account, total):
            self.account, self.total = account, D(total)

    cc, liv, loan = Account(id=1, name="CC", account_type="checking"), Account(id=2, name="Livret", account_type="livret_a"), Account(id=3, name="Prêt", account_type="loan")
    values = [V(cc, 100), V(liv, 300), V(Account(id=4, name="Livret 2", account_type="ldds"), 100), V(loan, -50)]
    assert [(k, t) for k, _, t in analytics.repartition(values, "categorie")] == [("epargne", D("400")), ("liquidites", D("100"))]
    assert [label for _, label, _ in analytics.repartition(values, "compte")] == ["Livret", "CC", "Livret 2"]


def test_charts_are_plain_svg_with_tooltips():
    series = [analytics.Series("a", "Compte <A>", [(date(2026, 9, d), D(1000 + d * 10)) for d in range(1, 29)])]
    svg = charts.line_chart(series)
    assert svg.startswith("<svg") and "<polyline" in svg and "<polygon" in svg and "Compte &lt;A&gt;" in svg and "<script" not in svg
    assert 'class="line s0"' in svg and "style=" not in svg  # colours come from classes: the CSP forbids inline styles
    assert 'class="pop"' in svg and "01/09/2026" in svg and "<title" not in svg  # an instant tooltip, not the delayed native one
    assert charts.line_chart([]) == "" and charts.line_chart([analytics.Series("a", "A", [(date(2026, 9, 1), D(1))])]) == ""
    flows = [analytics.MonthFlow(date(2026, 8, 1), D(100), D(40)), analytics.MonthFlow(date(2026, 9, 1), D(0), D(0))]
    assert charts.flow_chart(flows).count("bar-") == 4 and charts.flow_chart([]) == ""
    ring = charts.donut([("a", "Épargne", D(300)), ("b", "Liquidités", D(100))])
    assert "75.000 25.000" in ring and "Épargne" in ring and charts.donut([]) == ""
    assert charts.euro(D("-1234.5"), 2) == "−1 234,50 €"
    # The last tick always reaches the highest value (a chart whose axis stops early would clip its bars).
    for low, high in ((0, 2460), (0, 1), (-2000, 1700), (1000, 1003), (5, 5), (0, 99_999)):
        ticks = charts._nice_ticks(low, high)
        assert ticks[0] <= low and ticks[-1] >= high, (low, high, ticks)
    dot = charts.line_chart([analytics.Series("a", "A", [(date(2026, 9, 1), D(1000)), (date(2026, 9, 20), D(1100))]), analytics.Series("b", "Livret", [(date(2026, 9, 20), D(500))])])
    assert "<circle" in dot and 'class="dot s1"' in dot


# ------------------------------------------------------------------------------------------------- web
@pytest.fixture
def client(settings):
    app = create_app(settings)
    with TestClient(app) as c:
        c.core = app.state.core
        yield c


def csrf_of(html):
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def test_web_views_filters_and_totals(client):
    client.post("/setup", data={"setup_token": client.core.setup_token, "email": ADMIN_EMAIL, "password": PASSWORD, "confirm": PASSWORD})
    csrf = csrf_of(client.get("/documents").text)
    spec = Spec(sample_entries())
    page = client.post("/documents", data={"csrf": csrf}, files={"files": ("r.pdf", boursobank_statement_pdf(spec), "application/pdf")})
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))
    client.post(f"/documents/{doc_id}/import/0", data={
        "csrf": csrf, "account_id": "new", "name": "Compte courant", "update_balance": "on", "balance": str(spec.closing), "import_transactions": "on"})
    client.post("/accounts/new", data={"csrf": csrf, "name": "Livret A", "account_type": "livret_a", "balance": "5000"})

    evolution = client.get("/analytics?periode=tout")
    assert evolution.status_code == 200 and "<svg" in evolution.text and "Patrimoine net" in evolution.text
    assert "Total de la sélection" in evolution.text and "Compte courant" in evolution.text and "Livret A" in evolution.text
    per_account = client.get("/analytics?vue=evolution&par=compte&periode=tout")
    assert per_account.text.count('class="line s') >= 1 and "legend-label" in per_account.text
    assert "Tous les comptes" in evolution.text

    only_savings = client.get("/analytics?vue=repartition&par=compte&c=2")
    assert "1 compte(s) sélectionné(s)" in only_savings.text and "donut-svg" in only_savings.text
    assert "5 000,00 €" in only_savings.text.replace("\u202f", " ").replace("\xa0", " ")  # thousands separator is a non-breaking space
    assert only_savings.text.count("Compte courant") == 1  # only in the list of accounts to tick, not in the figures

    flux = client.get("/analytics?vue=flux&periode=tout")
    assert flux.status_code == 200 and "bar-pos" in flux.text and "09/2026" in flux.text and "Total" in flux.text
    with_internal = client.get("/analytics?vue=flux&periode=tout&interne=1")
    assert with_internal.status_code == 200 and "checked" in with_internal.text

    depenses = client.get("/analytics?vue=depenses&periode=tout")
    assert depenses.status_code == 200 and "par catégorie" in depenses.text and "Courses" in depenses.text and "Impôts et taxes" in depenses.text

    # Nonsense parameters fall back to defaults instead of failing.
    weird = client.get("/analytics?vue=nope&periode=zzz&par=zzz&c=999")
    assert weird.status_code == 200 and "Analyses" in weird.text
    assert 'href="/analytics"' in client.get("/").text


def test_web_analytics_needs_login(client):
    assert client.get("/analytics", follow_redirects=False).status_code == 303
