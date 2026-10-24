"""What every Fortuneo document says about the account and its holder (statement, fees recap...)."""

from __future__ import annotations

import re

from ..base import ParsedStatement
from ..common import find_iban, norm

ACCOUNT_RE = re.compile(r"\b(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\b")
BIC_RE = re.compile(r"\bB\s?IC\s+([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b")  # the font splits "BIC" into "B" and "IC"
HOLDER_RE = re.compile(r"(?i)titulaire\(s\)\s+du\s+compte\s*:?\s*(.+)$")
STREET_WORDS = {"rue", "avenue", "av", "bd", "boulevard", "chemin", "route", "impasse", "allee", "place", "quai", "residence", "lieu", "cours", "passage", "square"}


def read_identity(st: ParsedStatement, texts: list[str]) -> None:
    """Account number (RIB parts), IBAN, BIC and holder(s), wherever they are on the page."""
    for text in texts:
        if "numero de compte" in norm(text) and (m := ACCOUNT_RE.search(text)):
            st.bank_code, st.branch_code, st.account_number, st.rib_key = m.groups()
        if "IBAN" in text:
            st.iban = find_iban(text) or st.iban
        if m := BIC_RE.search(text):
            st.bic = m.group(1)
        if m := HOLDER_RE.search(text):
            st.holder_name = m.group(1).strip() or st.holder_name


def read_address_block(rows: list[list[dict]], after: int, before: int, from_x: float = 280) -> tuple[str | None, str | None]:
    """(name, address) of the block printed on the right, between two rows. The left-hand block (the bank's own address
    and phone numbers) sits on the same visual rows, so only the words from `from_x` on are read. The address starts at
    the first line that begins with a number (or has a street word); what precedes it is the name."""
    block = []
    for i in range(after + 1, before):
        text = " ".join(w["text"] for w in rows[i] if w["x0"] >= from_x).strip()
        if text:
            block.append(text)
    if not block:
        return None, None
    start = next((i for i, t in enumerate(block) if i > 0 and (t[0].isdigit() or STREET_WORDS & set(norm(t).split()))), max(len(block) - 2, 1))
    return (" ".join(block[:start]) or None), ("\n".join(block[start:]) or None)
