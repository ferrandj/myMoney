"""Fortuneo Vie (contract managed by Suravenir): the confirmation of an arbitrage.

A document to keep, not to import: it says which funds were sold and which were bought, for the same amount. It changes
no cash balance, so it is recognised, dated and filed with the holder's other papers; the amounts and the funds are kept
and shown. The sums are checked (what was bought adds up to the net amount), which is what the reliability reflects.

Some of these PDFs lose their spaces ("Montantbrutdel'arbitrage"), so the markers are looked for in the text with its
spaces and punctuation removed.
"""

from __future__ import annotations

import re
from datetime import date
from decimal import Decimal

from ..base import Adapter, Detection, ParsedStatement, SourceDocument
from ..common import french_date, norm, parse_amount

MONTH = r"(janvier|fevrier|mars|avril|mai|juin|juillet|aout|septembre|octobre|novembre|decembre)"
ISIN = r"[A-Z]{2}[A-Z0-9]{9}\d"
# fund (no spaces guaranteed), ISIN (the euro fund has none), valuation date, units, unit value, amount
LINE_RE = re.compile(
    rf"^(?P<name>\S.*?)\s+(?:(?P<isin>{ISIN})\s+)?(?P<day>\d{{2}}/\d{{2}}/\d{{4}})\s+(?P<units>\d[\d ]*,\d+)\s+(?P<price>\d[\d ]*,\d+)\s+(?P<amount>\d[\d ]*,\d{{2}})$"
)


def _squash(text: str) -> str:
    return re.sub(r"[^a-z0-9,]", "", norm(text).replace(" ", ""))


class FortuneoLifeArbitragePdf(Adapter):
    id = "fortuneo.life_arbitrage_pdf"
    institution = "Fortuneo"
    kind = "info"
    label = "Fortuneo Vie · confirmation d'arbitrage (PDF)"
    description = "La lettre de l'assureur (Suravenir) qui confirme un arbitrage sur ton contrat d'assurance-vie : fonds vendus et achetés."
    formats = ("pdf",)

    @classmethod
    def detect(cls, doc: SourceDocument) -> Detection:
        if doc.format != "pdf":
            return Detection()
        c = re.sub(r"[^a-z0-9]", "", doc.compact)
        score, reasons = 0.0, []
        for ok, points, why in (
            ("fortuneo" in c, 0.25, "mentionne Fortuneo"),
            ("confirmationdarbitrage" in c, 0.45, "« Confirmation d'arbitrage »"),
            ("suravenir" in c, 0.15, "mentionne Suravenir"),
            ("desinvestissement" in c and "reinvestissement" in c, 0.15, "fonds désinvestis puis réinvestis"),
        ):
            if ok:
                score += points
                reasons.append(why)
        if "fortuneo" not in c:  # the layout alone is not enough: another insurer's letter must not look like ours
            score = min(score, 0.3)
        return Detection(round(min(score, 1.0), 2), reasons)

    def parse(self, doc: SourceDocument) -> list[ParsedStatement]:
        st = ParsedStatement(source_format="pdf", institution=self.institution, adapter_id=self.id, document_kind=self.kind, confidence="high")
        squashed = _squash(doc.text)

        def long_date(prefix: str) -> date | None:
            m = re.search(prefix + r"(\d{1,2})" + MONTH + r"(\d{4})", squashed)
            return french_date(f"{m.group(1)} {m.group(2)} {m.group(3)}") if m else None

        def amount(label: str) -> Decimal | None:
            m = re.search(label + r":?(\d+,\d{2})", squashed)
            return parse_amount(m.group(1)) if m else None

        text = doc.text.replace("\xa0", " ")
        letter = long_date(r"le")
        if letter:
            st.balance_date = letter
            st.extra["issued_on"] = letter.isoformat()
            st.extra["year"] = letter.year
        done = long_date("datedelarbitrage")
        if done:
            st.extra["arbitrage_date"] = done.isoformat()
        if m := re.search(r"Contrat\s*:\s*([^\n]+)", text):
            st.extra["contract"] = m.group(1).strip()
        if m := re.search(r"N\s*°\s*:?\s*(\d{8,})", text):
            st.extra["contract_number"] = m.group(1)
        gross, fees, net = amount("montantbrutdelarbitrage"), amount("fraisdarbitrage"), amount("montantnet")
        for key, value in (("gross", gross), ("fees", fees), ("net", net)):
            if value is not None:
                st.extra[key] = str(value)

        sold: list[dict] = []
        bought: list[dict] = []
        target: list[dict] | None = None
        for raw in text.splitlines():
            line = raw.strip()
            key = _squash(line)
            if len(key) < 30 and "desinvestissement" in key:  # a bullet may precede the heading
                target = sold
            elif len(key) < 30 and "reinvestissement" in key:
                target = bought
            elif target is not None and (m := LINE_RE.match(line)):
                target.append({
                    "fund": m.group("name").strip(), "isin": m.group("isin"), "date": _iso(m.group("day")),
                    "units": str(parse_amount(m.group("units"))), "unit_value": str(parse_amount(m.group("price"))),
                    "amount": str(parse_amount(m.group("amount"))),
                })
        st.extra["sold"], st.extra["bought"] = sold, bought

        what = f"Confirmation d'arbitrage · {st.extra['contract']}" if st.extra.get("contract") else "Confirmation d'arbitrage"
        st.extra["title"] = what
        st.extra["category"] = "Assurance-vie"
        bought_total = sum((Decimal(x["amount"]) for x in bought), Decimal(0))
        sold_total = sum((Decimal(x["amount"]) for x in sold), Decimal(0))
        problems = []
        if not sold or not bought:
            problems.append("Les fonds vendus ou achetés n'ont pas pu être lus en entier")
        if net is not None and bought and abs(bought_total - net) > Decimal("0.05"):
            problems.append(f"Les fonds achetés ({bought_total}) ne correspondent pas au montant net annoncé ({net})")
        if net is not None and sold and abs(sold_total - net) > Decimal("0.05"):
            problems.append(f"Les fonds vendus ({sold_total}) ne correspondent pas au montant net annoncé ({net})")
        if net is None or letter is None:
            problems.append("Le montant ou la date de la lettre n'a pas été trouvé")
        st.warnings += problems
        if problems:
            st.confidence = "low" if not (sold and bought) else "medium"
        return [st]


def _iso(day: str) -> str:
    d, m, y = day.split("/")
    return f"{y}-{m}-{d}"
