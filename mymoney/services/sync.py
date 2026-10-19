"""Store what connectors and importers return."""

from __future__ import annotations

import hashlib
import re
from collections import Counter
from datetime import date
from decimal import Decimal

from sqlalchemy import select
from sqlalchemy.orm import Session

from ..connectors.base import AccountData, TransactionData
from ..models import Account, Connection, Holding, Snapshot, Transaction, utcnow


def _norm(label: str) -> str:
    return re.sub(r"[^a-z0-9]", "", label.lower())


def fingerprints(transactions: list[TransactionData]) -> list[str]:
    """Stable ids for de-duplication. Identical lines on the same day are told apart by their rank."""
    seen: Counter = Counter()
    out = []
    for t in transactions:
        if t.external_id:
            key = f"id:{t.external_id}"
        else:
            base = f"{t.date.isoformat()}|{Decimal(t.amount).normalize()}|{_norm(t.label)}"
            seen[base] += 1
            key = f"{base}|{seen[base]}"
        out.append(hashlib.sha256(key.encode()).hexdigest())
    return out


def add_transactions(
    session: Session, account: Account, transactions: list[TransactionData], source: str, document_id: int | None = None
) -> int:
    if not transactions:
        return 0
    existing = set(session.scalars(select(Transaction.fingerprint).where(Transaction.account_id == account.id)))
    # Lines already known from another source (e.g. a statement imported, then synced): same day and amount.
    known = Counter(
        (d, Decimal(a).normalize()) for d, a in session.execute(select(Transaction.date, Transaction.amount).where(Transaction.account_id == account.id))
    ) if source == "import" else Counter()
    added = 0
    for t, fp in zip(transactions, fingerprints(transactions)):
        if fp in existing:
            continue
        key = (t.date, Decimal(t.amount).normalize())
        if known[key] > 0:
            known[key] -= 1
            continue
        session.add(
            Transaction(
                account_id=account.id,
                fingerprint=fp,
                date=t.date,
                label=t.label[:500],
                amount=t.amount,
                currency=t.currency,
                source=source,
                document_id=document_id,
            )
        )
        existing.add(fp)
        added += 1
    return added


def apply_accounts(session: Session, connection: Connection, accounts: list[AccountData]) -> dict:
    stats = {"accounts": 0, "transactions": 0}
    for data in accounts:
        account = session.scalar(
            select(Account).where(Account.connection_id == connection.id, Account.external_id == data.external_id)
        )
        if account is None:
            account = Account(
                connection_id=connection.id,
                external_id=data.external_id,
                name=data.name,
                account_type=data.account_type,
                institution=data.institution,
            )
            session.add(account)
        # Name and type stay as the user edited them; figures come from the source.
        account.currency = data.currency
        account.balance = data.balance
        account.balance_date = utcnow()
        if data.iban:
            account.iban = data.iban
        session.flush()
        if data.holdings is not None:
            for holding in [h for h in account.holdings if not h.manual]:
                session.delete(holding)
            for h in data.holdings:
                session.add(
                    Holding(
                        account_id=account.id,
                        symbol=h.symbol,
                        isin=h.isin,
                        name=h.name,
                        asset_class=h.asset_class,
                        quantity=h.quantity,
                        unit_price=h.unit_price,
                        value=h.value,
                        value_currency=h.value_currency,
                        cost_basis=h.cost_basis,
                        extra=h.extra,
                    )
                )
        stats["transactions"] += add_transactions(session, account, data.transactions, "sync")
        stats["accounts"] += 1
    session.flush()
    return stats


def record_snapshot(session: Session, account_id: int, value: Decimal, day: date | None = None) -> None:
    day = day or date.today()
    snap = session.scalar(select(Snapshot).where(Snapshot.account_id == account_id, Snapshot.day == day))
    if snap is None:
        session.add(Snapshot(account_id=account_id, day=day, value=value))
    else:
        snap.value = value
