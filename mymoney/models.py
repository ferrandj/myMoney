from __future__ import annotations

from datetime import date, datetime, timezone
from decimal import Decimal

from sqlalchemy import (
    JSON,
    Boolean,
    Date,
    DateTime,
    ForeignKey,
    Integer,
    LargeBinary,
    String,
    Text,
    TypeDecorator,
    UniqueConstraint,
)
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship


def utcnow() -> datetime:
    return datetime.now(timezone.utc).replace(tzinfo=None)


class DecimalText(TypeDecorator):
    """Exact decimals stored as text (SQLite has no native decimal type)."""

    impl = String(64)
    cache_ok = True

    def process_bind_param(self, value, dialect):
        return None if value is None else str(Decimal(value))

    def process_result_value(self, value, dialect):
        return None if value is None else Decimal(value)


# code -> (label, category)
ACCOUNT_TYPES: dict[str, tuple[str, str]] = {
    "checking": ("Compte courant", "liquidites"),
    "card": ("Carte (débit différé)", "liquidites"),
    "cash": ("Espèces", "liquidites"),
    "livret_a": ("Livret A", "epargne"),
    "ldds": ("LDDS", "epargne"),
    "lep": ("LEP", "epargne"),
    "livret_jeune": ("Livret Jeune", "epargne"),
    "cel": ("CEL", "epargne"),
    "pel": ("PEL", "epargne"),
    "savings": ("Livret / épargne bancaire", "epargne"),
    "term_deposit": ("Compte à terme", "epargne"),
    "pea": ("PEA", "investissements"),
    "cto": ("Compte-titres", "investissements"),
    "life_insurance": ("Assurance-vie", "investissements"),
    "per": ("PER", "investissements"),
    "employee_savings": ("Épargne salariale (PEE/PERCO)", "investissements"),
    "crowdfunding": ("Crowdfunding / crowdlending", "investissements"),
    "scpi": ("SCPI", "immobilier"),
    "crypto_exchange": ("Plateforme crypto", "crypto"),
    "crypto_wallet": ("Wallet crypto", "crypto"),
    "real_estate": ("Bien immobilier", "immobilier"),
    "other_asset": ("Autre actif", "autres"),
    "loan": ("Crédit", "dettes"),
    "mortgage": ("Crédit immobilier", "dettes"),
}

CATEGORIES: dict[str, str] = {
    "liquidites": "Liquidités",
    "epargne": "Épargne",
    "investissements": "Investissements",
    "crypto": "Crypto",
    "immobilier": "Immobilier",
    "autres": "Autres",
    "dettes": "Dettes",
}


class Base(DeclarativeBase):
    type_annotation_map = {Decimal: DecimalText()}


class User(Base):
    """A person who can sign in. Their password is not stored here: it opens their own key slot in the keystore."""

    __tablename__ = "users"

    id: Mapped[int] = mapped_column(primary_key=True)
    email: Mapped[str] = mapped_column(String(320), unique=True)  # lower case: the identity Cloudflare Access vouches for
    name: Mapped[str | None] = mapped_column(String(200))
    is_admin: Mapped[bool] = mapped_column(Boolean, default=False)
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)
    last_login: Mapped[datetime | None] = mapped_column(DateTime)

    @property
    def label(self) -> str:
        return self.name or self.email


