"""What an adapter receives (a dropped file), what it returns (parsed statements) and the contract it follows."""

from __future__ import annotations

from dataclasses import dataclass, field, fields
from datetime import date
from decimal import Decimal
from functools import cached_property
from typing import Any, ClassVar

from .common import strip_accents


@dataclass
class HoldingData:
    symbol: str | None
    quantity: Decimal
    name: str | None = None
    isin: str | None = None
    asset_class: str = "other"
    unit_price: Decimal | None = None
    value: Decimal | None = None
    value_currency: str | None = None
    cost_basis: Decimal | None = None
    extra: dict | None = None


@dataclass
class TransactionData:
    date: date
    label: str
    amount: Decimal
    currency: str = "EUR"
    # Id given by the source when it has a really unique one (bank transfer reference, OFX FITID...).
    external_id: str | None = None
    value_date: date | None = None
    # card | transfer | instant_transfer | direct_debit | loan_payment | fee | cash | cheque | interest | other
    kind: str | None = None
    # Who the money went to / came from, as printed by the bank, and their IBAN when it shows.
    counterparty: str | None = None
    counterparty_iban: str | None = None
    reference: str | None = None
    memo: str | None = None
    # The full original text of the line: fingerprints are computed from it so they survive label clean-ups.
    raw: str | None = None
    details: dict[str, Any] | None = None


@dataclass
class ParsedStatement:
    source_format: str
    institution: str | None = None
    adapter_id: str | None = None
    document_kind: str | None = None
    account_label: str | None = None
    account_type: str | None = None
    currency: str = "EUR"
    # Identity of the account and of its holder, as printed on the document.
    holder_name: str | None = None
    holder_address: str | None = None
    iban: str | None = None
    bic: str | None = None
    bank_code: str | None = None
    branch_code: str | None = None
    account_number: str | None = None
    rib_key: str | None = None
    balance: Decimal | None = None
    balance_date: date | None = None
    opening_balance: Decimal | None = None
    opening_date: date | None = None
    period_start: date | None = None
    period_end: date | None = None
    transactions: list[TransactionData] = field(default_factory=list)
    holdings: list[HoldingData] | None = None
    # Amounts found in documents that are not statements (e.g. "valeur de rachat" of a life insurance).
    candidate_balances: list[tuple[str, Decimal]] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    confidence: str = "medium"  # high | medium | low
    extra: dict[str, Any] = field(default_factory=dict)

    # -- JSON (stored with the document, in the encrypted database) -----------------------------------------
    def to_json(self) -> dict:
        return {
            "transactions": [_dump(t) for t in self.transactions],
            "holdings": None if self.holdings is None else [_dump(h) for h in self.holdings],
            "candidate_balances": [[label, str(amount)] for label, amount in self.candidate_balances],
            **{f.name: _dump_value(getattr(self, f.name)) for f in fields(self) if f.name not in ("transactions", "holdings", "candidate_balances")},
        }

    @classmethod
    def from_json(cls, data: dict) -> "ParsedStatement":
        values: dict[str, Any] = {}
        for f in fields(cls):
            if f.name in ("transactions", "holdings", "candidate_balances") or f.name not in data:
                continue
            values[f.name] = _load_value(f.name, data[f.name])
        st = cls(**values)
        st.transactions = [_load(TransactionData, t) for t in data.get("transactions", [])]
        st.holdings = None if data.get("holdings") is None else [_load(HoldingData, h) for h in data["holdings"]]
        st.candidate_balances = [(label, Decimal(v)) for label, v in data.get("candidate_balances", [])]
        return st


_DATE_FIELDS = {"balance_date", "opening_date", "period_start", "period_end", "date", "value_date"}
_DECIMAL_FIELDS = {"balance", "opening_balance", "amount", "quantity", "unit_price", "value", "cost_basis"}


def _dump_value(value: Any) -> Any:
    if isinstance(value, (date, Decimal)):
        return value.isoformat() if isinstance(value, date) else str(value)
    return value


def _dump(obj: Any) -> dict:
    return {f.name: _dump_value(getattr(obj, f.name)) for f in fields(obj)}


def _load_value(name: str, value: Any) -> Any:
    if value is None:
        return None
    if name in _DATE_FIELDS:
        return date.fromisoformat(value)
    if name in _DECIMAL_FIELDS:
        return Decimal(value)
    return value


def _load(cls, data: dict):
    known = {f.name for f in fields(cls)}
    return cls(**{k: _load_value(k, v) for k, v in data.items() if k in known})


# ------------------------------------------------------------------------------------- source document
class SourceDocument:
    """A dropped file. Text and layout are extracted once and shared by every adapter that looks at it."""

    def __init__(self, filename: str, data: bytes):
        self.filename = filename
        self.data = data

    @cached_property
    def format(self) -> str:
        name = self.filename.lower()
        head = self.data[:2048].lstrip()
        if head.startswith(b"%PDF"):
            return "pdf"
        if name.endswith((".ofx", ".qfx")) or b"OFXHEADER" in head or b"<OFX>" in head.upper():
            return "ofx"
        if name.endswith(".qif") or head.startswith(b"!Type"):
            return "qif"
        if name.endswith((".csv", ".txt", ".tsv")):
            return "csv"
        return "unknown"

    @cached_property
    def text(self) -> str:
        """Plain text: the text layer of a PDF, or the decoded content of a text file."""
        if self.format == "pdf":
            from . import pdftools

            return pdftools.extract_text(self.data)
        return decode_text(self.data)

    @cached_property
    def compact(self) -> str:
        """Lower-case, accent-free text without any whitespace: markers survive PDFs that lose their spaces."""
        return "".join(strip_accents(self.text).lower().split())

    def words(self, x_tolerance: float = 1.0) -> list[list[dict]]:
        """Words of each PDF page with their position (x0, x1, top), accents repaired."""
        from . import pdftools

        cache = self.__dict__.setdefault("_words", {})
        if x_tolerance not in cache:
            cache[x_tolerance] = pdftools.extract_words(self.data, x_tolerance)
        return cache[x_tolerance]


def decode_text(data: bytes) -> str:
    for encoding in ("utf-8-sig", "cp1252", "latin-1"):
        try:
            return data.decode(encoding)
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", errors="replace")


# --------------------------------------------------------------------------------------------- contract
@dataclass
class Detection:
    """How sure an adapter is that a document is one it knows, and why (shown to the user when they must choose)."""

    score: float = 0.0
    reasons: list[str] = field(default_factory=list)


class Adapter:
    """Reads one kind of document from one institution.

    An institution usually has several (monthly statement PDF, CSV export, savings statement...): each is its own
    adapter, with its own `detect` that looks for what distinguishes that document from the others.
    `detect` must be cheap, never raise and never read anything personal into the result: it only scores.
    """

    id: ClassVar[str]  # "<institution>.<document kind>", e.g. "boursobank.statement_pdf"
    institution: ClassVar[str | None]  # None for generic fallbacks
    kind: ClassVar[str]
    label: ClassVar[str]
    description: ClassVar[str] = ""
    formats: ClassVar[tuple[str, ...]] = ()
    # Generic fallbacks guess from the content: they are never trusted without the user's confirmation.
    generic: ClassVar[bool] = False

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        raise NotImplementedError

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        raise NotImplementedError

