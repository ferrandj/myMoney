"""Text, number, date and identifier helpers shared by all adapters."""

from __future__ import annotations

import re
import unicodedata
from datetime import date
from decimal import Decimal, InvalidOperation


def strip_accents(text: str) -> str:
    return "".join(c for c in unicodedata.normalize("NFD", text) if unicodedata.category(c) != "Mn")


def norm(text: str) -> str:
    return re.sub(r"\s+", " ", strip_accents(text).lower()).strip()


_AMOUNT_CLEAN = re.compile(r"[€$£\s  ']|EUR|USD", re.I)


def parse_amount(raw: str | None) -> Decimal | None:
    """Parse "1 234,56", "-1,234.56", "1.234,56 €", "12,00-", "(45.10)"."""
    if raw is None:
        return None
    text = _AMOUNT_CLEAN.sub("", str(raw)).strip()
    if not text:
        return None
    negative = False
    if text.startswith("(") and text.endswith(")"):
        negative, text = True, text[1:-1]
    if text.endswith("-"):
        negative, text = True, text[:-1]
    if text.startswith("+"):
        text = text[1:]
    if text.startswith("-"):
        negative, text = True, text[1:]
    if "," in text and "." in text:
        if text.rfind(",") > text.rfind("."):
            text = text.replace(".", "").replace(",", ".")
        else:
            text = text.replace(",", "")
    elif "," in text:
        if re.fullmatch(r"\d{1,3}(,\d{3})+", text) and not re.search(r",\d{1,2}$", text):
            text = text.replace(",", "")  # 1,234,567 (thousands)
        else:
            text = text.replace(",", ".")
    elif text.count(".") > 1 and re.fullmatch(r"\d{1,3}(\.\d{3})+", text):
        text = text.replace(".", "")  # 1.234.567 (thousands)
    try:
        value = Decimal(text)
    except InvalidOperation:
        return None
    return -value if negative else value


