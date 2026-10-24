"""What was the money spent on? A category for every operation.

The guess comes, in this order, from:
 1. the user's own rule for that merchant ("Carrefour is Courses"), kept per user;
 2. what the bank says the operation is (a loan instalment, a fee, a cash withdrawal, interest);
 3. well-known merchants and words in the label (supermarkets, rail, energy, telecom...);
 4. the kind of operation: a transfer to a person, an unknown direct debit...
and falls back to "Autres". Nothing is sent anywhere: it is plain keyword matching on the operation's own text.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from decimal import Decimal

from sqlalchemy import select
from sqlalchemy.orm import Session

from ..adapters.common import name_key, norm
from ..models import CategoryRule, Transaction
from . import access


@dataclass(frozen=True)
class Category:
    id: str
    label: str
    direction: str  # out | in | both
    keywords: tuple[str, ...] = ()


# Order matters: the first category whose words appear wins, so specific ones come first ("uber eats" before "uber").
# Words match whole words; a trailing * lets a word start something longer ("pizz*" is pizza, pizzeria).
OUT: tuple[Category, ...] = (
    Category("logement", "Logement", "out", ("loyer", "syndic", "foncia", "nexity", "charges copro", "copropriete", "agence immobiliere", "taxe fonciere", "taxe habitation")),
    Category("impots", "Impôts et taxes", "out", ("dgfip", "direction generale des finance*", "impot", "impots", "tresor public", "urssaf", "amende", "antai", "taxe")),
    Category("energie", "Énergie et eau", "out", ("edf", "engie", "totalenergies", "eaux du", "veolia", "suez", "saur", "electricite", "gaz de", "enedis", "eau")),
    Category("telecom", "Téléphone et internet", "out", ("orange", "sfr", "bouygues*", "free mobile", "freebox", "free telecom", "ovh*", "red by sfr", "sosh", "la poste mobile", "lebara")),
    Category("abonnements", "Abonnements", "out", ("netflix", "spotify", "disney*", "prime video", "amazon prime", "canal+", "canal plus", "deezer", "youtube*", "icloud", "apple.com/bill", "adobe", "dropbox", "openai", "chatgpt", "microsoft 365", "playstation plus", "xbox", "twitch", "patreon", "notion")),
    Category("epargne", "Épargne et investissement", "out", ("versement periodique", "livret*", "assurance vie", "assurance-vie", "pea", "coinbase", "binance", "bitstack", "kraken", "crypto*", "bitpanda", "trade republic", "degiro", "interactive brokers", "yomoni", "linxea", "ramify")),
    Category("assurances", "Assurances", "out", ("assurance*", "maif", "macif", "axa", "matmut", "groupama", "allianz", "april", "maaf", "gmf", "lemonade", "hiscox")),
    Category("sante", "Santé", "out", ("pharmacie*", "medecin*", "docteur", "dentiste", "doctolib", "hopital", "clinique", "optique", "opticien*", "laboratoire*", "kine*", "osteo*", "mutuelle", "harmonie mutuelle", "cpam")),
    Category("restaurants", "Restaurants et sorties", "out", ("uber eats", "ubereats", "deliveroo", "just eat", "restaurant*", "resto*", "mcdo", "mcdonald*", "burger king", "kfc", "quick", "subway", "brasserie*", "pizz*", "sushi*", "kebab*", "boulangerie*", "patisserie*", "cafe", "starbucks", "bar", "pub", "traiteur", "creperie*")),
    Category("courses", "Courses", "out", ("carrefour*", "leclerc", "auchan", "lidl", "intermarche", "monoprix", "franprix", "casino", "aldi", "picard", "biocoop", "super u", "hyper u", "u express", "netto", "cora", "grand frais", "naturalia", "supermarche*", "epicerie*", "primeur*", "boucherie*", "marche", "drive")),
    Category("voyages", "Voyages et vacances", "out", ("airbnb", "booking*", "ryanair", "easyjet", "air france", "transavia", "expedia", "hotel*", "hostel*", "abritel", "vacances", "camping*", "eurostar", "lufthansa", "volotea", "trainline", "ouigo")),
    Category("transport", "Transports", "out", ("sncf*", "trains de france", "ratp", "navigo", "uber", "bolt", "heetch", "blablacar", "total access", "esso", "shell", "station*", "bp", "peage*", "vinci autoroutes", "sanef", "aprr", "parking*", "indigo", "velib", "lime", "dott", "garage*", "norauto", "midas", "feu vert", "carburant*")),
    Category("shopping", "Achats et maison", "out", ("amazon", "fnac", "darty", "boulanger", "decathlon", "zara", "h&m", "cdiscount", "aliexpress", "zalando", "vinted", "ikea", "leroy merlin", "castorama", "bricorama", "brico*", "action", "kiabi", "uniqlo", "shein", "temu", "cultura", "gifi", "maisons du monde", "conforama")),
    Category("loisirs", "Loisirs et culture", "out", ("cinema*", "ugc", "pathe", "gaumont", "steam", "nintendo", "ticketmaster", "google play", "spectacle*", "musee*", "concert*", "theatre*", "librairie*", "gym", "basic fit", "fitness", "piscine*", "sport*")),
)
INCOME: tuple[Category, ...] = (
    Category("salaire", "Salaire et revenus", "in", ("salaire*", "paie", "remuneration*", "traitement", "indemnite*", "prime", "bulletin", "employeur")),
    Category("aides", "Aides et allocations", "in", ("caf", "france travail", "pole emploi", "cpam", "ameli", "allocation*", "msa", "apl", "prime d'activite", "cnav", "retraite*")),
    Category("remboursements", "Remboursements", "in", ("remboursement*", "rbt", "avoir", "refund", "annulation", "mutuelle")),
)
FIXED = {
    "credit": Category("credit", "Crédit et prêts", "out"),
    "frais": Category("frais", "Frais bancaires", "out"),
    "retraits": Category("retraits", "Retraits d'espèces", "out"),
    "proches": Category("proches", "Virements à des personnes", "out"),
    "prelevements": Category("prelevements", "Autres prélèvements", "out"),
    "autres": Category("autres", "Autres dépenses", "out"),
    "interets": Category("interets", "Intérêts", "in"),
    "recus": Category("recus", "Virements reçus", "in"),
    "autres_entrees": Category("autres_entrees", "Autres entrées", "in"),
    "interne": Category("interne", "Entre mes comptes", "both"),
}
ALL = {c.id: c for c in (*OUT, *INCOME, *FIXED.values())}


def _pattern(words: tuple[str, ...]) -> re.Pattern | None:
    parts = []
    for word in words:
        prefix = word.endswith("*")
        w = norm(word.rstrip("*")).strip()
        parts.append((r"\b" if w[0].isalnum() else "") + re.escape(w) + (r"\b" if w[-1].isalnum() and not prefix else ""))
    return re.compile("|".join(parts)) if parts else None


_PATTERNS = {c.id: _pattern(c.keywords) for c in (*OUT, *INCOME)}


def options(direction: str) -> list[Category]:
    """The categories one can choose for an operation going `direction` ("out" or "in")."""
    wanted = {"out": (*OUT, *(FIXED[k] for k in ("credit", "frais", "retraits", "proches", "prelevements", "autres"))),
              "in": (*INCOME, *(FIXED[k] for k in ("interets", "recus", "autres_entrees")))}[direction]
    return list(wanted) + [FIXED["interne"]]


def merchant(t: Transaction) -> tuple[str, str, int | None]:
    """(rule key, label to show, contact id) of the other party of an operation."""
    if t.counterparty is not None:
        return f"c{t.counterparty_id}", t.counterparty.name, t.counterparty_id
    label = (t.counterparty_name or t.label or "?").strip()
    return name_key(label) or label.lower(), label, None


def load_rules(session: Session) -> dict[str, str]:
    me = access.current()
    owner = me.id if me else None
    return {r.key: r.category for r in session.scalars(select(CategoryRule).where(CategoryRule.owner_id == owner))}


def set_rule(session: Session, key: str, category: str) -> None:
    """Remember where a merchant goes (`category` "auto" forgets the rule)."""
    if category != "auto" and category not in ALL:
        raise ValueError("Catégorie inconnue")
    me = access.current()
    owner = me.id if me else None
    row = session.scalar(select(CategoryRule).where(CategoryRule.owner_id == owner, CategoryRule.key == key))
    if category == "auto":
        if row is not None:
            session.delete(row)
    elif row is None:
        session.add(CategoryRule(owner_id=owner, key=key, category=category))
    else:
        row.category = category
    session.flush()


def categorize(t: Transaction, rules: dict[str, str]) -> str:
    """The id of the category of one operation."""
    if t.internal:
        return "interne"
    key, _, _ = merchant(t)
    chosen = rules.get(key)
    if chosen in ALL:
        return chosen
    income = Decimal(t.amount) >= 0
    if t.kind == "interest":
        return "interets"
    if not income:
        if t.kind == "loan_payment":
            return "credit"
        if t.kind == "fee":
            return "frais"
        if t.kind == "cash":
            return "retraits"
    text = norm(" ".join(x for x in (t.counterparty.name if t.counterparty else None, t.counterparty_name, t.label, t.memo) if x).replace("*", " "))
    text = f" {text} "
    for category in (INCOME if income else OUT):
        pattern = _PATTERNS[category.id]
        if pattern and pattern.search(text):
            return category.id
    if income:
        return "recus" if t.kind in ("transfer", "instant_transfer") else "autres_entrees"
    if t.kind in ("transfer", "instant_transfer"):
        return "proches"
    if t.kind == "direct_debit":
        return "prelevements"
    return "autres"
