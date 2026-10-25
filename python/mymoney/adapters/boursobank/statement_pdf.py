"""BoursoBank (Boursorama): monthly "Extrait de votre compte en EUR" PDF (current account).

Beyond the shared table (see _table.py) this document has:
  * a banner with the RIB (bank, branch, account, key), the currency and the period "du ... au ...";
  * a line with the BIC and the IBAN, both checked against the RIB.
"""

from __future__ import annotations

import re

from ...services.rates import guess_account_type
from ..base import Detection, ParsedStatement, SourceDocument
from ..common import find_iban, iban_matches_rib, iban_valid, norm, parse_date, rib_valid
from ._table import PERIOD_RE, BoursoBankTablePdf

RIB_ROW_RE = re.compile(r"\b(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\s+([A-Z]{3})\b")
BIC_RE = re.compile(r"B\.?I\.?C\.?\s+([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b")


class BoursoBankStatementPdf(BoursoBankTablePdf):
    id = "boursobank.statement_pdf"
    institution = "BoursoBank"
    kind = "statement"
    label = "BoursoBank · extrait de compte mensuel (PDF)"
    description = "Le PDF « Extrait de votre compte en EUR » téléchargé dans l'espace client BoursoBank / Boursorama."
    formats = ("pdf",)

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

    def _read_header(self, st: ParsedStatement, rows: list[list[dict]]) -> None:
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
        self.read_holder(st, rows, texts)
        # Savings accounts use the same layout, with one more line under the title: "Compte d'épargne : <product>".
        product = next((t.split(":", 1)[1].strip() for t in texts if norm(t).startswith("compte d") and "pargne" in t and ":" in t), None)
        if product:
            st.extra["product"] = product
            st.account_type = guess_account_type(product, "savings")
        joint = bool(re.search(r"\b(OU|ET)\b", st.holder_name or ""))
        if st.account_number:
            kind = product or ("Compte joint" if joint else "Compte")
            st.account_label = f"{kind} BoursoBank …{st.account_number[-4:]}"

    def _check_identity(self, st: ParsedStatement) -> None:
        if st.iban and not iban_valid(st.iban):
            st.warnings.append("L'IBAN lu n'est pas valide (clé de contrôle)")
        if st.bank_code and st.branch_code and st.account_number and st.rib_key:
            if not rib_valid(st.bank_code, st.branch_code, st.account_number, st.rib_key):
                st.warnings.append("La clé RIB lue ne correspond pas au numéro de compte")
            if st.iban and not iban_matches_rib(st.iban, st.bank_code, st.branch_code, st.account_number, st.rib_key):
                st.warnings.append("L'IBAN ne correspond pas au RIB du bandeau")