class AccountShare(Base):
    """An account its owner shares with another user: read only, or read and edit."""

    __tablename__ = "account_shares"
    __table_args__ = (UniqueConstraint("account_id", "user_id"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"), index=True)
    user_id: Mapped[int] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    can_edit: Mapped[bool] = mapped_column(Boolean, default=False)
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)

    user: Mapped["User"] = relationship()


class Account(Base):
    __tablename__ = "accounts"

    id: Mapped[int] = mapped_column(primary_key=True)
    name: Mapped[str] = mapped_column(String(200))
    owner_id: Mapped[int | None] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    institution: Mapped[str | None] = mapped_column(String(200))
    account_type: Mapped[str] = mapped_column(String(40), default="checking")
    currency: Mapped[str] = mapped_column(String(10), default="EUR")
    # Cash balance in `currency`. Accounts with holdings add the holdings' value on top.
    balance: Mapped[Decimal] = mapped_column(default=Decimal(0))
    balance_date: Mapped[datetime | None] = mapped_column(DateTime)
    iban: Mapped[str | None] = mapped_column(String(40))
    # Identity of the account and of its holder, read from the documents (or typed by hand).
    bic: Mapped[str | None] = mapped_column(String(20))
    bank_code: Mapped[str | None] = mapped_column(String(10))
    branch_code: Mapped[str | None] = mapped_column(String(10))
    account_number: Mapped[str | None] = mapped_column(String(40))
    rib_key: Mapped[str | None] = mapped_column(String(4))
    holder_name: Mapped[str | None] = mapped_column(String(300))
    # The holder line exactly as the bank prints it (possibly cut): other accounts' statements print the same text
    # when they mention this account, which is how a transfer to it is recognised as "between my own accounts".
    holder_alias: Mapped[str | None] = mapped_column(String(300))
    holder_address: Mapped[str | None] = mapped_column(Text)
    # Interest rate in % per year. None = use the reference rate of the account type.
    interest_rate: Mapped[Decimal | None] = mapped_column()
    archived: Mapped[bool] = mapped_column(Boolean, default=False)
    notes: Mapped[str | None] = mapped_column(Text)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    holdings: Mapped[list["Holding"]] = relationship(back_populates="account", cascade="all, delete-orphan")
    transactions: Mapped[list["Transaction"]] = relationship(back_populates="account", cascade="all, delete-orphan")
    statements: Mapped[list["Statement"]] = relationship(back_populates="account", cascade="all, delete-orphan")
    loan: Mapped["Loan | None"] = relationship(back_populates="account", cascade="all, delete-orphan", uselist=False)
    shares: Mapped[list["AccountShare"]] = relationship(cascade="all, delete-orphan", order_by="AccountShare.id")
    owner: Mapped["User | None"] = relationship(foreign_keys=[owner_id])

    @property
    def rib(self) -> str | None:
        parts = [self.bank_code, self.branch_code, self.account_number, self.rib_key]
        return " ".join(parts) if all(parts) else None

    @property
    def type_label(self) -> str:
        return ACCOUNT_TYPES.get(self.account_type, (self.account_type, "autres"))[0]

    @property
    def category(self) -> str:
        return ACCOUNT_TYPES.get(self.account_type, ("", "autres"))[1]


class Holding(Base):
    """A position inside an account: a crypto asset, a share, a fund unit..."""

    __tablename__ = "holdings"

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"))
    symbol: Mapped[str | None] = mapped_column(String(60))
    isin: Mapped[str | None] = mapped_column(String(20))
    name: Mapped[str | None] = mapped_column(String(200))
    asset_class: Mapped[str] = mapped_column(String(20), default="other")  # crypto|stock|etf|fund|bond|other
    quantity: Mapped[Decimal] = mapped_column(default=Decimal(0))
    # Value reported by the source, when it provides one.
    unit_price: Mapped[Decimal | None] = mapped_column()
    value: Mapped[Decimal | None] = mapped_column()
    value_currency: Mapped[str | None] = mapped_column(String(10))
    cost_basis: Mapped[Decimal | None] = mapped_column()
    # Hints for price lookup, e.g. {"coingecko_id": "bitcoin"} or {"yahoo": "CW8.PA"}.
    extra: Mapped[dict | None] = mapped_column(JSON)
    manual: Mapped[bool] = mapped_column(Boolean, default=False)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    account: Mapped[Account] = relationship(back_populates="holdings")


class Document(Base):
    __tablename__ = "documents"

    id: Mapped[int] = mapped_column(primary_key=True)
    filename: Mapped[str] = mapped_column(String(300))
    owner_id: Mapped[int | None] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)  # who dropped it
    mime: Mapped[str | None] = mapped_column(String(100))
    size: Mapped[int] = mapped_column(Integer)
    sha256: Mapped[str] = mapped_column(String(64), unique=True)
    # Name of the encrypted file in data/documents (random, reveals nothing).
    blob_name: Mapped[str] = mapped_column(String(80))
    kind: Mapped[str] = mapped_column(String(30), default="statement")
    account_id: Mapped[int | None] = mapped_column(ForeignKey("accounts.id", ondelete="SET NULL"))
    # uploaded (stored only) | needs_choice (several adapters fit: the user picks) | parsed | imported | failed
    # | duplicate (nothing new compared with an imported file: set aside, deleted after a day unless kept)
    # | copy (a duplicate the user chose to keep: nothing to import)
    status: Mapped[str] = mapped_column(String(20), default="uploaded")
    # The imported document this one repeats (same account, same period), when there is one.
    duplicate_of: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    adapter_id: Mapped[str | None] = mapped_column(String(60))
    # {"adapter", "candidates", "decision", "message", "statements": [...], "error"}
    parse_result: Mapped[dict | None] = mapped_column(JSON)
    uploaded_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)


