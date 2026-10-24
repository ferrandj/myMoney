"""Fortuneo: the loan offer ("Offre de prêt immobilier"), the contract the loan starts from.

A long PDF (information sheet, particular and general conditions, insurance, guarantee) of which three things matter: the
particular conditions (amount, duration, rate, APR, what the loan costs), the borrower, and the original amortization
table at the end. That table is numbered, not dated: the dates depend on when the funds are released, so they come from
a later document (the early repayment notice, the dated table) or from the user.
"""

from __future__ import annotations

import re
from decimal import Decimal

from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import norm
from ._loan import AMT, REFERENCE_RE, amounts, check_rows, loan_label, money, numeric_date, parse_rows, row_texts

NAME_RE = re.compile(r"^(?:M\.|MME|MLLE|M|MADAME|MONSIEUR)\s+(.+?)\s*(?:\(\d+\)|né\(e\))", re.I)


class FortuneoLoanOfferPdf(Adapter):
    id = "fortuneo.loan_offer_pdf"
    institution = "Fortuneo"
    kind = "loan_offer"
    label = "Fortuneo · offre de prêt immobilier (PDF)"
    description = "L'offre de prêt : montant, durée, taux, coût du crédit et tableau d'amortissement d'origine."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact.replace("'", "").replace("’", "")
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.2, "mentionne Fortuneo"),
            ("offredepret" in c and "immobilier" in c, 0.3, "« Offre de prêt(s) immobilier(s) »"),
            ("conditionsparticulieres" in c, 0.15, "« Conditions particulières »"),
            ("tableaudamortissement" in c, 0.15, "« Tableau d'amortissement »"),
            ("tauxannueleffectifglobal" in c or "taeg" in c, 0.1, "mentionne le TAEG"),
            ("montanttotalarembourser" in c or "couttotaldupret" in c, 0.1, "coût total du prêt"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c:
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
            if n.startswith("le(s) emprunteur") and i + 1 < len(texts) and (m := NAME_RE.match(texts[i + 1])):
                st.holder_name = m.group(1).strip()
            elif n.startswith("article ii") and "caracteristiques" in n and (m := re.search(r"N°\s*(\S+)", text)):
                loan["project"] = m.group(1)
            elif n.startswith("objet du financement") and ":" in text:
                loan["object"] = text.split(":", 1)[1].strip()
            elif n.startswith("destination") and ":" in text:
                loan["destination"] = text.split(":", 1)[1].strip()
            elif n.startswith("pret :") and (m := re.search(r":\s*(\S+)\s+N°\s*(\S+)", text)):
                loan["product"], loan["reference"] = m.group(1), m.group(2)
            elif "montant (euros)" in n and "duree" in n:
                if m := re.search(rf"\(euros\)\s*:\s*({AMT})", text, re.I):
                    loan["principal"] = str(money(m.group(1)))
                if m := re.search(r"Durée\s*:\s*(\d+)", text):
                    loan["duration"] = int(m.group(1))
            elif n.startswith("taux de base"):
                if m := re.search(r":\s*(\d+,\d+)", text):
                    loan["rate"] = str(money(m.group(1)))
                if m := re.search(r"TAEG\s*\(%\)\s*:\s*(\d+,\d+)", text):
                    loan["taeg"] = str(money(m.group(1)))
            elif n.startswith("cout total du pret") and "interets" in n:
                values = amounts(text)
                if len(values) >= 2:
                    loan["total_cost"], loan["interest_total"] = str(values[0]), str(values[1])
            elif n.startswith("assurance obligatoire") and amounts(text):
                loan["insurance_mandatory"] = str(amounts(text)[-1])
            elif n.startswith("frais de garantie") and amounts(text):
                loan["guarantee_fees"] = str(amounts(text)[-1])
            elif n.startswith("frais de dossier") and amounts(text):
                loan["file_fees"] = str(amounts(text)[-1])
            elif n.startswith("frais de courtage") and amounts(text):
                loan["broker_fees"] = str(amounts(text)[-1])
            elif n.startswith("cout des assurances facultatives") and amounts(text):
                loan["insurance_optional"] = str(amounts(text)[0])
            elif "loan_date" not in loan and re.match(r"^le\s+\d{2}/\d{2}/\d{4}", n):
                loan["offer_date"] = numeric_date(text).isoformat()
        if "reference" not in loan:  # the guarantee clause repeats the number: "du crédit suivant : N° FT99000002"
            ref = next((m.group(1) for t in texts if "credit suivant" in norm(t) and (m := REFERENCE_RE.search(t))), None)
            if ref:
                loan["reference"] = ref
        rows = parse_rows(texts, dated=False)
        loan["rows"] = rows
        st.extra["loan"] = loan
        st.extra["title"] = "Offre de prêt"
        st.account_label = loan_label(loan.get("product"), loan.get("reference"))
        self._check(st, loan, rows)
        return [st]

    @staticmethod
    def _check(st: ParsedStatement, loan: dict, rows: list[dict]) -> None:
        warnings = st.warnings
        missing = [k for k in ("reference", "principal", "duration", "rate") if k not in loan]
        if missing:
            warnings.append("Informations introuvables dans l'offre : " + ", ".join(missing))
        if not rows:
            warnings.append("Tableau d'amortissement introuvable dans l'offre")
        else:
            principal = Decimal(loan["principal"]) if "principal" in loan else None
            check_rows(rows, principal, warnings)
            if loan.get("duration") and len(rows) != loan["duration"]:
                warnings.append(f"{len(rows)} échéances lues pour une durée de {loan['duration']} mois")
            if principal is not None and sum((Decimal(r["capital"]) for r in rows), Decimal(0)) != principal:
                warnings.append("La somme des amortissements n'est pas égale au montant emprunté")
            if "interest_total" in loan and sum((Decimal(r["interest"]) for r in rows), Decimal(0)) != Decimal(loan["interest_total"]):
                warnings.append("La somme des intérêts du tableau ne correspond pas au total annoncé dans les conditions particulières")
            if "insurance_mandatory" in loan:
                paid = sum((Decimal(r["insurance"]) for r in rows), Decimal(0))
                declared = Decimal(loan["insurance_mandatory"]) + Decimal(loan.get("insurance_optional", "0"))
                if paid != declared:
                    warnings.append("Le coût de l'assurance du tableau ne correspond pas aux assurances annoncées")
        if {"total_cost", "interest_total", "insurance_mandatory", "guarantee_fees"} <= loan.keys():
            parts = sum(Decimal(loan[k]) for k in ("interest_total", "insurance_mandatory", "guarantee_fees")) + Decimal(loan.get("file_fees", "0")) + Decimal(loan.get("broker_fees", "0"))
            if parts != Decimal(loan["total_cost"]):
                warnings.append("Le coût total du prêt ne correspond pas à la somme de ses composantes (intérêts, assurance obligatoire, frais)")
        st.confidence = "high" if not warnings else "low"
