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
from ..adapters.common import name_key
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


_JOINT = re.compile(r"\s+(?:OU|ET)\s+", re.I)
TRANSFER_KINDS = ("transfer", "instant_transfer")


def own_name_keys(session: Session) -> dict[int | None, set[str]]:
    """Per owner, the names that stand for them: the holder of an account held alone, and the holder line of every account
    as the bank prints it (a transfer to the joint account shows that line, even cut). Another user's name is not "me"."""
    keys: dict[int | None, set[str]] = {}
    for owner, holder, alias in session.execute(select(Account.owner_id, Account.holder_name, Account.holder_alias)):
        mine = keys.setdefault(owner, set())
        if holder and len(_JOINT.split(holder)) == 1:
            mine.add(name_key(holder))
        if alias:
            mine.add(name_key(alias))
    return keys


def mark_internal(session: Session) -> int:
    """Flag the transfers whose other party is the user (their other accounts): they are not income nor spending."""
    keys = own_name_keys(session)
    if not any(keys.values()):
        return 0
    owners = {i: o for i, o in session.execute(select(Account.id, Account.owner_id))}
    marked = 0
    for t in session.scalars(select(Transaction).where(Transaction.kind.in_(TRANSFER_KINDS), Transaction.counterparty_name.is_not(None))):
        if not t.internal and name_key(t.counterparty_name) in keys.get(owners.get(t.account_id), set()):
            t.details = {**(t.details or {}), "internal": True}
            marked += 1
    session.flush()
    return marked


def pair_transfers(session: Session) -> int:
    """Match a transfer leaving one account with the one arriving on another account the same day (or the day after).

    Names are unreliable (a statement may print the other account's label, or the holder who ordered the transfer), but an
    opposite amount on two of the user's accounts on the same day is not: both lines are then "between my own accounts".
    """
    from collections import defaultdict

    owners = {i: o for i, o in session.execute(select(Account.id, Account.owner_id))}  # two people's accounts are not "my own accounts"
    pool = [t for t in session.scalars(select(Transaction).where(Transaction.kind.in_(TRANSFER_KINDS)).order_by(Transaction.date, Transaction.id))
            if not (t.details or {}).get("paired_with")]
    by_amount: dict[Decimal, list[Transaction]] = defaultdict(list)
    for t in pool:
        by_amount[Decimal(t.amount)].append(t)
    paired = 0
    for t in pool:
        if (t.details or {}).get("paired_with") or t.amount >= 0:
            continue
        candidates = [
            c for c in by_amount.get(-Decimal(t.amount), [])
            if c.account_id != t.account_id and owners.get(c.account_id) == owners.get(t.account_id)
            and abs((c.date - t.date).days) <= 1 and not (c.details or {}).get("paired_with")
        ]
        if not candidates:
            continue
        other = min(candidates, key=lambda c: (abs((c.date - t.date).days), c.id))
        t.details = {**(t.details or {}), "internal": True, "paired_with": other.id}
        other.details = {**(other.details or {}), "internal": True, "paired_with": t.id}
        paired += 1
    session.flush()
    return paired


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
