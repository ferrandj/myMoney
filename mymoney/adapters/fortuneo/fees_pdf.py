"""Fortuneo: "Relevé de frais", the yearly recap of the fees and interest of an account.

No operations: the document says how much the account cost over a period (total fees, with each service, its unit price and
how many times it was billed), and the interest paid and earned. It is attached to the account, where it can be checked
against the fee lines of the imported statements.
"""

from __future__ import annotations

import re
from datetime import date
from decimal import Decimal

from ...services.rates import guess_account_type
from .. import pdftools
from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import french_date, norm, parse_amount, strip_accents
from ..common import _MONTHS
from ..lines import identity_warnings
from ._identity import read_address_block, read_identity

AMOUNT_RE = re.compile(r"^\d{1,3}(?:[ .]\d{3})*,\d{2}$|^\d+,\d{2}$")
PERIOD_RE = re.compile(r"(?i)du\s+(\d{1,2})(?:er)?\s+([^\W\d_]+)\s+au\s+(\d{1,2})(?:er)?\s+([^\W\d_]+)\s+(\d{4})")


class FortuneoFeesPdf(Adapter):
    id = "fortuneo.fees_pdf"
    institution = "Fortuneo"
    kind = "fees_summary"
    label = "Fortuneo · récapitulatif annuel des frais (PDF)"
    description = "Le « Relevé de frais » annuel : frais payés service par service, intérêts payés et acquis."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.25, "mentionne Fortuneo"),
            ("relevedefrais" in c, 0.3, "titre « Relevé de frais »"),
            ("recapitulatifdesfraisetinterets" in c, 0.25, "« Récapitulatif des frais et intérêts »"),
            ("decomptedesfraisprelevessurlecompte" in c, 0.2, "« Décompte des frais prélevés sur le compte »"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c:  # the layout alone is not enough: another bank's document must not look like ours
            score = min(score, 0.3)
        return Detection(round(min(score, 1.0), 2), reasons)

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        st = ParsedStatement(source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, account_type="checking")
        rows: list[list[dict]] = []
        for words in doc.words(1.0):
            rows += pdftools.lines(words)
        texts = [" ".join(w["text"] for w in row) for row in rows]
        read_identity(st, texts)

        product = next((re.sub(r"(?i)^intitul[eé] du compte\s*", "", t).strip() for t in texts if norm(t).startswith("intitule du compte")), "")
        if product:
            st.extra["product"] = product.capitalize()
            st.account_type = guess_account_type(product, "checking")
        for text in texts:
            if m := PERIOD_RE.search(text):
                end_year = int(m.group(5))
                sm, em = _MONTHS.get(strip_accents(m.group(2).lower())), _MONTHS.get(strip_accents(m.group(4).lower()))
                if sm and em:
                    st.period_end = date(end_year, em, int(m.group(3)))
                    st.period_start = date(end_year - (1 if sm > em else 0), sm, int(m.group(1)))
                break
        issued = next((t for t in texts if norm(t).startswith("date ") and re.search(r"\d{2}/\d{2}/\d{4}", t)), None)
        if issued and (m := re.search(r"(\d{2}/\d{2}/\d{4})", issued)):
            d, mo, y = (int(x) for x in m.group(1).split("/"))
            st.extra["issued_on"] = date(y, mo, d).isoformat()

        # Holder and address: the block between the bank's contact lines and the account box.
        last_contact = max((i for i, t in enumerate(texts) if "service clients" in norm(t)), default=None)
        box = next((i for i, t in enumerate(texts) if norm(t).startswith("intitule du compte")), None)
        if last_contact is not None and box is not None and box > last_contact:
            name, address = read_address_block(rows, last_contact, box, from_x=0)
            st.holder_name = st.holder_name or name
            st.holder_address = address

        self._read_totals(st, texts)
        self._read_services(st, rows, texts)
        if st.account_number:
            st.account_label = f"{st.extra.get('product', 'Compte')} Fortuneo …{st.account_number[-4:]}"
        self._check(st)
        return [st]

    @staticmethod
    def _read_totals(st: ParsedStatement, texts: list[str]) -> None:
        for text in texts:
            n = norm(text)
            amounts = [parse_amount(m) for m in re.findall(r"\d{1,3}(?:[ .]\d{3})*,\d{2}", text)]
            if n.startswith("total des frais payes") and amounts and "fees_total" not in st.extra:
                st.extra["fees_total"] = str(amounts[-1])
            elif n.startswith("total des interets payes") and amounts and "interest_paid" not in st.extra:
                st.extra["interest_paid"] = str(amounts[-1])
            elif n.startswith("total des interets acquis") and "interest_earned" not in st.extra:
                st.extra["interest_earned"] = str(amounts[-1]) if amounts else None  # "Intérêts non applicables"

    @staticmethod
    def _read_services(st: ParsedStatement, rows: list[list[dict]], texts: list[str]) -> None:
        """Rows of "Décompte des frais prélevés sur le compte": a category line, then one line per service with
        how many times it was used, its unit price, how many times it was billed and the total."""
        start = next((i for i, t in enumerate(texts) if norm(t).startswith("decompte des frais")), None)
        end = next((i for i, t in enumerate(texts) if i > (start or 0) and norm(t).startswith("total des frais payes")), None)
        services, category = [], None
        if start is None or end is None:
            st.extra["services"] = services
            return
        for row, text in zip(rows[start + 1 : end], texts[start + 1 : end]):
            numbers = [w for w in row if w["x0"] >= 220 and (AMOUNT_RE.match(w["text"]) or w["text"].isdigit())]
            name = " ".join(w["text"] for w in row if w["x0"] < 220).strip()
            if len(numbers) >= 4 and name:
                used, unit, billed, total = (n["text"] for n in numbers[:4])
                services.append({
                    "category": category, "name": name, "used": int(used), "unit_price": str(parse_amount(unit)),
                    "billed": int(billed), "total": str(parse_amount(total)),
                })
            elif name and not numbers and norm(name) not in ("service", "frais") and len(row) <= 6 and row[0]["x0"] < 60:
                category = name
        st.extra["services"] = services

    @staticmethod
    def _check(st: ParsedStatement) -> None:
        total = st.extra.get("fees_total")
        services = st.extra.get("services", [])
        if total is None:
            st.confidence = "low"
            st.warnings.append("Total des frais introuvable dans le document")
        elif services and sum((Decimal(s["total"]) for s in services), Decimal(0)) != Decimal(total):
            st.confidence = "low"
            st.warnings.append("Le total des frais ne correspond pas à la somme des services listés : vérifie le document")
        else:
            st.confidence = "high"
        if not (st.period_start and st.period_end):
            st.confidence = "low"
            st.warnings.append("Période du récapitulatif introuvable")
        st.warnings += identity_warnings(st)
        if st.confidence == "high" and st.warnings:
            st.confidence = "medium"
