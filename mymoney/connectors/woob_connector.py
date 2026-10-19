"""Woob: free open-source library that reads French bank and insurer websites.

Covers banks and products without open-banking access: assurance-vie, PEA and
securities positions, PER, épargne salariale... It logs in with your web
credentials (kept encrypted in the vault). Websites change: a module can break
until Woob ships a fix (use "Mettre à jour Woob" in the settings).
"""

from __future__ import annotations

import json
import os
from copy import deepcopy
from decimal import Decimal

from . import live
from .base import AccountData, Connector, Field, HoldingData, NeedsAuth, NeedsInteraction, Prompt, TransactionData, dec

# Woob account types -> ours
WOOB_TYPES = {
    1: "checking", 2: "savings", 3: "term_deposit", 4: "loan", 5: "cto", 6: "checking", 7: "card",
    8: "life_insurance", 9: "employee_savings", 10: "employee_savings", 11: "per", 12: "per", 13: "pea",
    14: "life_insurance", 15: "per", 16: "per", 17: "mortgage", 18: "loan", 19: "loan", 20: "per",
    21: "scpi", 22: "crowdfunding", 23: "ldds", 24: "pel", 25: "savings", 26: "cel", 27: "term_deposit",
    28: "livret_a", 29: "livret_a",
}

SUGGESTED_MODULES = "fortuneo, bforbank, hellobank, boursorama, bnporc, societegenerale, cragr, creditmutuel, caissedepargne, banquepopulaire, lcl, ing, axabanque, spirica, linebourse, amundi, yomoni, nalo, linxea"


class _DictStorage:
    """Woob storage kept in our encrypted state instead of a plaintext YAML file."""

    def __init__(self, values: dict | None = None):
        self.values = values or {}

    def load(self, what, name, default={}):
        current = self.values.setdefault(what, {}).get(name, {})
        merged = deepcopy(default)
        merged.update(current)
        self.values[what][name] = merged

    def save(self, what, name):
        pass

    def _walk(self, keys, create):
        node = self.values
        for key in keys:
            if key not in node:
                if not create:
                    raise KeyError(key)
                node[key] = {}
            node = node[key]
        return node

    def set(self, what, name, *args):
        *path, key, value = (what, name) + args
        self._walk(path, True)[key] = value

    def delete(self, what, name, *args):
        *path, key = (what, name) + args
        try:
            self._walk(path, False).pop(key, None)
        except KeyError:
            pass

    def get(self, what, name, *args, **kwargs):
        try:
            node = self._walk((what, name) + args[:-1], False)
            return node[args[-1]] if args else node
        except (KeyError, TypeError, IndexError):
            if "default" in kwargs:
                return kwargs["default"]
            raise KeyError(args[-1] if args else name)


def _woob(workdir: str):
    from woob.core import Woob

    os.makedirs(workdir, exist_ok=True)
    return Woob(workdir=workdir, datadir=workdir)


def update_modules(workdir: str) -> None:
    _woob(workdir).repositories.update()


