"""Fortuneo: the dated amortization table ("Tableau d'amortissement") downloaded from the customer area.

Unlike the one in the offer it carries real dates, and it describes the loan as it is now: after an early repayment it
starts from the capital still owed that day (the amount printed is not the amount borrowed) and ends at the new end date.
"""

from __future__ import annotations

import re
from datetime import date
from decimal import Decimal

from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import norm
from ._loan import AMT, REFERENCE_RE, check_rows, loan_label, money, numeric_date, parse_rows, row_texts


class FortuneoLoanSchedulePdf(Adapter):
    id = "fortuneo.loan_schedule_pdf"
    institution = "Fortuneo"
    kind = "loan_schedule"
    label = "Fortuneo · tableau d'amortissement daté (PDF)"
    description = "Le tableau d'amortissement de l'espace client : chaque échéance avec sa date, du jour à la dernière."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact.replace("'", "").replace("’", "")
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.25, "mentionne Fortuneo"),
            ("tableaudamortissement" in c, 0.25, "titre « Tableau d'amortissement »"),
            ("montantrestant" in c and "totalapayer" in c, 0.25, "colonnes « Total à payer » et « Montant restant dû »"),
            ("datedecheance" in c or "dateprevisionnelledefinduprt" in c or "dateprevisionnelledefindupret" in c, 0.15, "échéances datées"),
            ("votreprojet" in c and "listedu" in c, 0.1, "« Votre projet »"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c:
            score = min(score, 0.3)
        # The offer also has an (undated) amortization table: the offer adapter is the one that must win there.
        if "offredepret" in c and "conditionsparticulieres" in c:
            score = min(score, 0.3)
        return Detection(round(min(score, 1.0), 2), reasons)

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        st = ParsedStatement(
            source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, account_type="mortgage"
        )
        texts = row_texts(doc)
        loan: dict = {}
        for i, text in enumerate(texts):
            n = norm(text)
            if n.startswith("emprunteur") and "borrower" not in loan:
                name = re.split(r"(?i)\s+co-?\s?emprunteur", re.sub(r"(?i)^emprunteur\s*:?\s*", "", text))[0].strip()
                if name:
                    loan["borrower"] = name
            if n.startswith("projet") and (m := re.search(r"N°\s*(\S+)", text)) and "project" not in loan:
                loan["project"] = m.group(1)
            if n.startswith("projet") and "RÉFÉRENCE" in text.upper() and (m := REFERENCE_RE.findall(text)):
                loan["reference"] = m[-1]
            if n.startswith("montant du financement") and re.search(AMT, text):
                loan["project_amount"] = str(money(re.search(AMT, text).group(0)))
            if n.startswith("date previsionnelle de fin") and (when := numeric_date(text)):
                loan["end_date_stated"] = when.isoformat()
            if n.startswith("pret") and "montant" in n and "(euros)" in n and "montant" in n.split("pret", 1)[1]:
                if m := re.search(rf"MONTANT\s*\(euros\)\s*({AMT})", text, re.I):
                    loan["balance"] = str(money(m.group(1)))
                if m := re.match(r"(?i)^pr[eê]t\s+(.*?)\s+MONTANT", text):
                    loan["product"] = m.group(1).strip()
            if n.startswith("taux de base") and (m := re.search(r"(\d+,\d+)", text)):
                loan["rate"] = str(money(m.group(1)))
                if d := re.search(r"DUR[ÉE]E\s+(\d+)\s+termes", text, re.I):
                    loan["term"] = int(d.group(1))
                if "mensuel" in norm(text):
                    loan["periodicity"] = "mensuelle"
        # First page: the list of the project's loans: "FIXIMMO EXEMPLE 52 000,00 PRET 2,40 63 FT99000002 IMMOBILIER".
        for text in texts:
            if "reference" not in loan and (m := REFERENCE_RE.search(text)) and re.search(rf"{AMT}\s+.*\d+,\d{{2}}\s+\d+\s+{m.group(1)}", text):
                loan["reference"] = m.group(1)
                if "product" not in loan and (p := re.match(rf"^(.*?)\s+{AMT}", text)):
                    loan["product"] = p.group(1).strip()
        rows = parse_rows(texts, dated=True)
        loan["rows"] = rows
        st.holder_name = loan.get("borrower")
        st.extra["loan"] = loan
        st.extra["title"] = "Tableau d'amortissement"
        st.account_label = loan_label(loan.get("product"), loan.get("reference"))
        if rows:
            st.period_start, st.period_end = date.fromisoformat(rows[0]["date"]), date.fromisoformat(rows[-1]["date"])
        self._check(st, loan, rows)
        return [st]

    @staticmethod
    def _check(st: ParsedStatement, loan: dict, rows: list[dict]) -> None:
        warnings = st.warnings
        missing = [k for k in ("reference", "balance", "rate") if k not in loan]
        if missing:
            warnings.append("Informations introuvables dans le tableau : " + ", ".join(missing))
        if not rows:
            warnings.append("Aucune échéance lue dans le tableau")
        else:
            check_rows(rows, Decimal(loan["balance"]) if "balance" in loan else None, warnings)
            if loan.get("end_date_stated") and loan["end_date_stated"] != rows[-1]["date"]:
                warnings.append(f"La dernière échéance lue ({rows[-1]['date']}) n'est pas la date de fin annoncée ({loan['end_date_stated']})")
            dates = [date.fromisoformat(r["date"]) for r in rows]
            if any(b <= a for a, b in zip(dates, dates[1:])):
                warnings.append("Les dates des échéances ne se suivent pas")
        st.confidence = "high" if not warnings else "low"
