"""Who may see and change what.

Every account has an owner. The owner can share it with other users, one by one, read only or with the right to edit.
A user sees their own accounts and the ones shared with them, the documents they dropped and the documents of accounts
they can see; the owner alone deletes an account or changes who it is shared with.

The signed-in user is carried by a context variable set for each web request (`as_user`), so the rule applies the same way
in every service without every function having to be handed the user. Outside a request (tests, maintenance jobs) nothing
is set and nothing is restricted: `restricted()` is False.
"""

from __future__ import annotations

from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass

from sqlalchemy import select
from sqlalchemy.orm import Session

from ..models import Account, AccountShare, Document, User


@dataclass(frozen=True)
class Identity:
    id: int
    email: str
    name: str | None
    is_admin: bool

    @property
    def label(self) -> str:
        return self.name or self.email


class AccessDenied(Exception):
    """The user may not do this (the web layer answers 404: an account one cannot see does not exist for them)."""


_current: ContextVar[Identity | None] = ContextVar("mymoney_user", default=None)


def current() -> Identity | None:
    return _current.get()


def restricted() -> bool:
    return _current.get() is not None


@contextmanager
def as_user(identity: Identity | None):
    token = _current.set(identity)
    try:
        yield
    finally:
        _current.reset(token)


@contextmanager
def unrestricted():
    """Housekeeping that must see everything (purging set-aside duplicates, refreshing every loan)."""
    with as_user(None):
        yield


# ------------------------------------------------------------------------------------------------- accounts
def owner_for_new(session: Session) -> int | None:
    """Who a new account or document belongs to: the signed-in user, or the first administrator when nobody is signed in
    (the command line, maintenance)."""
    me = current()
    if me is not None:
        return me.id
    return session.scalar(select(User.id).where(User.is_admin.is_(True)).order_by(User.id))


def readable_ids(session: Session) -> set[int] | None:
    """Accounts the current user can see; None when nothing is restricted."""
    me = current()
    if me is None:
        return None
    owned = set(session.scalars(select(Account.id).where(Account.owner_id == me.id)))
    shared = set(session.scalars(select(AccountShare.account_id).where(AccountShare.user_id == me.id)))
    return owned | shared


def editable_ids(session: Session) -> set[int] | None:
    me = current()
    if me is None:
        return None
    owned = set(session.scalars(select(Account.id).where(Account.owner_id == me.id)))
    shared = set(session.scalars(select(AccountShare.account_id).where(AccountShare.user_id == me.id, AccountShare.can_edit.is_(True))))
    return owned | shared


def only_readable(session: Session, accounts: list[Account]) -> list[Account]:
    ids = readable_ids(session)
    return accounts if ids is None else [a for a in accounts if a.id in ids]


def only_editable(session: Session, accounts: list[Account]) -> list[Account]:
    ids = editable_ids(session)
    return accounts if ids is None else [a for a in accounts if a.id in ids]


def check_read(session: Session, account_id: int) -> None:
    ids = readable_ids(session)
    if ids is not None and account_id not in ids:
        raise AccessDenied("Compte introuvable")


def check_edit(session: Session, account_id: int) -> None:
    check_read(session, account_id)
    ids = editable_ids(session)
    if ids is not None and account_id not in ids:
        raise AccessDenied("Ce compte t'est partagé en lecture seule")


def check_owner(session: Session, account_id: int) -> None:
    check_read(session, account_id)
    me = current()
    if me is None:
        return
    owner = session.scalar(select(Account.owner_id).where(Account.id == account_id))
    if owner != me.id:
        raise AccessDenied("Seul le propriétaire du compte peut faire cela")


def role(session: Session, account: Account) -> str:
    """"owner", "edit" or "read" for the current user (an unrestricted caller counts as the owner)."""
    me = current()
    if me is None or account.owner_id == me.id:
        return "owner"
    share = session.scalar(select(AccountShare).where(AccountShare.account_id == account.id, AccountShare.user_id == me.id))
    if share is None:
        raise AccessDenied("Compte introuvable")
    return "edit" if share.can_edit else "read"


# ----------------------------------------------------------------------------------------------- documents
def document_visible(session: Session, doc: Document) -> bool:
    """The user's own drops, and the documents attached to an account they can see."""
    me = current()
    if me is None or doc.owner_id == me.id:
        return True
    if doc.account_id is None:
        return False
    ids = readable_ids(session)
    return doc.account_id in ids


def check_document(session: Session, doc: Document | None, edit: bool = False) -> Document:
    if doc is None or not document_visible(session, doc):
        raise AccessDenied("Document introuvable")
    me = current()
    if edit and me is not None and doc.owner_id != me.id and doc.account_id is not None:
        check_edit(session, doc.account_id)
    elif edit and me is not None and doc.owner_id != me.id:
        raise AccessDenied("Document introuvable")
    return doc


def visible_documents(session: Session) -> list[Document]:
    docs = list(session.scalars(select(Document).order_by(Document.uploaded_at.desc())))
    return [d for d in docs if document_visible(session, d)]
