"""Numbers behind the analysis views: balance history, repartition, income and spending.

History of an account is rebuilt from what was imported, not from when the app happened to be opened:
starting from the latest known balance and walking back through the operations (balance before = balance after minus
the operation), or forward for operations dated after it. Accounts without operations (manual savings, positions)
fall back to the daily snapshots. An account counts from its first known day: before that it is simply not in the total.
"""

from __future__ import annotations

import math
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import date, timedelta
from decimal import Decimal

from sqlalchemy import select
from sqlalchemy.orm import Session, selectinload

from ..models import CATEGORIES, Account, Snapshot, Transaction
from . import categories as category_service

PERIODS = {"1m": ("1 mois", 30), "3m": ("3 mois", 91), "6m": ("6 mois", 182), "1a": ("1 an", 365), "tout": ("Tout", None)}
MAX_POINTS = 150


@dataclass
class Series:
    key: str
    label: str
    points: list[tuple[date, Decimal]]


@dataclass
class Stats:
    first: Decimal
    last: Decimal
    low: Decimal
    high: Decimal

    @property
    def delta(self) -> Decimal:
        return self.last - self.first

    @property
    def pct(self) -> Decimal | None:
        return (self.delta / abs(self.first) * 100) if self.first else None


@dataclass
class MonthFlow:
    month: date  # first day of the month
    income: Decimal = Decimal(0)
    expense: Decimal = Decimal(0)  # positive number: what went out

    @property
    def net(self) -> Decimal:
        return self.income - self.expense


@dataclass
class Ranking:
    label: str
    total: Decimal
    count: int
    contact_id: int | None = None


@dataclass
class Spending:
    by_kind: list[Ranking] = field(default_factory=list)
    top_out: list[Ranking] = field(default_factory=list)
    top_in: list[Ranking] = field(default_factory=list)
    total_out: Decimal = Decimal(0)
    total_in: Decimal = Decimal(0)


# ------------------------------------------------------------------------------------------ history
def account_history(session: Session, account: Account, current: Decimal, today: date) -> dict[date, Decimal]:
    """End-of-day value in EUR of one account, one entry per day from its first known day to today."""
    rows = session.execute(select(Transaction.date, Transaction.amount).where(Transaction.account_id == account.id)).all()
    if rows and not account.holdings and account.balance_date is not None:
        # Value in EUR of one unit of the account's balance: 1 for euro accounts, the rate for others, -1 for debts.
        factor = current / account.balance if account.balance else Decimal(1)
        moves: dict[date, Decimal] = defaultdict(Decimal)
        for day, amount in rows:
            moves[day] += amount
        anchor_day, anchor = account.balance_date.date(), account.balance
        first = min(moves)
        history: dict[date, Decimal] = {}
        cursor, day = anchor, anchor_day
        while day >= first - timedelta(days=1):
            history[day] = cursor * factor
            cursor -= moves.get(day, Decimal(0))
            day -= timedelta(days=1)
        cursor, day = anchor, anchor_day + timedelta(days=1)
        while day <= max(today, max(moves)):
            cursor += moves.get(day, Decimal(0))
            history[day] = cursor * factor
            day += timedelta(days=1)
        return _extend(history, today)

    snapshots = {d: Decimal(v) for d, v in session.execute(select(Snapshot.day, Snapshot.value).where(Snapshot.account_id == account.id))}
    snapshots[today] = current
    start = min(snapshots)
    history, last, day = {}, snapshots[start], start
    while day <= today:
        last = snapshots.get(day, last)
        history[day] = last
        day += timedelta(days=1)
    return history


def _extend(history: dict[date, Decimal], today: date) -> dict[date, Decimal]:
    last_day = max(history)
    value = history[last_day]
    day = last_day + timedelta(days=1)
    while day <= today:
        history[day] = value
        day += timedelta(days=1)
    return history


def period_start(period: str, today: date, earliest: date) -> date:
    days = PERIODS.get(period, PERIODS["1a"])[1]
    return earliest if days is None else max(earliest, today - timedelta(days=days))


def _days(start: date, end: date) -> list[date]:
    count = (end - start).days + 1
    step = max(1, math.ceil(count / MAX_POINTS))
    days = [start + timedelta(days=i) for i in range(0, count, step)]
    if days[-1] != end:
        days.append(end)
    return days


