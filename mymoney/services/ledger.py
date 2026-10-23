"""Store imported operations without duplicating what is already there."""

from __future__ import annotations

import hashlib
import re
from collections import Counter
from dataclasses import dataclass, field
from datetime import date
from decimal import Decimal

from sqlalchemy import select
from sqlalchemy.orm import Session

from ..adapters.base import TransactionData
from ..models import Account, Snapshot, Transaction


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
            base = f"{t.date.isoformat()}|{Decimal(t.amount).normalize()}|{_norm(t.raw or t.label)}"
            seen[base] += 1
            key = f"{base}|{seen[base]}"
        out.append(hashlib.sha256(key.encode()).hexdigest())
    return out


@dataclass
class Classified:
    """The lines of a statement, sorted by what importing them would do."""

    new: list[tuple[TransactionData, str]] = field(default_factory=list)
    # Same fingerprint as a line already stored: the very same operation, seen again (overlapping statements).
    exact: list[TransactionData] = field(default_factory=list)
    # Same day and amount as a stored line but not the same text: probably the same operation from another source.
    probable: list[tuple[TransactionData, str]] = field(default_factory=list)


def classify(session: Session, account_id: int | None, transactions: list[TransactionData]) -> Classified:
    result = Classified()
    fps = fingerprints(transactions)
    if account_id is None:
        result.new = list(zip(transactions, fps))
        return result
    existing = set(session.scalars(select(Transaction.fingerprint).where(Transaction.account_id == account_id)))
    known = Counter(
        (d, Decimal(a).normalize())
        for d, a in session.execute(select(Transaction.date, Transaction.amount).where(Transaction.account_id == account_id))
    )
    for t, fp in zip(transactions, fps):
        if fp in existing:
            result.exact.append(t)
            continue
        key = (t.date, Decimal(t.amount).normalize())
        if known[key] > 0:
            known[key] -= 1
            result.probable.append((t, fp))
        else:
            result.new.append((t, fp))
    return result


def store(
    session: Session,
    account: Account,
    items: list[tuple[TransactionData, str]],
    source: str,
    document_id: int | None = None,
    counterparty_index: dict[tuple[str, str], int] | None = None,
) -> int:
    from . import counterparties

    index = counterparty_index if counterparty_index is not None else counterparties.load_index(session)
    added = 0
    for t, fp in items:
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
                value_date=t.value_date,
                kind=t.kind,
                counterparty_name=(t.counterparty or None) and t.counterparty[:300],
                counterparty_iban=t.counterparty_iban,
                counterparty_id=counterparties.match(index, t.counterparty, t.counterparty_iban),
                reference=(t.reference or None) and t.reference[:200],
                memo=t.memo,
                raw_label=t.raw,
                details=t.details,
            )
        )
        added += 1
    session.flush()
    return added


def record_snapshot(session: Session, account_id: int, value: Decimal, day: date | None = None) -> None:
    day = day or date.today()
    snap = session.scalar(select(Snapshot).where(Snapshot.account_id == account_id, Snapshot.day == day))
    if snap is None:
        session.add(Snapshot(account_id=account_id, day=day, value=value))
    else:
        snap.value = value
