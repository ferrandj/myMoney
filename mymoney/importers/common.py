from __future__ import annotations

import re
import unicodedata
from dataclasses import dataclass, field
from datetime import date
from decimal import Decimal, InvalidOperation

from ..connectors.base import HoldingData, TransactionData


@dataclass
class ParsedStatement:
    source_format: str
    institution: str | None = None
    account_label: str | None = None
    account_number: str | None = None
    iban: str | None = None
    account_type: str | None = None
    currency: str = "EUR"
    balance: Decimal | None = None
    balance_date: date | None = None
    opening_balance: Decimal | None = None
    period_start: date | None = None
    period_end: date | None = None
    transactions: list[TransactionData] = field(default_factory=list)
    holdings: list[HoldingData] | None = None
    # Amounts found in documents that are not statements (e.g. "valeur de rachat" of a life insurance).
    candidate_balances: list[tuple[str, Decimal]] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    confidence: str = "medium"  # high | medium | low

    def to_json(self) -> dict:
        return {
            "source_format": self.source_format,
            "institution": self.institution,
            "account_label": self.account_label,
            "account_number": self.account_number,
            "iban": self.iban,
            "account_type": self.account_type,
            "currency": self.currency,
            "balance": str(self.balance) if self.balance is not None else None,
            "balance_date": self.balance_date.isoformat() if self.balance_date else None,
            "opening_balance": str(self.opening_balance) if self.opening_balance is not None else None,
            "period_start": self.period_start.isoformat() if self.period_start else None,
            "period_end": self.period_end.isoformat() if self.period_end else None,
            "transactions": [
                {"date": t.date.isoformat(), "label": t.label, "amount": str(t.amount), "currency": t.currency, "external_id": t.external_id}
                for t in self.transactions
            ],
            "holdings": None
            if self.holdings is None
            else [
                {"symbol": h.symbol, "isin": h.isin, "name": h.name, "quantity": str(h.quantity), "asset_class": h.asset_class,
                 "value": str(h.value) if h.value is not None else None, "value_currency": h.value_currency}
                for h in self.holdings
            ],
            "candidate_balances": [[label, str(amount)] for label, amount in self.candidate_balances],
            "warnings": self.warnings,
            "confidence": self.confidence,
        }

    @classmethod
    def from_json(cls, data: dict) -> "ParsedStatement":
        def d(v):
            return Decimal(v) if v is not None else None

        def dt(v):
            return date.fromisoformat(v) if v else None

        return cls(
            source_format=data["source_format"],
            institution=data.get("institution"),
            account_label=data.get("account_label"),
            account_number=data.get("account_number"),
            iban=data.get("iban"),
            account_type=data.get("account_type"),
            currency=data.get("currency") or "EUR",
            balance=d(data.get("balance")),
            balance_date=dt(data.get("balance_date")),
            opening_balance=d(data.get("opening_balance")),
            period_start=dt(data.get("period_start")),
            period_end=dt(data.get("period_end")),
            transactions=[
                TransactionData(date.fromisoformat(t["date"]), t["label"], Decimal(t["amount"]), t.get("currency") or "EUR", t.get("external_id"))
                for t in data.get("transactions", [])
            ],
            holdings=None
            if data.get("holdings") is None
            else [
                HoldingData(symbol=h["symbol"], isin=h.get("isin"), name=h.get("name"), quantity=Decimal(h["quantity"]),
                            asset_class=h.get("asset_class") or "other", value=d(h.get("value")), value_currency=h.get("value_currency"))
                for h in data["holdings"]
            ],
            candidate_balances=[(label, Decimal(v)) for label, v in data.get("candidate_balances", [])],
            warnings=data.get("warnings", []),
            confidence=data.get("confidence", "medium"),
        )


def strip_accents(text: str) -> str:
    return "".join(c for c in unicodedata.normalize("NFD", text) if unicodedata.category(c) != "Mn")


def norm(text: str) -> str:
    return re.sub(r"\s+", " ", strip_accents(text).lower()).strip()


_AMOUNT_CLEAN = re.compile(r"[€$£\s  ']|EUR|USD", re.I)


def parse_amount(raw: str | None) -> Decimal | None:
    """Parse "1 234,56", "-1,234.56", "1.234,56 €", "12,00-", "(45.10)"."""
    if raw is None:
        return None
    text = _AMOUNT_CLEAN.sub("", str(raw)).strip()
    if not text:
        return None
    negative = False
    if text.startswith("(") and text.endswith(")"):
        negative, text = True, text[1:-1]
    if text.endswith("-"):
        negative, text = True, text[:-1]
    if text.startswith("+"):
        text = text[1:]
    if text.startswith("-"):
        negative, text = True, text[1:]
    if "," in text and "." in text:
        if text.rfind(",") > text.rfind("."):
            text = text.replace(".", "").replace(",", ".")
        else:
            text = text.replace(",", "")
    elif "," in text:
        if re.fullmatch(r"\d{1,3}(,\d{3})+", text) and not re.search(r",\d{1,2}$", text):
            text = text.replace(",", "")  # 1,234,567 (thousands)
        else:
            text = text.replace(",", ".")
    elif text.count(".") > 1 and re.fullmatch(r"\d{1,3}(\.\d{3})+", text):
        text = text.replace(".", "")  # 1.234.567 (thousands)
    try:
        value = Decimal(text)
    except InvalidOperation:
        return None
    return -value if negative else value


