"""Contacts: remember who an IBAN or a name belongs to, so the next import recognises them by itself."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import date
from decimal import Decimal

from sqlalchemy import select
from sqlalchemy.orm import selectinload
from sqlalchemy.orm import Session

from ..adapters.common import iban_valid, name_key
from ..models import COUNTERPARTY_KINDS, Counterparty, CounterpartyIdentifier, Transaction
from . import access

# Card payments are shops: listing each merchant as "someone to identify" would drown the people and organisations
# that matter. They are still matched when an identifier exists, and can be shown on request.
SHOP_KINDS = {"card"}


def normalize_iban(value: str | None) -> str | None:
    iban = "".join((value or "").split()).upper()
    return iban or None


def load_index(session: Session) -> dict[tuple[str, str], int]:
    return {(i.kind, i.value): i.counterparty_id for i in session.scalars(select(CounterpartyIdentifier))}


def match(index: dict[tuple[str, str], int], name: str | None, iban: str | None) -> int | None:
    """The contact an operation belongs to: the IBAN is exact, the name is compared in a normalised form."""
    iban = normalize_iban(iban)
    if iban and (hit := index.get(("iban", iban))):
        return hit
    if name:
        return index.get(("name", name_key(name)))
    return None


@dataclass
class Unresolved:
    kind: str  # iban | name
    key: str  # the IBAN or the name key
    name: str | None
    iban: str | None
    count: int
    total_in: Decimal
    total_out: Decimal
    last: date
    accounts: set[int]


def unresolved(
    session: Session, include_shops: bool = False, document_id: int | None = None, account_id: int | None = None
) -> list[Unresolved]:
    """Counterparties seen in the operations but not linked to a contact yet, most frequent first."""
    groups: dict[tuple[str, str], Unresolved] = {}
    query = select(Transaction).where(
        Transaction.counterparty_id.is_(None), (Transaction.counterparty_name.is_not(None)) | (Transaction.counterparty_iban.is_not(None))
    )
    if document_id is not None:
        query = query.where(Transaction.document_id == document_id)
    if account_id is not None:
        query = query.where(Transaction.account_id == account_id)
    visible = access.readable_ids(session)
    for t in session.scalars(query):
        if visible is not None and t.account_id not in visible:
            continue
        if t.internal or (t.kind in SHOP_KINDS and not include_shops and not t.counterparty_iban):
            continue
        iban = normalize_iban(t.counterparty_iban)
        key = ("iban", iban) if iban else ("name", name_key(t.counterparty_name or ""))
        g = groups.get(key)
        if g is None:
            g = groups[key] = Unresolved(key[0], key[1], t.counterparty_name, iban, 0, Decimal(0), Decimal(0), t.date, set())
        g.count += 1
        g.accounts.add(t.account_id)
        g.name = g.name or t.counterparty_name
        if t.amount >= 0:
            g.total_in += t.amount
        else:
            g.total_out += -t.amount
        g.last = max(g.last, t.date)
    return sorted(groups.values(), key=lambda g: (-g.count, g.name or ""))


def _check_identifier(session: Session, kind: str, value: str | None, owner_id: int | None) -> tuple[str, CounterpartyIdentifier | None] | None:
    """Normalise an identifier and make sure it is usable: (value, the identifier already holding it), or None if empty.

    Raises ValueError for an invalid IBAN, or one that already belongs to another contact (never silently stolen).
    """
    if not (value or "").strip():
        return None
    if kind == "iban":
        value = normalize_iban(value)
        if not iban_valid(value):
            raise ValueError("IBAN invalide (vérifie qu'il est complet)")
    else:
        value = name_key(value)
    existing = session.scalar(select(CounterpartyIdentifier).where(CounterpartyIdentifier.kind == kind, CounterpartyIdentifier.value == value))
    if existing is not None and existing.counterparty_id != owner_id:
        raise ValueError(f"Déjà associé à « {existing.counterparty.name} » : fusionne les deux contacts si c'est la même personne")
    return value, existing


def add_identifier(session: Session, cp: Counterparty, kind: str, value: str | None, label: str | None = None) -> bool:
    """Attach an IBAN or a printed name to a contact. False when there is nothing to add or it is already attached."""
    checked = _check_identifier(session, kind, value, cp.id)
    if checked is None or checked[1] is not None:
        return False
    session.add(CounterpartyIdentifier(counterparty=cp, kind=kind, value=checked[0], label=(label or None) and label[:300]))
    session.flush()
    return True


def save_contact(session: Session, contact_id: int | None, data: dict[str, str]) -> Counterparty:
    cp = session.get(Counterparty, contact_id) if contact_id else Counterparty()
    if contact_id is None:
        session.add(cp)
    cp.name = (data.get("name") or "").strip() or cp.name or "Sans nom"
    kind = data.get("kind") or cp.kind or "person"
    cp.kind = kind if kind in COUNTERPARTY_KINDS else "other"
    cp.phone = (data.get("phone") or "").strip() or None
    cp.email = (data.get("email") or "").strip() or None
    cp.notes = (data.get("notes") or "").strip() or None
    session.flush()
    return cp


def choices(session: Session) -> list[tuple[int, str]]:
    """(id, name) of every contact, for the "already known?" lists."""
    return [(c.id, c.name) for c in session.scalars(select(Counterparty).order_by(Counterparty.name))]


def identify(session: Session, data: dict[str, str]) -> Counterparty:
    """Name an unknown counterparty (new contact, or an existing one when `contact_id` is given) and remember it."""
    contact_id = int(data["contact_id"]) if (data.get("contact_id") or "").isdigit() else None
    if contact_id is None and not (data.get("name") or "").strip():
        raise ValueError("Donne un nom, ou choisis un contact déjà enregistré")
    # Everything is checked before anything is created: a refused identifier leaves no half-made contact behind.
    _check_identifier(session, "iban", data.get("iban"), contact_id)
    _check_identifier(session, "name", data.get("printed_name"), contact_id)
    if contact_id:
        cp = session.get(Counterparty, contact_id)
        if cp is None:
            raise ValueError("Contact introuvable")
    else:
        cp = save_contact(session, None, data)
    add_identifier(session, cp, "iban", data.get("iban"))
    if data.get("printed_name"):
        add_identifier(session, cp, "name", data["printed_name"], data["printed_name"])
    relink(session)
    return cp


def relink(session: Session) -> int:
    """Link every operation that now matches a contact (after an identifier was added)."""
    index = load_index(session)
    if not index:
        return 0
    linked = 0
    for t in session.scalars(
        select(Transaction).where(
            Transaction.counterparty_id.is_(None), (Transaction.counterparty_name.is_not(None)) | (Transaction.counterparty_iban.is_not(None))
        )
    ):
        hit = match(index, t.counterparty_name, t.counterparty_iban)
        if hit:
            t.counterparty_id = hit
            linked += 1
    session.flush()
    return linked


def remove_identifier(session: Session, identifier_id: int) -> None:
    ident = session.get(CounterpartyIdentifier, identifier_id)
    if ident is None:
        return
    # Operations matched through this identifier lose their link (another identifier of the contact may restore it).
    cp_id = ident.counterparty_id
    session.delete(ident)
    session.flush()
    index = load_index(session)
    for t in session.scalars(select(Transaction).where(Transaction.counterparty_id == cp_id)):
        if match(index, t.counterparty_name, t.counterparty_iban) != cp_id:
            t.counterparty_id = None
    session.flush()


def delete_contact(session: Session, contact_id: int) -> None:
    cp = session.get(Counterparty, contact_id)
    if cp is not None:
        session.delete(cp)  # operations keep their printed name and IBAN, only the link is dropped (ON DELETE SET NULL)
        session.flush()


def merge(session: Session, source_id: int, target_id: int) -> None:
    source, target = session.get(Counterparty, source_id), session.get(Counterparty, target_id)
    if source is None or target is None or source.id == target.id:
        return
    for ident in list(source.identifiers):
        ident.counterparty_id = target.id
    for t in session.scalars(select(Transaction).where(Transaction.counterparty_id == source.id)):
        t.counterparty_id = target.id
    target.phone = target.phone or source.phone
    target.email = target.email or source.email
    target.notes = "\n".join(n for n in (target.notes, source.notes) if n) or None
    session.flush()
    session.expire(source)
    session.delete(source)
    session.flush()


@dataclass
class ContactSummary:
    contact: Counterparty
    count: int
    total_in: Decimal
    total_out: Decimal
    last: date | None


def summaries(session: Session) -> list[ContactSummary]:
    stats: dict[int, ContactSummary] = {}
    contacts = list(session.scalars(select(Counterparty).options(selectinload(Counterparty.identifiers)).order_by(Counterparty.name)))
    for c in contacts:
        stats[c.id] = ContactSummary(c, 0, Decimal(0), Decimal(0), None)
    visible = access.readable_ids(session)
    for cid, amount, day, account_id in session.execute(
        select(Transaction.counterparty_id, Transaction.amount, Transaction.date, Transaction.account_id).where(Transaction.counterparty_id.is_not(None))
    ):
        if visible is not None and account_id not in visible:
            continue
        s = stats[cid]
        s.count += 1
        if amount >= 0:
            s.total_in += amount
        else:
            s.total_out += -amount
        s.last = max(s.last, day) if s.last else day
    return list(stats.values())
