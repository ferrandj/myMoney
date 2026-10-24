"""Fortuneo: monthly "Relevé de Compte" PDF.

Layout (read from word positions): a title ("Relevé de Compte N° 9 - 2026", the product, "Arrêté au <date>"), the holder's
address on the right, a box with the holder(s), the account number (RIB parts), the IBAN and the BIC, then the table
Date | Date de valeur | Opération | Débit | Crédit between "ANCIEN SOLDE CRÉDITEUR AU <date>" and
"NOUVEAU SOLDE CRÉDITEUR AU <date>", with "TOTAL DES OPÉRATIONS DU RELEVÉ" (total debits, total credits) just before.
Dates of operations have no year ("14/09"): it comes from the period. The declared totals are one more check on top of
"opening balance + operations = new balance".
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import date, timedelta
from decimal import Decimal

from ...services.rates import guess_account_type
from .. import pdftools
from ..base import Adapter, Detection, ParsedStatement, SourceDocument, TransactionData
from ..common import french_date, name_key, norm, parse_amount, parse_date
from ..lines import describe, identity_warnings, is_own
from ..lines import label as make_label
from ._identity import read_address_block, read_identity

DAY_MONTH_RE = re.compile(r"^(\d{2})/(\d{2})$")
FULL_DATE_RE = re.compile(r"^\d{2}/\d{2}/\d{4}$")
AMOUNT_RE = re.compile(r"^\d{1,3}(?:[ .]\d{3})*,\d{2}$|^\d+,\d{2}$")
CARD_RE = re.compile(r"^CARTE\s+(\d{2}/\d{2})\s+(.*)$", re.I)
ICS_RE = re.compile(r"^[A-Z]{2}\d{2}ZZZ[A-Z0-9]+$")
FEE_RE = re.compile(r"^(COTISATION|FRAIS|INACTIVIT[EÉ]|COMMISSION|ABONNEMENT)\b", re.I)
OWN_FORTUNEO_RE = re.compile(r"^virement\s+(avec|vers|depuis)\s+fortuneo", re.I)


@dataclass
class _Entry:
    day: int
    month: int
    value_date: date | None
    amount: Decimal
    first: str
    cont: list[str] = field(default_factory=list)


class FortuneoStatementPdf(Adapter):
    id = "fortuneo.statement_pdf"
    institution = "Fortuneo"
    kind = "statement"
    label = "Fortuneo · relevé de compte ou de livret (PDF)"
    description = "Le PDF « Relevé de Compte » (compte courant) ou « Relevé Livret » de l'espace client Fortuneo."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.25, "mentionne Fortuneo"),
            ("relevedecompte" in c or "relevelivret" in c, 0.25, "titre « Relevé de Compte » ou « Relevé Livret »"),
            ("anciensolde" in c and "nouveausolde" in c, 0.2, "« Ancien solde » et « Nouveau solde »"),
            ("totaldesoperationsdureleve" in c, 0.15, "« Total des opérations du relevé »"),
            ("datedevaleur" in c, 0.15, "colonne « Date de valeur »"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c:  # the layout alone is not enough: another bank's document must not look like ours
            score = min(score, 0.3)
        return Detection(round(min(score, 1.0), 2), reasons)

    # --------------------------------------------------------------------------------------------- parse
    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        st = ParsedStatement(source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, account_type="checking")
        entries: list[_Entry] = []
        current: _Entry | None = None
        boundary, amount_x, label_x = 461.0, 380.0, 130.0
        declared: dict[str, Decimal] = {}
        closing_seen = False

        for page_no, words in enumerate(doc.words(1.0)):
            rows = pdftools.lines(words, join_amounts=True)
            texts_by_row = [" ".join(w["text"] for w in row) for row in rows]
            if page_no == 0:
                self._read_header(st, rows, texts_by_row)
            in_table = False
            for row, text in zip(rows, texts_by_row):
                tokens = [w["text"] for w in row]
                compact = norm("".join(tokens)).replace(" ", "")
                if {"valeur", "operation", "debit", "credit"} <= {norm(t) for t in tokens}:
                    by = {norm(w["text"]): w for w in row}
                    # Amounts are right-aligned: the middle of the two headings' right edges splits debit and credit.
                    boundary = (by["debit"]["x1"] + by["credit"]["x1"]) / 2
                    amount_x = by["debit"]["x0"] - 40
                    in_table = True
                    continue
                if not in_table or closing_seen:
                    continue
                amounts = [w for w in row if AMOUNT_RE.match(w["text"]) and w["x0"] > amount_x]
                if compact.startswith("anciensolde"):
                    if amounts:
                        value = parse_amount(amounts[-1]["text"]) or Decimal(0)
                        st.opening_balance = -abs(value) if "debiteur" in compact else abs(value)
                        st.opening_date = french_date(text)
                elif compact.startswith("totaldesoperations"):
                    for w in amounts:
                        declared["credit" if w["x1"] > boundary else "debit"] = parse_amount(w["text"]) or Decimal(0)
                elif compact.startswith("nouveausolde"):
                    if amounts:
                        value = parse_amount(amounts[-1]["text"]) or Decimal(0)
                        st.balance = -abs(value) if "debiteur" in compact else abs(value)
                    st.balance_date = french_date(text) or st.period_end
                    closing_seen, in_table = True, False
                elif len(tokens) >= 3 and DAY_MONTH_RE.match(tokens[0]) and FULL_DATE_RE.match(tokens[1]) and row[0]["x0"] < 70:
                    if not amounts:
                        continue
                    day, month = (int(x) for x in DAY_MONTH_RE.match(tokens[0]).groups())
                    label_x = row[2]["x0"] if len(row) > 2 else label_x
                    first = " ".join(w["text"] for w in row[2:] if w not in amounts).strip()
                    value_amount = parse_amount(amounts[0]["text"]) or Decimal(0)
                    signed = abs(value_amount) if amounts[0]["x1"] > boundary else -abs(value_amount)
                    current = _Entry(day, month, parse_date(tokens[1]), signed, first)
                    entries.append(current)
                elif current is not None and row[0]["x0"] >= label_x - 5 and not amounts:
                    extra = " ".join(w["text"] for w in row if w["x0"] < amount_x).strip()
                    if extra:
                        current.cont.append(extra)

        if st.opening_date and not st.period_start:
            st.period_start = st.opening_date + timedelta(days=1)
        st.period_end = st.period_end or st.balance_date
        holder_key = name_key(st.holder_name) if st.holder_name else None
        st.transactions = [t for t in (_to_transaction(e, st, holder_key) for e in entries) if t is not None]
        if st.period_start is None and st.period_end:
            # The first statement of an account has no "ancien solde" date: it starts the month of its first operation.
            first = min((t.date for t in st.transactions), default=st.period_end)
            st.period_start = first.replace(day=1)
            st.extra["first_statement"] = True
        self._check(st, declared)
        return [st]

    # -------------------------------------------------------------------------------------------- header
    @staticmethod
    def _read_header(st: ParsedStatement, rows: list[list[dict]], texts: list[str]) -> None:
        read_identity(st, texts)
        title = next((i for i, t in enumerate(texts) if norm(t).startswith(("releve de compte", "releve livret"))), None)
        if title is not None:
            for text in texts[title : title + 4]:
                if m := re.search(r"N°\s*(\d+)(?:\s*-\s*(\d{4}))?", text):
                    st.extra["number"] = f"{m.group(1)}/{m.group(2)}" if m.group(2) else m.group(1)
            if norm(texts[title]).startswith("releve livret"):
                # A savings book: "Relevé Livret +", then "Compte 12345678 01" (client number and rank): no RIB, no IBAN.
                product = re.sub(r"(?i)^relev[ée]\s+", "", re.sub(r"\s*N°.*$", "", texts[title])).strip()
                st.extra["product"] = product
                st.account_type = guess_account_type(product, "savings")
                for text in texts[title + 1 : title + 4]:
                    if m := re.match(r"(?i)^compte\s+(\d{8})\s+(\d{2})\b", text):
                        st.account_number = m.group(1) + m.group(2)
            else:
                # The product ("COMPTE COURANT") is the first line under the title that is neither the number nor the page.
                for text in texts[title + 1 : title + 4]:
                    cleaned = re.sub(r"(?i)\s*page\s*\d+\s*/\s*\d+|N°\s*\d+(\s*-\s*\d{4})?", "", text).strip()
                    if cleaned and not norm(cleaned).startswith("arrete"):
                        st.extra["product"] = cleaned.capitalize()
                        st.account_type = guess_account_type(cleaned, "checking")
                        break
        for text in texts:
            if norm(text).startswith("arrete au"):
                st.period_end = french_date(text)
        arrete = next((i for i, t in enumerate(texts) if norm(t).startswith("arrete au")), None)
        titulaire = next((i for i, t in enumerate(texts) if "titulaire" in norm(t)), None)
        if arrete is not None and titulaire is not None and titulaire > arrete:
            name, address = read_address_block(rows, arrete, titulaire)
            st.holder_name = st.holder_name or name  # the box lists every holder: it is the better source
            st.holder_address = address
        # What the bank billed for its services the month before (the box at the bottom).
        for i, text in enumerate(texts):
            if norm(text).startswith("total facture") and (m := re.search(r"(\d[\d ]*,\d{2})", text)):
                st.extra["fees_billed"] = str(parse_amount(m.group(1)))
        product = st.extra.get("product", "Compte")
        if st.account_number:
            st.account_label = f"{product} Fortuneo …{st.account_number[-4:]}"

    # ------------------------------------------------------------------------------------ consistency
    @staticmethod
    def _check(st: ParsedStatement, declared: dict[str, Decimal]) -> None:
        total = sum((t.amount for t in st.transactions), Decimal(0))
        if st.opening_balance is not None and st.balance is not None:
            gap = st.balance - st.opening_balance - total
            if abs(gap) < Decimal("0.01"):
                st.confidence = "high"
            else:
                st.confidence = "low"
                st.warnings.append(f"Les opérations lues ne tombent pas juste : écart de {gap:.2f} entre l'ancien solde, les opérations et le nouveau solde")
        else:
            st.confidence = "low"
            st.warnings.append("Ancien solde ou nouveau solde introuvable : impossible de contrôler les opérations lues")
        debits = sum((-t.amount for t in st.transactions if t.amount < 0), Decimal(0))
        credits = sum((t.amount for t in st.transactions if t.amount > 0), Decimal(0))
        if declared:
            ok = declared.get("debit", Decimal(0)) == debits and declared.get("credit", Decimal(0)) == credits
            if not ok:
                st.confidence = "low"
                st.warnings.append(
                    f"Les totaux imprimés par la banque (débits {declared.get('debit', 0)}, crédits {declared.get('credit', 0)}) "
                    f"ne correspondent pas aux opérations lues ({debits}, {credits})"
                )
            st.extra["declared_totals"] = {k: str(v) for k, v in declared.items()}
        if st.period_start and st.period_end:
            outside = [t for t in st.transactions if not st.period_start - timedelta(days=7) <= t.date <= st.period_end]
            if outside:
                st.confidence = "low"
                st.warnings.append(f"{len(outside)} opération(s) datée(s) hors de la période du relevé")
        st.warnings += identity_warnings(st)
        if st.confidence == "high" and st.warnings:
            st.confidence = "medium"


# ---------------------------------------------------------------------------------------- operations
def _resolve(day: int, month: int, st: ParsedStatement) -> date | None:
    """Operation dates have no year: take the one that puts the date inside (or just around) the statement's period."""
    anchor = st.period_end or st.balance_date
    if anchor is None:
        return None
    if st.period_start is None:  # first statement: nothing says when it begins, and it can cover a whole year
        for year in (anchor.year, anchor.year - 1):
            try:
                candidate = date(year, month, day)
            except ValueError:
                continue
            if candidate <= anchor + timedelta(days=7):
                return candidate
        return None
    start = st.period_start
    for year in (anchor.year, anchor.year - 1, anchor.year + 1):
        try:
            candidate = date(year, month, day)
        except ValueError:
            continue
        if start - timedelta(days=7) <= candidate <= anchor + timedelta(days=7):
            return candidate
    return None


