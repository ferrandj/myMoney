"""Synthetic Fortuneo documents for the tests: made-up holder, address, IBAN and amounts, real-looking geometry.

Three documents are drawn: the monthly "Relevé de Compte", the yearly "Relevé de frais" and the FGDR letter.
Words sit where a real document puts them (columns, right-aligned amounts, the bank's own address on the left of the
holder's block, wrapped detail lines), so the adapters read the same shapes as in real life.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import Decimal

from fpdf import FPDF

from mymoney.adapters.common import rib_key

from .fixtures import fr, make_iban

F_BANK, F_BRANCH, F_ACCOUNT = "30002", "00999", "00098765432"
F_HOLDER = "M MARTIN PAUL"
F_ADDRESS = ["8 RUE DES TILLEULS", "69003 LYON"]
F_IBAN = make_iban(f"{F_BANK}{F_BRANCH}{F_ACCOUNT}{rib_key(F_BANK, F_BRANCH, F_ACCOUNT)}")
MONTHS = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"]


def fr_space(amount: Decimal) -> str:
    """Fortuneo prints thousands with a space ("1 500,00"), unlike the dots of the other banks."""
    return fr(amount).replace(".", " ")


def long_date(value: date) -> str:
    return f"{value.day} {MONTHS[value.month - 1]} {value.year}"


@dataclass
class FEntry:
    day: date
    lines: list[str]  # first line, then the detail lines
    amount: Decimal  # negative = debit
    value_day: date | None = None


@dataclass
class FSpec:
    entries: list[FEntry]
    opening: Decimal = Decimal("500.00")
    period_end: date = date(2026, 9, 30)
    number: str = "9 - 2026"
    holder: str = F_HOLDER
    address: list[str] = field(default_factory=lambda: list(F_ADDRESS))
    bank: str = F_BANK
    branch: str = F_BRANCH
    account: str = F_ACCOUNT
    brand: str = "FORTUNEO"
    wrong_totals: bool = False  # print totals that do not match the operations
    wrong_closing: Decimal | None = None
    title: str = ""  # document metadata only: gives the same statement a different file hash
    livret: bool = False  # a savings book: "Relevé Livret +", "Compte <client> 01", no RIB and no IBAN
    opening_on: date | None = None  # date of the "ancien solde" (default: the last day of the month before the period)
    first: bool = False  # the account's first statement: "ANCIEN SOLDE 0,00 €" with no date and no side

    @property
    def closing(self) -> Decimal:
        return self.opening + sum((e.amount for e in self.entries), Decimal(0))


def _pdf(title: str = "") -> FPDF:
    pdf = FPDF(unit="pt", format=(595, 842))
    pdf.core_fonts_encoding = "cp1252"
    pdf.set_auto_page_break(False)
    pdf.set_title(title)
    pdf.set_creation_date(datetime(2026, 10, 1))
    pdf.add_page()
    pdf.set_font("Helvetica", size=9)
    return pdf


def _text(pdf: FPDF, x: float, y: float, text: str, right: float | None = None) -> None:
    if right is not None:
        x = right - pdf.get_string_width(text)
    pdf.text(x, y, text)


def _iban_line(iban: str) -> str:
    return " ".join(iban[i : i + 4] for i in range(0, len(iban), 4))


def _bank_block(pdf: FPDF, brand: str, y: float = 118) -> None:
    _text(pdf, 45, y, brand)
    _text(pdf, 45, y + 9, "TOUR EXEMPLE")
    _text(pdf, 45, y + 19, "1 PLACE DU TEST")
    _text(pdf, 45, y + 28, "75000 PARIS")


def fortuneo_statement_pdf(spec: FSpec) -> bytes:
    pdf = _pdf(spec.title)
    end = spec.period_end
    key = rib_key(spec.bank, spec.branch, spec.account)
    iban = make_iban(f"{spec.bank}{spec.branch}{spec.account}{key}")
    opening_date = spec.opening_on or date.fromordinal(date(end.year, end.month, 1).toordinal() - 1)

    if spec.livret:
        _text(pdf, 264, 32, "Relevé Livret +"); _text(pdf, 531, 35, f"N° {spec.number}")
        _text(pdf, 264, 48, f"Compte {spec.account[:8]} 01"); _text(pdf, 541, 48, "Page 1/1")
    else:
        _text(pdf, 264, 32, "Relevé de Compte"); _text(pdf, 531, 35, f"N° {spec.number}")
        _text(pdf, 264, 48, "COMPTE COURANT"); _text(pdf, 541, 48, "Page 1/1")
    _text(pdf, 264, 68, f"Arrêté au {long_date(end)}")
    _bank_block(pdf, spec.brand)
    if len(spec.holder) > 30:  # a joint account: the bank cuts the holder line at 30 characters, in the middle of a word
        _text(pdf, 323, 165, spec.holder[:30]); _text(pdf, 323, 176, spec.holder[30:])
    else:
        _text(pdf, 323, 176, spec.holder)
    for i, line in enumerate(spec.address):
        _text(pdf, 323, 189 + 10 * i, line)
    _text(pdf, 45, 189, f"SITE INTERNET www.{spec.brand.lower()}.fr")
    _text(pdf, 45, 208, "SERVICE CLIENTS TEL 0 800 000 000")
    _text(pdf, 45, 221, "SERVICE CLIENTS FAX 0 800 000 001")
    _text(pdf, 58, 267, f"Titulaire(s) du compte : {spec.holder[:30]}")
    if len(spec.holder) > 30:
        _text(pdf, 146, 277, spec.holder[30:])
    if not spec.livret:
        _text(pdf, 58, 291, f"NUMÉRO DE COMPTE {spec.bank} {spec.branch} {spec.account} {key}")
        _text(pdf, 58, 301, f"IBAN {_iban_line(iban)}"); _text(pdf, 235, 301, "BIC FTNOFRP1XXX")

    _text(pdf, 57, 357, "Date"); _text(pdf, 84, 357, "Date de Valeur"); _text(pdf, 236, 357, "Opération")
    _text(pdf, 407, 357, "Débit"); _text(pdf, 495, 357, "Crédit")
    y = 375.0
    _text(pdf, 54, y, "ANCIEN SOLDE" if spec.first else f"ANCIEN SOLDE CRÉDITEUR AU {long_date(opening_date).upper()}")
    _text(pdf, 0, y, fr_space(spec.opening), right=546 if spec.opening >= 0 else 463)
    y += 22
    for e in spec.entries:
        _text(pdf, 54, y, e.day.strftime("%d/%m")); _text(pdf, 88, y, (e.value_day or e.day).strftime("%d/%m/%Y")); _text(pdf, 136, y, e.lines[0])
        _text(pdf, 0, y, fr_space(e.amount), right=546 if e.amount > 0 else 463)
        for extra in e.lines[1:]:
            y += 11
            _text(pdf, 136, y, extra)
        y += 11
    debits = sum((-e.amount for e in spec.entries if e.amount < 0), Decimal(0))
    credits = sum((e.amount for e in spec.entries if e.amount > 0), Decimal(0))
    if spec.wrong_totals:
        debits += Decimal("1.00")
    y += 11
    _text(pdf, 55, y, "TOTAL DES OPÉRATIONS DU RELEVÉ")
    if debits:
        _text(pdf, 0, y, fr_space(debits), right=463)
    if credits:
        _text(pdf, 0, y, fr_space(credits), right=546)
    y += 24
    closing = spec.wrong_closing if spec.wrong_closing is not None else spec.closing
    _text(pdf, 54, y, f"NOUVEAU SOLDE {'CRÉDITEUR' if closing >= 0 else 'DÉBITEUR'} AU {long_date(end).upper()}")
    _text(pdf, 0, y, fr_space(closing), right=546 if closing >= 0 else 463)
    _text(pdf, 50, y + 40, "Garantie de vos dépôts")
    _text(pdf, 50, y + 55, "Total de vos services et frais bancaires")
    _text(pdf, 50, y + 68, "Services et frais facturés"); _text(pdf, 439, y + 68, "0,00 €")
    _text(pdf, 320, y + 80, "TOTAL facturé"); _text(pdf, 439, y + 80, "0,00 €")
    _text(pdf, 74, 763, f"{spec.brand.capitalize()} est une marque commerciale d'un établissement de test.")
    return bytes(pdf.output())


def fortuneo_entries() -> list[FEntry]:
    """One of each kind of line the statement has (September 2026)."""
    D = Decimal
    return [
        FEntry(date(2026, 9, 1), ["VIR M MARTIN PAUL"], D("700.00")),
        FEntry(date(2026, 9, 3), ["VIR M MARTIN PAUL"], D("-60.00")),
        FEntry(date(2026, 9, 9), ["VIR M MARTIN PAUL"], D("200.00")),
        FEntry(date(2026, 9, 14), ["CARTE 12/09 Boulangerie Test Lyon"], D("-3.50"), value_day=date(2026, 9, 13)),
        FEntry(date(2026, 9, 14), ["ECH PRET 0000TEST00000001"], D("-250.00"), value_day=date(2026, 9, 15)),
        FEntry(date(2026, 9, 21), ["PRLV SEPA ASSURANCE EXEMPLE", "VERSEMENT PERIODIQUE - CONTRAT 99999999", "FR00ZZZ000000", "ABCD25139000000003485321", "V260918007125"], D("-80.00")),
        FEntry(date(2026, 9, 22), ["VIR SEPA ACME CONSULTING SAS", "Salaire septembre", "SCT000000000000000000000001"], D("1500.00")),
        FEntry(date(2026, 9, 25), ["VIR SEPA MARTIN Pauline", "cadeau", "SCT000000000000000000000002"], D("-40.00")),
    ]


def fortuneo_september(**kw) -> FSpec:
    return FSpec(fortuneo_entries(), **kw)


# ------------------------------------------------------------------------------------------- fees recap
@dataclass
class FeesSpec:
    total: Decimal = Decimal("9.00")
    year: int = 2023
    holder: str = F_HOLDER
    address: list[str] = field(default_factory=lambda: list(F_ADDRESS))
    bank: str = F_BANK
    branch: str = F_BRANCH
    account: str = F_ACCOUNT
    brand: str = "Fortuneo"
    services: list[tuple[str, str, int, str, int, str]] = field(default_factory=list)  # category, name, used, unit, billed, total
    interest_paid: Decimal = Decimal("0.00")
    title: str = ""


def fortuneo_fees_pdf(spec: FeesSpec) -> bytes:
    pdf = _pdf(spec.title)
    key = rib_key(spec.bank, spec.branch, spec.account)
    iban = make_iban(f"{spec.bank}{spec.branch}{spec.account}{key}")
    _text(pdf, 241, 59, "Relevé de frais")
    _text(pdf, 36, 111, spec.brand)
    _text(pdf, 36, 124, "TOUR EXEMPLE"); _text(pdf, 36, 137, "1 PLACE DU TEST"); _text(pdf, 36, 150, "75000 PARIS")
    _text(pdf, 36, 176, f"SITE INTERNET www.{spec.brand.lower()}.fr")
    _text(pdf, 36, 189, "SERVICE CLIENTS TEL 0 800 000 000"); _text(pdf, 36, 203, "SERVICE CLIENTS FAX 0 800 000 001")
    _text(pdf, 36, 285, spec.holder)
    for i, line in enumerate(spec.address):
        _text(pdf, 36, 298 + 13 * i, line)
    _text(pdf, 40, 333, "Intitulé du compte"); _text(pdf, 186, 333, "COMPTE COURANT")
    _text(pdf, 40, 353, "Identification du compte"); _text(pdf, 186, 353, "BIC"); _text(pdf, 338, 353, "FTNOFRP1XXX")
    _text(pdf, 186, 366, "IBAN"); _text(pdf, 338, 366, _iban_line(iban))
    _text(pdf, 186, 379, "NUMERO DE COMPTE"); _text(pdf, 338, 379, f"{spec.bank} {spec.branch} {spec.account} {key}")
    _text(pdf, 186, 392, "TITULAIRE(S) DU COMPTE"); _text(pdf, 338, 392, spec.holder)
    _text(pdf, 40, 412, "Période"); _text(pdf, 186, 412, f"Du 1er Janvier au 31 Décembre {spec.year}")
    _text(pdf, 40, 432, "Date"); _text(pdf, 186, 432, f"31/12/{spec.year}")
    _text(pdf, 36, 570, "Récapitulatif des frais et intérêts")
    _text(pdf, 40, 608, "Total des frais payés (frais afférents aux offres groupées de"); _text(pdf, 0, 608, fr(spec.total) + " €", right=564)
    _text(pdf, 40, 622, "services et tous autres frais)")
    _text(pdf, 40, 653, "Total des intérêts payés"); _text(pdf, 0, 653, fr(spec.interest_paid) + " €", right=564)
    _text(pdf, 40, 684, "Total des intérêts acquis"); _text(pdf, 437, 681, "Intérêts non applicables")
    if spec.services:
        pdf.add_page()
        _text(pdf, 40, 60, "Décompte des frais prélevés sur le compte")
        _text(pdf, 40, 80, "Service"); _text(pdf, 250, 80, "Utilisé"); _text(pdf, 320, 80, "Prix"); _text(pdf, 400, 80, "Facturé"); _text(pdf, 480, 80, "Total")
        y, category = 100.0, None
        for cat, name, used, unit, billed, total in spec.services:
            if cat != category:
                _text(pdf, 40, y, cat)
                category, y = cat, y + 13
            _text(pdf, 40, y, name)
            _text(pdf, 250, y, str(used)); _text(pdf, 320, y, unit); _text(pdf, 400, y, str(billed)); _text(pdf, 480, y, total)
            y += 13
        _text(pdf, 40, y + 10, "Total des frais payés"); _text(pdf, 0, y + 10, fr(spec.total) + " €", right=564)
    return bytes(pdf.output())


# ------------------------------------------------------------------------------------------- FGDR letter
def fortuneo_info_pdf(year: int = 2026, holder: str = F_HOLDER, address: list[str] | None = None, brand: str = "Fortuneo", title: str = "", ceiling: str = "100 000 €") -> bytes:
    pdf = _pdf(title)
    _text(pdf, 323, 53, f"Le 01 janvier {year},")
    _text(pdf, 323, 176, holder)
    for i, line in enumerate(address or F_ADDRESS):
        _text(pdf, 323, 187 + 12 * i, line)
    _text(pdf, 50, 267, "Monsieur,")
    _text(pdf, 50, 293, f"Vous détenez un ou plusieurs comptes chez {brand} éligible(s) à la garantie du Fonds de Garantie")
    _text(pdf, 50, 305, "des Dépôts et de Résolution (FGDR).")
    _text(pdf, 50, 430, f"L'Équipe {brand}")
    _text(pdf, 149, 468, "INFORMATIONS GÉNÉRALES SUR LA PROTECTION DES DÉPÔTS")
    _text(pdf, 53, 480, "La protection des dépôts Fonds de garantie des dépôts et de résolution (FGDR)")
    _text(pdf, 54, 514, f"Plafond de la protection : {ceiling} par déposant et par établissement de crédit (1)")
    return bytes(pdf.output())
