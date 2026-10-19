"""Self-custody wallets read from public blockchain data. Only public addresses are needed:
NEVER enter a seed phrase or a private key.

Privacy: public APIs see which addresses you look up. Point the URLs to your own
node / mempool / RPC if you run one on the NAS.
"""

from __future__ import annotations

import logging
from decimal import Decimal

from .base import AccountData, Connector, Field, HoldingData, dec

log = logging.getLogger(__name__)


def _lines(text: str | None) -> list[tuple[str | None, str]]:
    """Parse "label = address" lines (label optional)."""
    out = []
    for raw in (text or "").splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if "=" in line:
            label, value = (part.strip() for part in line.split("=", 1))
            out.append((label or None, value))
        else:
            out.append((None, line))
    return out


def _short(address: str) -> str:
    return f"{address[:6]}…{address[-4:]}"


# --------------------------------------------------------------------------- EVM
EVM_CHAINS: dict[str, dict] = {
    "ethereum": {"name": "Ethereum", "native": "ETH", "rpc": "https://ethereum-rpc.publicnode.com", "explorer": "https://eth.blockscout.com"},
    "base": {"name": "Base", "native": "ETH", "rpc": "https://base-rpc.publicnode.com", "explorer": "https://base.blockscout.com"},
    "arbitrum": {"name": "Arbitrum", "native": "ETH", "rpc": "https://arbitrum-one-rpc.publicnode.com", "explorer": "https://arbitrum.blockscout.com"},
    "optimism": {"name": "Optimism", "native": "ETH", "rpc": "https://optimism-rpc.publicnode.com", "explorer": "https://explorer.optimism.io"},
    "polygon": {"name": "Polygon", "native": "POL", "rpc": "https://polygon-bor-rpc.publicnode.com", "explorer": "https://polygon.blockscout.com"},
    "bsc": {"name": "BNB Chain", "native": "BNB", "rpc": "https://bsc-rpc.publicnode.com", "explorer": None},
    "avalanche": {"name": "Avalanche", "native": "AVAX", "rpc": "https://avalanche-c-chain-rpc.publicnode.com", "explorer": None},
    "gnosis": {"name": "Gnosis", "native": "XDAI", "rpc": "https://gnosis-rpc.publicnode.com", "explorer": "https://gnosis.blockscout.com"},
    "linea": {"name": "Linea", "native": "ETH", "rpc": "https://linea-rpc.publicnode.com", "explorer": None},
    "scroll": {"name": "Scroll", "native": "ETH", "rpc": "https://scroll-rpc.publicnode.com", "explorer": "https://scroll.blockscout.com"},
    "zksync": {"name": "zkSync Era", "native": "ETH", "rpc": "https://mainnet.era.zksync.io", "explorer": "https://zksync.blockscout.com"},
    "celo": {"name": "Celo", "native": "CELO", "rpc": "https://forno.celo.org", "explorer": "https://celo.blockscout.com"},
}

# Main tokens checked directly on-chain when the explorer (token discovery) is unreachable.
EVM_MAJOR_TOKENS: dict[str, dict[str, str]] = {
    "ethereum": {
        "USDC": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDT": "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        "DAI": "0x6B175474E89094C44Da98b954EedeAC495271d0F", "WETH": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "WBTC": "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", "STETH": "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84",
        "EURC": "0x1aBaEA1f7C830bD89Acc67eC4af516284b1bC33c", "LINK": "0x514910771AF9Ca656af840dff83E8264EcF986CA",
        "UNI": "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
    },
    "base": {
        "USDC": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", "WETH": "0x4200000000000000000000000000000000000006",
        "CBBTC": "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf", "EURC": "0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42",
    },
    "arbitrum": {
        "USDC": "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", "USDT": "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
        "WETH": "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", "ARB": "0x912CE59144191C1204E64559FE8253a0e49E6548",
        "WBTC": "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f",
    },
    "optimism": {
        "USDC": "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85", "USDT": "0x94b008aA00579c1307B0EF2c499aD98a8ce58e58",
        "WETH": "0x4200000000000000000000000000000000000006", "OP": "0x4200000000000000000000000000000000000042",
    },
    "polygon": {
        "USDC": "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", "USDT": "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
        "WETH": "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619", "WBTC": "0x1BFD67037B42Cf73acF2047067bd4F2C47D9BfD6",
    },
    "bsc": {
        "USDT": "0x55d398326f99059fF775485246999027B3197955", "USDC": "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
        "BTCB": "0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c", "ETH": "0x2170Ed0880ac9A755fd29B2688956BD959F933F8",
    },
    "avalanche": {
        "USDC": "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E", "USDT": "0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7",
    },
}

