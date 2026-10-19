"""OFX / QFX (exported by most French banks: Fortuneo, BforBank, Boursorama...) and QIF files."""

from __future__ import annotations

import io
from decimal import Decimal

from ..connectors.base import TransactionData
from ..services.rates import guess_account_type
from .common import ParsedStatement, find_institution, parse_amount, parse_date


def parse_ofx(data: bytes, filename: str = "") -> list[ParsedStatement]:
    import ofxparse

    ofx = ofxparse.OfxParser.parse(io.BytesIO(data))
    institution = None
    if getattr(ofx, "institution", None) is not None:
        institution = getattr(ofx.institution, "organization", None)
    institution = institution or find_institution(data[:4000].decode("latin-1", errors="ignore"))
    statements = []
    for account in ofx.accounts:
        stmt = account.statement
        st = ParsedStatement(
            source_format="ofx",
            institution=institution,
            account_number=account.account_id,
            account_label=getattr(account, "desc", None) or f"Compte {str(account.account_id)[-4:]}",
            currency=(getattr(stmt, "currency", None) or "EUR").upper(),
            confidence="high",
        )
        account_type = getattr(account, "account_type", "") or ""
        st.account_type = guess_account_type(st.account_label, "savings" if "SAVINGS" in account_type.upper() else "checking")
        if getattr(stmt, "balance", None) is not None:
            st.balance = Decimal(str(stmt.balance))
            st.balance_date = stmt.balance_date.date() if getattr(stmt, "balance_date", None) else None
        for t in stmt.transactions:
            label = " ".join(part for part in (t.payee, t.memo) if part and part.strip()) or "Opération"
            st.transactions.append(
                TransactionData(date=t.date.date(), label=label.strip(), amount=Decimal(str(t.amount)), currency=st.currency, external_id=t.id or None)
            )
        if st.transactions:
            st.transactions.sort(key=lambda t: t.date)
            st.period_start, st.period_end = st.transactions[0].date, st.transactions[-1].date
        statements.append(st)
    if not statements:
        raise ValueError("Aucun compte trouvé dans le fichier OFX")
    return statements


def parse_qif(data: bytes, filename: str = "") -> list[ParsedStatement]:
    text = data.decode("utf-8-sig", errors="ignore") if b"\xc3" in data else data.decode("latin-1")
    st = ParsedStatement(source_format="qif", account_label=filename.rsplit(".", 1)[0] or None, confidence="high")
    current: dict[str, str] = {}
    for line in text.splitlines():
        if not line:
            continue
        code, value = line[0], line[1:].strip()
        if code == "^":
            when = parse_date(current.get("D", "").replace("'", "/"))
            amount = parse_amount(current.get("T") or current.get("U"))
            if when and amount is not None:
                label = " ".join(v for v in (current.get("P"), current.get("M")) if v) or "Opération"
                st.transactions.append(TransactionData(date=when, label=label, amount=amount))
            current = {}
        elif code in "DTUPM":
            current[code] = value
    if not st.transactions:
        raise ValueError("Aucune opération dans le fichier QIF")
    st.transactions.sort(key=lambda t: t.date)
    st.period_start, st.period_end = st.transactions[0].date, st.transactions[-1].date
    st.account_type = guess_account_type(st.account_label or "", "checking")
    return [st]
