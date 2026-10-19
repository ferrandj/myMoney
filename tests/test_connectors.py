import json
from decimal import Decimal

import httpx

from mymoney.connectors.base import Context
from mymoney.connectors.enablebanking import EnableBankingConnector
from mymoney.connectors.wallets import BitcoinWalletConnector, EvmWalletConnector, SolanaWalletConnector, derive_addresses

ZPUB = "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs"


def ctx(handler) -> Context:
    return Context(connection_id=1, redirect_url="https://nas/connections/callback", http=httpx.Client(transport=httpx.MockTransport(handler)))


def test_bip84_vectors():
    assert derive_addresses(ZPUB, None, 0, 0, 2) == ["bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu", "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g"]
    assert derive_addresses(ZPUB, None, 1, 0, 1) == ["bc1q8c6fshw2dlwun7ekn9qwf37cu2rn755upcp6el"]


def test_bip44_bip49_vectors():
    xpub = "xpub6BosfCnifzxcFwrSzQiqu2DBVTshkCXacvNsWGYJVVhhawA7d4R5WSWGFNbi8Aw6ZRc1brxMyWMzG3DSSSSoekkudhUd9yLb6qx39T9nMdj"
    ypub = "ypub6Ww3ibxVfGzLrAH1PNcjyAWenMTbbAosGNB6VvmSEgytSER9azLDWCxoJwW7Ke7icmizBMXrzBx9979FfaHxHcrArf3zbeJJJUZPf663zsP"
    assert derive_addresses(xpub, None, 0, 0, 1) == ["1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA"]
    assert derive_addresses(ypub, None, 0, 0, 1) == ["37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf"]


def test_pure_python_ripemd160(monkeypatch):
    import hashlib

    from mymoney.connectors import btc

    original = hashlib.new
    monkeypatch.setattr(hashlib, "new", lambda name, *a, **k: (_ for _ in ()).throw(ValueError()) if name == "ripemd160" else original(name, *a, **k))
    assert btc._ripemd160(b"").hex() == "9c1185a5c5e9fc54612808977ee8f548b2258d31"
    assert btc._ripemd160(b"abc").hex() == "8eb208f7e05d987a9b044a8e98c6b087f15a0bfc"
    assert btc._ripemd160(b"a" * 1000).hex() == "aa69deee9a8922e92f8105e007f76110f381e9cf"


def test_bitcoin_xpub_scan_stops_at_gap_limit():
    used = {"bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu": 150_000, "bc1q8c6fshw2dlwun7ekn9qwf37cu2rn755upcp6el": 50_000}
    calls = []

    def handler(request):
        address = request.url.path.rsplit("/", 1)[-1]
        calls.append(address)
        sats = used.get(address, 0)
        stats = {"funded_txo_sum": sats, "spent_txo_sum": 0, "tx_count": 1 if sats else 0}
        return httpx.Response(200, json={"chain_stats": stats, "mempool_stats": {}})

    c = BitcoinWalletConnector({"entries": f"Ledger = {ZPUB}"}, {}, ctx(handler))
    [account] = c.sync()
    assert account.name == "Ledger"
    assert account.holdings[0].quantity == Decimal("0.002")
    assert len(calls) == 2 * 30  # 20 unused after the first used address, by batches of 10, on 2 branches


def test_evm_falls_back_to_rpc_when_explorer_blocked():
    address = "0x" + "ab" * 20

    def handler(request):
        if "blockscout" in request.url.host or "optimism.io" in request.url.host:
            return httpx.Response(403, text="Just a moment...", headers={"content-type": "text/html"})
        calls = json.loads(request.content)
        out = []
        for call in calls:
            if call["method"] == "eth_getBalance":
                result = hex(2 * 10**18)
            else:
                data = call["params"][0]["data"]
                to = call["params"][0]["to"]
                if data.startswith("0x70a08231"):
                    result = hex(1500 * 10**6) if to.lower() == "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48" else "0x0"
                elif data == "0x313ce567":
                    result = hex(6)
                else:
                    result = "0x"
            out.append({"jsonrpc": "2.0", "id": call["id"], "result": result})
        return httpx.Response(200, json=out)

    c = EvmWalletConnector({"addresses": address, "chains": "ethereum"}, {}, ctx(handler))
    [account] = c.sync()
    got = {h.symbol: h.quantity for h in account.holdings}
    assert got == {"ETH": Decimal(2), "USDC": Decimal(1500)}


