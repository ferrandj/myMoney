"""GoCardless Bank Account Data (ex-Nordigen): free tier, if you already have an account.

New sign-ups have reportedly been closed since 2025: prefer Enable Banking.
"""

from __future__ import annotations

import secrets
from datetime import date, datetime, timedelta, timezone

from ..services.rates import guess_account_type
from .base import AccountData, Connector, Field, NeedsAuth, TransactionData, dec

API = "https://bankaccountdata.gocardless.com/api/v2"
BALANCE_PREFERENCE = ["closingBooked", "interimBooked", "expected", "interimAvailable", "openingBooked"]


class GoCardlessConnector(Connector):
    type = "gocardless"
    label = "Banque via GoCardless Bank Account Data (DSP2)"
    category = "banque"
    description = "Alternative gratuite à Enable Banking pour les comptes GoCardless/Nordigen existants."
    auth_flow = "redirect"
    fields = [
        Field("secret_id", "Secret ID"),
        Field("secret_key", "Secret key", kind="password"),
        Field("institution_id", "Institution ID", help="ex. FORTUNEO_FTNOFRP1, BFORBANK_BFBKFRP1 (liste dans le portail GoCardless)"),
    ]

    def _token(self) -> str:
        cached = self.state.get("access")
        if cached and self.state.get("access_until", 0) > datetime.now(timezone.utc).timestamp() + 60:
            return cached
        r = self.ctx.http.post(
            f"{API}/token/new/", json={"secret_id": self.config["secret_id"], "secret_key": self.config["secret_key"]}
        )
        if r.status_code >= 400:
            raise RuntimeError(f"GoCardless : identifiants refusés ({r.status_code})")
        data = r.json()
        self.state["access"] = data["access"]
        self.state["access_until"] = datetime.now(timezone.utc).timestamp() + int(data.get("access_expires", 3600))
        return data["access"]

    def _request(self, method: str, path: str, **kwargs) -> dict:
        r = self.ctx.http.request(method, f"{API}{path}", headers={"Authorization": f"Bearer {self._token()}"}, **kwargs)
        if r.status_code in (401, 403) and path.startswith("/accounts"):
            raise NeedsAuth("Consentement expiré : reconnecte la banque")
        if r.status_code == 429:
            raise RuntimeError("GoCardless : quota journalier atteint (4 appels/jour/compte), réessaie plus tard")
        if r.status_code >= 400:
            raise RuntimeError(f"GoCardless {r.status_code} : {r.text[:300]}")
        return r.json()

    @property
    def authorized(self) -> bool:
        return bool(self.state.get("account_ids"))

    def start_auth(self) -> str:
        reference = secrets.token_urlsafe(16)
        req = self._request(
            "POST",
            "/requisitions/",
            json={
                "redirect": self.ctx.redirect_url,
                "institution_id": self.config["institution_id"].strip(),
                "reference": reference,
                "user_language": "FR",
            },
        )
        self.state["requisition_id"] = req["id"]
        self.state["pending_ref"] = reference
        return req["link"]

    def finish_auth(self, params: dict[str, str]) -> None:
        if params.get("ref") and params["ref"] != self.state.get("pending_ref"):
            raise RuntimeError("Réponse de la banque invalide (référence inattendue)")
        req = self._request("GET", f"/requisitions/{self.state['requisition_id']}/")
        if not req.get("accounts"):
            raise RuntimeError(f"Aucun compte autorisé (statut {req.get('status')})")
        self.state["account_ids"] = req["accounts"]
        self.state.pop("pending_ref", None)

    def sync(self) -> list[AccountData]:
        if not self.authorized:
            raise NeedsAuth("Connexion à la banque requise")
        since = (self.ctx.last_sync.date() - timedelta(days=10)) if self.ctx.last_sync else date.today() - timedelta(days=730)
        result = []
        for account_id in self.state["account_ids"]:
            details = self._request("GET", f"/accounts/{account_id}/details/").get("account", {})
            balances = self._request("GET", f"/accounts/{account_id}/balances/").get("balances", [])
            by_type = {b.get("balanceType"): b for b in balances}
            chosen = next((by_type[t] for t in BALANCE_PREFERENCE if t in by_type), balances[0] if balances else None)
            name = details.get("name") or details.get("product") or "Compte"
            account_type = "card" if details.get("cashAccountType") == "CARD" else guess_account_type(
                f"{name} {details.get('product') or ''}", "savings" if details.get("cashAccountType") == "SVGS" else "checking"
            )
            txs = self._request(
                "GET", f"/accounts/{account_id}/transactions/", params={"date_from": since.isoformat()}
            ).get("transactions", {})
            result.append(
                AccountData(
                    external_id=account_id,
                    name=name,
                    account_type=account_type,
                    currency=(chosen or {}).get("balanceAmount", {}).get("currency") or details.get("currency") or "EUR",
                    balance=dec((chosen or {}).get("balanceAmount", {}).get("amount")),
                    institution=self.config["institution_id"].split("_")[0].title(),
                    iban=details.get("iban"),
                    transactions=[
                        TransactionData(
                            date=date.fromisoformat((t.get("bookingDate") or t.get("valueDate"))[:10]),
                            label=(
                                t.get("remittanceInformationUnstructured")
                                or " ".join(t.get("remittanceInformationUnstructuredArray") or [])
                                or t.get("creditorName")
                                or t.get("debtorName")
                                or "Opération"
                            ).strip(),
                            amount=dec(t["transactionAmount"]["amount"]),
                            currency=t["transactionAmount"].get("currency", "EUR"),
                            external_id=t.get("transactionId") or t.get("internalTransactionId"),
                        )
                        for t in txs.get("booked", [])
                    ],
                )
            )
        return result
