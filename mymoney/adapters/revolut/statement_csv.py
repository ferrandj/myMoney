"""Revolut: the CSV export of an account ("Account statement" in the app)."""

from __future__ import annotations

import csv
import io

from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import norm

EXPECTED = {"type", "product", "started date", "completed date", "description", "amount", "fee", "currency", "state", "balance"}


class RevolutStatementCsv(Adapter):
    id = "revolut.statement_csv"
    institution = "Revolut"
    kind = "statement"
    label = "Revolut · relevé de compte (CSV)"
    description = "L'export CSV d'un compte Revolut (colonnes Type, Product, Started Date, Completed Date…)."
    formats = ("csv",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "csv":
            return Detection()
        first = doc.text.lstrip("﻿").splitlines()[:1]
        if not first:
            return Detection()
        header = {norm(cell) for cell in next(csv.reader(io.StringIO(first[0])), [])}
        found = EXPECTED & header
        if len(found) >= 8:
            return Detection(0.95, [f"{len(found)} colonnes sur {len(EXPECTED)} du relevé Revolut"])
        return Detection()

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        from ..generic.csv_parser import parse_csv

        statements = parse_csv(doc.data, doc.filename)
        for st in statements:
            st.institution = self.institution
        return statements