_BALANCE_OF = "0x70a08231"
_DECIMALS = "0x313ce567"
_SYMBOL = "0x95d89b41"


def _decode_string(hex_data: str) -> str:
    data = bytes.fromhex(hex_data[2:]) if hex_data and hex_data != "0x" else b""
    if len(data) >= 64:
        length = int.from_bytes(data[32:64], "big")
        return data[64 : 64 + length].decode(errors="ignore")
    return data.rstrip(b"\0").decode(errors="ignore")


class EvmWalletConnector(Connector):
    type = "evm"
    label = "Wallet EVM (Ethereum, Base, Arbitrum, BNB Chain, Polygon…)"
    category = "wallet"
    description = "Lit les soldes publics de tes adresses 0x… sur toutes les chaînes EVM, sans clé privée."
    fields = [
        Field("addresses", "Adresses publiques", kind="textarea", help="Une par ligne, optionnellement « Ledger = 0x… »"),
        Field(
            "chains",
            "Chaînes",
            default=",".join(EVM_CHAINS),
            help="Liste séparée par des virgules parmi : " + ", ".join(EVM_CHAINS),
        ),
        Field(
            "tokens",
            "Tokens supplémentaires",
            kind="textarea",
            required=False,
            help="Un par ligne : « chaîne:0xcontrat », pour les tokens non détectés automatiquement",
        ),
        Field(
            "include_unpriced",
            "Filtre anti-spam",
            kind="select",
            required=False,
            options=[
                ("no", "Masquer les tokens sans prix, de moins de 1 $ ou peu détenus (airdrops)"),
                ("yes", "Tout afficher"),
            ],
            default="no",
        ),
        Field(
            "ignored",
            "Tokens à masquer",
            kind="textarea",
            required=False,
            help="Symboles ou adresses de contrat, un par ligne",
        ),
    ]
    MIN_VALUE_USD = Decimal(1)
    MIN_HOLDERS = 250

    def _rpc(self, url: str, calls: list[tuple[str, list]]) -> list:
        payload = [{"jsonrpc": "2.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(calls)]
        r = self.ctx.http.post(url, json=payload)
        r.raise_for_status()
        data = r.json()
        if isinstance(data, dict):  # some nodes refuse batches
            data = [data]
        by_id = {item.get("id"): item.get("result") for item in data}
        return [by_id.get(i) for i in range(len(calls))]

    def _explorer_tokens(self, base: str, address: str) -> list[dict] | None:
        try:
            r = self.ctx.http.get(f"{base}/api/v2/addresses/{address}/token-balances", follow_redirects=True)
            if r.status_code != 200 or not r.headers.get("content-type", "").startswith("application/json"):
                return None
            return r.json()
        except Exception:
            return None

    def _chain_holdings(self, chain: str, address: str, extra_tokens: list[str]) -> list[HoldingData]:
        info = EVM_CHAINS[chain]
        include_unpriced = self.config.get("include_unpriced") == "yes"
        holdings: list[HoldingData] = []

        (native_hex,) = self._rpc(info["rpc"], [("eth_getBalance", [address, "latest"])])
        native = Decimal(int(native_hex or "0x0", 16)) / Decimal(10**18)
        if native > 0:
            holdings.append(
                HoldingData(symbol=info["native"], quantity=native, name=f"{info['native']} ({info['name']})", extra={"chain": chain})
            )

        explorer = self._explorer_tokens(info["explorer"], address) if info.get("explorer") else None
        contracts: dict[str, str | None] = {}
        if explorer is not None:
            for item in explorer:
                token = item.get("token") or {}
                if token.get("type") != "ERC-20" or (token.get("reputation") or "ok") != "ok":
                    continue
                decimals = int(token.get("decimals") or 0)
                quantity = Decimal(int(item.get("value") or 0)) / Decimal(10**decimals)
                if quantity <= 0:
                    continue
                rate = token.get("exchange_rate")
                if not include_unpriced and (
                    rate is None
                    or quantity * dec(rate) < self.MIN_VALUE_USD
                    or int(token.get("holders_count") or 0) < self.MIN_HOLDERS
                ):
                    continue
                holdings.append(
                    HoldingData(
                        symbol=(token.get("symbol") or "?").upper(),
                        quantity=quantity,
                        name=f"{token.get('name') or token.get('symbol')} ({info['name']})",
                        unit_price=dec(rate) if rate is not None else None,
                        value=quantity * dec(rate) if rate is not None else None,
                        value_currency="USD" if rate is not None else None,
                        extra={"chain": chain, "contract": token.get("address_hash")},
                    )
                )
        else:
            contracts = {addr: symbol for symbol, addr in EVM_MAJOR_TOKENS.get(chain, {}).items()}
        for contract in extra_tokens:
            contracts.setdefault(contract, None)
        if contracts:
            holdings.extend(self._rpc_tokens(chain, address, contracts))
        ignored = {line.lower() for _, line in _lines(self.config.get("ignored"))}
        return [
            h for h in holdings
            if (h.symbol or "").lower() not in ignored and ((h.extra or {}).get("contract") or "").lower() not in ignored
        ]

    def _rpc_tokens(self, chain: str, address: str, contracts: dict[str, str | None]) -> list[HoldingData]:
        info = EVM_CHAINS[chain]
        padded = address.lower().replace("0x", "").rjust(64, "0")
        items = list(contracts.items())
        calls = []
        for contract, _ in items:
            calls.append(("eth_call", [{"to": contract, "data": _BALANCE_OF + padded}, "latest"]))
            calls.append(("eth_call", [{"to": contract, "data": _DECIMALS}, "latest"]))
            calls.append(("eth_call", [{"to": contract, "data": _SYMBOL}, "latest"]))
        results = self._rpc(info["rpc"], calls)
        out = []
        for i, (contract, symbol) in enumerate(items):
            raw_balance, raw_decimals, raw_symbol = results[3 * i : 3 * i + 3]
            if not raw_balance or raw_balance == "0x" or int(raw_balance, 16) == 0:
                continue
            decimals = int(raw_decimals, 16) if raw_decimals and raw_decimals != "0x" else 18
            symbol = symbol or _decode_string(raw_symbol or "") or "?"
            out.append(
                HoldingData(
                    symbol=symbol.upper(),
                    quantity=Decimal(int(raw_balance, 16)) / Decimal(10**decimals),
                    name=f"{symbol} ({info['name']})",
                    extra={"chain": chain, "contract": contract},
                )
            )
        return out

    def sync(self) -> list[AccountData]:
        chains = [c.strip() for c in (self.config.get("chains") or ",".join(EVM_CHAINS)).split(",") if c.strip() in EVM_CHAINS]
        extra: dict[str, list[str]] = {}
        for _, line in _lines(self.config.get("tokens")):
            if ":" in line:
                chain, contract = line.split(":", 1)
                extra.setdefault(chain.strip().lower(), []).append(contract.strip())
        accounts = []
        for label, address in _lines(self.config.get("addresses")):
            if not (address.startswith("0x") and len(address) == 42):
                raise ValueError(f"Adresse EVM invalide : {address}")
            holdings: list[HoldingData] = []
            errors = []
            for chain in chains:
                try:
                    holdings.extend(self._chain_holdings(chain, address, extra.get(chain, [])))
                except Exception as exc:
                    log.warning("EVM %s %s : %s", chain, _short(address), exc)
                    errors.append(chain)
            if errors and len(errors) == len(chains):
                raise RuntimeError(f"Aucune chaîne joignable pour {_short(address)}")
            accounts.append(
                AccountData(
                    external_id=address.lower(),
                    name=label or f"Wallet {_short(address)}",
                    account_type="crypto_wallet",
                    institution="EVM",
                    holdings=holdings,
                )
            )
        return accounts


# ----------------------------------------------------------------------- Bitcoin
_SLIP132 = {"xpub": "p2pkh", "ypub": "p2sh-p2wpkh", "zpub": "p2wpkh"}


def derive_addresses(extended_key: str, script: str | None, branch: int, start: int, count: int) -> list[str]:
    """Derive receive (branch 0) or change (branch 1) addresses from an account-level xpub/ypub/zpub."""
    from . import btc

    script = script or _SLIP132.get(extended_key[:4])
    encoder = {"p2wpkh": btc.p2wpkh, "p2sh-p2wpkh": btc.p2sh_p2wpkh, "p2pkh": btc.p2pkh}.get(script or "")
    if encoder is None:
        raise ValueError("Clé étendue non reconnue (xpub, ypub ou zpub attendu)")
    node = btc.ExtendedPublicKey.parse(extended_key).child(branch)
    return [encoder(node.child(index).public_key) for index in range(start, start + count)]


class BitcoinWalletConnector(Connector):
    type = "bitcoin"
    label = "Wallet Bitcoin (adresses ou xpub/ypub/zpub)"
    category = "wallet"
    description = "Solde on-chain via l'API mempool.space (ou ton propre mempool sur le NAS). Clés publiques uniquement."
    fields = [
        Field(
            "entries",
            "Adresses ou clés publiques étendues",
            kind="textarea",
            help="Une par ligne, ex. « Ledger = zpub6r… ». Pour un xpub segwit natif : « xpub…:p2wpkh »",
        ),
        Field("api", "URL de l'API Esplora/mempool", default="https://mempool.space/api", required=False),
    ]
    GAP_LIMIT = 20

    def _address_stats(self, address: str) -> tuple[int, int]:
        r = self.ctx.http.get(f"{(self.config.get('api') or 'https://mempool.space/api').rstrip('/')}/address/{address}")
        r.raise_for_status()
        data = r.json()
        sats = 0
        txs = 0
        for key in ("chain_stats", "mempool_stats"):
            stats = data.get(key) or {}
            sats += stats.get("funded_txo_sum", 0) - stats.get("spent_txo_sum", 0)
            txs += stats.get("tx_count", 0)
        return sats, txs

    def _xpub_balance(self, key: str, script: str | None) -> int:
        total = 0
        for branch in (0, 1):
            index, unused = 0, 0
            while unused < self.GAP_LIMIT and index < 2000:
                for address in derive_addresses(key, script, branch, index, 10):
                    sats, txs = self._address_stats(address)
                    total += sats
                    unused = 0 if txs else unused + 1
                index += 10
        return total

    def sync(self) -> list[AccountData]:
        accounts = []
        for label, entry in _lines(self.config.get("entries")):
            script = None
            if ":" in entry:
                entry, script = (p.strip() for p in entry.split(":", 1))
            if entry[:4] in _SLIP132:
                sats = self._xpub_balance(entry, script)
            else:
                sats, _ = self._address_stats(entry)
            accounts.append(
                AccountData(
                    external_id=entry,
                    name=label or f"Bitcoin {_short(entry)}",
                    account_type="crypto_wallet",
                    institution="Bitcoin",
                    holdings=[HoldingData(symbol="BTC", quantity=Decimal(sats) / Decimal(10**8), name="Bitcoin")],
                )
            )
        return accounts


# ------------------------------------------------------------------------ Solana
SOLANA_TOKEN_PROGRAMS = ["TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"]
SOLANA_MINTS = {
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v": "USDC",
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB": "USDT",
    "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN": "JUP",
    "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263": "BONK",
    "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm": "WIF",
    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So": "MSOL",
    "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn": "JITOSOL",
    "So11111111111111111111111111111111111111112": "SOL",
}


class SolanaWalletConnector(Connector):
    type = "solana"
    label = "Wallet Solana"
    category = "wallet"
    description = "SOL et tokens SPL d'adresses publiques Solana via le RPC public (ou le tien)."
    fields = [
        Field("addresses", "Adresses publiques", kind="textarea", help="Une par ligne, optionnellement « Phantom = adresse »"),
        Field("rpc", "URL RPC", default="https://api.mainnet-beta.solana.com", required=False),
    ]

    def _rpc(self, method: str, params: list):
        r = self.ctx.http.post(
            self.config.get("rpc") or "https://api.mainnet-beta.solana.com",
            json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params},
        )
        r.raise_for_status()
        data = r.json()
        if "error" in data:
            raise RuntimeError(f"Solana RPC : {data['error'].get('message')}")
        return data["result"]

    def sync(self) -> list[AccountData]:
        accounts = []
        for label, address in _lines(self.config.get("addresses")):
            lamports = self._rpc("getBalance", [address])["value"]
            holdings = [HoldingData(symbol="SOL", quantity=Decimal(lamports) / Decimal(10**9), name="Solana", extra={"chain": "solana"})]
            for program in SOLANA_TOKEN_PROGRAMS:
                result = self._rpc("getTokenAccountsByOwner", [address, {"programId": program}, {"encoding": "jsonParsed"}])
                for item in result.get("value", []):
                    info = item["account"]["data"]["parsed"]["info"]
                    amount = dec(info["tokenAmount"].get("uiAmountString"))
                    if amount <= 0 or info["tokenAmount"].get("decimals") == 0:  # NFTs
                        continue
                    mint = info["mint"]
                    holdings.append(
                        HoldingData(
                            symbol=SOLANA_MINTS.get(mint, mint[:6]),
                            quantity=amount,
                            name=SOLANA_MINTS.get(mint, f"Token {mint[:6]}…"),
                            extra={"chain": "solana", "contract": mint},
                        )
                    )
            accounts.append(
                AccountData(
                    external_id=address,
                    name=label or f"Solana {_short(address)}",
                    account_type="crypto_wallet",
                    institution="Solana",
                    holdings=[h for h in holdings if h.quantity > 0],
                )
            )
        return accounts
