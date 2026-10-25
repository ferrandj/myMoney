"""Which adapter reads a dropped file.

Every adapter scores the file (`Adapter.detect`). A clear winner is used straight away; when two adapters are close,
when the institution is recognised but not the kind of document, or when only a guessing fallback applies, the
decision is returned as "ask" or "fallback" so the app can put the question to the user instead of guessing silently.
"""

from __future__ import annotations

import hashlib
import logging
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path

from .base import Adapter, ParsedStatement, SourceDocument
from .boursobank.savings_pdf import BoursoBankSavingsPdf
from .boursobank.statement_pdf import BoursoBankStatementPdf
from .common import find_iban, find_institution
from .fortuneo.deposit_guarantee_pdf import FortuneoDepositGuaranteePdf
from .fortuneo.fees_pdf import FortuneoFeesPdf
from .fortuneo.life_arbitrage_pdf import FortuneoLifeArbitragePdf
from .fortuneo.loan_offer_pdf import FortuneoLoanOfferPdf
from .fortuneo.loan_prepayment_pdf import FortuneoLoanPrepaymentPdf
from .fortuneo.loan_schedule_pdf import FortuneoLoanSchedulePdf
from .fortuneo.statement_pdf import FortuneoStatementPdf
from .generic import GenericCsv, GenericOfx, GenericPdf, GenericQif
from .revolut.statement_csv import RevolutStatementCsv

log = logging.getLogger(__name__)


@lru_cache(maxsize=1)
def reader_version() -> str:
    """A short fingerprint of the reading code (every adapter and the PDF helpers). A document remembers the one that read it:
    when it differs from the current one, the reading may be out of date and is redone."""
    digest = hashlib.sha256()
    for path in sorted(Path(__file__).parent.rglob("*.py")):
        digest.update(path.relative_to(Path(__file__).parent).as_posix().encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()[:8]

# One entry per kind of document per institution. To support a new one: write the adapter, add it here.
ADAPTERS: list[type[Adapter]] = [
    BoursoBankStatementPdf,
    BoursoBankSavingsPdf,
    FortuneoStatementPdf,
    FortuneoFeesPdf,
    FortuneoDepositGuaranteePdf,
    FortuneoLifeArbitragePdf,
    FortuneoLoanOfferPdf,
    FortuneoLoanSchedulePdf,
    FortuneoLoanPrepaymentPdf,
    RevolutStatementCsv,
    GenericOfx,
    GenericQif,
    GenericCsv,
    GenericPdf,
]

AUTO_SCORE = 0.8  # a candidate at least this sure, and clearly ahead of the others, is used without asking
AUTO_MARGIN = 0.2
PLAUSIBLE = 0.4


@dataclass
class Candidate:
    adapter: type[Adapter]
    score: float
    reasons: list[str] = field(default_factory=list)

    def to_json(self) -> dict:
        return {"adapter": self.adapter.id, "score": self.score, "reasons": self.reasons}


@dataclass
class Identification:
    candidates: list[Candidate]
    institution: str | None
    decision: str  # auto | ask | fallback | none
    chosen: Candidate | None
    message: str = ""


def get_adapter(adapter_id: str) -> type[Adapter]:
    for cls in ADAPTERS:
        if cls.id == adapter_id:
            return cls
    raise ValueError(f"Adapter inconnu : {adapter_id}")


def identify(doc: SourceDocument) -> Identification:
    candidates = []
    for cls in ADAPTERS:
        try:
            detection = cls.detect(doc)
        except Exception:  # a broken adapter must never block the others
            log.exception("Détection %s", cls.id)
            continue
        if detection.score > 0:
            candidates.append(Candidate(cls, detection.score, detection.reasons))
    candidates.sort(key=lambda c: (c.score, not c.adapter.generic), reverse=True)

    try:
        institution = find_institution(doc.text) if doc.format != "unknown" else None
    except Exception:
        institution = None
    if not candidates:
        return Identification([], institution, "none", None, "Format non reconnu (PDF, CSV, OFX/QFX ou QIF attendu).")

    best = candidates[0]
    if best.score >= AUTO_SCORE and (len(candidates) == 1 or candidates[1].score <= best.score - AUTO_MARGIN):
        return Identification(candidates, best.adapter.institution or institution, "auto", best)

    close = [c for c in candidates if c.score >= PLAUSIBLE and c.score > best.score - AUTO_MARGIN]
    specific = [c for c in candidates if c.adapter.institution and c.score >= PLAUSIBLE]
    if len(close) > 1 and any(not c.adapter.generic for c in close) or specific:
        return Identification(
            candidates, institution, "ask", None, "Plusieurs lectures sont possibles pour ce document : choisis celle qui correspond."
        )
    message = "Aucun adapter dédié à ce document : lecture générique, vérifie bien ce qui a été compris."
    if institution:
        message = f"{institution} reconnu, mais aucun adapter ne connaît ce type de document. " + message
    return Identification(candidates, institution, "fallback", best, message)


def parse_with(adapter_id: str, doc: SourceDocument) -> list[ParsedStatement]:
    adapter = get_adapter(adapter_id)()
    statements = adapter.parse(doc)
    for st in statements:
        st.adapter_id = st.adapter_id or adapter.id
        st.document_kind = st.document_kind or adapter.kind
        st.institution = st.institution or adapter.institution
        _enrich(st)
    return statements


def _enrich(st: ParsedStatement) -> None:
    """Facts that any adapter benefits from: the IBAN of the other party, when the line shows one."""
    own = (st.iban or "").replace(" ", "")
    for t in st.transactions:
        if not t.counterparty_iban:
            found = find_iban(" ".join(filter(None, (t.raw, t.label, t.memo))))
            if found and found != own:
                t.counterparty_iban = found


def catalog() -> list[tuple[str, list[type[Adapter]]]]:
    """Adapters by institution, for the "supported documents" list."""
    groups: dict[str, list[type[Adapter]]] = {}
    for cls in ADAPTERS:
        groups.setdefault(cls.institution or "Formats génériques", []).append(cls)
    return list(groups.items())