def build_series(
    histories: dict[int, dict[date, Decimal]], accounts: dict[int, Account], selected: list[int], group: str, start: date, end: date
) -> list[Series]:
    """Lines to draw: the total of the selection, one line per account, or one per category."""
    chosen = [i for i in selected if i in histories]
    days = _days(start, end)

    def line(ids: list[int]) -> list[tuple[date, Decimal]]:
        points = []
        for day in days:
            known = [histories[i][day] for i in ids if day in histories[i]]
            if known:
                points.append((day, sum(known, Decimal(0))))
        return points

    if group == "compte":
        return [Series(str(i), accounts[i].name, line([i])) for i in chosen if line([i])]
    if group == "categorie":
        out = []
        for code, label in CATEGORIES.items():
            ids = [i for i in chosen if accounts[i].category == code]
            if ids and (points := line(ids)):
                out.append(Series(code, label, points))
        return out
    points = line(chosen)
    return [Series("total", "Total", points)] if points else []


def stats(points: list[tuple[date, Decimal]]) -> Stats | None:
    if not points:
        return None
    values = [v for _, v in points]
    return Stats(values[0], values[-1], min(values), max(values))


def earliest_day(histories: dict[int, dict[date, Decimal]], selected: list[int], today: date) -> date:
    days = [min(histories[i]) for i in selected if i in histories]
    return min(days) if days else today


# --------------------------------------------------------------------------------------- repartition
def repartition(values: list, group: str) -> list[tuple[str, str, Decimal]]:
    """(key, label, amount) of the positive holdings, largest first. Debts are reported apart."""
    amounts: dict[str, tuple[str, Decimal]] = {}
    for v in values:
        if v.total <= 0:
            continue
        key, label = (v.account.category, CATEGORIES[v.account.category]) if group == "categorie" else (str(v.account.id), v.account.name)
        amounts[key] = (label, amounts.get(key, ("", Decimal(0)))[1] + v.total)
    return sorted(((k, label, total) for k, (label, total) in amounts.items()), key=lambda x: -x[2])


# ---------------------------------------------------------------------------------- income and spending
def _transactions(session: Session, account_ids: list[int], start: date, end: date, include_internal: bool) -> list[Transaction]:
    if not account_ids:
        return []
    rows = session.scalars(
        select(Transaction)
        .where(Transaction.account_id.in_(account_ids), Transaction.date >= start, Transaction.date <= end)
        .options(selectinload(Transaction.counterparty))
    )
    return [t for t in rows if include_internal or not t.internal]