def _to_transaction(e: _Entry, st: ParsedStatement, holder_key: str | None) -> TransactionData | None:
    when = _resolve(e.day, e.month, st)
    if when is None:
        return None
    raw = " | ".join([e.first, *e.cont])
    details: dict = {}
    kind, counterparty, reference, memo = "other", None, None, " ".join(e.cont) or None
    first = e.first.strip()
    ids = [line for line in e.cont if " " not in line and len(line) >= 10 and re.fullmatch(r"[A-Za-z0-9]+", line)]
    words = [line for line in e.cont if line not in ids]

    if m := CARD_RE.match(first):
        kind, counterparty = "card", m.group(2).strip() or None
        bought = _resolve(*(int(x) for x in m.group(1).split("/")), st) if st.period_end else None
        details["purchase_date"] = bought.isoformat() if bought else None
        memo = " ".join(e.cont) or None
        label = make_label(counterparty, memo)
    elif m := re.match(r"^PRLV\s+(?:SEPA\s+)?(.*)$", first, flags=re.I):
        kind, counterparty = "direct_debit", m.group(1).strip() or None
        ics = next((i for i in ids if ICS_RE.match(i)), None)
        others = [i for i in ids if i != ics]
        details["ics"], details["ids"] = ics, others
        reference = others[0] if others else None
        memo = " ".join(words) or None
        label = make_label(counterparty, describe(words))
    elif m := re.match(r"^VIR\s+(?:(SEPA|INST)\s+)?(.*)$", first, flags=re.I):
        kind = "instant_transfer" if (m.group(1) or "").upper() == "INST" else "transfer"
        rest = m.group(2).strip()
        if OWN_FORTUNEO_RE.match(rest):  # between the holder's own Fortuneo accounts
            details["internal"] = True
            memo = " ".join([rest, *e.cont]).strip()
            label = make_label(rest, describe(e.cont))
        else:
            counterparty = rest or None
            reference = ids[0] if ids else None
            memo = " ".join(words) or None
            label = make_label(counterparty or "Virement", describe(words))
    elif m := re.match(r"^ECH\s+PRET\s+(\S+)", first, flags=re.I):
        kind, reference = "loan_payment", m.group(1)
        details["loan_number"] = m.group(1)
        label = f"Échéance de prêt …{m.group(1)[-4:]}"
    elif FEE_RE.match(first):
        kind = "fee"
        label = make_label(first, memo)
    elif re.match(r"^INT[EÉ]R[EÊ]TS?\b", first, flags=re.I):
        kind = "interest"
        label = make_label(first, memo)
    elif m := re.match(r"^(?:REM(?:ISE)?\.?\s+)?CH(?:Q|[EÈ]QUE)\b\s*(.*)$", first, flags=re.I):
        kind = "cheque"
        reference = (m.group(1).split() or [None])[0]
        label = make_label("Remise de chèque" if first.upper().startswith("REM") else "Chèque", f"n° {reference}" if reference else None)
    else:
        label = make_label(first, memo)
    if kind in ("transfer", "instant_transfer") and is_own(counterparty, holder_key):
        details["internal"] = True
    return TransactionData(
        date=when, label=label or first, amount=e.amount, currency=st.currency, value_date=e.value_date, kind=kind,
        counterparty=counterparty, reference=reference, memo=memo, raw=raw, details={k: v for k, v in details.items() if v not in (None, [])} or None,
    )