class Transaction(Base):
    __tablename__ = "transactions"
    __table_args__ = (UniqueConstraint("account_id", "fingerprint"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"), index=True)
    fingerprint: Mapped[str] = mapped_column(String(64))
    date: Mapped[date] = mapped_column(Date, index=True)
    label: Mapped[str] = mapped_column(String(500))
    amount: Mapped[Decimal] = mapped_column()
    currency: Mapped[str] = mapped_column(String(10), default="EUR")
    category: Mapped[str | None] = mapped_column(String(60))
    source: Mapped[str] = mapped_column(String(20), default="import")  # import|manual
    document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)
    value_date: Mapped[date | None] = mapped_column(Date)
    # card | transfer | instant_transfer | direct_debit | loan_payment | fee | cash | cheque | interest | other
    kind: Mapped[str | None] = mapped_column(String(20))
    # The other party as printed by the bank, their IBAN when shown, and the contact they were matched to.
    counterparty_name: Mapped[str | None] = mapped_column(String(300))
    counterparty_iban: Mapped[str | None] = mapped_column(String(40), index=True)
    counterparty_id: Mapped[int | None] = mapped_column(ForeignKey("counterparties.id", ondelete="SET NULL"), index=True)
    reference: Mapped[str | None] = mapped_column(String(200))
    memo: Mapped[str | None] = mapped_column(Text)
    raw_label: Mapped[str | None] = mapped_column(Text)
    details: Mapped[dict | None] = mapped_column(JSON)

    account: Mapped[Account] = relationship(back_populates="transactions")
    counterparty: Mapped["Counterparty | None"] = relationship(back_populates="transactions")

    @property
    def internal(self) -> bool:
        return bool((self.details or {}).get("internal"))


class Statement(Base):
    """One imported statement: the period it covers and its balances, to check that periods follow each other."""

    __tablename__ = "statements"

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"), index=True)
    document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    period_start: Mapped[date | None] = mapped_column(Date)
    period_end: Mapped[date | None] = mapped_column(Date)
    opening_balance: Mapped[Decimal | None] = mapped_column()
    closing_balance: Mapped[Decimal | None] = mapped_column()
    tx_count: Mapped[int] = mapped_column(Integer, default=0)
    added_count: Mapped[int] = mapped_column(Integer, default=0)
    imported_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)

    account: Mapped[Account] = relationship(back_populates="statements")