class WoobConnector(Connector):
    type = "woob"
    label = "Banque / assurance via Woob (identifiants web)"
    category = "banque"
    description = "Pour ce que l'open banking ne couvre pas : assurance-vie, PEA, compte-titres, PER, épargne salariale…"
    auth_flow = "interactive"
    requires = "woob"
    fields = [
        Field("module", "Module Woob", help=f"ex. {SUGGESTED_MODULES}"),
        Field("login", "Identifiant"),
        Field("password", "Mot de passe", kind="password"),
        Field(
            "params",
            "Paramètres supplémentaires",
            kind="textarea",
            required=False,
            help="Un par ligne « clé=valeur » si le module en demande (ex. website=par)",
        ),
    ]
    workdir: str = "/data/woob"

    def _backend(self):
        cached = live.get(self.ctx.connection_id)
        if cached is not None:
            return cached
        params = {"login": self.config["login"], "password": self.config["password"]}
        for line in (self.config.get("params") or "").splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                params[key.strip()] = value.strip()
        woob = _woob(self.workdir)
        module = self.config["module"].strip()
        info = woob.repositories.get_module_info(module)
        if info is None:
            woob.repositories.update()
            info = woob.repositories.get_module_info(module)
            if info is None:
                raise RuntimeError(f"Module Woob inconnu : {module}")
        if not info.is_installed():
            woob.repositories.install(module)
        storage = _DictStorage(self.state.get("storage"))
        backend = woob.load_backend(module, f"mm{self.ctx.connection_id}", params=params, storage=storage)
        backend._mm_storage = storage
        if "request_information" in backend.config:
            backend.config["request_information"].set({})  # we can answer 2FA prompts
        return backend

    def _persist(self, backend) -> None:
        try:
            backend.dump_state()
        except Exception:
            pass
        self.state["storage"] = json.loads(json.dumps(backend._mm_storage.values, default=str))

    @property
    def authorized(self) -> bool:
        return True  # credentials are enough; 2FA is asked when the bank wants it

    def login(self, answers: dict[str, str] | None) -> Prompt | None:
        backend = live.get(self.ctx.connection_id)
        if backend is not None and answers is not None:
            for key, value in answers.items():
                if key == "_resume":
                    if "resume" in backend.config:
                        backend.config["resume"].set(True)
                elif key in backend.config:
                    backend.config[key].set(value)
        return None

    def sync(self) -> list[AccountData]:
        from woob.exceptions import BrowserIncorrectPassword, BrowserQuestion, DecoupledValidation, NeedInteractive

        backend = self._backend()
        try:
            accounts = list(backend.iter_accounts())
        except BrowserQuestion as question:
            live.put(self.ctx.connection_id, backend)
            raise NeedsInteraction(
                Prompt(
                    "Ta banque demande une vérification.",
                    [Field(f.id, f.label or f.description or f.id) for f in question.fields],
                )
            )
        except DecoupledValidation as validation:
            live.put(self.ctx.connection_id, backend)
            raise NeedsInteraction(
                Prompt((validation.message or "Valide la connexion dans l'application de ta banque") + ", puis clique sur Continuer.", [Field("_resume", "", kind="hidden", required=False, default="1")])
            )
        except NeedInteractive as exc:
            raise NeedsAuth(f"La banque demande une validation : relance la synchro depuis l'app ({exc})")
        except BrowserIncorrectPassword as exc:
            live.drop(self.ctx.connection_id)
            raise RuntimeError(f"Identifiants refusés par la banque : {exc}")

        result = []
        try:
            for acc in accounts:
                account_type = WOOB_TYPES.get(int(acc.type or 0), "checking")
                holdings = None
                if account_type in ("cto", "pea", "life_insurance", "per", "employee_savings", "crowdfunding", "scpi"):
                    holdings = []
                    try:
                        for inv in backend.iter_investment(acc):
                            holdings.append(
                                HoldingData(
                                    symbol=None,
                                    isin=inv.code if getattr(inv, "code_type", None) in (None, "ISIN") and inv.code else None,
                                    name=inv.label,
                                    quantity=dec(inv.quantity) if inv.quantity else Decimal(0),
                                    asset_class="fund",
                                    unit_price=dec(inv.unitvalue) if inv.unitvalue else None,
                                    value=dec(inv.valuation) if inv.valuation is not None else None,
                                    value_currency=str(acc.currency or "EUR"),
                                )
                            )
                    except NotImplementedError:
                        holdings = None
                transactions = []
                if holdings is None:
                    try:
                        for i, tr in enumerate(backend.iter_history(acc)):
                            if i >= 500:
                                break
                            transactions.append(
                                TransactionData(
                                    date=tr.date if not hasattr(tr.date, "date") else tr.date.date(),
                                    label=tr.label or tr.raw or "Opération",
                                    amount=dec(tr.amount),
                                    currency=str(acc.currency or "EUR"),
                                    external_id=tr.id or None,
                                )
                            )
                    except NotImplementedError:
                        pass
                # Investment accounts: the reported balance is the total, positions detail it.
                balance = dec(acc.balance) if acc.balance is not None else Decimal(0)
                if holdings:
                    balance -= sum((h.value or Decimal(0)) for h in holdings)
                    if abs(balance) < Decimal("0.01"):
                        balance = Decimal(0)
                result.append(
                    AccountData(
                        external_id=str(acc.id),
                        name=acc.label or str(acc.id),
                        account_type=account_type,
                        currency=str(acc.currency or "EUR"),
                        balance=balance,
                        institution=self.config["module"].title(),
                        iban=acc.iban or None,
                        holdings=holdings,
                        transactions=transactions,
                    )
                )
        finally:
            self._persist(backend)
            live.drop(self.ctx.connection_id)
        return result
