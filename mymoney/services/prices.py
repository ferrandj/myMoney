"""Free price sources: ECB rates (Frankfurter), CoinGecko (crypto) and Yahoo Finance (stocks, ETFs, funds)."""

from __future__ import annotations

import json
import logging
import time
from decimal import Decimal
from typing import Callable, Iterable

import httpx

log = logging.getLogger(__name__)

FIAT = {"EUR", "USD", "GBP", "CHF", "JPY", "CAD", "AUD", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "RON", "BGN", "TRY", "CNY", "HKD", "SGD", "NZD", "ZAR", "MXN", "BRL", "INR", "KRW", "ILS", "THB", "IDR", "MYR", "PHP", "ISK"}

# Symbols are ambiguous on CoinGecko (hundreds of tokens are called "ETH" something),
# so the main ones are pinned. Others go through the search endpoint, ranked by market cap.
COINGECKO_IDS = {
    "BTC": "bitcoin", "ETH": "ethereum", "USDT": "tether", "USDC": "usd-coin", "BNB": "binancecoin",
    "SOL": "solana", "XRP": "ripple", "ADA": "cardano", "DOGE": "dogecoin", "TRX": "tron",
    "DOT": "polkadot", "MATIC": "matic-network", "POL": "polygon-ecosystem-token", "AVAX": "avalanche-2",
    "LINK": "chainlink", "ATOM": "cosmos", "LTC": "litecoin", "BCH": "bitcoin-cash", "XLM": "stellar",
    "ETC": "ethereum-classic", "XMR": "monero", "ALGO": "algorand", "NEAR": "near", "APT": "aptos",
    "ARB": "arbitrum", "OP": "optimism", "SUI": "sui", "TON": "the-open-network", "UNI": "uniswap",
    "AAVE": "aave", "DAI": "dai", "EURC": "euro-coin", "EURT": "tether-eurt", "FIL": "filecoin",
    "ICP": "internet-computer", "HBAR": "hedera-hashgraph", "VET": "vechain", "XTZ": "tezos",
    "EGLD": "elrond-erd-2", "KAS": "kaspa", "INJ": "injective-protocol", "TIA": "celestia",
    "NEXO": "nexo", "CRO": "crypto-com-chain", "KSM": "kusama", "GRT": "the-graph", "MKR": "maker",
    "SHIB": "shiba-inu", "PEPE": "pepe", "WBTC": "wrapped-bitcoin", "STETH": "staked-ether",
    "WETH": "weth", "CBBTC": "coinbase-wrapped-btc", "FLR": "flare-networks", "SEI": "sei-network",
    "PAXG": "pax-gold", "XAUT": "tether-gold", "RNDR": "render-token", "RENDER": "render-token",
    "FET": "fetch-ai", "HYPE": "hyperliquid", "ENA": "ethena", "JUP": "jupiter-exchange-solana",
    "BONK": "bonk", "WIF": "dogwifcoin", "STRK": "starknet", "MNT": "mantle", "IMX": "immutable-x",
    "SAND": "the-sandbox", "MANA": "decentraland", "AXS": "axie-infinity", "CRV": "curve-dao-token",
    "LDO": "lido-dao", "RPL": "rocket-pool", "SNX": "havven", "COMP": "compound-governance-token",
    "1INCH": "1inch", "ZEC": "zcash", "DASH": "dash", "EOS": "eos", "NEO": "neo", "QNT": "quant-network",
    "BAT": "basic-attention-token", "CHZ": "chiliz", "FTM": "fantom", "S": "sonic-3", "GNO": "gnosis",
    "XDAI": "xdai", "CELO": "celo", "KAVA": "kava", "OSMO": "osmosis", "RUNE": "thorchain",
}

# Chain names used by the wallet connectors -> CoinGecko "asset platform" ids.
COINGECKO_PLATFORMS = {
    "ethereum": "ethereum", "base": "base", "optimism": "optimistic-ethereum", "arbitrum": "arbitrum-one",
    "polygon": "polygon-pos", "gnosis": "xdai", "bsc": "binance-smart-chain", "avalanche": "avalanche",
    "scroll": "scroll", "zksync": "zksync", "linea": "linea", "celo": "celo", "solana": "solana",
}

FX_TTL = 6 * 3600
PRICE_TTL = 15 * 60


class PriceError(Exception):
    pass


