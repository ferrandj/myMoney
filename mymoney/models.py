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


class Connection(Base):
    __tablename__ = "connections"

    id: Mapped[int] = mapped_column(primary_key=True)
    name: Mapped[str] = mapped_column(String(200))
    connector_type: Mapped[str] = mapped_column(String(50))
    # Credentials and API keys: encrypted a second time on top of the database encryption.
    config_enc: Mapped[bytes | None] = mapped_column(LargeBinary)
    # Sessions, tokens, cookies kept between syncs (also encrypted).
    state_enc: Mapped[bytes | None] = mapped_column(LargeBinary)
    status: Mapped[str] = mapped_column(String(30), default="new")  # new|ok|error|needs_auth
    last_sync_at: Mapped[datetime | None] = mapped_column(DateTime)
    last_error: Mapped[str | None] = mapped_column(Text)
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)

    accounts: Mapped[list["Account"]] = relationship(back_populates="connection")


class Account(Base):
    __tablename__ = "accounts"
    __table_args__ = (UniqueConstraint("connection_id", "external_id"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    connection_id: Mapped[int | None] = mapped_column(ForeignKey("connections.id", ondelete="SET NULL"))
    external_id: Mapped[str | None] = mapped_column(String(200))
    name: Mapped[str] = mapped_column(String(200))
    institution: Mapped[str | None] = mapped_column(String(200))
    account_type: Mapped[str] = mapped_column(String(40), default="checking")
    currency: Mapped[str] = mapped_column(String(10), default="EUR")
    # Cash balance in `currency`. Accounts with holdings add the holdings' value on top.
    balance: Mapped[Decimal] = mapped_column(default=Decimal(0))
    balance_date: Mapped[datetime | None] = mapped_column(DateTime)
    iban: Mapped[str | None] = mapped_column(String(40))
    # Interest rate in % per year. None = use the reference rate of the account type.
    interest_rate: Mapped[Decimal | None] = mapped_column()
    archived: Mapped[bool] = mapped_column(Boolean, default=False)
    notes: Mapped[str | None] = mapped_column(Text)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    connection: Mapped[Connection | None] = relationship(back_populates="accounts")
    holdings: Mapped[list["Holding"]] = relationship(back_populates="account", cascade="all, delete-orphan")
    transactions: Mapped[list["Transaction"]] = relationship(back_populates="account", cascade="all, delete-orphan")

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
    # Hints for price lookup, e.g. {"chain": "ethereum", "contract": "0x..."} or {"coingecko_id": "bitcoin"}.
    extra: Mapped[dict | None] = mapped_column(JSON)
    manual: Mapped[bool] = mapped_column(Boolean, default=False)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    account: Mapped[Account] = relationship(back_populates="holdings")


class Document(Base):
    __tablename__ = "documents"

    id: Mapped[int] = mapped_column(primary_key=True)
    filename: Mapped[str] = mapped_column(String(300))
    mime: Mapped[str | None] = mapped_column(String(100))
    size: Mapped[int] = mapped_column(Integer)
    sha256: Mapped[str] = mapped_column(String(64), unique=True)
    # Name of the encrypted file in data/documents (random, reveals nothing).
    blob_name: Mapped[str] = mapped_column(String(80))
    kind: Mapped[str] = mapped_column(String(30), default="statement")
    account_id: Mapped[int | None] = mapped_column(ForeignKey("accounts.id", ondelete="SET NULL"))
    status: Mapped[str] = mapped_column(String(20), default="uploaded")  # uploaded|parsed|imported|failed
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
    source: Mapped[str] = mapped_column(String(20), default="sync")  # sync|import|manual
    document_id: Mapped[int | None] = mapped_column(ForeignKey("documents.id", ondelete="SET NULL"))
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)

    account: Mapped[Account] = relationship(back_populates="transactions")


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
