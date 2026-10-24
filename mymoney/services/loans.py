"""Loans: what the documents of a loan say, put together and checked against each other.

Three documents describe one loan, each from its own angle, and none of them is enough alone:
- the offer: amount, duration, rate, cost, and the original amortization table (numbered, not dated);
- the notice of an early repayment: the capital owed, the last instalment paid, the new instalment and the new end date;
- the dated amortization table: every instalment from today to the end, starting from the capital owed that day.

`build` replays them in order (the contract until the repayment, a computed bridge up to the dated table, then the table)
and every junction is a check: the capital owed before the repayment must be the one the contract gives at that point, the
computed bridge must land on the capital the dated table starts from, the end dates must agree, and in the end the capital
must reach zero. The instalments found in the account statements are compared with the plan too.
"""

from __future__ import annotations

import calendar
from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import ROUND_HALF_UP, Decimal

from sqlalchemy import select
from sqlalchemy.orm import Session

from ..adapters.base import ParsedStatement
from ..adapters.common import name_key
from ..models import Account, Document, Loan, LoanEvent, LoanInstallment, Transaction

LOAN_KINDS = ("loan_offer", "loan_event", "loan_schedule")
LOAN_ACCOUNT_TYPES = ("mortgage", "loan")
CENT = Decimal("0.01")
ZERO = Decimal(0)


def cents(value: Decimal) -> Decimal:
    return value.quantize(CENT, ROUND_HALF_UP)


def add_months(day: date, months: int) -> date:
    index = day.year * 12 + day.month - 1 + months
    year, month = divmod(index, 12)
    return date(year, month + 1, min(day.day, calendar.monthrange(year, month + 1)[1]))


def months_between(start: date, end: date) -> int:
    return (end.year - start.year) * 12 + end.month - start.month


def _d(value: str | None) -> date | None:
    return date.fromisoformat(value) if value else None


def _dec(value: str | None) -> Decimal | None:
    return Decimal(value) if value not in (None, "") else None


# ---------------------------------------------------------------------------------------------- finding
def find_accounts(session: Session, st: ParsedStatement) -> list[Account]:
    """The loan accounts a loan document can belong to. The offer and the dated table carry the loan's number; the early
    repayment notice does not, so it is matched by figures: the capital owed before must be one the loan's table has."""
    data = st.extra.get("loan") or {}
    loans = list(session.scalars(select(Loan)))
    ref = data.get("reference")
    if ref:
        found = [loan for loan in loans if loan.reference == ref]
        if not found and st.document_kind == "loan_offer":  # a loan first seen through its repayment notice, with no number yet
            remaining = {Decimal(r["remaining"]) for r in data.get("rows", [])}
            found = [loan for loan in loans if loan.reference is None and any(e.capital_before in remaining for e in loan.events)]
        return [loan.account for loan in found]
    event = data.get("event")
    if event and event.get("before", {}).get("capital"):
        capital = Decimal(event["before"]["capital"])
        found = [loan for loan in loans if any(i.remaining == capital for i in loan.installments)]
        if not found and st.holder_name:  # no table to compare with yet (the offer is missing): the borrower's name is the only link
            found = [loan for loan in loans if loan.borrower and name_key(loan.borrower) == name_key(st.holder_name)]
        return [loan.account for loan in found]
    return []


def unverified(session: Session, st: ParsedStatement, account: Account | None) -> str | None:
    """A repayment notice whose capital cannot be found in the loan's own table: it may still be right, but it is not proven."""
    loan = account.loan if account else None
    event = (st.extra.get("loan") or {}).get("event")
    if st.document_kind != "loan_event" or loan is None or not event:
        return None
    capital = Decimal(event["before"]["capital"])
    if any(i.remaining == capital for i in loan.installments):
        return None
    return (f"Le capital restant dû avant remboursement ({_e(capital)}) ne figure dans aucun tableau de ce prêt : "
            "il manque peut-être le contrat (offre) pour le vérifier. Rattachement proposé d'après le nom de l'emprunteur.")


