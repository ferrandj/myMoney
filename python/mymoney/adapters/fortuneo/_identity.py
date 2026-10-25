"""What every Fortuneo document says about the account and its holder (statement, fees recap...)."""

from __future__ import annotations

import re

from ..base import ParsedStatement
from ..common import find_iban, norm

ACCOUNT_RE = re.compile(r"\b(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\b")
BIC_RE = re.compile(r"\bB\s?IC\s+([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b")  # the font splits "BIC" into "B" and "IC"
HOLDER_RE = re.compile(r"(?i)titulaire\(s\)\s+du\s+compte\s*:?\s*(.+)$")
STREET_WORDS = {"rue", "avenue", "av", "bd", "boulevard", "chemin", "route", "impasse", "allee", "place", "quai", "residence", "lieu", "cours", "passage", "square"}


WRAP_WIDTH = 30  # the bank cuts a name line at 30 characters, even in the middle of a word
_NOT_A_NAME = ("numero de compte", "iban", "periode", "date", "identification", "intitule", "bic", "adresse")


def join_wrapped_name(parts: list[str]) -> str:
    """The holder line of a joint account runs over several rows: a row cut at the width continues the word on the next
    one ("…MME DUPO" + "NT MARTIN…" is "DUPONT MARTIN…"), a shorter row ended where the name did."""
    out = ""
    for part in (p.strip() for p in parts):
        if not part:
            continue
        out = part if not out else out + ("" if len(out) >= WRAP_WIDTH else " ") + part
    return out


def read_identity(st: ParsedStatement, texts: list[str]) -> None:
    """Account number (RIB parts), IBAN, BIC and holder(s), wherever they are on the page."""
    for i, text in enumerate(texts):
        if "numero de compte" in norm(text) and (m := ACCOUNT_RE.search(text)):
            st.bank_code, st.branch_code, st.account_number, st.rib_key = m.groups()
        if "IBAN" in text:
            st.iban = find_iban(text) or st.iban
        if m := BIC_RE.search(text):
            st.bic = m.group(1)
        if m := HOLDER_RE.search(text):
            parts = [m.group(1).strip()]
            j = i + 1
            while len(parts[-1]) >= WRAP_WIDTH - 1 and j < len(texts) and len(texts[j]) <= 40 and not norm(texts[j]).startswith(_NOT_A_NAME):
                parts.append(texts[j].strip())  # the holder line continues on the next row
                j += 1
            st.holder_name = join_wrapped_name(parts) or st.holder_name
            if len(parts) > 1:  # glued from rows cut at the width: probably right, but only the user can be sure of the spelling
                st.extra["holder_maybe_truncated"] = True


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
    return (join_wrapped_name(block[:start]) or None), ("\n".join(block[start:]) or None)