class FeeSummary(Base):
    """The yearly recap of an account's fees and interest, as the bank states it (no operations in it)."""

    __tablename__ = "fee_summaries"

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"), index=True)
    document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    period_start: Mapped[date] = mapped_column(Date)
    period_end: Mapped[date] = mapped_column(Date)
    fees_total: Mapped[Decimal] = mapped_column(default=Decimal(0))
    interest_paid: Mapped[Decimal | None] = mapped_column()
    interest_earned: Mapped[Decimal | None] = mapped_column()  # None: "intérêts non applicables"
    # [{"category", "name", "used", "unit_price", "billed", "total"}]
    services: Mapped[list | None] = mapped_column(JSON)
    imported_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)


class Loan(Base):
    """What the documents of a loan say about it. The account (type "mortgage" or "loan") holds the balance."""

    __tablename__ = "loans"

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"), unique=True)
    reference: Mapped[str | None] = mapped_column(String(60), index=True)  # the loan's number
    project_reference: Mapped[str | None] = mapped_column(String(60))
    product: Mapped[str | None] = mapped_column(String(120))
    borrower: Mapped[str | None] = mapped_column(String(300))
    purpose: Mapped[str | None] = mapped_column(String(200))  # "Achat ancien · Résidence principale"
    # From the offer.
    principal: Mapped[Decimal | None] = mapped_column()
    duration_months: Mapped[int | None] = mapped_column(Integer)
    rate: Mapped[Decimal | None] = mapped_column()  # % per year, fixed
    taeg: Mapped[Decimal | None] = mapped_column()
    interest_total: Mapped[Decimal | None] = mapped_column()
    insurance_mandatory: Mapped[Decimal | None] = mapped_column()
    insurance_optional: Mapped[Decimal | None] = mapped_column()
    guarantee_fees: Mapped[Decimal | None] = mapped_column()
    total_cost: Mapped[Decimal | None] = mapped_column()
    offer_date: Mapped[date | None] = mapped_column(Date)
    # Date of the first instalment: the offer only numbers them, a later document (or the user) gives the dates.
    first_due_date: Mapped[date | None] = mapped_column(Date)
    end_date: Mapped[date | None] = mapped_column(Date)  # as stated by the latest document
    offer_document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    schedule_document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    account: Mapped[Account] = relationship(back_populates="loan")
    installments: Mapped[list["LoanInstallment"]] = relationship(back_populates="loan", cascade="all, delete-orphan", order_by="LoanInstallment.id")
    events: Mapped[list["LoanEvent"]] = relationship(back_populates="loan", cascade="all, delete-orphan", order_by="LoanEvent.on_date")


class LoanInstallment(Base):
    """One line of an amortization table: the original one in the offer (numbered) or the current one (dated)."""

    __tablename__ = "loan_installments"

    id: Mapped[int] = mapped_column(primary_key=True)
    loan_id: Mapped[int] = mapped_column(ForeignKey("loans.id", ondelete="CASCADE"), index=True)
    document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"), index=True)
    source: Mapped[str] = mapped_column(String(10))  # offer | schedule
    number: Mapped[int | None] = mapped_column(Integer)
    due_date: Mapped[date | None] = mapped_column(Date)
    capital: Mapped[Decimal] = mapped_column()
    interest: Mapped[Decimal] = mapped_column()
    deferred_interest: Mapped[Decimal] = mapped_column(default=Decimal(0))
    insurance: Mapped[Decimal] = mapped_column(default=Decimal(0))
    total: Mapped[Decimal] = mapped_column()
    remaining: Mapped[Decimal] = mapped_column()  # capital still owed after this instalment

    loan: Mapped[Loan] = relationship(back_populates="installments")


