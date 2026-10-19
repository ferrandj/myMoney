"""Enable Banking: free open-banking (DSP2) access to your own accounts.

Create a free account on https://enablebanking.com, register a production
application and activate it "by linking your own accounts" (restricted mode,
allowed for personal, non-commercial use). You get an application id and a
private key (.pem).
"""

from __future__ import annotations

import secrets
import time
from datetime import date, datetime, timedelta, timezone

import jwt

from ..services.rates import guess_account_type
from .base import AccountData, Connector, Field, NeedsAuth, TransactionData, dec

API = "https://api.enablebanking.com"
BALANCE_PREFERENCE = ["CLBD", "ITBD", "XPCD", "ITAV", "CLAV", "OPBD", "PRCD", "OTHR"]
CASH_ACCOUNT_TYPES = {"CACC": "checking", "CARD": "card", "LOAN": "loan", "SVGS": "savings", "CASH": "checking"}


class EnableBankingConnector(Connector):
    type = "enablebanking"
    label = "Banque via Enable Banking (DSP2)"
    category = "banque"
    description = (
        "Accès officiel et sécurisé (open banking) à tes comptes : Fortuneo, BforBank, Hello bank!, "
        "Boursorama, Revolut, Trade Republic (espèces), etc. Gratuit pour tes propres comptes."
    )
    auth_flow = "redirect"
    fields = [
        Field("application_id", "Application ID", help="Identifiant de l'application dans le panneau Enable Banking"),
        Field("private_key", "Clé privée (.pem)", kind="textarea", help="Contenu du fichier .pem téléchargé à la création"),
        Field("bank", "Nom de la banque", help="Exactement comme dans la liste Enable Banking, ex. « Fortuneo », « BforBank »"),
        Field("country", "Pays", default="FR"),
    ]

    def _headers(self) -> dict:
        now = int(time.time())
        token = jwt.encode(
            {"iss": "enablebanking.com", "aud": "api.enablebanking.com", "iat": now, "exp": now + 3600},
            self.config["private_key"].strip(),
            algorithm="RS256",
            headers={"kid": self.config["application_id"].strip()},
        )
        return {"Authorization": f"Bearer {token}"}

    def _request(self, method: str, path: str, **kwargs) -> dict:
        r = self.ctx.http.request(method, f"{API}{path}", headers=self._headers(), **kwargs)
        if r.status_code in (401, 403) and path.startswith("/accounts"):
            raise NeedsAuth("Consentement bancaire expiré ou révoqué : reconnecte la banque")
        if r.status_code >= 400:
            raise RuntimeError(f"Enable Banking {r.status_code} : {r.text[:300]}")
        return r.json()

    def list_banks(self) -> list[str]:
        data = self._request("GET", "/aspsps", params={"country": self.config.get("country") or "FR"})
        return sorted(a["name"] for a in data.get("aspsps", []))

    @property
    def authorized(self) -> bool:
        valid_until = self.state.get("valid_until")
        if not self.state.get("session_id") or not valid_until:
            return False
        return datetime.fromisoformat(valid_until.replace("Z", "+00:00")) > datetime.now(timezone.utc)

    def start_auth(self) -> str:
        nonce = secrets.token_urlsafe(24)
        self.state["pending_state"] = nonce
        body = {
            "access": {"valid_until": (datetime.now(timezone.utc) + timedelta(days=180)).isoformat()},
            "aspsp": {"name": self.config["bank"].strip(), "country": (self.config.get("country") or "FR").strip()},
            "state": nonce,
            "redirect_url": self.ctx.redirect_url,
            "psu_type": "personal",
        }
        return self._request("POST", "/auth", json=body)["url"]

    def finish_auth(self, params: dict[str, str]) -> None:
        if params.get("error"):
            raise RuntimeError(f"Autorisation refusée : {params.get('error_description') or params['error']}")
        if not params.get("state") or params.get("state") != self.state.get("pending_state"):
            raise RuntimeError("Réponse de la banque invalide (state ne correspond pas)")
        session = self._request("POST", "/sessions", json={"code": params["code"]})
        self.state.pop("pending_state", None)
        self.state["session_id"] = session["session_id"]
        self.state["valid_until"] = session.get("access", {}).get("valid_until")
        self.state["accounts"] = [
            {
                "uid": a["uid"],
                "name": a.get("name") or a.get("product") or a.get("details"),
                "product": a.get("product"),
                "iban": (a.get("account_id") or {}).get("iban"),
                "currency": a.get("currency") or "EUR",
                "cash_account_type": a.get("cash_account_type"),
            }
            for a in session.get("accounts", [])
        ]

    def sync(self) -> list[AccountData]:
        if not self.authorized:
            raise NeedsAuth("Connexion à la banque requise (ou consentement de 180 jours expiré)")
        since = (self.ctx.last_sync.date() - timedelta(days=10)) if self.ctx.last_sync else date.today() - timedelta(days=730)
        result = []
        for acc in self.state.get("accounts", []):
            balances = self._request("GET", f"/accounts/{acc['uid']}/balances").get("balances", [])
            balance, currency = self._pick_balance(balances, acc["currency"])
            name = acc.get("name") or acc.get("product") or "Compte"
            account_type = CASH_ACCOUNT_TYPES.get(acc.get("cash_account_type") or "", "checking")
            if account_type in ("checking", "savings"):
                account_type = guess_account_type(f"{name} {acc.get('product') or ''}", account_type)
            result.append(
                AccountData(
                    external_id=acc["uid"],
                    name=name,
                    account_type=account_type,
                    currency=currency,
                    balance=balance,
                    institution=self.config["bank"],
                    iban=acc.get("iban"),
                    transactions=self._transactions(acc["uid"], since),
                )
            )
        return result

    @staticmethod
    def _pick_balance(balances: list[dict], default_currency: str):
        if not balances:
            return dec(0), default_currency
        by_type = {b.get("balance_type"): b for b in balances}
        chosen = next((by_type[t] for t in BALANCE_PREFERENCE if t in by_type), balances[0])
        amount = chosen["balance_amount"]
        return dec(amount["amount"]), amount.get("currency") or default_currency

    def _transactions(self, uid: str, since: date) -> list[TransactionData]:
        out: list[TransactionData] = []
        params: dict = {"date_from": since.isoformat()}
        for _ in range(50):
            data = self._request("GET", f"/accounts/{uid}/transactions", params=params)
            for t in data.get("transactions", []):
                if t.get("status") not in (None, "BOOK"):
                    continue
                amount = dec(t["transaction_amount"]["amount"])
                if t.get("credit_debit_indicator") == "DBIT" and amount > 0:
                    amount = -amount
                when = t.get("booking_date") or t.get("value_date") or t.get("transaction_date")
                info = " ".join(t.get("remittance_information") or [])
                counterpart = (t.get("creditor") or {}).get("name") if amount < 0 else (t.get("debtor") or {}).get("name")
                out.append(
                    TransactionData(
                        date=date.fromisoformat(when[:10]),
                        label=(info or counterpart or t.get("note") or "Opération").strip(),
                        amount=amount,
                        currency=t["transaction_amount"].get("currency", "EUR"),
                        external_id=t.get("entry_reference") or t.get("transaction_id"),
                    )
                )
            key = data.get("continuation_key")
            if not key:
                break
            params = {"date_from": since.isoformat(), "continuation_key": key}
        return out
