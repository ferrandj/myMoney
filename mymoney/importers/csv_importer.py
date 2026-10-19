"""CSV exports: bank statements (any bank, columns detected automatically), Revolut, and position lists."""

from __future__ import annotations

import csv
import io
from decimal import Decimal

from ..connectors.base import HoldingData, TransactionData
from ..services.rates import guess_account_type
from .common import ParsedStatement, find_iban, norm, parse_amount, parse_date

COLUMNS = {
    "date": ["date operation", "date de l'operation", "date d'operation", "date comptable", "completed date", "booking date",
             "date", "dateop", "date op", "date de comptabilisation", "date valeur", "value date", "started date", "date/heure", "date / time"],
    "label": ["libelle", "libelle operation", "libelle de l'operation", "description", "label", "detail", "details", "operation",
              "intitule", "wording", "payee", "beneficiaire", "reference", "nature de l'operation", "libelle simplifie"],
    "amount": ["montant", "amount", "montant (eur)", "montant(euros)", "montant en euros", "montant eur", "valeur"],
    "debit": ["debit", "debit (eur)", "montant debit", "debit euros", "sortie", "withdrawal"],
    "credit": ["credit", "credit (eur)", "montant credit", "credit euros", "entree", "deposit"],
    "currency": ["devise", "currency", "monnaie"],
    "balance": ["solde", "balance", "solde (eur)"],
    "fee": ["fee", "frais"],
    "state": ["state", "statut", "etat"],
    "symbol": ["symbol", "symbole", "ticker", "asset", "actif", "crypto", "coin"],
    "isin": ["isin", "code isin"],
    "quantity": ["quantity", "quantite", "qty", "nombre", "parts", "units"],
    "name": ["name", "nom", "libelle valeur", "support"],
    "value": ["value", "valorisation", "montant total", "market value"],
}


def _decode(data: bytes) -> str:
    for encoding in ("utf-8-sig", "cp1252", "latin-1"):
        try:
            return data.decode(encoding)
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", errors="replace")


def _match_columns(header: list[str]) -> dict[str, int]:
    found: dict[str, int] = {}
    normalized = [norm(h).strip(" :") for h in header]
    for key, names in COLUMNS.items():
        for name in names:  # names are ordered by preference
            if name in normalized and normalized.index(name) not in found.values():
                found[key] = normalized.index(name)
                break
    return found


def parse_csv(data: bytes, filename: str = "") -> list[ParsedStatement]:
    text = _decode(data)
    sample = text[:5000]
    try:
        dialect = csv.Sniffer().sniff(sample, delimiters=";,\t|")
        delimiter = dialect.delimiter
    except csv.Error:
        delimiter = ";" if sample.count(";") > sample.count(",") else ","
    rows = [row for row in csv.reader(io.StringIO(text), delimiter=delimiter) if any(cell.strip() for cell in row)]

    header_index, columns = None, {}
    for i, row in enumerate(rows[:30]):
        candidate = _match_columns(row)
        if ("date" in candidate and ("amount" in candidate or "debit" in candidate or "credit" in candidate)) or (
            "quantity" in candidate and ("symbol" in candidate or "isin" in candidate)
        ):
            header_index, columns = i, candidate
            break
    if header_index is None:
        raise ValueError("Colonnes non reconnues : il faut au moins une date et un montant (ou un symbole et une quantité)")

    preamble = "\n".join(";".join(r) for r in rows[:header_index])
    st = ParsedStatement(source_format="csv", iban=find_iban(preamble) if preamble else None)
    is_revolut = "completed date" in [norm(h) for h in rows[header_index]] and "product" in [norm(h) for h in rows[header_index]]
    if is_revolut:
        st.institution = "Revolut"

    body = rows[header_index + 1 :]
    if "date" not in columns:
        return [_parse_holdings(st, body, columns)]

    def cell(row: list[str], key: str) -> str | None:
        idx = columns.get(key)
        return row[idx].strip() if idx is not None and idx < len(row) else None

    balances: list = []
    currencies = set()
    skipped = 0
    for row in body:
        when = parse_date(cell(row, "date"))
        if when is None:
            skipped += 1
            continue
        if is_revolut and (cell(row, "state") or "").upper() not in ("COMPLETED", ""):
            continue
        if "amount" in columns:
            amount = parse_amount(cell(row, "amount"))
        else:
            debit = parse_amount(cell(row, "debit")) or Decimal(0)
            credit = parse_amount(cell(row, "credit")) or Decimal(0)
            amount = credit - abs(debit)
        if amount is None:
            skipped += 1
            continue
        fee = parse_amount(cell(row, "fee"))
        if fee:
            amount -= abs(fee)
        currency = (cell(row, "currency") or "EUR").upper()
        currencies.add(currency)
        st.transactions.append(TransactionData(date=when, label=cell(row, "label") or "Opération", amount=amount, currency=currency))
        balance = parse_amount(cell(row, "balance"))
        if balance is not None:
            balances.append((when, balance))

    if not st.transactions:
        raise ValueError("Aucune opération lisible dans ce fichier")
    st.transactions.sort(key=lambda t: t.date)
    st.period_start, st.period_end = st.transactions[0].date, st.transactions[-1].date
    if len(currencies) == 1:
        st.currency = currencies.pop()
    elif len(currencies) > 1:
        st.warnings.append("Plusieurs devises dans le fichier : les montants sont gardés dans leur devise d'origine")
    if balances:
        # Exports are sorted oldest-first or newest-first: the balance to keep is on the newest line.
        ascending = balances[0][0] <= balances[-1][0]
        st.balance_date, st.balance = balances[-1] if ascending else balances[0]
    if skipped:
        st.warnings.append(f"{skipped} ligne(s) ignorée(s) (date ou montant illisible)")
    st.account_label = filename.rsplit(".", 1)[0] if filename else None
    st.account_type = guess_account_type(st.account_label or "", "checking")
    st.confidence = "high"
    return [st]


def _parse_holdings(st: ParsedStatement, body: list[list[str]], columns: dict[str, int]) -> ParsedStatement:
    def cell(row, key):
        idx = columns.get(key)
        return row[idx].strip() if idx is not None and idx < len(row) else None

    st.holdings = []
    for row in body:
        quantity = parse_amount(cell(row, "quantity"))
        symbol, isin = cell(row, "symbol"), cell(row, "isin")
        if quantity is None or not (symbol or isin):
            continue
        value = parse_amount(cell(row, "value"))
        st.holdings.append(
            HoldingData(
                symbol=symbol.upper() if symbol else None,
                isin=isin.upper() if isin else None,
                name=cell(row, "name") or symbol or isin,
                quantity=quantity,
                asset_class="stock" if isin else "crypto",
                value=value,
                value_currency=(cell(row, "currency") or "EUR").upper() if value is not None else None,
            )
        )
    if not st.holdings:
        raise ValueError("Aucune position lisible dans ce fichier")
    st.account_type = "crypto_exchange" if all(h.asset_class == "crypto" for h in st.holdings) else "cto"
    st.confidence = "high"
    return st
