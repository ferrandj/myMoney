"""Turn a parsed statement into accounts and operations, checking that everything stays consistent.

Nothing here writes before `plan` has looked at the statement against what is already stored:
which account it belongs to, which lines are already there (same operation seen twice, or probably the same one from
another source), whether its period overlaps or leaves a hole, and whether its balances continue the previous statement.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime, timedelta
from decimal import Decimal

from sqlalchemy import delete, select
from sqlalchemy.orm import Session

from ..adapters.base import ParsedStatement, TransactionData
from ..models import ACCOUNT_TYPES, Account, Document, Holding, Statement, Transaction
from . import counterparties, ledger
from . import coverage as coverage_service


@dataclass
class DedupReport:
    """A statement for a period that was already imported: does the new file change anything?"""

    previous: Statement  # the statement already imported for this account and period
    document: Document | None  # the file it came from (None once that file was deleted)
    extra: list[TransactionData]  # lines the new file has and the account does not (new, or same day and amount but another text)
    missing: list[Transaction]  # lines the old file brought that the new file no longer has
    opening: tuple[Decimal, Decimal] | None  # (old, new) when the opening balance differs
    closing: tuple[Decimal, Decimal] | None
    new_count: int = 0  # of `extra`: lines nothing like them is stored
    probable_count: int = 0  # of `extra`: same day and amount as a stored line, another text

    @property
    def identical(self) -> bool:
        return not (self.extra or self.missing or self.opening or self.closing)

    @property
    def can_replace(self) -> bool:
        """Replacing needs the old file: it is what tells which lines to remove."""
        return self.document is not None


@dataclass
class ImportPlan:
    statement: ParsedStatement
    account: Account | None
    candidates: list[Account]  # every account the identity could match (more than one = ask the user)
    classified: ledger.Classified
    already_imported: Statement | None  # same account, same period: this statement was imported before
    overlaps: list[Statement] = field(default_factory=list)
    issues: list[str] = field(default_factory=list)  # consistency problems with the neighbouring statements
    newer_balance: bool = True  # this statement ends after the balance currently stored
    # The holder line looks cut by the bank and the full names were never given: ask once, before importing.
    ask_holder: bool = False
    dedup: DedupReport | None = None  # set when this statement repeats one already imported

    def statuses(self) -> list[str]:
        """For each line of the statement: new | exact (already stored) | probable (same day and amount as a stored line)."""
        exact = {id(t) for t in self.classified.exact}
        probable = {id(t) for t, _ in self.classified.probable}
        return ["exact" if id(t) in exact else "probable" if id(t) in probable else "new" for t in self.statement.transactions]

    @property
    def clean(self) -> bool:
        """Nothing to ask and nothing suspicious: safe to import without a review."""
        st = self.statement
        return (
            self.account is not None
            and len(self.candidates) == 1
            and st.confidence == "high"
            and not st.warnings
            and not self.issues
            and not self.overlaps
            and self.already_imported is None
            and not self.classified.probable
            and not self.ask_holder
        )


def find_accounts(session: Session, st: ParsedStatement) -> list[Account]:
    """Accounts this statement can belong to, best match first. Identity comes from the document, not the label."""
    accounts = list(session.scalars(select(Account)))
    scored: list[tuple[int, Account]] = []
    iban = (st.iban or "").replace(" ", "")
    for a in accounts:
        score = 0
        if iban and a.iban and a.iban.replace(" ", "") == iban:
            score = 3
        elif st.account_number and st.bank_code and (a.bank_code, a.account_number) == (st.bank_code, st.account_number):
            score = 3
        elif st.account_number and a.account_number == st.account_number and (not st.institution or a.institution == st.institution):
            score = 3  # documents without IBAN (savings plans): the account number is unique within a bank
        elif st.account_number and (
            (a.iban and st.account_number in a.iban) or (a.account_number and a.account_number.endswith(st.account_number[-6:]))
        ):
            score = 1  # weak: same trailing digits
        if score:
            scored.append((score, a))
    best = max((s for s, _ in scored), default=0)
    return [a for s, a in sorted(scored, key=lambda x: -x[0]) if s == best]


def plan_import(session: Session, st: ParsedStatement, account: Account | None) -> ImportPlan:
    candidates = find_accounts(session, st)
    classified = ledger.classify(session, account.id if account else None, st.transactions)
    plan = ImportPlan(st, account, candidates, classified, None)
    plan.ask_holder = _holder_unconfirmed(st, account)
    if account is None:
        return plan
    existing = list(session.scalars(select(Statement).where(Statement.account_id == account.id).order_by(Statement.period_start)))
    if st.period_start and st.period_end:
        for other in existing:
            if other.period_start is None or other.period_end is None:
                continue
            if (other.period_start, other.period_end) == (st.period_start, st.period_end):
                plan.already_imported = other
            elif other.period_start <= st.period_end and other.period_end >= st.period_start:
                plan.overlaps.append(other)
        plan.issues = _continuity(st, [s for s in existing if s is not plan.already_imported], coverage_service.acknowledged_ranges(session, account.id))
        if plan.already_imported is not None:
            plan.dedup = _compare(session, st, plan.already_imported, classified)
    if st.balance is not None and account.balance_date is not None and st.balance_date is not None:
        plan.newer_balance = st.balance_date >= account.balance_date.date()
    return plan


def _compare(session: Session, st: ParsedStatement, previous: Statement, classified: ledger.Classified) -> DedupReport:
    """What the new file has that the account lacks, and what the old file had that it lost."""
    document = session.get(Document, previous.document_id) if previous.document_id else None
    missing: list[Transaction] = []
    if document is not None:
        new_fingerprints = set(ledger.fingerprints(st.transactions))
        old = session.scalars(select(Transaction).where(Transaction.document_id == document.id))
        missing = [t for t in old if t.fingerprint not in new_fingerprints]
    opening = (previous.opening_balance, st.opening_balance) if None not in (previous.opening_balance, st.opening_balance) and previous.opening_balance != st.opening_balance else None
    closing = (previous.closing_balance, st.balance) if None not in (previous.closing_balance, st.balance) and previous.closing_balance != st.balance else None
    extra = [t for t, _ in classified.new] + [t for t, _ in classified.probable]
    return DedupReport(previous, document, extra, missing, opening, closing, len(classified.new), len(classified.probable))


def _holder_unconfirmed(st: ParsedStatement, account: Account | None) -> bool:
    if not st.extra.get("holder_maybe_truncated"):
        return False
    # Once the user has typed the full names they differ from what the bank prints: never ask again for this account.
    return account is None or not account.holder_name or account.holder_name == (account.holder_alias or st.holder_name)


def _continuity(st: ParsedStatement, existing: list[Statement], acknowledged: list[tuple[date, date]] = ()) -> list[str]:
    """The statement just before and just after must touch this one, and their balances must chain.
    A hole the user marked as normal is not reported again."""
    issues: list[str] = []

    def hole(start: date, end: date) -> list[str]:
        if any(a <= start and end <= b for a, b in acknowledged):
            return []
        return [f"Il manque une période : rien entre le {_d(start)} et le {_d(end)}"]

    before = [s for s in existing if s.period_end and s.period_end < st.period_start]
    after = [s for s in existing if s.period_start and s.period_start > st.period_end]
    if before:
        prev = max(before, key=lambda s: s.period_end)
        if (st.period_start - prev.period_end).days > 1:
            issues += hole(prev.period_end + timedelta(days=1), st.period_start - timedelta(days=1))
        elif prev.closing_balance is not None and st.opening_balance is not None and prev.closing_balance != st.opening_balance:
            issues.append(
                f"Le solde initial ({st.opening_balance} {st.currency}) ne reprend pas le solde final du relevé précédent "
                f"({prev.closing_balance} au {_d(prev.period_end)}) : un relevé ou une opération manque peut-être"
            )
    if after:
        nxt = min(after, key=lambda s: s.period_start)
        if (nxt.period_start - st.period_end).days > 1:
            issues += hole(st.period_end + timedelta(days=1), nxt.period_start - timedelta(days=1))
        elif nxt.opening_balance is not None and st.balance is not None and nxt.opening_balance != st.balance:
            issues.append(
                f"Le solde final ({st.balance} {st.currency}) ne correspond pas au solde initial du relevé suivant ({nxt.opening_balance} au {_d(nxt.period_start)})"
            )
    return issues


def _d(value: date) -> str:
    return value.strftime("%d/%m/%Y")


# --------------------------------------------------------------------------------------------- applying
def _fill_identity(account: Account, st: ParsedStatement, latest: bool) -> None:
    for attr in ("iban", "bic", "bank_code", "branch_code", "account_number", "rib_key"):
        value = getattr(st, attr)
        if value and not getattr(account, attr):
            setattr(account, attr, value)
    if st.holder_name and not account.holder_alias:
        account.holder_alias = st.holder_name
    # Holder and address change over time: the newest statement wins, older ones only fill the blanks. Names the user
    # completed by hand (different from the printed line) are never overwritten by the cut version of the bank.
    edited = bool(account.holder_name and account.holder_alias and account.holder_name != account.holder_alias)
    for attr in ("holder_name", "holder_address"):
        value = getattr(st, attr)
        if value and (latest or not getattr(account, attr)) and not (attr == "holder_name" and edited):
            setattr(account, attr, value)


def _decimal(raw: str | None) -> Decimal | None:
    raw = (raw or "").replace(",", ".").replace(" ", "")
    return Decimal(raw) if raw else None


def import_statement(session: Session, doc: Document, st: ParsedStatement, form: dict[str, str]) -> dict:
    target = form.get("account_id") or "new"
    if target == "new":
        account = Account(
            name=form.get("name") or st.account_label or doc.filename,
            account_type=form.get("account_type") if form.get("account_type") in ACCOUNT_TYPES else (st.account_type or "checking"),
            institution=form.get("institution") or st.institution,
            currency=st.currency,
        )
        session.add(account)
        session.flush()
    else:
        account = session.get(Account, int(target))
        if account is None:
            raise ValueError("Compte introuvable")
    plan = plan_import(session, st, account)
    if plan.already_imported is not None and form.get("force") != "on":
        raise ValueError("Ce relevé a déjà été importé dans ce compte (même période)")

    latest = not any(s.period_end and st.period_end and s.period_end > st.period_end for s in account.statements)
    _fill_identity(account, st, latest)
    full_names = " ".join((form.get("holder_name") or "").split())
    if full_names:  # the user completed the names the bank cut
        account.holder_name = full_names

    balance = _decimal(form.get("balance"))
    if form.get("update_balance") == "on" and balance is not None:
        account.balance = balance
        account.balance_date = datetime.combine(st.balance_date or date.today(), datetime.min.time())

    added = 0
    skipped_exact = len(plan.classified.exact)
    skipped_probable = len(plan.classified.probable)
    if form.get("import_transactions") == "on":
        items = list(plan.classified.new)
        if form.get("import_probable") == "on":
            items += plan.classified.probable
            skipped_probable = 0
        added = ledger.store(session, account, items, "import", document_id=doc.id)
    if st.holdings and form.get("import_holdings") == "on":
        for h in [h for h in account.holdings if h.manual]:
            session.delete(h)
        for h in st.holdings:
            session.add(
                Holding(account_id=account.id, symbol=h.symbol, isin=h.isin, name=h.name, asset_class=h.asset_class,
                        quantity=h.quantity, value=h.value, value_currency=h.value_currency, manual=True)
            )
    if (form.get("import_transactions") == "on" or st.holdings) and plan.already_imported is None:
        session.add(
            Statement(
                account_id=account.id, document_id=doc.id, period_start=st.period_start, period_end=st.period_end,
                opening_balance=st.opening_balance, closing_balance=st.balance, tx_count=len(st.transactions), added_count=added,
            )
        )
    doc.status = "imported"
    doc.account_id = account.id
    session.flush()
    ledger.mark_internal(session)  # a new account (or new names) can turn earlier transfers into "between my own accounts"
    ledger.pair_transfers(session)
    return {"account_id": account.id, "added": added, "skipped_exact": skipped_exact, "skipped_probable": skipped_probable}


def undo_import(session: Session, doc: Document) -> int:
    """Remove what a document brought in (its operations and statement records). The document itself stays."""
    gone = set(session.scalars(select(Transaction.id).where(Transaction.document_id == doc.id)))
    for t in session.scalars(select(Transaction).where(Transaction.kind.in_(ledger.TRANSFER_KINDS))):
        if (t.details or {}).get("paired_with") in gone:  # its other half is leaving: this line is free again
            t.details = {k: v for k, v in t.details.items() if k not in ("paired_with", "internal")} or None
    removed = session.execute(delete(Transaction).where(Transaction.document_id == doc.id)).rowcount or 0
    session.execute(delete(Statement).where(Statement.document_id == doc.id))
    doc.status = "parsed" if (doc.parse_result or {}).get("statements") else "uploaded"
    doc.account_id = None
    session.flush()
    ledger.mark_internal(session)  # a line can still be internal by its name
    return removed


def default_import_form(plan: ImportPlan) -> dict[str, str]:
    """What a one-click import does: the account the identity points to, operations, balance if it is newer."""
    st = plan.statement
    form = {"account_id": str(plan.account.id) if plan.account else "new", "import_transactions": "on"}
    if st.balance is not None and plan.newer_balance:
        form["update_balance"] = "on"
        form["balance"] = str(st.balance)
    if st.holdings:
        form["import_holdings"] = "on"
    return form


def coverage(session: Session, account_id: int) -> tuple[list[Statement], list[str]]:
    """Statements of an account in order, and the places where consecutive balances do not chain.
    (Missing periods are in services/coverage.py, where they can be marked as normal.)"""
    rows = [s for s in session.scalars(select(Statement).where(Statement.account_id == account_id).order_by(Statement.period_start)) if s.period_start and s.period_end]
    broken = []
    for prev, nxt in zip(rows, rows[1:]):
        if (nxt.period_start - prev.period_end).days == 1 and prev.closing_balance is not None and nxt.opening_balance is not None and prev.closing_balance != nxt.opening_balance:
            broken.append(f"Les soldes ne se suivent pas entre le {_d(prev.period_end)} ({prev.closing_balance}) et le {_d(nxt.period_start)} ({nxt.opening_balance})")
    return rows, broken
