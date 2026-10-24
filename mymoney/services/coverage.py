"""Which periods of an account no imported statement covers, and which of those the user said are fine.

A gap is a stretch of days between the statements of one account that none of them covers (statements that overlap
leave no gap). Each one is reported until the missing statement is imported, or the user marks it as normal
(an account with nothing to report that month, a statement the bank never issued...). A "normal" mark covers the gap
it was given for and anything inside it: when a partial statement later splits the gap, the pieces stay quiet, but a
new, larger or different hole is reported again.
"""

from __future__ import annotations

import calendar
from dataclasses import dataclass
from datetime import date, timedelta

from sqlalchemy import select
from sqlalchemy.orm import Session

from ..models import Account, GapAck, Statement

MONTHS = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"]


@dataclass
class Gap:
    account_id: int
    account_name: str
    start: date
    end: date
    ack_id: int | None = None  # set when the user marked it as normal

    @property
    def acknowledged(self) -> bool:
        return self.ack_id is not None

    @property
    def days(self) -> int:
        return (self.end - self.start).days + 1

    @property
    def label(self) -> str:
        return describe(self.start, self.end)


def describe(start: date, end: date) -> str:
    """"mai 2026", "mai → juillet 2026", or the exact dates when the gap does not follow whole months."""
    whole = start.day == 1 and end.day == calendar.monthrange(end.year, end.month)[1]
    if not whole:
        return f"du {start:%d/%m/%Y} au {end:%d/%m/%Y}"
    if (start.year, start.month) == (end.year, end.month):
        return f"{MONTHS[start.month - 1]} {start.year}"
    first = MONTHS[start.month - 1] + ("" if start.year == end.year else f" {start.year}")
    return f"{first} → {MONTHS[end.month - 1]} {end.year}"


def gaps_between(periods: list[tuple[date, date]]) -> list[tuple[date, date]]:
    """Uncovered stretches between periods, whatever their order or overlaps."""
    gaps: list[tuple[date, date]] = []
    covered_until: date | None = None
    for start, end in sorted(periods):
        if covered_until is not None and start - covered_until > timedelta(days=1):
            gaps.append((covered_until + timedelta(days=1), start - timedelta(days=1)))
        covered_until = end if covered_until is None else max(covered_until, end)
    return gaps


def account_gaps(session: Session, account: Account, include_acknowledged: bool = False) -> list[Gap]:
    periods = [
        (s.period_start, s.period_end)
        for s in session.scalars(select(Statement).where(Statement.account_id == account.id))
        if s.period_start and s.period_end
    ]
    acks = list(session.scalars(select(GapAck).where(GapAck.account_id == account.id)))
    result = []
    for start, end in gaps_between(periods):
        ack = next((a for a in acks if a.period_start <= start and end <= a.period_end), None)
        if ack is None or include_acknowledged:
            result.append(Gap(account.id, account.name, start, end, ack.id if ack else None))
    return result


def all_gaps(session: Session, include_acknowledged: bool = False) -> list[Gap]:
    """Gaps of every account that is still followed (archived accounts are left alone)."""
    accounts = session.scalars(select(Account).where(Account.archived.is_(False)).order_by(Account.institution, Account.name))
    return [g for a in accounts for g in account_gaps(session, a, include_acknowledged)]


def acknowledged_ranges(session: Session, account_id: int) -> list[tuple[date, date]]:
    return [(a.period_start, a.period_end) for a in session.scalars(select(GapAck).where(GapAck.account_id == account_id))]


def acknowledge(session: Session, account_id: int, start: date, end: date, note: str | None = None) -> None:
    """Mark a gap as normal. Only a gap that exists can be marked: a stale form changes nothing."""
    account = session.get(Account, account_id)
    if account is None or end < start:
        raise ValueError("Compte ou période invalide")
    if not any(g.start <= start and end <= g.end for g in account_gaps(session, account)):
        raise ValueError("Cette période n'est plus un trou : recharge la page")
    session.add(GapAck(account_id=account_id, period_start=start, period_end=end, note=(note or "").strip() or None))
    session.flush()


def restore(session: Session, ack_id: int) -> None:
    """Report the gap again (the user changed their mind)."""
    ack = session.get(GapAck, ack_id)
    if ack is not None:
        session.delete(ack)
        session.flush()