def monthly_flows(session: Session, account_ids: list[int], start: date, end: date, include_internal: bool) -> list[MonthFlow]:
    months: dict[date, MonthFlow] = {}
    cursor = date(start.year, start.month, 1)
    while cursor <= end:
        months[cursor] = MonthFlow(cursor)
        cursor = date(cursor.year + cursor.month // 12, cursor.month % 12 + 1, 1)
    for t in _transactions(session, account_ids, start, end, include_internal):
        flow = months[date(t.date.year, t.date.month, 1)]
        if t.amount >= 0:
            flow.income += t.amount
        else:
            flow.expense += -t.amount
    return list(months.values())


KIND_NAMES = {
    "card": "Cartes", "transfer": "Virements", "instant_transfer": "Virements instantanés", "direct_debit": "Prélèvements",
    "loan_payment": "Prêts", "fee": "Frais", "cash": "Retraits", "cheque": "Chèques", "interest": "Intérêts",
}


def spending(session: Session, account_ids: list[int], start: date, end: date, include_internal: bool, limit: int = 12) -> Spending:
    result = Spending()
    kinds: dict[str, Ranking] = {}
    out: dict[str, Ranking] = {}
    inc: dict[str, Ranking] = {}
    for t in _transactions(session, account_ids, start, end, include_internal):
        amount = abs(t.amount)
        who = t.counterparty.name if t.counterparty else (t.counterparty_name or t.label)
        contact = t.counterparty_id
        if t.amount < 0:
            result.total_out += amount
            label = KIND_NAMES.get(t.kind or "", "Autres")
            kinds.setdefault(label, Ranking(label, Decimal(0), 0))
            kinds[label].total += amount
            kinds[label].count += 1
            bucket = out
        else:
            result.total_in += amount
            bucket = inc
        key = f"c{contact}" if contact else who.lower()
        entry = bucket.setdefault(key, Ranking(who, Decimal(0), 0, contact))
        entry.total += amount
        entry.count += 1
    rank = lambda d: sorted(d.values(), key=lambda r: -r.total)  # noqa: E731
    result.by_kind = rank(kinds)
    result.top_out, result.top_in = rank(out)[:limit], rank(inc)[:limit]
    return result


# ---------------------------------------------------------------------------- spending by category
@dataclass
class CategoryTotal:
    id: str
    label: str
    total: Decimal = Decimal(0)
    count: int = 0
    share: Decimal = Decimal(0)  # % of the total of that direction


@dataclass
class MerchantTotal:
    key: str
    label: str
    contact_id: int | None
    total: Decimal = Decimal(0)
    count: int = 0
    category: str = ""
    ruled: bool = False  # the user chose this category (not a guess)


@dataclass
class SpendingReport:
    direction: str  # out | in
    total: Decimal = Decimal(0)  # of that direction
    total_out: Decimal = Decimal(0)
    total_in: Decimal = Decimal(0)
    categories: list[CategoryTotal] = field(default_factory=list)
    months: list[tuple[date, Decimal]] = field(default_factory=list)
    # With a category chosen:
    category: CategoryTotal | None = None
    merchants: list[MerchantTotal] = field(default_factory=list)
    operations: list[tuple[Transaction, str]] = field(default_factory=list)  # (operation, account name), newest first
    top_merchants: list[MerchantTotal] = field(default_factory=list)  # all categories mixed

    @property
    def saved(self) -> Decimal:
        return self.total_in - self.total_out


def spending_report(
    session: Session, account_ids: list[int], start: date, end: date, include_internal: bool, direction: str = "out", category: str | None = None
) -> SpendingReport:
    """Where the money went (or came from): per category, then per merchant and operation inside one category."""
    report = SpendingReport(direction)
    rules = category_service.load_rules(session)
    names = {a.id: a.name for a in session.scalars(select(Account).where(Account.id.in_(account_ids)))} if account_ids else {}
    totals: dict[str, CategoryTotal] = {}
    merchants: dict[tuple[str, str], MerchantTotal] = {}
    months: dict[date, Decimal] = {}
    cursor = date(start.year, start.month, 1)
    while cursor <= end:
        months[cursor] = Decimal(0)
        cursor = date(cursor.year + cursor.month // 12, cursor.month % 12 + 1, 1)
    chosen_ops: list[tuple[Transaction, str]] = []
    for t in _transactions(session, account_ids, start, end, include_internal):
        amount = Decimal(t.amount)
        if amount == 0:
            continue
        if amount < 0:
            report.total_out += -amount
        else:
            report.total_in += amount
        if (amount < 0) != (direction == "out"):
            continue
        cat = category_service.categorize(t, rules)
        key, label, contact_id = category_service.merchant(t)
        value = abs(amount)
        report.total += value
        entry = totals.setdefault(cat, CategoryTotal(cat, category_service.ALL[cat].label))
        entry.total += value
        entry.count += 1
        m = merchants.setdefault((cat, key), MerchantTotal(key, label, contact_id, category=cat, ruled=key in rules and rules[key] in category_service.ALL))
        m.total += value
        m.count += 1
        if category is None or cat == category:
            months[date(t.date.year, t.date.month, 1)] += value
        if cat == category:
            chosen_ops.append((t, names.get(t.account_id, "")))
    for entry in totals.values():
        entry.share = entry.total / report.total * 100 if report.total else Decimal(0)
    report.categories = sorted(totals.values(), key=lambda c: -c.total)
    report.months = sorted(months.items())
    by_key: dict[str, MerchantTotal] = {}
    for (cat, key), m in merchants.items():
        top = by_key.setdefault(key, MerchantTotal(key, m.label, m.contact_id, category=cat, ruled=m.ruled))
        top.total += m.total
        top.count += m.count
    report.top_merchants = sorted(by_key.values(), key=lambda m: -m.total)[:12]
    if category is not None and category in totals:
        report.category = totals[category]
        report.merchants = sorted((m for (cat, _), m in merchants.items() if cat == category), key=lambda m: -m.total)
        report.operations = sorted(chosen_ops, key=lambda x: (x[0].date, x[0].id), reverse=True)[:300]
    return report
