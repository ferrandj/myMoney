"""Users: who can sign in, and the key slot that holds each one's password."""

from __future__ import annotations

import hashlib
import re

from sqlalchemy import func, select
from sqlalchemy.orm import Session

from ..models import Account, AccountShare, Document, User
from ..security.vault import Vault
from .access import Identity

EMAIL_RE = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")


def normalize_email(email: str | None) -> str:
    value = (email or "").strip().lower()
    if not EMAIL_RE.match(value) or len(value) > 320:
        raise ValueError("Adresse e-mail invalide")
    return value


def slot_id(email: str) -> str:
    """Name of the user's key slot in the keystore: a hash, so the keystore file does not list e-mail addresses."""
    return hashlib.sha256(("mymoney-user|" + normalize_email(email)).encode()).hexdigest()[:32]


def identity(user: User) -> Identity:
    return Identity(user.id, user.email, user.name, user.is_admin)


def count(session: Session) -> int:
    return session.scalar(select(func.count()).select_from(User)) or 0


def get_by_email(session: Session, email: str | None) -> User | None:
    try:
        return session.scalar(select(User).where(User.email == normalize_email(email)))
    except ValueError:
        return None


def all_users(session: Session) -> list[User]:
    return list(session.scalars(select(User).order_by(User.name, User.email)))


def first_admin(session: Session) -> User | None:
    return session.scalar(select(User).where(User.is_admin.is_(True)).order_by(User.id))


def create(session: Session, email: str, name: str | None, is_admin: bool = False) -> User:
    email = normalize_email(email)
    if get_by_email(session, email) is not None:
        raise ValueError("Cette adresse e-mail a déjà un utilisateur")
    user = User(email=email, name=(name or "").strip()[:200] or None, is_admin=is_admin)
    session.add(user)
    session.flush()
    return user


def claim_legacy_data(session: Session, user: User) -> None:
    """Accounts and documents that existed before there were users belong to the first user."""
    for model in (Account, Document):
        for row in session.scalars(select(model).where(model.owner_id.is_(None))):
            row.owner_id = user.id
    session.flush()


def add_with_password(session: Session, vault: Vault, email: str, name: str | None, password: str, is_admin: bool = False) -> User:
    """A new user with their own password to the vault (checked for strength before anything is written)."""
    from ..security.vault import check_password_strength

    check_password_strength(password)
    user = create(session, email, name, is_admin)
    vault.add_user_slot(slot_id(user.email), password)
    return user


def remove(session: Session, vault: Vault, user: User, heir: User) -> int:
    """Delete a user and their key slot. Their accounts go to `heir`; shares they hold disappear. Returns accounts moved."""
    if user.id == heir.id:
        raise ValueError("Impossible de se supprimer soi-même")
    if user.is_admin and sum(1 for u in all_users(session) if u.is_admin) <= 1:
        raise ValueError("Il doit rester au moins un administrateur")
    moved = 0
    for account in session.scalars(select(Account).where(Account.owner_id == user.id)):
        account.owner_id = heir.id
        moved += 1
        for share in [s for s in account.shares if s.user_id == heir.id]:
            session.delete(share)  # the heir owns it now: a share to themselves would be meaningless
    for doc in session.scalars(select(Document).where(Document.owner_id == user.id)):
        doc.owner_id = heir.id
    for share in session.scalars(select(AccountShare).where(AccountShare.user_id == user.id)):
        session.delete(share)
    session.flush()
    vault.remove_user_slot(slot_id(user.email))
    session.delete(user)
    session.flush()
    return moved


# ---------------------------------------------------------------------------------------------- sharing
def share(session: Session, account: Account, user: User, can_edit: bool) -> AccountShare:
    if account.owner_id == user.id:
        raise ValueError("Cette personne est déjà propriétaire du compte")
    existing = next((s for s in account.shares if s.user_id == user.id), None)
    if existing is not None:
        existing.can_edit = can_edit
        session.flush()
        return existing
    row = AccountShare(account_id=account.id, user_id=user.id, can_edit=can_edit)
    session.add(row)
    session.flush()
    account.shares.append(row)
    return row


def unshare(session: Session, account: Account, user_id: int) -> None:
    for row in [s for s in account.shares if s.user_id == user_id]:
        account.shares.remove(row)
        session.delete(row)
    session.flush()
