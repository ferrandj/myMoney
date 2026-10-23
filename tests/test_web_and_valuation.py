import re
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient

from mymoney.models import Account
from mymoney.services.valuation import Valuator
from mymoney.web.app import create_app

from .conftest import PASSWORD


class FakePrices:
    def convert(self, amount, from_ccy, to_ccy="EUR"):
        return amount * {"EUR": Decimal(1), "USD": Decimal("0.9")}[from_ccy.upper()]

    def crypto_prices(self, symbols, currency="EUR", ids=None):
        return {s.upper(): {"BTC": Decimal(50000), "ETH": Decimal(2000)}[s.upper()] for s in symbols if s.upper() in ("BTC", "ETH")}

    def security_price(self, isin=None, symbol=None):
        return (Decimal(100), "EUR") if isin == "IE00B4L5Y983" else None


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