def test_evm_explorer_filters_spam():
    address = "0x" + "cd" * 20
    tokens = [
        {"token": {"type": "ERC-20", "symbol": "USDC", "name": "USD Coin", "decimals": "6", "exchange_rate": "1.0", "holders_count": "5000000", "reputation": "ok", "address_hash": "0xusdc"}, "value": "2500000"},
        {"token": {"type": "ERC-20", "symbol": "SCAM", "name": "Claim reward", "decimals": "18", "exchange_rate": None, "reputation": "ok", "address_hash": "0xscam"}, "value": "1"},
        {"token": {"type": "ERC-20", "symbol": "BAD", "decimals": "18", "exchange_rate": "5", "reputation": "scam", "address_hash": "0xbad"}, "value": str(10**18)},
    ]

    def handler(request):
        if request.url.host == "eth.blockscout.com":
            return httpx.Response(200, json=tokens)
        return httpx.Response(200, json=[{"jsonrpc": "2.0", "id": 0, "result": "0x0"}])

    c = EvmWalletConnector({"addresses": address, "chains": "ethereum"}, {}, ctx(handler))
    [account] = c.sync()
    assert [(h.symbol, h.quantity, h.value_currency) for h in account.holdings] == [("USDC", Decimal("2.5"), "USD")]


def test_solana():
    def handler(request):
        body = json.loads(request.content)
        if body["method"] == "getBalance":
            return httpx.Response(200, json={"result": {"value": 3 * 10**9}})
        if body["params"][1]["programId"].startswith("Tokenkeg"):
            item = {"account": {"data": {"parsed": {"info": {"mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "tokenAmount": {"uiAmountString": "42.5"}}}}}}
            return httpx.Response(200, json={"result": {"value": [item]}})
        return httpx.Response(200, json={"result": {"value": []}})

    [account] = SolanaWalletConnector({"addresses": "So1ana"}, {}, ctx(handler)).sync()
    assert {h.symbol: h.quantity for h in account.holdings} == {"SOL": Decimal(3), "USDC": Decimal("42.5")}


def _rsa_pem() -> str:
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import rsa

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    return key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()).decode()


def test_enable_banking_flow():
    seen = {}

    def handler(request):
        assert request.headers["authorization"].startswith("Bearer ")
        path = request.url.path
        if path == "/auth":
            seen["auth"] = json.loads(request.content)
            return httpx.Response(200, json={"url": "https://bank.example/login"})
        if path == "/sessions":
            return httpx.Response(200, json={
                "session_id": "S1", "access": {"valid_until": "2099-01-01T00:00:00+00:00"},
                "accounts": [{"uid": "U1", "name": "LIVRET A", "currency": "EUR", "cash_account_type": "SVGS", "account_id": {"iban": "FR76..."}}],
            })
        if path.endswith("/balances"):
            return httpx.Response(200, json={"balances": [
                {"balance_amount": {"amount": "99.00", "currency": "EUR"}, "balance_type": "ITAV"},
                {"balance_amount": {"amount": "100.00", "currency": "EUR"}, "balance_type": "CLBD"},
            ]})
        if path.endswith("/transactions"):
            if "continuation_key" not in request.url.params:
                return httpx.Response(200, json={"transactions": [
                    {"entry_reference": "T1", "transaction_amount": {"amount": "10.00", "currency": "EUR"}, "credit_debit_indicator": "DBIT",
                     "booking_date": "2026-02-01", "remittance_information": ["RETRAIT"], "status": "BOOK"},
                ], "continuation_key": "next"})
            return httpx.Response(200, json={"transactions": [
                {"entry_reference": "T2", "transaction_amount": {"amount": "5.00", "currency": "EUR"}, "credit_debit_indicator": "CRDT",
                 "booking_date": "2026-02-02", "remittance_information": ["INTERETS"], "status": "BOOK"},
            ]})
        return httpx.Response(404)

    config = {"application_id": "app-1", "private_key": _rsa_pem(), "bank": "Fortuneo", "country": "FR"}
    state: dict = {}
    c = EnableBankingConnector(config, state, ctx(handler))
    assert c.start_auth() == "https://bank.example/login"
    assert seen["auth"]["aspsp"] == {"name": "Fortuneo", "country": "FR"}
    try:
        c.finish_auth({"code": "C", "state": "forged"})
        raise AssertionError("state must be checked")
    except RuntimeError:
        pass
    c.finish_auth({"code": "C", "state": state["pending_state"]})
    [account] = c.sync()
    assert account.account_type == "livret_a"
    assert account.balance == Decimal("100.00")
    assert [t.amount for t in account.transactions] == [Decimal("-10.00"), Decimal("5.00")]
