"""PDF statements and other documents (life-insurance statements, account summaries...).

PDFs have no structure, so this works from the position of each word on the page:
  * the "Débit" / "Crédit" (or "Montant", "Solde") headers give the column of each amount;
  * a line starting with a date is an operation, the lines below it continue its label;
  * "ancien solde" / "nouveau solde" lines give the balances, used to check the result.
What was understood is always shown for review before anything is saved.
"""

from __future__ import annotations

import io
import re
from datetime import date, timedelta
from decimal import Decimal

from ..connectors.base import HoldingData, TransactionData
from ..services.rates import guess_account_type
from .common import ParsedStatement, find_iban, find_institution, norm, parse_amount, parse_date

AMOUNT_RE = re.compile(r"^[+-]?\d{1,3}(?:[.   ]?\d{3})*,\d{2}-?€?$|^[+-]?\d+\.\d{2}-?€?$")
INT_GROUP_RE = re.compile(r"^[+-]?\d{1,3}$")
DATE_RE = re.compile(r"^\d{1,2}[/.]\d{1,2}(?:[/.]\d{2,4})?$")
FULL_DATE_RE = re.compile(r"\b(\d{1,2}[/.]\d{1,2}[/.]\d{4})\b")
PERIOD_RE = re.compile(r"(?:du|periode du)\s+(\d{1,2}[/.]\d{1,2}[/.]\d{2,4})\s+au\s+(\d{1,2}[/.]\d{1,2}[/.]\d{2,4})", re.I)
ISIN_RE = re.compile(r"\b([A-Z]{2}[A-Z0-9]{9}\d)\b")

OPENING = ("ancien solde", "solde precedent", "solde initial", "solde au debut", "solde debut", "solde reporte", "report solde")
CLOSING = ("nouveau solde", "solde final", "solde en fin", "solde au", "solde crediteur au", "solde debiteur au", "solde fin", "solde a la date")
CREDIT_HINTS = ("vir recu", "virement recu", "vir sepa recu", "virement de", "vir inst recu", "remise", "interets", "remboursement",
                "avoir ", "depot", "versement", "salaire", "prime", "retrocession")
CANDIDATE_KEYS = ("solde", "valeur de rachat", "epargne acquise", "valorisation", "encours", "montant total", "capital", "total")
PRODUCT_WORDS = ("livret", "compte courant", "compte de depot", "compte cheque", "ldds", " lep", "plan epargne", "pel", " cel ",
                 "assurance vie", "assurance-vie", "pea", "compte titres", "compte-titres", "compte sur livret", "contrat", "per ")


class _Word:
    __slots__ = ("text", "x0", "x1", "top")

    def __init__(self, w: dict):
        self.text, self.x0, self.x1, self.top = w["text"], w["x0"], w["x1"], w["top"]


def _lines(words: list[dict]) -> list[list[_Word]]:
    items = sorted((_Word(w) for w in words), key=lambda w: (w.top, w.x0))
    lines: list[list[_Word]] = []
    for w in items:
        if lines and abs(lines[-1][0].top - w.top) <= 3:
            lines[-1].append(w)
        else:
            lines.append([w])
    for line in lines:
        line.sort(key=lambda w: w.x0)
    return lines


def _merge_amounts(line: list[_Word]) -> list[_Word]:
    """Re-join amounts split by a thousands space: "1" "234,56" -> "1 234,56"."""
    out: list[_Word] = []
    for w in line:
        if out and AMOUNT_RE.match(w.text) and INT_GROUP_RE.match(out[-1].text) and w.x0 - out[-1].x1 < 6:
            prev = out.pop()
            merged = _Word({"text": f"{prev.text} {w.text}", "x0": prev.x0, "x1": w.x1, "top": w.top})
            out.append(merged)
        else:
            out.append(w)
    return out


