"""BoursoBank (Boursorama): "Extrait de votre <produit d'épargne>" PDF (Plan Épargne Logement, and the like).

Same movements table as the current account (see _table.py), but a different header: the title names the product
("Extrait de votre Plan Epargne Logement") and there is a plain "Numéro de compte" instead of the RIB banner,
so no IBAN or BIC on the document. The balances check works exactly as on a current account.
"""

from __future__ import annotations

import re

from ...services.rates import guess_account_type
from ..base import Detection, ParsedStatement, SourceDocument
from ..common import norm, parse_date
from ._table import PERIOD_RE, BoursoBankTablePdf

ACCOUNT_ROW_RE = re.compile(r"\b(\d{2}/\d{2}/\d{4})\s+([0-9A-Z]{8,})\s+([A-Z]{3})\b")
SAVINGS_WORDS = ("epargne", "livret", "pel", "ldds", "lep", "cel", "plan")


class BoursoBankSavingsPdf(BoursoBankTablePdf):
    id = "boursobank.savings_pdf"
    institution = "BoursoBank"
    kind = "savings_statement"
    label = "BoursoBank · extrait d'épargne : PEL, livret… (PDF)"
    description = "Le PDF « Extrait de votre Plan Epargne Logement » (ou d'un autre produit d'épargne) : numéro de compte, pas de RIB."
    formats = ("pdf",)
    default_account_type = "savings"

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = doc.compact
        score, reasons = 0.0, []
        for ok, points, why in (
            ("boursobank" in c or "boursorama" in c, 0.3, "mentionne BoursoBank / Boursorama"),
            ("extraitdevotre" in c, 0.15, "titre « Extrait de votre … »"),
            ("numerodecompte" in c, 0.15, "bandeau « Numéro de compte » (sans RIB)"),
            ("mouvementsen" in c, 0.15, "tableau « Mouvements en EUR »"),
            ("soldeeneurau" in c and "nouveausolde" in c, 0.1, "« Solde en EUR au » et « Nouveau solde »"),
            (any(f"extraitdevotre{w}" in c for w in SAVINGS_WORDS), 0.15, "produit d'épargne dans le titre"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "clerib" in c or "extraitdevotrecompte" in c:
            # The current-account layout (RIB banner): that is the other adapter's document.
            score, reasons = min(score, 0.3), reasons + ["bandeau RIB : plutôt un compte courant"]
        return Detection(round(min(score, 1.0), 2), reasons)

    def _read_header(self, st: ParsedStatement, rows: list[list[dict]]) -> None:
        texts = [" ".join(w["text"] for w in row) for row in rows]
        for text in texts:
            row = ACCOUNT_ROW_RE.search(text)
            period = PERIOD_RE.search(text)
            if row and period:
                issued, st.account_number, st.currency = row.groups()
                st.period_start, st.period_end = parse_date(period.group(1)), parse_date(period.group(2))
                st.extra["issued_on"] = parse_date(issued).isoformat() if parse_date(issued) else None
        title = next((t for t in texts if "extrait de votre" in norm(t)), "")
        product = re.sub(r"(?i)^.*extrait de votre\s+", "", title).strip()
        if product:
            st.extra["product"] = product
            st.account_type = guess_account_type(product, "savings")
        self.read_holder(st, rows, texts)
        if st.account_number:
            st.account_label = f"{product or 'Épargne'} BoursoBank …{st.account_number[-4:]}"
