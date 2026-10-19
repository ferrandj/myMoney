"""Crypto exchanges through ccxt (free, open source): Binance, Kraken, Coinbase, Bitstamp...

Always create READ-ONLY API keys (no trading, no withdrawal), ideally restricted to your NAS IP.
"""

from __future__ import annotations

import re
from decimal import Decimal

from .base import AccountData, Connector, Field, HoldingData, dec

POPULAR = ["binance", "kraken", "coinbase", "bitstamp", "bitpanda", "cryptocom", "kucoin", "bybit", "okx", "bitget", "gateio", "bitfinex", "gemini", "mexc", "htx"]
NAMES = {"cryptocom": "Crypto.com", "gateio": "Gate.io", "okx": "OKX", "htx": "HTX"}


def _exchange_options() -> list[tuple[str, str]]:
    try:
        import ccxt

        available = [e for e in POPULAR if e in ccxt.exchanges]
        others = [e for e in ccxt.exchanges if e not in POPULAR]
    except ImportError:
        available, others = POPULAR, []
    return [(e, NAMES.get(e, e.title())) for e in available + others]


# Kraken suffixes for staked / earning balances: ETH.F, DOT.S, ADA.B, USDC.M...
_SUFFIX = re.compile(r"\.(S|F|B|M|P)$")


class ExchangeConnector(Connector):
    type = "exchange"
    label = "Plateforme crypto (API lecture seule)"
    category = "crypto"
    description = "Binance, Kraken, Coinbase, Bitstamp, Crypto.com, KuCoin, Bybit, OKX… via la bibliothèque libre ccxt."
    requires = "ccxt"
    fields = [
        Field("exchange", "Plateforme", kind="select", options=_exchange_options()),
        Field("api_key", "Clé API", kind="password", help="Crée une clé en LECTURE SEULE uniquement"),
        Field("secret", "Secret API", kind="password"),
        Field("password", "Passphrase (OKX, KuCoin, Bitget…)", kind="password", required=False),
    ]

    def _client(self):
        import ccxt

        params = {"apiKey": self.config["api_key"].strip(), "secret": self.config["secret"].strip(), "enableRateLimit": True}
        if self.config.get("password"):
            params["password"] = self.config["password"]
        return getattr(ccxt, self.config["exchange"])(params)

    def sync(self) -> list[AccountData]:
        exchange_id = self.config["exchange"]
        client = self._client()
        totals: dict[str, Decimal] = {}

        def add(symbol: str, amount) -> None:
            symbol = _SUFFIX.sub("", str(symbol).upper())
            if symbol.startswith("LD") and exchange_id == "binance" and len(symbol) > 3:
                symbol = symbol[2:]  # legacy Binance "LDxxx" = flexible savings
            value = dec(amount)
            if value:
                totals[symbol] = totals.get(symbol, Decimal(0)) + value

        for symbol, amount in (client.fetch_balance().get("total") or {}).items():
            add(symbol, amount)
        if exchange_id == "binance":
            self._binance_extras(client, add)

        name = NAMES.get(exchange_id, exchange_id.title())
        return [
            AccountData(
                external_id=exchange_id,
                name=name,
                account_type="crypto_exchange",
                institution=name,
                holdings=[HoldingData(symbol=s, quantity=q, asset_class="crypto") for s, q in sorted(totals.items()) if q > 0],
            )
        ]

    @staticmethod
    def _binance_extras(client, add) -> None:
        """Spot balances miss Earn and Funding wallets: add them when the key allows it."""
        for method, key in (
            ("sapi_get_simple_earn_flexible_position", "totalAmount"),
            ("sapi_get_simple_earn_locked_position", "amount"),
        ):
            try:
                rows = getattr(client, method)({"size": 100}).get("rows", [])
            except Exception:
                continue
            for row in rows:
                add(row["asset"], row.get(key))
        try:
            for row in client.sapi_post_asset_get_funding_asset({}):
                add(row["asset"], dec(row.get("free")) + dec(row.get("locked")) + dec(row.get("freeze")))
        except Exception:
            pass
