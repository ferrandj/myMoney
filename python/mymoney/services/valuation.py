"""Value every account in the base currency (EUR)."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from decimal import Decimal

from ..models import Account, Holding
from .prices import PriceError, PriceService

log = logging.getLogger(__name__)
BASE = "EUR"


@dataclass
class HoldingValue:
    holding: Holding
    unit_price_eur: Decimal | None
    value_eur: Decimal | None


@dataclass
class AccountValue:
    account: Account
    cash_eur: Decimal
    holdings: list[HoldingValue] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    @property
    def total(self) -> Decimal:
        total = self.cash_eur + sum((h.value_eur or Decimal(0)) for h in self.holdings)
        if self.account.category == "dettes":
            return -abs(total)
        return total


class Valuator:
    def __init__(self, prices: PriceService):
        self.prices = prices
        self._crypto: dict[str, Decimal] = {}

    def prefetch(self, accounts: list[Account]) -> None:
        """Fetch all crypto prices in as few calls as possible (the free CoinGecko tier is rate-limited)."""
        symbols: set[str] = set()
        ids: dict[str, str] = {}
        for account in accounts:
            for h in account.holdings:
                if h.asset_class != "crypto" or h.value is not None:
                    continue
                extra = h.extra or {}
                if extra.get("coingecko_id") and h.symbol:
                    ids[h.symbol.upper()] = extra["coingecko_id"]
                if h.symbol:
                    symbols.add(h.symbol.upper())
            if account.currency.upper() not in ("EUR", "USD") and len(account.currency) > 3:
                symbols.add(account.currency.upper())
        try:
            self._crypto = self.prices.crypto_prices(symbols, BASE, ids) if symbols else {}
        except Exception as exc:
            log.warning("Prix crypto indisponibles : %s", exc)

    def _convert(self, amount: Decimal, currency: str | None) -> Decimal | None:
        try:
            return self.prices.convert(amount, currency or BASE, BASE)
        except (PriceError, KeyError):
            return None

    def holding_value(self, h: Holding) -> HoldingValue:
        qty = h.quantity or Decimal(0)
        if h.value is not None:
            value = self._convert(h.value, h.value_currency)
            return HoldingValue(h, (value / qty) if value is not None and qty else None, value)
        extra = h.extra or {}
        if h.asset_class == "crypto":
            price = None
            if h.symbol:
                price = self._crypto.get(h.symbol.upper())
            if price is None and h.symbol and h.symbol.upper() not in self._crypto:
                price = self.prices.crypto_prices([h.symbol], BASE).get(h.symbol.upper())
            if price is not None:
                return HoldingValue(h, price, price * qty)
        elif h.isin or extra.get("yahoo"):
            quote = self.prices.security_price(isin=h.isin, symbol=extra.get("yahoo"))
            if quote:
                price = self._convert(quote[0], quote[1])
                if price is not None:
                    return HoldingValue(h, price, price * qty)
        if h.unit_price is not None:
            price = self._convert(h.unit_price, h.value_currency)
            if price is not None:
                return HoldingValue(h, price, price * qty)
        return HoldingValue(h, None, None)

    def account_value(self, account: Account) -> AccountValue:
        warnings = []
        cash = self._convert(account.balance or Decimal(0), account.currency)
        if cash is None:
            warnings.append(f"Taux de change {account.currency} indisponible")
            cash = Decimal(0)
        holdings = [self.holding_value(h) for h in account.holdings]
        missing = [h.holding.name or h.holding.symbol or "?" for h in holdings if h.value_eur is None]
        if missing:
            warnings.append("Prix introuvable : " + ", ".join(missing[:5]))
        return AccountValue(account, cash, holdings, warnings)