class LoanEvent(Base):
    """Something that changed the loan: a partial early repayment, as the bank's notice states it."""

    __tablename__ = "loan_events"

    id: Mapped[int] = mapped_column(primary_key=True)
    loan_id: Mapped[int] = mapped_column(ForeignKey("loans.id", ondelete="CASCADE"), index=True)
    document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"), index=True)
    kind: Mapped[str] = mapped_column(String(20), default="prepayment")
    on_date: Mapped[date] = mapped_column(Date)  # the day the repayment is accounted for
    issued_on: Mapped[date | None] = mapped_column(Date)
    capital_before: Mapped[Decimal] = mapped_column()
    capital_after: Mapped[Decimal] = mapped_column()
    last_due_date: Mapped[date | None] = mapped_column(Date)  # last instalment paid before the event
    last_payment: Mapped[Decimal | None] = mapped_column()
    first_due_date: Mapped[date | None] = mapped_column(Date)  # first instalment of the new plan
    first_payment: Mapped[Decimal | None] = mapped_column()
    end_before: Mapped[date | None] = mapped_column(Date)
    end_after: Mapped[date | None] = mapped_column(Date)

    loan: Mapped[Loan] = relationship(back_populates="events")


class CategoryRule(Base):
    """"This merchant always goes in that category": the user's answer, which wins over the automatic guess."""

    __tablename__ = "category_rules"
    __table_args__ = (UniqueConstraint("owner_id", "key"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    owner_id: Mapped[int | None] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    key: Mapped[str] = mapped_column(String(300))  # "c<contact id>" or the merchant's name as the bank prints it, simplified
    category: Mapped[str] = mapped_column(String(40))
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)


class GapAck(Base):
    """A stretch of days without statement that the user said is normal: it is not reported any more."""

    __tablename__ = "gap_acks"

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"), index=True)
    period_start: Mapped[date] = mapped_column(Date)
    period_end: Mapped[date] = mapped_column(Date)
    note: Mapped[str | None] = mapped_column(Text)
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)


class Counterparty(Base):
    """Someone or something money goes to or comes from, remembered by IBAN and by name for the next import."""

    __tablename__ = "counterparties"

    id: Mapped[int] = mapped_column(primary_key=True)
    name: Mapped[str] = mapped_column(String(200))
    kind: Mapped[str] = mapped_column(String(20), default="person")  # person | company | self | other
    phone: Mapped[str | None] = mapped_column(String(40))
    email: Mapped[str | None] = mapped_column(String(200))
    notes: Mapped[str | None] = mapped_column(Text)
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)

    identifiers: Mapped[list["CounterpartyIdentifier"]] = relationship(
        back_populates="counterparty", cascade="all, delete-orphan", order_by="CounterpartyIdentifier.id"
    )
    transactions: Mapped[list[Transaction]] = relationship(back_populates="counterparty")


COUNTERPARTY_KINDS = {"person": "Personne", "company": "Entreprise / organisme", "self": "Moi (autre compte)", "other": "Autre"}


class CounterpartyIdentifier(Base):
    """What lets an import recognise a contact: an IBAN (exact) or a name as banks print it (normalised key)."""

    __tablename__ = "counterparty_identifiers"
    __table_args__ = (UniqueConstraint("kind", "value"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    counterparty_id: Mapped[int] = mapped_column(ForeignKey("counterparties.id", ondelete="CASCADE"), index=True)
    kind: Mapped[str] = mapped_column(String(10))  # iban | name
    value: Mapped[str] = mapped_column(String(200))  # IBAN without spaces, or the name key
    label: Mapped[str | None] = mapped_column(String(300))  # the name as printed, for display

    counterparty: Mapped[Counterparty] = relationship(back_populates="identifiers")


class Snapshot(Base):
    """Daily value of each account in the base currency, to draw the history."""

    __tablename__ = "snapshots"
    __table_args__ = (UniqueConstraint("account_id", "day"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    account_id: Mapped[int] = mapped_column(ForeignKey("accounts.id", ondelete="CASCADE"))
    day: Mapped[date] = mapped_column(Date, index=True)
    value: Mapped[Decimal] = mapped_column()


class PriceCache(Base):
    __tablename__ = "price_cache"

    key: Mapped[str] = mapped_column(String(200), primary_key=True)
    value: Mapped[str] = mapped_column(Text)
    fetched_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)


class Setting(Base):
    __tablename__ = "settings"

    key: Mapped[str] = mapped_column(String(100), primary_key=True)
    value: Mapped[str] = mapped_column(Text)
