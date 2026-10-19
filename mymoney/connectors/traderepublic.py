"""Trade Republic through pytr (unofficial open-source client of the web app).

Login is confirmed in the Trade Republic mobile app (or with an authenticator code).
The web session cookie is then kept encrypted and reused until Trade Republic expires it.
"""

from __future__ import annotations

import asyncio
import os
import tempfile
from contextlib import contextmanager
from decimal import Decimal

from . import live
from .base import AccountData, Connector, Field, HoldingData, NeedsAuth, Prompt, dec


@contextmanager
def _cookie_file(content: str | None):
    """Temporary cookie jar for pytr, wiped right after use (/tmp is RAM-only in the container)."""
    fd, path = tempfile.mkstemp(prefix="tr-", suffix=".txt")
    try:
        with os.fdopen(fd, "w") as fh:
            fh.write(content or "# Netscape HTTP Cookie File\n")
        yield path
    finally:
        try:
            os.remove(path)
        except FileNotFoundError:
            pass


class TradeRepublicConnector(Connector):
    type = "traderepublic"
    label = "Trade Republic"
    category = "bourse"
    description = "Portefeuille (actions, ETF, crypto) et espèces. Connexion validée dans l'app Trade Republic."
    auth_flow = "interactive"
    requires = "pytr"
    fields = [
        Field("phone", "Numéro de téléphone", help="Format international, ex. +33612345678"),
        Field("pin", "Code PIN", kind="password"),
    ]

    def _api(self, cookies_path: str):
        from pytr.api import TradeRepublicApi

        return TradeRepublicApi(
            phone_no=self.config["phone"].strip(),
            pin=self.config["pin"].strip(),
            locale="fr",
            save_cookies=True,
            cookies_file=cookies_path,
            use_v2_login=True,
        )

    @property
    def authorized(self) -> bool:
        return bool(self.state.get("cookies"))

    def login(self, answers: dict[str, str] | None) -> Prompt | None:
        pending = live.get(self.ctx.connection_id)
        if pending is None or answers is None:
            path_ctx = _cookie_file(None)
            path = path_ctx.__enter__()
            api = self._api(path)
            api.initiate_weblogin()
            live.put(self.ctx.connection_id, (api, path_ctx, path))
            if api.weblogin_needs_authenticator:
                return Prompt("Saisis le code de ton application d'authentification.", [Field("code", "Code")])
            return Prompt("Valide la connexion dans l'app Trade Republic sur ton téléphone, puis clique sur Continuer.")
        api, path_ctx, path = pending
        try:
            api.complete_weblogin((answers or {}).get("code") or None)
            with open(path) as fh:
                self.state["cookies"] = fh.read()
        finally:
            live.drop(self.ctx.connection_id)
            path_ctx.__exit__(None, None, None)
        return None

    def sync(self) -> list[AccountData]:
        if not self.authorized:
            raise NeedsAuth("Connexion à Trade Republic requise")
        from pytr.portfolio import Portfolio

        with _cookie_file(self.state["cookies"]) as path:
            api = self._api(path)
            if not api.resume_websession():
                self.state.pop("cookies", None)
                raise NeedsAuth("Session Trade Republic expirée : reconnecte-toi")
            portfolio = Portfolio(api)
            asyncio.run(portfolio.portfolio_loop())
            api.save_websession()
            with open(path) as fh:
                self.state["cookies"] = fh.read()

        holdings = []
        for pos in portfolio.positions:
            isin = pos["instrumentId"]
            quantity = dec(pos.get("netSize"))
            if quantity <= 0:
                continue
            price = dec(pos.get("price"))
            holdings.append(
                HoldingData(
                    symbol=None,
                    isin=isin,
                    name=pos.get("name") or isin,
                    quantity=quantity,
                    asset_class="crypto" if isin.startswith("XF000") else "stock",
                    unit_price=price,
                    value=quantity * price,
                    value_currency="EUR",
                    cost_basis=quantity * dec(pos.get("averageBuyIn")) if pos.get("averageBuyIn") else None,
                )
            )
        cash = portfolio.cash[0] if getattr(portfolio, "cash", None) else {"amount": 0, "currencyId": "EUR"}
        return [
            AccountData(
                external_id="cash",
                name="Trade Republic – Espèces",
                account_type="savings",
                currency=cash.get("currencyId", "EUR"),
                balance=dec(cash.get("amount")),
                institution="Trade Republic",
            ),
            AccountData(
                external_id="portfolio",
                name="Trade Republic – Portefeuille",
                account_type="cto",
                balance=Decimal(0),
                institution="Trade Republic",
                holdings=holdings,
            ),
        ]