def existing_for(session: Session, st: ParsedStatement, account: Account | None) -> str | None:
    """A sentence when this account already has this very document's content (the user may still replace it)."""
    loan = account.loan if account else None
    data = st.extra.get("loan") or {}
    if loan is None:
        return None
    if st.document_kind == "loan_offer" and loan.offer_document_id is not None:
        return "Ce prêt a déjà son offre (contrat) importée"
    if st.document_kind == "loan_schedule" and loan.schedule_document_id is not None:
        return "Ce prêt a déjà un tableau d'amortissement daté importé"
    if st.document_kind == "loan_event":
        ev = data["event"]
        if any(e.on_date.isoformat() == ev.get("on_date") and str(e.capital_before) == ev["before"].get("capital") for e in loan.events):
            return "Ce remboursement anticipé est déjà enregistré pour ce prêt"
    return None


# --------------------------------------------------------------------------------------------- applying
def attach(session: Session, doc: Document, st: ParsedStatement, account: Account, form: dict[str, str]) -> dict:
    data = st.extra.get("loan") or {}
    force = form.get("force") == "on"
    if account.account_type not in LOAN_ACCOUNT_TYPES:
        raise ValueError("Ce document décrit un prêt : choisis un compte de type crédit")
    loan = account.loan
    if loan is None:
        loan = Loan(account_id=account.id)
        session.add(loan)
        session.flush()
        account.loan = loan
    ref = data.get("reference")
    if ref and loan.reference and ref != loan.reference:
        raise ValueError(f"Ce compte porte déjà le prêt n° {loan.reference}, pas le n° {ref}")
    if existing_for(session, st, account) and not force:
        raise ValueError(existing_for(session, st, account))

    loan.reference = ref or loan.reference
    loan.project_reference = data.get("project") or loan.project_reference
    loan.product = data.get("product") or loan.product
    loan.borrower = data.get("borrower") or st.holder_name or loan.borrower
    if st.document_kind == "loan_offer":
        _drop(session, loan, source="offer")
        purpose = " · ".join(x for x in (data.get("object"), data.get("destination")) if x)
        loan.purpose = purpose or loan.purpose
        loan.principal, loan.duration_months = _dec(data.get("principal")), data.get("duration")
        loan.rate, loan.taeg = _dec(data.get("rate")), _dec(data.get("taeg"))
        loan.interest_total, loan.total_cost = _dec(data.get("interest_total")), _dec(data.get("total_cost"))
        loan.insurance_mandatory, loan.insurance_optional = _dec(data.get("insurance_mandatory")), _dec(data.get("insurance_optional"))
        loan.guarantee_fees = _dec(data.get("guarantee_fees"))
        loan.offer_date = _d(data.get("offer_date"))
        loan.offer_document_id = doc.id
        for r in data.get("rows", []):
            session.add(_installment(loan, doc, "offer", r))
    elif st.document_kind == "loan_schedule":
        if loan.rate is not None and _dec(data.get("rate")) not in (None, loan.rate) and not force:
            raise ValueError(f"Le taux du tableau ({data['rate']} %) n'est pas celui du prêt ({loan.rate} %)")
        _drop(session, loan, source="schedule")
        loan.rate = loan.rate or _dec(data.get("rate"))
        loan.schedule_document_id = doc.id
        loan.principal = loan.principal or _dec(data.get("project_amount"))
        for r in data.get("rows", []):
            session.add(_installment(loan, doc, "schedule", r))
    else:
        ev = data["event"]
        for old in [e for e in loan.events if e.on_date.isoformat() == ev.get("on_date") and str(e.capital_before) == ev["before"]["capital"]]:
            session.delete(old)
        session.flush()
        before, after = ev["before"], ev["after"]
        session.add(
            LoanEvent(
                loan_id=loan.id, document_id=doc.id, kind="prepayment", on_date=_d(ev["on_date"]), issued_on=_d(ev.get("issued_on")),
                capital_before=Decimal(before["capital"]), capital_after=Decimal(after["capital"]),
                last_due_date=_d(before.get("due")), last_payment=_dec(before.get("payment")),
                first_due_date=_d(after.get("due")), first_payment=_dec(after.get("payment")),
                end_before=_d(before.get("end")), end_after=_d(after.get("end")),
            )
        )
    session.flush()
    session.refresh(loan)
    if st.holder_name and not account.holder_name:
        account.holder_name = st.holder_name
    account.institution = account.institution or st.institution
    doc.status = "imported"
    doc.account_id = account.id
    rebuild(session, loan)
    return {"account_id": account.id, "kind": st.document_kind, "added": 0, "skipped_exact": 0, "skipped_probable": 0}