class PriceService:
    def __init__(
        self,
        client: httpx.Client | None = None,
        coingecko_api_key: str | None = None,
        cache_get: Callable[[str], tuple[str, float] | None] | None = None,
        cache_set: Callable[[str, str], None] | None = None,
    ):
        self.client = client or httpx.Client(timeout=20, headers={"User-Agent": "Mozilla/5.0 (mymoney)"})
        self.coingecko_api_key = coingecko_api_key
        self._mem: dict[str, tuple[str, float]] = {}
        self._cache_get = cache_get
        self._cache_set = cache_set

    # -- cache -----------------------------------------------------------
    def _cached(self, key: str, ttl: float) -> str | None:
        hit = self._mem.get(key)
        if hit is None and self._cache_get:
            hit = self._cache_get(key)
            if hit:
                self._mem[key] = hit
        if hit and time.time() - hit[1] < ttl:
            return hit[0]
        return None

    def _stale(self, key: str) -> str | None:
        hit = self._mem.get(key) or (self._cache_get(key) if self._cache_get else None)
        return hit[0] if hit else None

    def _store(self, key: str, value: str) -> None:
        self._mem[key] = (value, time.time())
        if self._cache_set:
            self._cache_set(key, value)

    # -- FX --------------------------------------------------------------
    def fx_rate(self, from_ccy: str, to_ccy: str = "EUR") -> Decimal:
        from_ccy, to_ccy = from_ccy.upper(), to_ccy.upper()
        if from_ccy == to_ccy:
            return Decimal(1)
        if from_ccy not in FIAT:
            # Stablecoins and other crypto used as a "currency" (e.g. Binance balances in USDT).
            return self.crypto_prices([from_ccy], to_ccy)[from_ccy]
        key = f"fx:{from_ccy}:{to_ccy}"
        cached = self._cached(key, FX_TTL)
        if cached:
            return Decimal(cached)
        try:
            r = self.client.get("https://api.frankfurter.dev/v1/latest", params={"from": from_ccy, "to": to_ccy})
            r.raise_for_status()
            value = str(r.json()["rates"][to_ccy])
        except Exception as exc:
            stale = self._stale(key)
            if stale:
                log.warning("FX %s->%s indisponible, dernier taux connu utilisé", from_ccy, to_ccy)
                return Decimal(stale)
            raise PriceError(f"Taux de change {from_ccy}->{to_ccy} introuvable") from exc
        self._store(key, value)
        return Decimal(value)

    def convert(self, amount: Decimal, from_ccy: str, to_ccy: str = "EUR") -> Decimal:
        return amount * self.fx_rate(from_ccy, to_ccy)

    # -- crypto ----------------------------------------------------------
    def _coingecko_get(self, path: str, params: dict) -> dict | list:
        headers = {"x-cg-demo-api-key": self.coingecko_api_key} if self.coingecko_api_key else {}
        for attempt in range(3):
            r = self.client.get(f"https://api.coingecko.com/api/v3{path}", params=params, headers=headers)
            if r.status_code == 429 and attempt < 2:
                time.sleep(20 * (attempt + 1))
                continue
            r.raise_for_status()
            return r.json()
        raise PriceError("CoinGecko : trop de requêtes")

    def coingecko_id(self, symbol: str) -> str | None:
        symbol = symbol.upper()
        if symbol in COINGECKO_IDS:
            return COINGECKO_IDS[symbol]
        key = f"cgid:{symbol}"
        cached = self._cached(key, 30 * 86400)
        if cached is not None:
            return cached or None
        try:
            coins = self._coingecko_get("/search", {"query": symbol}).get("coins", [])
        except Exception:
            return None
        matches = [c for c in coins if c.get("symbol", "").upper() == symbol]
        matches.sort(key=lambda c: c.get("market_cap_rank") or 10**9)
        coin_id = matches[0]["id"] if matches else ""
        self._store(key, coin_id)
        return coin_id or None

    def crypto_prices(self, symbols: Iterable[str], currency: str = "EUR", ids: dict[str, str] | None = None) -> dict[str, Decimal]:
        """Prices by symbol. `ids` can force the CoinGecko id of some symbols."""
        currency = currency.lower()
        result: dict[str, Decimal] = {}
        wanted: dict[str, str] = {}
        for symbol in {s.upper() for s in symbols}:
            coin_id = (ids or {}).get(symbol) or self.coingecko_id(symbol)
            if not coin_id:
                continue
            cached = self._cached(f"cg:{coin_id}:{currency}", PRICE_TTL)
            if cached:
                result[symbol] = Decimal(cached)
            else:
                wanted[symbol] = coin_id
        unique_ids = sorted(set(wanted.values()))
        for i in range(0, len(unique_ids), 200):
            chunk = unique_ids[i : i + 200]
            try:
                data = self._coingecko_get("/simple/price", {"ids": ",".join(chunk), "vs_currencies": currency})
            except Exception as exc:
                log.warning("CoinGecko indisponible : %s", exc)
                data = {}
            for coin_id in chunk:
                price = data.get(coin_id, {}).get(currency)
                if price is not None:
                    self._store(f"cg:{coin_id}:{currency}", str(price))
        for symbol, coin_id in wanted.items():
            value = self._stale(f"cg:{coin_id}:{currency}")
            if value is not None:
                result[symbol] = Decimal(value)
        return result

    def token_prices(self, chain: str, contracts: Iterable[str], currency: str = "EUR") -> dict[str, Decimal]:
        """Prices of tokens by contract address (EVM chains, Solana mints)."""
        platform = COINGECKO_PLATFORMS.get(chain)
        contracts = sorted({c.lower() if chain != "solana" else c for c in contracts})
        if not platform or not contracts:
            return {}
        currency = currency.lower()
        result: dict[str, Decimal] = {}
        missing = []
        for contract in contracts:
            cached = self._cached(f"cgt:{platform}:{contract}:{currency}", PRICE_TTL)
            if cached:
                result[contract] = Decimal(cached)
            else:
                missing.append(contract)
        # The free tier only allows one contract per call: keep it to the tokens we don't know yet.
        for contract in missing[:25]:
            try:
                data = self._coingecko_get(
                    f"/simple/token_price/{platform}", {"contract_addresses": contract, "vs_currencies": currency}
                )
            except Exception:
                continue
            for addr, prices in data.items():
                if currency in prices:
                    key = addr if chain == "solana" else addr.lower()
                    self._store(f"cgt:{platform}:{key}:{currency}", str(prices[currency]))
                    result[key] = Decimal(str(prices[currency]))
        return result

    # -- stocks, ETFs, funds --------------------------------------------
    def yahoo_symbol(self, isin_or_query: str) -> str | None:
        key = f"yq:{isin_or_query}"
        cached = self._cached(key, 30 * 86400)
        if cached is not None:
            return cached or None
        try:
            r = self.client.get(
                "https://query1.finance.yahoo.com/v1/finance/search",
                params={"q": isin_or_query, "quotesCount": 5, "newsCount": 0},
            )
            r.raise_for_status()
            quotes = r.json().get("quotes", [])
        except Exception:
            return None
        # Prefer euro listings (Paris, Amsterdam, Xetra, Milan) for a French portfolio.
        preferred = (".PA", ".AS", ".DE", ".MI", ".F")
        quotes.sort(key=lambda q: (not q.get("symbol", "").endswith(preferred), -(q.get("score") or 0)))
        symbol = quotes[0]["symbol"] if quotes else ""
        self._store(key, symbol)
        return symbol or None

    def security_price(self, isin: str | None = None, symbol: str | None = None) -> tuple[Decimal, str] | None:
        symbol = symbol or (self.yahoo_symbol(isin) if isin else None)
        if not symbol:
            return None
        key = f"yp:{symbol}"
        cached = self._cached(key, PRICE_TTL)
        if cached:
            data = json.loads(cached)
            return Decimal(data["p"]), data["c"]
        try:
            r = self.client.get(
                f"https://query1.finance.yahoo.com/v8/finance/chart/{symbol}", params={"range": "1d", "interval": "1d"}
            )
            r.raise_for_status()
            meta = r.json()["chart"]["result"][0]["meta"]
            price, currency = Decimal(str(meta["regularMarketPrice"])), meta.get("currency", "EUR")
        except Exception:
            stale = self._stale(key)
            if stale:
                data = json.loads(stale)
                return Decimal(data["p"]), data["c"]
            return None
        if currency == "GBp":  # London quotes in pence
            price, currency = price / 100, "GBP"
        self._store(key, json.dumps({"p": str(price), "c": currency}))
        return price, currency
