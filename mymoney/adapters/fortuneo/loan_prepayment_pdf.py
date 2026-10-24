"""Fortuneo: the notice of a partial early repayment ("Remboursement anticipé partiel").

A letter that states the loan before and after the repayment: the capital still owed, the last instalment paid (or the
first of the new plan, with its amount) and the end date. It names no loan number: it is tied to its loan by the
figures, since the capital owed before must be the one the loan's own table gives at that point.
"""

from __future__ import annotations

import re
from datetime import date

from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import french_date, norm
from ._loan import amounts, numeric_date, row_texts


class FortuneoLoanPrepaymentPdf(Adapter):
    id = "fortuneo.loan_prepayment_pdf"
    institution = "Fortuneo"
    kind = "loan_event"
    label = "Fortuneo · remboursement anticipé partiel (PDF)"
    description = "Le courrier qui arrête le décompte d'un remboursement anticipé : capital, échéance et date de fin avant et après."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact.replace("'", "").replace("’", "")
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.25, "mentionne Fortuneo"),
            ("remboursementanticipe" in c, 0.3, "« Remboursement anticipé »"),
            ("avantremboursementanticipe" in c and "apresremboursementanticipe" in c, 0.25, "situation avant / après remboursement"),
            ("capitalrestantdu" in c, 0.1, "« Capital restant dû »"),
            ("decomptearreteau" in c, 0.1, "décompte arrêté"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c or ("offredepret" in c and "conditionsparticulieres" in c):
            score = min(score, 0.3)  # the offer has a clause about early repayment, but it is not the notice
        return Detection(round(min(score, 1.0), 2), reasons)

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        st = ParsedStatement(
            source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, account_type="mortgage"
        )
        side = None
        before: dict = {}
        after: dict = {}
        event: dict = {"kind": "prepayment", "partial": True}
        for text in row_texts(doc):
            n = norm(text)
            if n.startswith("paris, le") or (n.startswith("paris") and french_date(text)):
                event["issued_on"] = (french_date(text) or date.min).isoformat()
            elif n.startswith("l'(les)emprunteur") or n.startswith("l'(les) emprunteur"):
                name = text.split(":", 1)[-1].strip()
                if name:
                    st.holder_name = name
            elif "remboursement anticipe" in n and "partiel" in n:
                event["partial"] = True
            elif "decompte arrete au" in n and (when := numeric_date(text)):
                event["on_date"] = when.isoformat()
            elif n.startswith("avant remboursement"):
                side = before
            elif n.startswith("apres remboursement"):
                side = after
            elif side is not None:
                if n.startswith("capital restant du") and amounts(text):
                    side["capital"] = str(amounts(text)[-1])
                elif (n.startswith("derniere echeance") or n.startswith("premiere echeance")) and (when := numeric_date(text)) and amounts(text):
                    side["due"], side["payment"] = when.isoformat(), str(amounts(text)[-1])
                elif n.startswith("date de fin") and (when := numeric_date(text)):
                    side["end"] = when.isoformat()
        event["before"], event["after"] = before, after
        if "on_date" in event:
            st.balance_date = date.fromisoformat(event["on_date"])
        st.extra["loan"] = {"event": event, "borrower": st.holder_name}
        st.extra["title"] = "Remboursement anticipé partiel"
        st.account_label = "Prêt Fortuneo"
        self._check(st, event)
        return [st]

    @staticmethod
    def _check(st: ParsedStatement, event: dict) -> None:
        from decimal import Decimal

        warnings = st.warnings
        before, after = event["before"], event["after"]
        for label, side, keys in (("avant", before, ("capital", "due", "payment", "end")), ("après", after, ("capital", "due", "payment", "end"))):
            missing = [k for k in keys if k not in side]
            if missing:
                warnings.append(f"Informations introuvables ({label} remboursement) : {', '.join(missing)}")
        if "on_date" not in event:
            warnings.append("Date d'arrêté du décompte introuvable")
        if "capital" in before and "capital" in after and Decimal(after["capital"]) >= Decimal(before["capital"]):
            warnings.append("Le capital restant dû n'a pas diminué")
        if "end" in before and "end" in after and after["end"] > before["end"]:
            warnings.append("La date de fin du prêt est repoussée au lieu d'être avancée")
        st.confidence = "high" if not warnings else "low"