def _installment(loan: Loan, doc: Document, source: str, r: dict) -> LoanInstallment:
    return LoanInstallment(
        loan_id=loan.id, document_id=doc.id, source=source, number=r.get("n"), due_date=_d(r.get("date")),
        capital=Decimal(r["capital"]), interest=Decimal(r["interest"]), deferred_interest=Decimal(r["deferred"]),
        insurance=Decimal(r["insurance"]), total=Decimal(r["total"]), remaining=Decimal(r["remaining"]),
    )


def _drop(session: Session, loan: Loan, source: str) -> None:
    for i in [i for i in loan.installments if i.source == source]:
        loan.installments.remove(i)
        session.delete(i)
    session.flush()


def detach(session: Session, doc: Document) -> None:
    """Take back what a document brought to its loan. A loan left with nothing is removed (its account stays)."""
    for i in session.scalars(select(LoanInstallment).where(LoanInstallment.document_id == doc.id)):
        session.delete(i)
    for e in session.scalars(select(LoanEvent).where(LoanEvent.document_id == doc.id)):
        session.delete(e)
    session.flush()
    for loan in session.scalars(select(Loan).where((Loan.offer_document_id == doc.id) | (Loan.schedule_document_id == doc.id))):
        if loan.offer_document_id == doc.id:
            loan.offer_document_id = None
            for attr in ("principal", "duration_months", "rate", "taeg", "interest_total", "total_cost", "insurance_mandatory",
                         "insurance_optional", "guarantee_fees", "offer_date", "purpose"):
                setattr(loan, attr, None)
        if loan.schedule_document_id == doc.id:
            loan.schedule_document_id = None
    session.flush()
    for loan in session.scalars(select(Loan)):
        session.refresh(loan)
        if not (loan.installments or loan.events):
            session.delete(loan)
        else:
            rebuild(session, loan)
    session.flush()


# --------------------------------------------------------------------------------------------- the plan
@dataclass
class Row:
    kind: str  # installment | prepayment
    date: date | None
    number: int | None
    capital: Decimal
    interest: Decimal
    insurance: Decimal
    total: Decimal
    remaining: Decimal
    source: str  # contrat | calculé | échéancier | arrêté
    status: str = "future"  # paid | next | future
    confirmed: Decimal | None = None  # the amount debited, when the account statements show this instalment
    note: str = ""


@dataclass
class Check:
    status: str  # ok | warn | info
    text: str


@dataclass
class Plan:
    rows: list[Row] = field(default_factory=list)
    checks: list[Check] = field(default_factory=list)
    summary: dict = field(default_factory=dict)

    @property
    def installments(self) -> list[Row]:
        return [r for r in self.rows if r.kind == "installment"]

    @property
    def problems(self) -> list[Check]:
        return [c for c in self.checks if c.status == "warn"]


def infer_first_due(loan: Loan) -> date | None:
    """The date of instalment n°1, from a dated document that points at one of the contract's numbered instalments."""
    offer = sorted((i for i in loan.installments if i.source == "offer"), key=lambda i: i.number or 0)
    if not offer:
        return None
    for ev in loan.events:
        if ev.last_due_date:
            for row in offer:
                if row.remaining == ev.capital_before:
                    return add_months(ev.last_due_date, -(row.number - 1))
    schedule = sorted((i for i in loan.installments if i.source == "schedule"), key=lambda i: i.due_date)
    if schedule and not loan.events:
        balance = schedule[0].remaining + schedule[0].capital
        for row in offer:
            if row.remaining == balance:
                return add_months(schedule[0].due_date, -row.number)
    return None


def project(capital: Decimal, rate: Decimal, payment: Decimal, insurance: Decimal, first_due: date, source: str = "calculé") -> list[Row]:
    """Instalments of a fixed-rate loan from `capital`: interest = capital × rate / 12 (rounded), the rest of the payment
    repays capital, the last instalment clears what is left."""
    rows: list[Row] = []
    for k in range(1200):
        if capital <= 0:
            break
        interest = cents(capital * rate / 1200)
        principal = payment - insurance - interest
        if principal <= 0:
            break
        if principal >= capital or k == 1199:
            principal = capital
        capital -= principal
        rows.append(Row("installment", add_months(first_due, k), None, principal, interest, insurance, principal + interest + insurance, capital, source))
    return rows


