"""Common interface of all connectors (bank aggregators, exchanges, wallets...)."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import Decimal
from typing import Any, ClassVar

import httpx


@dataclass
class Field:
    name: str
    label: str
    kind: str = "text"  # text | password | textarea | select | number
    required: bool = True
    help: str = ""
    options: list[tuple[str, str]] = field(default_factory=list)
    default: str = ""

    @property
    def secret(self) -> bool:
        return self.kind == "password" or (self.kind == "textarea" and "key" in self.name)


@dataclass
class HoldingData:
    symbol: str | None
    quantity: Decimal
    name: str | None = None
    isin: str | None = None
    asset_class: str = "crypto"
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
    external_id: str | None = None


@dataclass
class AccountData:
    external_id: str
    name: str
    account_type: str
    currency: str = "EUR"
    balance: Decimal = Decimal(0)
    institution: str | None = None
    iban: str | None = None
    # None = the source doesn't report positions for this account (plain bank account).
    holdings: list[HoldingData] | None = None
    transactions: list[TransactionData] = field(default_factory=list)


class NeedsAuth(Exception):
    """The access (bank consent, session...) must be renewed by the user."""


@dataclass
class Prompt:
    """A question the connector needs answered to finish logging in (2FA code...)."""

    message: str
    fields: list[Field] = field(default_factory=list)


class NeedsInteraction(Exception):
    def __init__(self, prompt: Prompt):
        super().__init__(prompt.message)
        self.prompt = prompt


@dataclass
class Context:
    connection_id: int
    redirect_url: str
    last_sync: datetime | None = None
    http: httpx.Client = field(default_factory=lambda: httpx.Client(timeout=30, headers={"User-Agent": "mymoney"}))


class Connector:
    type: ClassVar[str]
    label: ClassVar[str]
    category: ClassVar[str]  # banque | bourse | crypto | wallet
    description: ClassVar[str] = ""
    fields: ClassVar[list[Field]] = []
    # "redirect": the user logs in on the bank's site (open banking).
    # "interactive": the user must answer prompts (2FA) from the app.
    auth_flow: ClassVar[str | None] = None
    requires: ClassVar[str | None] = None  # optional Python package needed

    def __init__(self, config: dict[str, Any], state: dict[str, Any], ctx: Context):
        self.config = config
        self.state = state
        self.ctx = ctx

    @classmethod
    def available(cls) -> bool:
        if not cls.requires:
            return True
        try:
            __import__(cls.requires)
            return True
        except ImportError:
            return False

    def sync(self) -> list[AccountData]:
        raise NotImplementedError

    # redirect flow
    def start_auth(self) -> str:
        raise NotImplementedError

    def finish_auth(self, params: dict[str, str]) -> None:
        raise NotImplementedError

    # interactive flow: returns None once logged in, or the next prompt
    def login(self, answers: dict[str, str] | None) -> Prompt | None:
        raise NotImplementedError

    @property
    def authorized(self) -> bool:
        return True


def dec(value: Any) -> Decimal:
    if value is None or value == "":
        return Decimal(0)
    return Decimal(str(value))
