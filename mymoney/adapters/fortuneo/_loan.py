"""What the Fortuneo loan documents have in common: money and dates as printed, reference numbers, amortization rows.

The documents of a loan (offer, early repayment notice, amortization table) never repeat each other's figures in the same
form, so every adapter turns what it reads into the same plain structure (`extra["loan"]`, strings only) that the loans
service stores and cross-checks.
"""

from __future__ import annotations

import re
from datetime import date
from decimal import Decimal

from .. import pdftools
from ..base import SourceDocument
from ..common import norm

AMT = r"\d{1,3}(?: \d{3})*,\d{2}|\d+,\d{2}"  # "122 082,74" (the thousands are a separate word in the PDF) or "102399,31"
ROW_RE = re.compile(rf"(?P<head>\S+)\s+({AMT})\s+({AMT})\s+({AMT})\s+({AMT})\s+({AMT})\s+({AMT})")
REFERENCE_RE = re.compile(r"\b([A-Z]{2}\d{8,})\b")


def money(text: str) -> Decimal:
    return Decimal(text.replace(" ", "").replace(" ", "").replace(",", "."))


def amounts(text: str) -> list[Decimal]:
    return [money(m) for m in re.findall(AMT, text)]


def numeric_date(text: str) -> date | None:
    m = re.search(r"(\d{2})/(\d{2})/(\d{4})", text)
    if not m:
        return None
    try:
        return date(int(m.group(3)), int(m.group(2)), int(m.group(1)))
    except ValueError:
        return None


def row_texts(doc: SourceDocument) -> list[str]:
    """Every row of every page, words joined with a space (the PDF's own text loses most of its spaces)."""
    out: list[str] = []
    for words in doc.words(1.0):
        out += [" ".join(w["text"] for w in row) for row in pdftools.lines(words)]
    return out


def parse_rows(texts: list[str], dated: bool) -> list[dict]:
    """The lines of an amortization table, in order. The first column is the date (current table) or the number (offer)."""
    rows: list[dict] = []
    seen_header = False
    for text in texts:
        n = norm(text)
        if "amortissement" in n and "interets" in n:
            seen_header = True
            continue
        if not seen_header:
            continue
        m = ROW_RE.match(text)
        if not m:
            continue
        head = m.group("head")
        if dated:
            when = numeric_date(head)
            if when is None or not re.fullmatch(r"\d{2}/\d{2}/\d{4}", head):
                continue
            first: dict = {"date": when.isoformat()}
        else:
            if not head.isdigit() or int(head) != len(rows) + 1:
                continue
            first = {"n": int(head)}
        capital, interest, deferred, insurance, total, remaining = (str(money(g)) for g in m.groups()[1:])
        rows.append({**first, "capital": capital, "interest": interest, "deferred": deferred, "insurance": insurance, "total": total, "remaining": remaining})
    return rows


def check_rows(rows: list[dict], start: Decimal | None, warnings: list[str]) -> None:
    """Each line adds up, the remaining capital goes down line by line from `start` and ends at zero."""
    bad_sum, bad_chain = [], []
    balance = start
    for i, r in enumerate(rows, 1):
        capital, remaining = Decimal(r["capital"]), Decimal(r["remaining"])
        if capital + Decimal(r["interest"]) + Decimal(r["deferred"]) + Decimal(r["insurance"]) != Decimal(r["total"]):
            bad_sum.append(i)
        if balance is not None and balance - capital != remaining:
            bad_chain.append(i)
        balance = remaining
    if bad_sum:
        warnings.append(f"Capital + intérêts + assurance ≠ total à payer sur {len(bad_sum)} échéance(s) (n° {', '.join(map(str, bad_sum[:5]))}…)")
    if bad_chain:
        warnings.append(f"Le capital restant dû ne suit pas les amortissements sur {len(bad_chain)} échéance(s) (n° {', '.join(map(str, bad_chain[:5]))}…)")
    if rows and Decimal(rows[-1]["remaining"]) != 0:
        warnings.append(f"Le capital restant dû après la dernière échéance n'est pas nul ({rows[-1]['remaining']})")


def loan_label(product: str | None, reference: str | None) -> str:
    word = (product or "").split()[0].capitalize() if product else ""
    return " ".join(x for x in (f"Prêt {word}".strip(), f"…{reference[-4:]}" if reference else "") if x)
