"""Fortuneo: the yearly letter about the deposit guarantee (FGDR).

A document to keep, not to import: nothing in it changes a balance. It is recognised, dated and filed with the
holder's other papers (encrypted, downloadable), and the ceiling it announces is kept.
"""

from __future__ import annotations

import re

from .. import pdftools
from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import french_date, norm
from ._identity import read_address_block

CEILING_RE = re.compile(r"(\d[\d\s\u00a0\u202f]*\d)\s*€\s*par\s*d[ée]posant")


class FortuneoDepositGuaranteePdf(Adapter):
    id = "fortuneo.deposit_guarantee_pdf"
    institution = "Fortuneo"
    kind = "info"
    label = "Fortuneo · information sur la garantie des dépôts (PDF)"
    description = "La lettre annuelle « Informations générales sur la protection des dépôts » (FGDR), à classer avec tes papiers."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.25, "mentionne Fortuneo"),
            ("informationsgeneralessurlaprotectiondesdepots" in c, 0.4, "« Informations générales sur la protection des dépôts »"),
            ("garantiedesdepotsetderesolution" in c or "fgdr" in c, 0.2, "mentionne le FGDR"),
            ("plafonddelaprotection" in c, 0.15, "« Plafond de la protection »"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c:  # the layout alone is not enough: another bank's document must not look like ours
            score = min(score, 0.3)
        return Detection(round(min(score, 1.0), 2), reasons)

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        st = ParsedStatement(source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, confidence="high")
        rows = pdftools.lines(doc.words(1.0)[0])
        texts = [" ".join(w["text"] for w in row) for row in rows]
        dated = next((i for i, t in enumerate(texts) if norm(t).startswith("le ") and french_date(t)), None)
        salutation = next((i for i, t in enumerate(texts) if norm(t).startswith("monsieur") or norm(t).startswith("madame")), None)
        if dated is not None:
            st.balance_date = french_date(texts[dated])
            st.extra["year"] = st.balance_date.year
            st.extra["issued_on"] = st.balance_date.isoformat()
        if dated is not None and salutation is not None and salutation > dated:
            st.holder_name, st.holder_address = read_address_block(rows, dated, salutation, from_x=280)
        st.extra["title"] = "Information sur la garantie des dépôts (FGDR)"
        if m := CEILING_RE.search(doc.text.replace("\xa0", " ")):
            digits = "".join(ch for ch in m.group(1) if ch.isdigit())
            st.extra["ceiling"] = f"{int(digits):,}".replace(",", " ") + " €"  # "100 000 €", whatever the PDF's spacing
        return [st]