_DATE_PATTERNS = [
    (re.compile(r"^(\d{4})-(\d{1,2})-(\d{1,2})"), ("y", "m", "d")),
    (re.compile(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{4})"), ("d", "m", "y")),
    (re.compile(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2})$"), ("d", "m", "yy")),
    (re.compile(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2})\b"), ("d", "m", "yy")),
]
_MONTHS = {
    "janv": 1, "jan": 1, "fevr": 2, "fev": 2, "feb": 2, "mars": 3, "mar": 3, "avr": 4, "apr": 4, "mai": 5, "may": 5,
    "juin": 6, "jun": 6, "juil": 7, "jul": 7, "aout": 8, "aug": 8, "sept": 9, "sep": 9, "oct": 10, "nov": 11,
    "dec": 12, "janvier": 1, "fevrier": 2, "avril": 4, "juillet": 7, "septembre": 9, "octobre": 10, "novembre": 11, "decembre": 12,
}


def parse_date(raw: str | None, default_year: int | None = None) -> date | None:
    if not raw:
        return None
    text = str(raw).strip()
    for pattern, order in _DATE_PATTERNS:
        m = pattern.match(text)
        if m:
            parts = dict(zip(order, m.groups()))
            year = int(parts.get("y") or 0) or 2000 + int(parts["yy"])
            try:
                return date(year, int(parts["m"]), int(parts["d"]))
            except ValueError:
                return None
    m = re.match(r"^(\d{1,2})\s+([a-zA-Zéèûô]+)\.?\s+(\d{4})", text)
    if m:
        month = _MONTHS.get(strip_accents(m.group(2).lower()))
        if month:
            try:
                return date(int(m.group(3)), month, int(m.group(1)))
            except ValueError:
                return None
    m = re.match(r"^(\d{1,2})[/.](\d{1,2})$", text)
    if m and default_year:
        try:
            return date(default_year, int(m.group(2)), int(m.group(1)))
        except ValueError:
            return None
    return None


_IBAN = re.compile(r"\b([A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){3,7}(?:[ ]?[A-Z0-9]{1,3})?)\b")


def iban_valid(iban: str) -> bool:
    iban = iban.replace(" ", "").upper()
    if not re.fullmatch(r"[A-Z]{2}\d{2}[A-Z0-9]{10,30}", iban):
        return False
    rearranged = iban[4:] + iban[:4]
    digits = "".join(str(int(c, 36)) for c in rearranged)
    return int(digits) % 97 == 1


def find_iban(text: str) -> str | None:
    for m in _IBAN.finditer(text.upper()):
        candidate = m.group(1).replace(" ", "")
        # The regex may grab a trailing word: shrink until the checksum matches.
        for end in range(len(candidate), 13, -1):
            if iban_valid(candidate[:end]):
                return candidate[:end]
    return None


INSTITUTIONS = [
    ("Fortuneo", ("fortuneo",)), ("BforBank", ("bforbank",)), ("Hello bank!", ("hello bank", "hellobank")),
    ("BoursoBank", ("boursorama", "boursobank")), ("BNP Paribas", ("bnp paribas",)), ("Société Générale", ("societe generale",)),
    ("Crédit Agricole", ("credit agricole",)), ("LCL", ("lcl", "credit lyonnais")), ("Caisse d'Épargne", ("caisse d'epargne", "caisse depargne")),
    ("Banque Populaire", ("banque populaire",)), ("Crédit Mutuel", ("credit mutuel",)), ("CIC", ("cic ",)),
    ("La Banque Postale", ("banque postale",)), ("ING", ("ing direct", "ing bank")), ("Revolut", ("revolut",)),
    ("Trade Republic", ("trade republic",)), ("Cashbee", ("cashbee",)), ("Nexo", ("nexo",)), ("Linxea", ("linxea",)),
    ("Spirica", ("spirica",)), ("Generali", ("generali",)), ("Suravenir", ("suravenir",)), ("Yomoni", ("yomoni",)),
    ("Nalo", ("nalo",)), ("Binance", ("binance",)), ("Kraken", ("kraken", "payward")), ("Monabanq", ("monabanq",)),
    ("Shine", ("shine",)), ("Qonto", ("qonto",)), ("N26", ("n26",)), ("AXA", ("axa banque", "axa ")), ("Ramify", ("ramify",)),
]


def find_institution(text: str) -> str | None:
    lowered = norm(text[:5000])
    best = None
    for name, keys in INSTITUTIONS:
        for key in keys:
            pos = lowered.find(key)
            if pos >= 0 and (best is None or pos < best[0]):
                best = (pos, name)
    return best[1] if best else None