def build(session: Session, loan: Loan, today: date | None = None) -> Plan:
    today = today or date.today()
    plan = Plan()
    checks = plan.checks
    offer = sorted((i for i in loan.installments if i.source == "offer"), key=lambda i: i.number or 0)
    schedule = sorted((i for i in loan.installments if i.source == "schedule"), key=lambda i: i.due_date)
    events = sorted(loan.events, key=lambda e: e.on_date)
    first_due = loan.first_due_date
    rate = loan.rate
    insurance = (offer[-1].insurance if offer else schedule[-1].insurance if schedule else ZERO)

    # 1. The contract.
    rows: list[Row] = []
    for i in offer:
        when = add_months(first_due, i.number - 1) if first_due else None
        rows.append(Row("installment", when, i.number, i.capital, i.interest, i.insurance, i.total, i.remaining, "contrat"))
    if offer:
        _check_offer(loan, offer, rate, checks)
        if first_due is None:
            checks.append(Check("info", "Les dates des échéances du contrat ne sont pas connues : dépose l'arrêté de remboursement anticipé ou le tableau daté, ou indique la date de la première échéance."))

    # 2. Early repayments: the contract stops where the capital owed matches, the new plan starts from what is left.
    start_of_new = None
    for ev in events:
        cut = next((k for k in range(len(rows) - 1, -1, -1) if rows[k].kind == "installment" and rows[k].remaining == ev.capital_before), None)
        if cut is not None:
            n = rows[cut].number
            label = f"échéance n° {n}" if n else "échéance"
            checks.append(Check("ok", f"Remboursement anticipé du {_f(ev.on_date)} : le capital restant dû avant ({_e(ev.capital_before)}) est exactement celui du plan après l'{label}"
                                      + (f" ({_f(rows[cut].date)})" if rows[cut].date else "") + "."))
            if rows[cut].date and ev.last_due_date and rows[cut].date != ev.last_due_date:
                checks.append(Check("warn", f"La dernière échéance avant le remboursement est le {_f(ev.last_due_date)} dans le courrier, le {_f(rows[cut].date)} dans le plan."))
            rows = rows[: cut + 1]
        elif rows:
            checks.append(Check("warn", f"Le capital restant dû avant le remboursement anticipé du {_f(ev.on_date)} ({_e(ev.capital_before)}) ne correspond à aucune échéance du plan : une échéance ou un autre remboursement manque peut-être."))
            rows = [r for r in rows if r.date and ev.last_due_date and r.date <= ev.last_due_date]
        else:
            checks.append(Check("info", f"Remboursement anticipé du {_f(ev.on_date)} : sans le contrat, impossible de vérifier le capital restant dû avant ({_e(ev.capital_before)})."))
        if rows and rows[-1].date and ev.end_before and offer and first_due:
            planned_end = add_months(first_due, len(offer) - 1)
            checks.append(Check("ok" if planned_end == ev.end_before else "warn",
                                f"Date de fin avant remboursement : {_f(ev.end_before)} dans le courrier, {_f(planned_end)} d'après le contrat ({len(offer)} échéances)."))
        repaid = ev.capital_before - ev.capital_after
        rows.append(Row("prepayment", ev.on_date, None, repaid, ZERO, ZERO, repaid, ev.capital_after, "arrêté",
                        note=f"Remboursement anticipé partiel de {_e(repaid)} : capital restant dû {_e(ev.capital_before)} → {_e(ev.capital_after)}"))
        if rate and ev.first_due_date and ev.first_payment:
            new = project(ev.capital_after, rate, ev.first_payment, insurance, ev.first_due_date)
            rows += new
            start_of_new = len(rows) - len(new)
            if new and ev.end_after:
                checks.append(Check("ok" if new[-1].date == ev.end_after else "warn",
                                    f"Après le remboursement : {len(new)} échéances de {_e(ev.first_payment)} au taux de {_pct(rate)} mènent au {_f(new[-1].date)}"
                                    f" ({'comme' if new[-1].date == ev.end_after else 'courrier :'} {_f(ev.end_after)} dans le courrier)."))
        elif ev.first_due_date and ev.first_payment and not rate:
            checks.append(Check("info", "Le taux du prêt est inconnu (dépose l'offre ou le tableau daté) : l'échéancier après le remboursement ne peut pas être calculé."))

    # 3. The dated table: it takes over from its first date and must start from the capital the plan has reached.
    if schedule:
        first = schedule[0]
        balance = first.remaining + first.capital
        _check_schedule(loan, schedule, rate, checks)
        before = [r for r in rows if r.date and r.date < first.due_date]
        if offer and not before and not events and any(r.date is None for r in rows):
            near = min(offer, key=lambda o: abs(o.remaining - balance))
            checks.append(Check("warn", f"Le tableau daté part de {_e(balance)} au {_f(first.due_date)} mais aucune échéance du contrat ne mène à ce capital "
                                        f"(la plus proche : n° {near.number}, {_e(near.remaining)}). Un remboursement anticipé a probablement eu lieu : dépose son courrier pour que tout soit reconstitué."))
            rows = []  # the contract's undated instalments cannot be placed against the dated table
        elif before:
            last = before[-1]
            gap = last.remaining - balance
            if gap == 0:
                checks.append(Check("ok", f"Le tableau daté part de {_e(balance)} au {_f(first.due_date)} : c'est exactement le capital auquel mène le plan ({last.source}) après le {_f(last.date)}."))
            elif abs(gap) <= Decimal("0.05"):
                checks.append(Check("ok", f"Le tableau daté part de {_e(balance)} ; le plan calculé donne {_e(last.remaining)} (écart d'arrondi de {_e(abs(gap))})."))
            else:
                hint = " Un remboursement anticipé manque peut-être (dépose son courrier)." if not events and gap > 0 else ""
                checks.append(Check("warn", f"Le tableau daté part de {_e(balance)} au {_f(first.due_date)} mais le plan donne {_e(last.remaining)} : écart de {_e(abs(gap))}.{hint}"))
        elif not rows:
            checks.append(Check("info", f"Le tableau daté part de {_e(balance)} au {_f(first.due_date)} ; sans contrat ni courrier, rien d'antérieur n'est reconstitué."))
        # Does the computed plan (when there is one) fall on the dated table?
        if start_of_new is not None:
            computed = [r for r in rows[start_of_new:] if r.date and r.date >= first.due_date]
            diffs = [abs(c.remaining - s.remaining) for c, s in zip(computed, schedule)]
            same_end = bool(computed) and computed[-1].date == schedule[-1].due_date and len(computed) == len(schedule)
            if computed and same_end and max(diffs) <= Decimal("0.10"):
                checks.append(Check("ok", f"Le plan recalculé retombe sur le tableau daté : mêmes {len(schedule)} échéances, écart maximal de {_e(max(diffs))} sur le capital restant dû."))
            elif computed:
                checks.append(Check("warn", "Le plan recalculé après le remboursement ne correspond pas au tableau daté (dates ou capitaux différents)."))
        rows = [r for r in rows if not (r.date and r.date >= first.due_date)]
        rows += [Row("installment", s.due_date, None, s.capital, s.interest, s.insurance, s.total, s.remaining, "échéancier") for s in schedule]
        if schedule and events and events[-1].end_after and schedule[-1].due_date != events[-1].end_after:
            checks.append(Check("warn", f"Le tableau daté se termine le {_f(schedule[-1].due_date)}, le courrier annonce le {_f(events[-1].end_after)}."))
        elif schedule and events:
            checks.append(Check("ok", f"Fin du prêt le {_f(schedule[-1].due_date)} : la date du tableau daté et celle du courrier de remboursement anticipé concordent."))
    elif events and not schedule and rows:
        checks.append(Check("info", "Pas de tableau daté : l'échéancier après le remboursement est calculé à partir du courrier et du taux."))

    plan.rows = rows
    _statuses(plan, today)
    _payments(session, loan, plan, checks)
    _finish(loan, plan, checks, today, offer, events)
    return plan