def _header_columns(line: list[_Word]) -> dict[str, float] | None:
    cols = {}
    for w in line:
        t = norm(w.text).strip(" :()€")
        if t in ("debit", "debits", "debit eur"):
            cols["debit"] = w.x1
        elif t in ("credit", "credits", "credit eur"):
            cols["credit"] = w.x1
        elif t in ("montant", "montants"):
            cols["amount"] = w.x1
        elif t in ("solde",):
            cols["balance"] = w.x1
    if ("debit" in cols and "credit" in cols) or ("amount" in cols and len(line) <= 12):
        return cols
    return None


def _nearest(cols: dict[str, float], x: float) -> str:
    return min(cols, key=lambda k: abs(cols[k] - x))


def _resolve_date(raw: str, year: int, period_end: date | None) -> date | None:
    d = parse_date(raw, default_year=year)
    if d and len(raw) <= 5 and period_end and d > period_end + timedelta(days=5):
        d = d.replace(year=d.year - 1)
    return d


def parse_pdf(data: bytes, filename: str = "") -> list[ParsedStatement]:
    import pdfplumber

    with pdfplumber.open(io.BytesIO(data)) as pdf:
        pages = [page.extract_words(x_tolerance=1.5, y_tolerance=2) for page in pdf.pages]
        text = "\n".join((page.extract_text() or "") for page in pdf.pages)
    if not text.strip():
        raise ValueError("Ce PDF ne contient pas de texte (document scanné) : saisis le solde à la main")

    st = ParsedStatement(source_format="pdf", institution=find_institution(text), iban=find_iban(text))
    ntext = norm(text)

    period = PERIOD_RE.search(ntext)
    if period:
        st.period_start, st.period_end = parse_date(period.group(1)), parse_date(period.group(2))
    full_dates = [d for d in (parse_date(m) for m in FULL_DATE_RE.findall(text)) if d and d <= date.today() + timedelta(days=31)]
    if not st.period_end and full_dates:
        st.period_end = max(full_dates)
    year = (st.period_end or date.today()).year

    for raw_line in text.splitlines()[:80]:
        line = f" {norm(raw_line)} "
        if any(word in line for word in PRODUCT_WORDS) and len(raw_line) < 90:
            st.account_label = raw_line.strip()
            break
    number = re.search(r"(?:n°|numero|no\.?)\s*(?:de\s+)?(?:compte|contrat)?\s*:?\s*([0-9][0-9 ]{6,}[0-9])", ntext)
    if number:
        st.account_number = number.group(1).replace(" ", "")
    st.account_type = guess_account_type(st.account_label or "", "checking") if st.account_label else None

    cols: dict[str, float] | None = None
    unsure_signs = 0
    last_tx: TransactionData | None = None
    last_top = 0.0
    label_x = 0.0
    continuation = 0
    holdings: list[HoldingData] = []

    for words in pages:
        for line in _lines(words):
            line = _merge_amounts(line)
            header = _header_columns(line)
            if header:
                cols = header
                last_tx = None
                continue
            texts = [w.text for w in line]
            joined = norm(" ".join(texts))
            amounts = [w for w in line if AMOUNT_RE.match(w.text)]

            isin = ISIN_RE.search(" ".join(texts))
            if isin and amounts:
                numbers = [parse_amount(w.text) for w in amounts]
                qty = next((n for w, n in zip(amounts, numbers) if n is not None and re.search(r"[,.]\d{3,}", w.text)), None)
                name = " ".join(t for t in texts if t != isin.group(1) and not AMOUNT_RE.match(t))[:120]
                holdings.append(
                    HoldingData(symbol=None, isin=isin.group(1), name=name or isin.group(1), quantity=qty or Decimal(0),
                                asset_class="fund", value=numbers[-1], value_currency="EUR")
                )
                continue

            if "solde" in joined and amounts:
                w = amounts[-1]
                value = parse_amount(w.text)
                if value is not None:
                    if cols and _nearest(cols, w.x1) == "debit" or "debiteur" in joined:
                        value = -abs(value)
                    if any(k in joined for k in OPENING):
                        st.opening_balance = st.opening_balance if st.opening_balance is not None else value
                    elif any(k in joined for k in CLOSING) or "solde" in joined:
                        st.balance = value
                        found = next((parse_date(t) for t in texts if FULL_DATE_RE.fullmatch(t)), None)
                        st.balance_date = found or st.period_end
                last_tx = None
                continue
            if any(k in joined for k in CANDIDATE_KEYS) and amounts:
                st.candidate_balances.append((" ".join(t for t in texts if not AMOUNT_RE.match(t))[:80], parse_amount(amounts[-1].text)))

            if line and DATE_RE.match(line[0].text) and amounts:
                when = _resolve_date(line[0].text, year, st.period_end)
                if when is None:
                    continue
                rest = [w for w in line[1:] if w not in amounts]
                while rest and DATE_RE.match(rest[0].text):  # value date column
                    rest.pop(0)
                label = " ".join(w.text for w in rest).strip()
                if "total" in norm(label) or not label:
                    continue
                if cols:
                    tx_amounts = [(w, _nearest(cols, w.x1)) for w in amounts]
                    tx_amounts = [(w, c) for w, c in tx_amounts if c != "balance"]
                else:
                    tx_amounts = [(amounts[0], None)]
                if not tx_amounts:
                    continue
                w, column = tx_amounts[0]
                value = parse_amount(w.text)
                if value is None:
                    continue
                if column == "debit":
                    value = -abs(value)
                elif column == "credit":
                    value = abs(value)
                elif not w.text.startswith(("+", "-")) and not w.text.endswith("-"):
                    unsure_signs += 1
                    value = abs(value) if any(h in f" {norm(label)} " for h in CREDIT_HINTS) else -abs(value)
                last_tx = TransactionData(date=when, label=label, amount=value, currency=st.currency)
                st.transactions.append(last_tx)
                last_top = line[0].top
                label_x = rest[0].x0 if rest else line[0].x1
                continuation = 0
            elif last_tx and not amounts and continuation < 2 and 0 < line[0].top - last_top < 20 and line[0].x0 >= label_x - 3:
                extra = " ".join(texts).strip()
                if extra and not DATE_RE.match(line[0].text):
                    last_tx.label = f"{last_tx.label} {extra}"[:500]
                    last_top = line[0].top
                    continuation += 1

    st.transactions.sort(key=lambda t: t.date)
    if st.transactions:
        st.period_start = st.period_start or st.transactions[0].date
        st.period_end = st.period_end or st.transactions[-1].date
    if holdings and not st.transactions:
        st.holdings = holdings
        st.account_type = st.account_type if st.account_type in ("life_insurance", "pea", "cto", "per") else "life_insurance"

    # Check: opening balance + operations = closing balance.
    if st.transactions and st.opening_balance is not None and st.balance is not None:
        gap = st.balance - st.opening_balance - sum(t.amount for t in st.transactions)
        if abs(gap) < Decimal("0.01"):
            st.confidence = "high"
            st.warnings.clear()
        else:
            st.confidence = "low"
            st.warnings.append(f"Écart de {gap:.2f} entre les soldes et les opérations lues : vérifie les lignes")
    elif st.transactions:
        st.confidence = "low" if unsure_signs else "medium"
    else:
        st.confidence = "low"
    if unsure_signs and st.confidence != "high":
        st.warnings.append(f"Sens (débit/crédit) deviné pour {unsure_signs} opération(s)")
    if not st.transactions and st.balance is None and not st.holdings:
        seen = set()
        st.candidate_balances = [(l, a) for l, a in st.candidate_balances if a is not None and not (a in seen or seen.add(a))][:10]
        if not st.candidate_balances:
            st.warnings.append("Aucune opération ni montant reconnu : document conservé, saisis le solde à la main si besoin")
    else:
        st.candidate_balances = []
    return [st]
