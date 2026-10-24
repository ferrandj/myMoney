"""Fallbacks for documents no institution-specific adapter knows.

OFX and QIF are standards, so reading them is not a guess. CSV and PDF are: columns and layouts are guessed from the
content, which is why these two never run without the user seeing (and confirming) what was understood.
"""

from __future__ import annotations

import re

from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import norm


class GenericOfx(Adapter):
    id = "generic.ofx"
    institution = None
    kind = "ofx"
    label = "Fichier OFX / QFX (standard bancaire)"
    description = "Export OFX ou QFX proposé par la plupart des banques."
    formats = ("ofx",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        return Detection(0.9, ["fichier OFX"]) if doc.format == "ofx" else Detection()

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        from .ofx_parser import parse_ofx

        return parse_ofx(doc.data, doc.filename)


class GenericQif(Adapter):
    id = "generic.qif"
    institution = None
    kind = "qif"
    label = "Fichier QIF"
    description = "Ancien format d'export Quicken."
    formats = ("qif",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        return Detection(0.9, ["fichier QIF"]) if doc.format == "qif" else Detection()

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        from .ofx_parser import parse_qif

        return parse_qif(doc.data, doc.filename)


class GenericCsv(Adapter):
    id = "generic.csv"
    institution = None
    kind = "csv"
    label = "CSV générique (colonnes devinées)"
    description = "Cherche une colonne date et une colonne montant (ou débit / crédit), ou une liste de positions."
    formats = ("csv",)
    generic = True

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "csv":
            return Detection()
        from .csv_parser import looks_like_statement

        if looks_like_statement(doc.data):
            return Detection(0.5, ["colonnes date et montant reconnues"])
        return Detection(0.1, ["fichier texte, colonnes non reconnues"])

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        from .csv_parser import parse_csv

        return parse_csv(doc.data, doc.filename)


class GenericPdf(Adapter):
    id = "generic.pdf"
    institution = None
    kind = "pdf"
    label = "PDF générique (colonnes devinées)"
    description = "Cherche des lignes datées avec des colonnes débit / crédit, ou des positions avec leur code ISIN."
    formats = ("pdf",)
    generic = True

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf" or not doc.text.strip():
            return Detection()
        text = norm(doc.text)
        if "debit" in text and "credit" in text and re.search(r"\d{2}/\d{2}", text):
            return Detection(0.4, ["colonnes débit et crédit trouvées"])
        return Detection(0.3, ["PDF avec du texte"])

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        from .pdf_parser import parse_pdf

        return parse_pdf(doc.data, doc.filename)