def _statuses(plan: Plan, today: date) -> None:
    seen_next = False
    for r in plan.rows:
        if r.date is None:
            continue
        if r.date <= today:
            r.status = "paid"
        elif r.kind == "installment" and not seen_next:
            r.status, seen_next = "next", True


def _check_offer(loan: Loan, offer: list[LoanInstallment], rate: Decimal | None, checks: list[Check]) -> None:
    capital = sum((i.capital for i in offer), ZERO)
    interest = sum((i.interest for i in offer), ZERO)
    problems = []
    if loan.principal is not None and capital != loan.principal:
        problems.append("la somme des amortissements n'est pas le montant emprunté")
    if loan.duration_months and len(offer) != loan.duration_months:
        problems.append(f"{len(offer)} échéances pour {loan.duration_months} mois")
    if loan.interest_total is not None and interest != loan.interest_total:
        problems.append("les intérêts du tableau ne sont pas ceux des conditions particulières")
    if problems:
        checks.append(Check("warn", "Contrat : " + " ; ".join(problems) + "."))
    else:
        checks.append(Check("ok", f"Contrat : {len(offer)} échéances, {_e(capital)} de capital remboursé (= montant emprunté), {_e(interest)} d'intérêts (= conditions particulières)."))
    if loan.total_cost is not None and loan.interest_total is not None and loan.insurance_mandatory is not None and loan.guarantee_fees is not None:
        total = loan.interest_total + loan.insurance_mandatory + loan.guarantee_fees
        checks.append(Check("ok" if total == loan.total_cost else "warn",
                            f"Coût total du crédit {_e(loan.total_cost)} = intérêts {_e(loan.interest_total)} + assurance obligatoire {_e(loan.insurance_mandatory)} + frais de garantie {_e(loan.guarantee_fees)}."
                            if total == loan.total_cost else f"Le coût total annoncé ({_e(loan.total_cost)}) n'est pas la somme de ses composantes ({_e(total)})."))
    if rate:
        _check_interest(offer, rate, "du contrat", checks, start=loan.principal)


