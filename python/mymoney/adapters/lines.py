"""Helpers shared by the adapters that turn the lines of a statement into labelled operations."""

from __future__ import annotations

import re

from .common import name_key, norm

# Words that show a transfer goes to / comes from one of the holder's own products rather than a person.
OWN_PRODUCT_WORDS = {"livret", "epargne", "pel", "pea", "ldds", "ldd", "lep", "cel", "compte", "assurance"}
WRAP_WIDTH = 32  # a label line at least this long was probably wrapped by the bank, even in the middle of a word


def describe(lines: list[str]) -> str | None:
    """The line that says what the operation is for: not an identifier (long digit runs), and made of real words."""
    for line in lines:
        words = line.split()
        if not any(re.search(r"\d{6,}", w) for w in words) and any(len(w) >= 3 and w.isalpha() for w in words):
            return line
    return lines[0] if lines else None


def join_wrapped(lines: list[str]) -> str:
    """Join detail lines; a short lowercase fragment under a full-width line is the end of a word the bank cut ("Mathi" + "eu")."""
    text, previous = "", ""
    for line in (l.strip() for l in lines):
        if not line:
            continue
        glued = bool(text) and len(previous) >= WRAP_WIDTH and " " not in line and line.islower() and len(line) <= 4
        text = f"{text}{line}" if glued else f"{text} {line}".strip()
        previous = line
    return text


def label(primary: str | None, secondary: str | None) -> str:
    parts = [p.strip() for p in (primary, secondary) if p and p.strip()]
    return " — ".join(parts)[:300]


def is_own(name: str | None, holder_key: str | None) -> bool:
    """The other party is the holder (same name, whatever the order) or one of the holder's own products."""
    if not name:
        return False
    if holder_key and name_key(name) == holder_key:
        return True
    return bool(OWN_PRODUCT_WORDS & set(norm(name).split()))


def identity_warnings(st) -> list[str]:
    """Checks on what identifies the account: the IBAN, the RIB key, and the IBAN against the RIB."""
    from .common import iban_matches_rib, iban_valid, rib_valid

    warnings = []
    if st.iban and not iban_valid(st.iban):
        warnings.append("L'IBAN lu n'est pas valide (clé de contrôle)")
    if st.bank_code and st.branch_code and st.account_number and st.rib_key:
        if not rib_valid(st.bank_code, st.branch_code, st.account_number, st.rib_key):
            warnings.append("La clé RIB lue ne correspond pas au numéro de compte")
        if st.iban and not iban_matches_rib(st.iban, st.bank_code, st.branch_code, st.account_number, st.rib_key):
            warnings.append("L'IBAN ne correspond pas au RIB du document")
    return warnings
