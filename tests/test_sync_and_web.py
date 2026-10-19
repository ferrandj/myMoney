import re
from datetime import date
from decimal import Decimal

import pytest
from sqlalchemy import select
from fastapi.testclient import TestClient

from mymoney.connectors.base import AccountData, HoldingData, TransactionData
from mymoney.core import MyMoney
from mymoney.models import Account, Connection, Transaction
from mymoney.services import sync as sync_service
from mymoney.services.valuation import Valuator
from mymoney.web.app import create_app

from .conftest import PASSWORD


class FakePrices:
    def convert(self, amount, from_ccy, to_ccy="EUR"):
        return amount * {"EUR": Decimal(1), "USD": Decimal("0.9")}[from_ccy.upper()]

    def crypto_prices(self, symbols, currency="EUR", ids=None):
        return {s.upper(): {"BTC": Decimal(50000), "ETH": Decimal(2000)}[s.upper()] for s in symbols if s.upper() in ("BTC", "ETH")}

    def token_prices(self, chain, contracts, currency="EUR"):
        return {}

    def security_price(self, isin=None, symbol=None):
        return (Decimal(100), "EUR") if isin == "IE00B4L5Y983" else None


def test_sync_dedupes_and_keeps_user_edits(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    with app.db.session() as s:
        conn = Connection(name="Banque", connector_type="enablebanking")
        s.add(conn)
        s.flush()
        data = [AccountData("A1", "COMPTE", "checking", balance=Decimal(10), transactions=[
            TransactionData(date(2026, 2, 1), "CB", Decimal(-5)), TransactionData(date(2026, 2, 1), "CB", Decimal(-5)),
        ])]
        assert sync_service.apply_accounts(s, conn, data)["transactions"] == 2
        account = s.scalar(select(Account))
    app.save_account(account.id, {"name": "Mon compte", "account_type": "checking", "currency": "EUR"})
    with app.db.session() as s:
        conn = s.get(Connection, 1)
        data[0].balance = Decimal(20)
        assert sync_service.apply_accounts(s, conn, data)["transactions"] == 0
        acc = s.get(Account, account.id)
        assert acc.name == "Mon compte" and acc.balance == Decimal(20)


def test_import_does_not_duplicate_synced_lines(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    with app.db.session() as s:
        acc = Account(name="CC", account_type="checking")
        s.add(acc)
        s.flush()
        sync_service.add_transactions(s, acc, [TransactionData(date(2026, 2, 1), "CARTE X", Decimal("-5"), external_id="bank-1")], "sync")
        added = sync_service.add_transactions(s, acc, [
            TransactionData(date(2026, 2, 1), "CB CARTE X 01/02", Decimal("-5.00")), TransactionData(date(2026, 2, 2), "AUTRE", Decimal("-1")),
        ], "import")
        assert added == 1


def test_valuation():
    acc = Account(name="Mix", account_type="crypto_exchange", currency="EUR", balance=Decimal(0))
    acc.holdings = []
    from mymoney.models import Holding

    acc.holdings.append(Holding(symbol="BTC", quantity=Decimal("0.1"), asset_class="crypto"))
    acc.holdings.append(Holding(symbol="USDC", quantity=Decimal(10), asset_class="crypto", value=Decimal(10), value_currency="USD"))
    acc.holdings.append(Holding(isin="IE00B4L5Y983", quantity=Decimal(3), asset_class="etf"))
    v = Valuator(FakePrices())
    v.prefetch([acc])
    assert v.account_value(acc).total == Decimal(5000) + Decimal(9) + Decimal(300)
    loan = Account(name="Prêt", account_type="mortgage", currency="EUR", balance=Decimal(1000))
    loan.holdings = []
    assert v.account_value(loan).total == Decimal(-1000)


@pytest.fixture
def client(settings):
    app = create_app(settings)
    with TestClient(app) as c:
        c.app_state = app.state
        yield c


def _csrf(html: str) -> str:
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def test_web_setup_requires_token_and_sets_secure_headers(client):
    r = client.get("/", follow_redirects=False)
    assert r.headers["location"] == "/setup"
    r = client.post("/setup", data={"setup_token": "nope", "password": PASSWORD, "confirm": PASSWORD})
    assert r.status_code == 403
    r = client.post("/setup", data={"setup_token": client.app_state.core.setup_token, "password": PASSWORD, "confirm": PASSWORD})
    assert r.status_code == 200 and "code de secours" in r.text.lower()
    cookie = r.headers["set-cookie"].lower()
    assert "httponly" in cookie and "samesite=strict" in cookie
    r = client.get("/")
    assert "default-src 'none'" in r.headers["content-security-policy"]
    assert r.headers["x-frame-options"] == "DENY" and r.headers["cache-control"] == "no-store"


def test_web_requires_login_and_csrf(client):
    core = client.app_state.core
    client.post("/setup", data={"setup_token": core.setup_token, "password": PASSWORD, "confirm": PASSWORD})
    page = client.get("/accounts/new")
    # Missing / wrong CSRF token
    assert client.post("/accounts/new", data={"name": "X", "account_type": "checking"}).status_code == 403
    assert client.post("/accounts/new", data={"csrf": "bad", "name": "X"}).status_code == 403
    # Cross-origin POST refused
    r = client.post("/accounts/new", data={"csrf": _csrf(page.text), "name": "X"}, headers={"origin": "https://evil.example"})
    assert r.status_code == 403
    r = client.post("/accounts/new", data={"csrf": _csrf(page.text), "name": "Livret", "account_type": "livret_a", "balance": "100"})
    assert r.status_code == 200 and "1,70" in r.text

    # Locking wipes the key: every page goes back to the login screen.
    client.post("/logout", data={"csrf": _csrf(r.text), "lock": "1"})
    assert not core.vault.unlocked
    assert client.get("/", follow_redirects=False).headers["location"] == "/login"
    assert client.post("/login", data={"password": "Wrong-Password-1!"}).status_code == 401
    assert client.post("/login", data={"password": PASSWORD}).status_code == 200
    assert core.vault.unlocked


def test_web_document_import_creates_account(client):
    core = client.app_state.core
    client.post("/setup", data={"setup_token": core.setup_token, "password": PASSWORD, "confirm": PASSWORD})
    csrf = _csrf(client.get("/documents").text)
    csv = "Date;Libellé;Montant;Solde\n01/02/2026;INTERETS;12,00;5 012,00\n".encode()
    r = client.post("/documents", data={"csrf": csrf}, files={"files": ("LIVRET A Fortuneo.csv", csv, "text/csv")})
    assert r.status_code == 200 and "Valider l'import" in r.text
    doc_id = int(re.search(r"/documents/(\d+)/import/0", r.text).group(1))
    r = client.post(f"/documents/{doc_id}/import/0", data={
        "csrf": csrf, "account_id": "new", "name": "Livret A", "institution": "Fortuneo", "account_type": "livret_a",
        "update_balance": "on", "balance": "5012.00", "import_transactions": "on",
    })
    assert r.status_code == 200
    with core.db.session() as s:
        acc = s.scalar(select(Account).where(Account.name == "Livret A"))
        assert acc.balance == Decimal("5012.00")
        assert s.query(Transaction).count() == 1
    assert client.get(f"/documents/{doc_id}/download").content == csv