def _check_schedule(loan: Loan, schedule: list[LoanInstallment], rate: Decimal | None, checks: list[Check]) -> None:
    if rate:
        balance = schedule[0].remaining + schedule[0].capital
        _check_interest(schedule, rate, "du tableau daté", checks, start=balance)


def _check_interest(rows: list[LoanInstallment], rate: Decimal, label: str, checks: list[Check], start: Decimal | None) -> None:
    """Interest = capital owed × rate / 12: recomputed on every instalment (the last one is often adjusted by the bank)."""
    balance = start if start is not None else rows[0].remaining + rows[0].capital
    off = 0
    for i, row in enumerate(rows):
        expected = cents(balance * rate / 1200)
        if abs(expected - row.interest) > CENT and i < len(rows) - 1:
            off += 1
        balance = row.remaining
    n = len(rows) - 1
    if off == 0:
        checks.append(Check("ok", f"Taux de {_pct(rate)} : les intérêts de {n} échéances {label} se recalculent à un centime près."))
    else:
        checks.append(Check("warn", f"Taux de {_pct(rate)} : {off} échéance(s) {label} ont des intérêts différents du calcul capital × taux / 12."))


def _payments(session: Session, loan: Loan, plan: Plan, checks: list[Check]) -> None:
    """The instalments the account statements show (lines of kind loan_payment naming this loan), against the plan."""
    if not loan.reference:
        return
    payments = [
        t for t in session.scalars(select(Transaction).where(Transaction.kind == "loan_payment").order_by(Transaction.date))
        if str((t.details or {}).get("loan_number") or t.reference or "").endswith(loan.reference)
    ]
    if not payments:
        return
    rows = [r for r in plan.installments if r.date]
    good, odd, unknown = 0, [], []
    for t in payments:
        near = [r for r in rows if abs((r.date - t.date).days) <= 5]
        if not near:
            unknown.append(t)
            continue
        row = min(near, key=lambda r: abs((r.date - t.date).days))
        if abs(-t.amount - row.total) <= CENT:
            good += 1
            row.confirmed = -t.amount
        else:
            odd.append((t, row))
    if good:
        checks.append(Check("ok", f"{good} prélèvement(s) retrouvé(s) dans tes relevés : montant et date conformes au plan."))
    for t, row in odd:
        checks.append(Check("warn", f"Prélèvement du {_f(t.date)} : {_e(-t.amount)} débités, {_e(row.total)} prévus au plan."))
    for t in unknown:
        checks.append(Check("warn", f"Prélèvement du {_f(t.date)} ({_e(-t.amount)}) : aucune échéance du plan à cette date."))


