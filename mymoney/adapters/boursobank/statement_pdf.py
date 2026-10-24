"""BoursoBank (Boursorama): monthly "Extrait de votre compte en EUR" PDF.

Layout this adapter relies on (all of it is read from word positions, nothing is hard-coded to one account):
  * top right: the holder's name and postal address, under the title block;
  * a banner with the RIB (bank, branch, account, key), the currency and the period "du ... au ...";
  * a line with the BIC and the IBAN;
  * a table "Mouvements en EUR": operation date | label (first line, then detail lines) | value date | debit | credit,
    opened by "SOLDE AU : <date>" and closed by "Nouveau solde en EUR :".
Debit and credit are told apart by the column the amount sits in, then double-checked against the balances:
opening balance + operations must give the new balance, otherwise the statement is flagged and nothing is trusted blindly.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import date
from decimal import Decimal

from .. import pdftools
from ..base import Adapter, Detection, ParsedStatement, SourceDocument, TransactionData
from ..common import find_iban, iban_matches_rib, iban_valid, name_key, norm, parse_amount, parse_date, rib_valid

DATE_RE = re.compile(r"^\d{2}/\d{2}/\d{4}$")
AMOUNT_RE = re.compile(r"^\d{1,3}(?:\.\d{3})*,\d{2}$|^\d+,\d{2}$")
RIB_ROW_RE = re.compile(r"\b(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\s+([A-Z]{3})\b")
PERIOD_RE = re.compile(r"du\s+(\d{2}/\d{2}/\d{4})\s+au\s+(\d{2}/\d{2}/\d{4})")
BIC_RE = re.compile(r"B\.?I\.?C\.?\s+([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b")
REF_RE = re.compile(r"^R[^\W\d_]f\s*:?\s*(\S.*)$")  # "Réf : X", "Rèf : X", "Ref X" (the accent is not reliable in the PDF)
RUM_RE = re.compile(r"^RUM\s+(\S.*)$")
CARD_RE = re.compile(r"^CARTE\s+(\d{2}/\d{2}/\d{2})\s+(.*?)(?:\s+CB\*(\d{4}))?\s*$")
STREET_WORDS = {"rue", "avenue", "av", "bd", "boulevard", "chemin", "route", "impasse", "allee", "place", "quai", "residence", "lieu", "cours", "passage", "square"}
# Words that show a transfer goes to / comes from one of the holder's own products rather than a person.
OWN_PRODUCT_WORDS = {"livret", "epargne", "pel", "pea", "ldds", "ldd", "lep", "cel", "compte", "assurance"}
DEFAULT_BOUNDARY = 486.0


@dataclass
class _Entry:
    date: date
    value_date: date | None
    amount: Decimal
    first: str
    cont: list[str] = field(default_factory=list)


class BoursoBankStatementPdf(Adapter):
    id = "boursobank.statement_pdf"
    institution = "BoursoBank"
    kind = "statement"
    label = "BoursoBank · extrait de compte mensuel (PDF)"
    description = "Le PDF « Extrait de votre compte en EUR » téléchargé dans l'espace client BoursoBank / Boursorama."
    formats = ("pdf",)

    # ----------------------------------------------------------------------------------------- detection
    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact
        score, reasons = 0.0, []
        for ok, points, why in (
            ("boursobank" in c or "boursorama" in c, 0.3, "mentionne BoursoBank / Boursorama"),
            ("extraitdevotrecompte" in c, 0.25, "titre « Extrait de votre compte »"),
            ("mouvementsen" in c, 0.2, "tableau « Mouvements en EUR »"),
            ("clerib" in c, 0.1, "bandeau RIB (banque, guichet, compte, clé)"),
            ("soldeau" in c and "nouveausolde" in c, 0.15, "« Solde au » et « Nouveau solde »"),
        ):
            if ok:
                score += points
                reasons.append(why)
        return Detection(round(min(score, 1.0), 2), reasons)

    # --------------------------------------------------------------------------------------------- parsing
    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        pages = doc.words(1.0)
        st = ParsedStatement(
            source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, account_type="checking"
        )
        entries: list[_Entry] = []
        current: _Entry | None = None
        boundary, value_x = DEFAULT_BOUNDARY, 381.0
        closing_seen = False

        for page_no, words in enumerate(pages):
            rows = pdftools.lines(words)
            if page_no == 0:
                self._read_header(st, rows)
            in_table = False
            for row in rows:
                texts = [w["text"] for w in row]
                compact = norm("".join(texts)).replace(" ", "")
                if _is_table_header(texts):
                    in_table = True
                    boundary, value_x = _columns(row, boundary, value_x)
                    continue
                if not in_table or closing_seen:
                    continue
                if compact == "operation" or compact.startswith("areception"):
                    in_table = compact == "operation"
                    continue
                amounts = [w for w in row if AMOUNT_RE.match(w["text"]) and w["x0"] > value_x + 20]
                if "nouveausolde" in compact:
                    if amounts:
                        st.balance = _signed(amounts[-1], boundary)
                    st.balance_date = st.period_end
                    closing_seen, in_table = True, False
                    continue
                if "soldeau" in compact:
                    day = next((parse_date(w["text"]) for w in row if DATE_RE.match(w["text"])), None)
                    if amounts:
                        st.opening_balance, st.opening_date = _signed(amounts[-1], boundary), day
                    continue
                first = row[0]
                if DATE_RE.match(first["text"]) and first["x0"] < 70:
                    when = parse_date(first["text"])
                    if when is None or not amounts:
                        continue
                    value_date = next((parse_date(w["text"]) for w in row[1:] if DATE_RE.match(w["text"]) and w["x0"] >= value_x - 12), None)
                    label = " ".join(w["text"] for w in row[1:] if w["x0"] < value_x - 12).strip()
                    current = _Entry(when, value_date, _signed(amounts[0], boundary), label)
                    entries.append(current)
                elif current is not None:
                    text = " ".join(w["text"] for w in row if w["x0"] < value_x - 12).strip()
                    if text:
                        current.cont.append(text)

        holder_key = name_key(st.holder_name) if st.holder_name else None
        st.transactions = [_to_transaction(e, st.currency, holder_key) for e in entries]
        if st.account_number:
            st.account_label = f"Compte BoursoBank …{st.account_number[-4:]}"
        self._check(st, entries)
        return [st]

    # ------------------------------------------------------------------------------------------- header
    @staticmethod
    def _read_header(st: ParsedStatement, rows: list[list[dict]]) -> None:
        texts = [" ".join(w["text"] for w in row) for row in rows]
        for text in texts:
            rib = RIB_ROW_RE.search(text)
            period = PERIOD_RE.search(text)
            if rib and period:
                st.bank_code, st.branch_code, st.account_number, st.rib_key, st.currency = rib.groups()
                st.period_start, st.period_end = parse_date(period.group(1)), parse_date(period.group(2))
                issued = parse_date(text.split()[0])
                if issued:
                    st.extra["issued_on"] = issued.isoformat()
            bic = BIC_RE.search(text)
            if bic:
                st.bic = bic.group(1)
            if "I.B.A.N" in text.upper() or "IBAN" in text.upper():
                st.iban = find_iban(text) or st.iban

        # Holder and address: the lines under the title block, above the RIB banner.
        title = next((i for i, t in enumerate(texts) if "extrait de votre compte" in norm(t)), None)
        banner = next((i for i, t in enumerate(texts) if "devise" in norm(t) and "periode" in norm(t)), None)
        if title is None or banner is None:
            return
        end = title
        while end + 1 < banner and rows[end + 1][0]["top"] - rows[end][0]["top"] < 20:
            end += 1
        block = [t.strip() for i, t in enumerate(texts[end + 1 : banner], end + 1) if rows[i][0]["x0"] > 250 and t.strip()]
        if not block:
            return
        start = next((i for i, t in enumerate(block) if i > 0 and (t[0].isdigit() or STREET_WORDS & set(norm(t).split()))), max(len(block) - 2, 1))
        st.holder_name = " ".join(block[:start]) or None
        st.holder_address = "\n".join(block[start:]) or None

    # ------------------------------------------------------------------------------------ consistency
    @staticmethod
    def _check(st: ParsedStatement, entries: list[_Entry]) -> None:
        total = sum((t.amount for t in st.transactions), Decimal(0))
        if st.opening_balance is not None and st.balance is not None:
            gap = st.balance - st.opening_balance - total
            if abs(gap) < Decimal("0.01"):
                st.confidence = "high"
            else:
                st.confidence = "low"
                st.warnings.append(
                    f"Les opérations lues ne tombent pas juste : écart de {gap:.2f} entre le solde initial, les opérations et le nouveau solde"
                )
        else:
            st.confidence = "low"
            st.warnings.append("Solde initial ou nouveau solde introuvable : impossible de contrôler les opérations lues")
        if not entries and st.confidence != "high":
            st.warnings.append("Aucune opération lue dans ce relevé")
        if st.period_start and st.period_end:
            outside = [t for t in st.transactions if not st.period_start <= t.date <= st.period_end]
            if outside:
                st.confidence = "low"
                st.warnings.append(f"{len(outside)} opération(s) datée(s) hors de la période du relevé")
        if st.iban and not iban_valid(st.iban):
            st.warnings.append("L'IBAN lu n'est pas valide (clé de contrôle)")
        if st.bank_code and st.branch_code and st.account_number and st.rib_key:
            if not rib_valid(st.bank_code, st.branch_code, st.account_number, st.rib_key):
                st.warnings.append("La clé RIB lue ne correspond pas au numéro de compte")
            if st.iban and not iban_matches_rib(st.iban, st.bank_code, st.branch_code, st.account_number, st.rib_key):
                st.warnings.append("L'IBAN ne correspond pas au RIB du bandeau")
        if st.confidence == "high" and st.warnings:
            st.confidence = "medium"


# --------------------------------------------------------------------------------------------- layout
def _is_table_header(texts: list[str]) -> bool:
    tokens = {norm(t).strip(" :") for t in texts}
    return {"libelle", "valeur", "debit", "credit"} <= tokens


def _columns(row: list[dict], boundary: float, value_x: float) -> tuple[float, float]:
    by_name = {norm(w["text"]).strip(" :"): w for w in row}
    if "debit" in by_name and "credit" in by_name:
        # Amounts are right-aligned in their column: the middle of the gap between the two headings splits them.
        boundary = (by_name["debit"]["x1"] + by_name["credit"]["x0"]) / 2
    if "valeur" in by_name:
        value_x = by_name["valeur"]["x0"]
    return boundary, value_x


def _signed(word: dict, boundary: float) -> Decimal:
    value = parse_amount(word["text"]) or Decimal(0)
    return abs(value) if word["x1"] > boundary else -abs(value)


# ---------------------------------------------------------------------------------------- operations
def _split_reference(lines: list[str]) -> tuple[list[str], str | None, str | None]:
    """(other lines, "Réf" value, "RUM" value): the bank prints them on their own lines."""
    other, ref, rum = [], None, None
    for line in lines:
        if m := REF_RE.match(line):
            ref = m.group(1).strip()
        elif m := RUM_RE.match(line):
            rum = m.group(1).strip()
        else:
            other.append(line)
    return other, ref, rum


def _describe(lines: list[str]) -> str | None:
    """The line that says what the operation is for: not an identifier (long digit runs), and made of real words."""
    for line in lines:
        words = line.split()
        if not any(re.search(r"\d{6,}", w) for w in words) and any(len(w) >= 3 and w.isalpha() for w in words):
            return line
    return lines[0] if lines else None


def _label(primary: str | None, secondary: str | None) -> str:
    parts = [p.strip() for p in (primary, secondary) if p and p.strip()]
    return " — ".join(parts)[:300]


def _is_own(name: str | None, holder_key: str | None) -> bool:
    if not name:
        return False
    if holder_key and name_key(name) == holder_key:
        return True
    return bool(OWN_PRODUCT_WORDS & set(norm(name).split()))


def _to_transaction(e: _Entry, currency: str, holder_key: str | None) -> TransactionData:
    raw = " | ".join([e.first, *e.cont])
    details: dict = {}
    kind: str | None = "other"
    counterparty: str | None = None
    reference: str | None = None
    memo: str | None = None
    label = ""
    first = e.first.strip()
    upper = first.upper()
    others, ref, rum = _split_reference(e.cont)

    if m := (CARD_RE.match(first) if upper.startswith("CARTE") else None):
        kind, counterparty = "card", m.group(2).strip() or None
        bought = parse_date(m.group(1))
        details.update(purchase_date=bought.isoformat() if bought else None, card_last4=m.group(3))
        memo = " ".join(others) or None
        label = _label(counterparty, memo)
    elif upper.startswith("PRLV"):
        kind = "direct_debit"
        counterparty = re.sub(r"^PRLV(\s+SEPA)?\s*", "", first, flags=re.I).strip() or None
        memo, reference = " ".join(others) or None, ref
        details["rum"] = rum
        label = _label(counterparty, _describe(others))
    elif m := re.match(r"^VIR\s+(SEPA|INST)\b\s*(.*)$", first, flags=re.I):
        kind = "instant_transfer" if m.group(1).upper() == "INST" else "transfer"
        counterparty = m.group(2).strip() or None
        memo, reference = " ".join(others) or None, ref
        label = _label(counterparty or "Virement", _describe(others))
    elif upper.startswith("VIR"):
        # Transfers between Boursorama accounts: the first line is the reason, the last line the other party.
        kind, reference = "transfer", ref
        reason = first[3:].strip()
        if others:
            counterparty = others[-1]
            memo = " ".join([reason, *others[:-1]]).strip() or None
        else:
            memo = reason or None
        label = _label(memo or "Virement", counterparty)
    elif m := re.match(r"^ECH\s+PRET\s*:?\s*(\S+)", first, flags=re.I):
        kind = "loan_payment"
        details["loan_number"] = m.group(1)
        memo = " ".join(others) or None
        label = f"Échéance de prêt …{m.group(1)[-4:]}"
        reference = m.group(1)
    elif re.match(r"^\*?COTIS", first, flags=re.I):
        kind = "fee"
        memo = " ".join(others) or None
        label = _label(first.lstrip("*"), memo)
    else:
        memo = " ".join(others) or None
        label = _label(first, memo)
        reference = ref

    if _is_own(counterparty, holder_key) and kind in ("transfer", "instant_transfer"):
        details["internal"] = True
    external_id = ref if ref and ref.startswith("SCT") else None
    return TransactionData(
        date=e.date,
        label=label or first,
        amount=e.amount,
        currency=currency,
        external_id=external_id,
        value_date=e.value_date,
        kind=kind,
        counterparty=counterparty,
        reference=reference,
        memo=memo,
        raw=raw,
        details={k: v for k, v in details.items() if v is not None} or None,
    )
