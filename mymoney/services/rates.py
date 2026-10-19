"""Reference interest rates of French regulated savings accounts.

Rates are set by the government on 1 February and 1 August. Add a line to
RATE_HISTORY when a new rate is announced; the right one is picked by date.
A rate typed on an account always wins over these references.
"""

from __future__ import annotations

import re
from datetime import date
from decimal import Decimal

D = Decimal

# (effective date, {account type: % per year})
RATE_HISTORY: list[tuple[date, dict[str, Decimal]]] = [
    (date(2023, 2, 1), {"livret_a": D("3.0"), "ldds": D("3.0"), "lep": D("6.1"), "cel": D("2.0")}),
    (date(2023, 8, 1), {"livret_a": D("3.0"), "ldds": D("3.0"), "lep": D("6.0"), "cel": D("2.0")}),
    (date(2024, 2, 1), {"livret_a": D("3.0"), "ldds": D("3.0"), "lep": D("5.0"), "cel": D("2.0")}),
    (date(2025, 2, 1), {"livret_a": D("2.4"), "ldds": D("2.4"), "lep": D("3.5"), "cel": D("1.5")}),
    (date(2025, 8, 1), {"livret_a": D("1.7"), "ldds": D("1.7"), "lep": D("2.7"), "cel": D("1.25")}),
    (date(2026, 2, 1), {"livret_a": D("1.5"), "ldds": D("1.5"), "lep": D("2.5"), "cel": D("1.0")}),
    (date(2026, 8, 1), {"livret_a": D("1.7"), "ldds": D("1.7"), "lep": D("2.5"), "cel": D("1.25")}),
]

# The Livret Jeune pays at least the Livret A rate (each bank may pay more).
_FLOOR_OF = {"livret_jeune": "livret_a"}


def reference_rate(account_type: str, on: date | None = None) -> tuple[Decimal, date] | None:
    """Return (rate %, effective since) for a regulated account type, or None."""
    on = on or date.today()
    key = _FLOOR_OF.get(account_type, account_type)
    for since, rates in sorted(RATE_HISTORY, key=lambda item: item[0], reverse=True):
        if since <= on and key in rates:
            return rates[key], since
    return None


def effective_rate(account_type: str, override: Decimal | None, on: date | None = None) -> tuple[Decimal | None, str]:
    """The rate that applies to an account and where it comes from."""
    if override is not None:
        return override, "saisi manuellement"
    ref = reference_rate(account_type, on)
    if ref is None:
        return None, ""
    rate, since = ref
    suffix = " (minimum légal)" if account_type in _FLOOR_OF else ""
    return rate, f"taux réglementé depuis le {since:%d/%m/%Y}{suffix}"


_KEYWORDS: list[tuple[str, tuple[str, ...]]] = [
    ("livret_a", ("livret a", "livret bleu")),
    ("ldds", ("ldds", "ldd ", "developpement durable", "développement durable")),
    ("lep", ("lep", "epargne populaire", "épargne populaire")),
    ("livret_jeune", ("livret jeune",)),
    ("pel", ("pel", "plan epargne logement", "plan d'épargne logement", "plan epargne-logement")),
    ("cel", ("cel", "compte epargne logement", "compte d'épargne logement")),
    ("pea", ("pea",)),
    ("per", ("per ", "plan epargne retraite", "plan d'épargne retraite")),
    ("life_insurance", ("assurance vie", "assurance-vie", "contrat vie")),
    ("cto", ("compte titres", "compte-titres", "cto", "ord. titres")),
    ("term_deposit", ("compte a terme", "compte à terme", "cat ")),
    ("mortgage", ("pret immo", "prêt immo", "credit immo", "crédit immo")),
    ("loan", ("pret ", "prêt ", "credit ", "crédit ")),
    ("savings", ("livret", "epargne", "épargne", "compte sur livret", "csl")),
    ("checking", ("compte courant", "compte de depot", "compte de dépôt", "compte cheque", "compte chèque")),
]


def guess_account_type(label: str, default: str = "checking") -> str:
    text = label.lower()
    for code, words in _KEYWORDS:
        for word in words:
            if re.search(r"(?<!\w)" + re.escape(word.strip()) + r"(?!\w)", text):
                return code
    return default