def _finish(loan: Loan, plan: Plan, checks: list[Check], today: date, offer: list, events: list[LoanEvent]) -> None:
    rows = plan.rows
    inst = plan.installments
    if not inst:
        return
    # Capital owed today: what the last line up to today leaves, or the amount borrowed before the first one.
    owed = rows[0].remaining + rows[0].capital if rows[0].kind == "installment" else loan.principal  # before the first line
    for r in rows:
        if r.date and r.date <= today:
            owed = r.remaining
    past = [r for r in inst if r.date and r.date <= today]
    future = [r for r in inst if r.date and r.date > today]
    end = inst[-1].date
    summary = {
        "as_of": today, "owed": owed, "end_date": end, "paid_count": len(past), "remaining_count": len(future), "total_count": len(inst),
        "next": future[0] if future else None, "dated": all(r.date for r in rows),
        "future_interest": sum((r.interest for r in future), ZERO), "future_insurance": sum((r.insurance for r in future), ZERO),
        "future_total": sum((r.total for r in future), ZERO), "paid_total": sum((r.total for r in past), ZERO),
        "paid_interest": sum((r.interest for r in past), ZERO), "lifetime_interest": sum((r.interest for r in inst), ZERO),
        "repaid_early": sum((r.capital for r in rows if r.kind == "prepayment"), ZERO),
    }
    # What the early repayment saves: the contract's remaining instalments against the ones that replaced them.
    if events and offer and loan.first_due_date:
        ev = events[0]
        cut = next((o for o in offer if o.remaining == ev.capital_before), None)
        if cut is not None:
            old = [o for o in offer if o.number > cut.number]
            after = [r for r in inst if r.date and r.date > (add_months(loan.first_due_date, cut.number - 1))]
            summary["saved_interest"] = sum((o.interest for o in old), ZERO) - sum((r.interest for r in after), ZERO)
            summary["saved_insurance"] = sum((o.insurance for o in old), ZERO) - sum((r.insurance for r in after), ZERO)
            summary["saved_months"] = len(old) - len(after)
    plan.summary = summary
    if inst and inst[-1].remaining == 0 and all(r.date for r in rows):
        ok = not plan.problems
        checks.append(Check("ok" if ok else "warn",
                            f"En appliquant le contrat{', le remboursement anticipé' if events else ''}{' et le tableau daté' if any(r.source == 'échéancier' for r in rows) else ''}, "
                            f"le prêt est entièrement remboursé le {_f(end)} (capital restant dû 0,00 €)."
                            + ("" if ok else " Des incohérences sont signalées ci-dessus.")))


# --------------------------------------------------------------------------------------------- syncing
def rebuild(session: Session, loan: Loan, today: date | None = None) -> Plan:
    """Refresh what is derived from the documents (dates, end, the account's balance) and return the plan."""
    session.flush()
    session.refresh(loan)
    inferred = infer_first_due(loan)
    if inferred is not None:
        loan.first_due_date = inferred
    plan = build(session, loan, today)
    if plan.rows:
        loan.end_date = plan.summary.get("end_date") or loan.end_date
        account = loan.account
        if plan.summary.get("owed") is not None:
            account.balance = plan.summary["owed"]
            account.balance_date = datetime.combine(plan.summary["as_of"], datetime.min.time())
        if loan.rate is not None and account.interest_rate is None:
            account.interest_rate = loan.rate
    session.flush()
    return plan


def refresh_all(session: Session, today: date | None = None) -> None:
    """The capital owed moves every month: bring the balance of every loan account up to date."""
    for loan in session.scalars(select(Loan)):
        if loan.installments or loan.events:
            rebuild(session, loan, today)


def set_first_due(session: Session, loan: Loan, day: date) -> Plan:
    loan.first_due_date = day
    return rebuild_without_inference(session, loan)


def rebuild_without_inference(session: Session, loan: Loan) -> Plan:
    session.flush()
    plan = build(session, loan)
    if plan.rows and plan.summary.get("owed") is not None:
        loan.account.balance = plan.summary["owed"]
        loan.account.balance_date = datetime.combine(plan.summary["as_of"], datetime.min.time())
        loan.end_date = plan.summary.get("end_date") or loan.end_date
    return plan


# ------------------------------------------------------------------------------------------- formatting
def _f(day: date | None) -> str:
    return day.strftime("%d/%m/%Y") if day else "?"


def _e(value: Decimal) -> str:
    whole, cents_ = f"{abs(value):.2f}".split(".")
    groups = []
    while len(whole) > 3:
        whole, tail = whole[:-3], whole[-3:]
        groups.insert(0, tail)
    groups.insert(0, whole)
    return ("−" if value < 0 else "") + " ".join(groups) + "," + cents_ + " €"


def _pct(value: Decimal) -> str:
    return f"{value:.2f}".replace(".", ",") + " %"