_DATE_PATTERNS = [
    (re.compile(r"^(\d{4})-(\d{1,2})-(\d{1,2})"), ("y", "m", "d")),
    (re.compile(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{4})"), ("d", "m", "y")),
    (re.compile(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2})$"), ("d", "m", "yy")),
    (re.compile(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2})\b"), ("d", "m", "yy")),
]
_MONTHS = {
    "janv": 1, "jan": 1, "fevr": 2, "fev": 2, "feb": 2, "mars": 3, "mar": 3, "avr": 4, "apr": 4, "mai": 5, "may": 5,
    "juin": 6, "jun": 6, "juil": 7, "jul": 7, "aout": 8, "aug": 8, "sept": 9, "sep": 9, "oct": 10, "nov": 11,
    "dec": 12, "janvier": 1, "fevrier": 2, "avril": 4, "juillet": 7, "septembre": 9, "octobre": 10, "novembre": 11, "decembre": 12,
}


def parse_date(raw: str | None, default_year: int | None = None) -> date | None:
    if not raw:
        return None
    text = str(raw).strip()
    for pattern, order in _DATE_PATTERNS:
        m = pattern.match(text)
        if m:
            parts = dict(zip(order, m.groups()))
            year = int(parts.get("y") or 0) or 2000 + int(parts["yy"])
            try:
                return date(year, int(parts["m"]), int(parts["d"]))
            except ValueError:
                return None
    m = re.match(r"^(\d{1,2})\s+([a-zA-Zéèûô]+)\.?\s+(\d{4})", text)
    if m:
        month = _MONTHS.get(strip_accents(m.group(2).lower()))
        if month:
            try:
                return date(int(m.group(3)), month, int(m.group(1)))
            except ValueError:
                return None
    m = re.match(r"^(\d{1,2})[/.](\d{1,2})$", text)
    if m and default_year:
        try:
            return date(default_year, int(m.group(2)), int(m.group(1)))
        except ValueError:
            return None
    return None


_IBAN = re.compile(r"\b([A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){3,7}(?:[ ]?[A-Z0-9]{1,3})?)\b")


def iban_valid(iban: str) -> bool:
    iban = iban.replace(" ", "").upper()
    if not re.fullmatch(r"[A-Z]{2}\d{2}[A-Z0-9]{10,30}", iban):
        return False
    rearranged = iban[4:] + iban[:4]
    digits = "".join(str(int(c, 36)) for c in rearranged)
    return int(digits) % 97 == 1


def find_iban(text: str) -> str | None:
    for m in _IBAN.finditer(text.upper()):
        candidate = m.group(1).replace(" ", "")
        # The regex may grab a trailing word: shrink until the checksum matches.
        for end in range(len(candidate), 13, -1):
            if iban_valid(candidate[:end]):
                return candidate[:end]
    return None


INSTITUTIONS = [
    ("Fortuneo", ("fortuneo",)), ("BforBank", ("bforbank",)), ("Hello bank!", ("hello bank", "hellobank")),
    ("BoursoBank", ("boursorama", "boursobank")), ("BNP Paribas", ("bnp paribas",)), ("Société Générale", ("societe generale",)),
    ("Crédit Agricole", ("credit agricole",)), ("LCL", ("lcl", "credit lyonnais")), ("Caisse d'Épargne", ("caisse d'epargne", "caisse depargne")),
    ("Banque Populaire", ("banque populaire",)), ("Crédit Mutuel", ("credit mutuel",)), ("CIC", ("cic ",)),
    ("La Banque Postale", ("banque postale",)), ("ING", ("ing direct", "ing bank")), ("Revolut", ("revolut",)),
    ("Trade Republic", ("trade republic",)), ("Cashbee", ("cashbee",)), ("Nexo", ("nexo",)), ("Linxea", ("linxea",)),
    ("Spirica", ("spirica",)), ("Generali", ("generali",)), ("Suravenir", ("suravenir",)), ("Yomoni", ("yomoni",)),
    ("Nalo", ("nalo",)), ("Binance", ("binance",)), ("Kraken", ("kraken", "payward")), ("Monabanq", ("monabanq",)),
    ("Shine", ("shine",)), ("Qonto", ("qonto",)), ("N26", ("n26",)), ("AXA", ("axa banque", "axa ")), ("Ramify", ("ramify",)),
]


def find_institution(text: str) -> str | None:
    lowered = norm(text[:5000])
    best = None
    for name, keys in INSTITUTIONS:
        for key in keys:
            pos = lowered.find(key)
            if pos >= 0 and (best is None or pos < best[0]):
                best = (pos, name)
    return best[1] if best else None


# Words that say nothing about who someone is: civilities, "or"/"and" of joint accounts, legal forms.
_NAME_NOISE = {"m", "mr", "mme", "mlle", "monsieur", "madame", "mademoiselle", "ou", "et", "or", "and", "sas", "sarl", "sa", "sasu", "eurl"}


def name_key(name: str) -> str:
    """Order- and civility-insensitive key: "M DUPONT Jean" and "Jean Dupont" are the same person."""
    tokens = set(re.findall(r"[a-z0-9]+", strip_accents(name).lower())) - _NAME_NOISE
    return " ".join(sorted(tokens)) or norm(name)


def _rib_digits(value: str) -> int:
    # RIB letters count as digits: A-I -> 1-9, J-R -> 1-9, S-Z -> 2-9.
    table = {c: str((i % 9) + 1) for i, c in enumerate("ABCDEFGHIJKLMNOPQR")}
    table.update({c: str((i % 9) + 2) for i, c in enumerate("STUVWXYZ")})
    return int("".join(table.get(c, c) for c in value.upper()))


def rib_key(bank: str, branch: str, account: str) -> str:
    return f"{97 - (89 * _rib_digits(bank) + 15 * _rib_digits(branch) + 3 * _rib_digits(account)) % 97:02d}"


def rib_valid(bank: str, branch: str, account: str, key: str) -> bool:
    try:
        return rib_key(bank, branch, account) == key
    except ValueError:
        return False


def iban_matches_rib(iban: str, bank: str, branch: str, account: str, key: str) -> bool:
    """A French IBAN is "FR" + 2 check digits + the RIB."""
    return iban.replace(" ", "").upper()[4:] == f"{bank}{branch}{account}{key}".upper()
