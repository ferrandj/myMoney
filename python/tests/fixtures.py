"""Synthetic documents for the tests. Everything here is made up: no real name, address, IBAN or amount.

`boursobank_statement_pdf` draws a statement with the same layout as a real BoursoBank monthly extract
(holder block, RIB banner, BIC/IBAN line, debit/credit columns, wrapped detail lines, page headers and footers),
so the adapter is exercised on realistic geometry without a single personal byte in the repository.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import Decimal

from fpdf import FPDF

from mymoney.adapters.common import rib_key

BANK, BRANCH, ACCOUNT = "12345", "67890", "00012345678"
HOLDER = ["M DUPONT JEAN"]
ADDRESS = ["12 RUE DES LILAS", "75011 PARIS"]


def make_iban(bban: str, country: str = "FR") -> str:
    digits = "".join(str(int(c, 36)) for c in bban + country + "00")
    return f"{country}{98 - int(digits) % 97:02d}{bban}"


def fake_iban(seed: int = 0) -> str:
    """A valid IBAN that belongs to nobody (other bank code, account derived from the seed)."""
    account = f"{90000000000 + seed}"
    return make_iban(f"20041{'01005'}{account}{rib_key('20041', '01005', account)}")


IBAN = make_iban(f"{BANK}{BRANCH}{ACCOUNT}{rib_key(BANK, BRANCH, ACCOUNT)}")


@dataclass
class Entry:
    day: date
    lines: list[str]  # first line, then the detail lines
    amount: Decimal  # negative = debit
    value_day: date | None = None


@dataclass
class Spec:
    entries: list[Entry]
    opening: Decimal = Decimal("1000.00")
    opening_date: date = date(2026, 8, 31)
    period: tuple[date, date] = (date(2026, 9, 1), date(2026, 9, 30))
    holder: list[str] = field(default_factory=lambda: list(HOLDER))
    address: list[str] = field(default_factory=lambda: list(ADDRESS))
    bank: str = BANK
    branch: str = BRANCH
    account: str = ACCOUNT
    garble: bool = False  # mimic fonts whose accents come out as other glyphs when the text is extracted
    wrong_closing: Decimal | None = None  # a closing balance that does not match, to test the consistency check
    title: str = ""  # document metadata only: gives the same statement a different file hash
    product: str = ""  # savings accounts print "Compte d'épargne : <product>" under the title

    @property
    def closing(self) -> Decimal:
        return self.opening + sum((e.amount for e in self.entries), Decimal(0))


# A joint account: the bank cuts the holder line at about 29 characters, and the address has a second line.
JOINT_HOLDER = ["M DUPONT OU MME LEROY CLAIRE"]
JOINT_ADDRESS = ["12 RUE DES LILAS", "BAT B APPT 12", "75011 PARIS"]


def fr(amount: Decimal) -> str:
    whole, cents = f"{abs(amount):.2f}".split(".")
    groups = []
    while len(whole) > 3:
        whole, tail = whole[:-3], whole[-3:]
        groups.insert(0, tail)
    groups.insert(0, whole)
    return ".".join(groups) + "," + cents


def d(value: date) -> str:
    return value.strftime("%d/%m/%Y")


_GARBLE = str.maketrans({"é": "Ø"})


def boursobank_statement_pdf(spec: Spec) -> bytes:
    pdf = FPDF(unit="pt", format=(595, 842))
    pdf.set_auto_page_break(False)
    pdf.set_title(spec.title)
    pdf.set_creation_date(datetime(2026, 10, 1))  # same spec, same bytes: only `title` makes another "download" of the statement
    pdf.set_font("Helvetica", size=9)
    key = rib_key(spec.bank, spec.branch, spec.account)
    iban = make_iban(f"{spec.bank}{spec.branch}{spec.account}{key}")
    pages_needed = {"n": 1}

    def txt(x: float, y: float, text: str, right: float | None = None) -> None:
        if spec.garble:
            text = text.translate(_GARBLE).replace("è", "(cid:232)").replace("à", "(cid:224)").replace("°", "(cid:176)")
        if right is not None:
            x = right - pdf.get_string_width(text)
        pdf.text(x, y, text)

    def header(page: int, total: int, first: bool) -> float:
        pdf.add_page()
        top = 0.0 if first else -190.0
        if first:
            txt(320, 40, "Extrait de votre compte en EUR")
            txt(320, 52, "(Protégé par la Garantie des Dépôts www.garantiedesdepots.fr)")
            txt(322, 62, "BOURSOBANK")
            if spec.product:
                txt(320, 74, f"Compte d'épargne : {spec.product}")
            y = 198.0
            for line in [*spec.holder, *spec.address]:
                txt(320, y, line)
                y += 12
        banner = 305 + top
        txt(46, banner, "Date"); txt(94, banner, "N° de RIB"); txt(262, banner, "Devise"); txt(293, banner, "Période")
        txt(412, banner, "Montant DA*"); txt(472, banner, "TAEG*"); txt(524, banner, "Page")
        txt(94, banner + 12, "Banque"); txt(128, banner + 12, "Guichet"); txt(162, banner + 12, "N° de compte"); txt(227, banner + 12, "Clé RIB")
        row = banner + 23
        txt(46, row, d(date(2026, 10, 1))); txt(97, row, spec.bank); txt(131, row, spec.branch); txt(166, row, spec.account); txt(236, row, key)
        txt(266, row, "EUR"); txt(295, row, f"du {d(spec.period[0])} au {d(spec.period[1])}"); txt(426, row, "0,00"); txt(473, row, "0,000000 %")
        txt(531, row, f"{page}/{total}")
        bic_row = banner + 58
        txt(43, bic_row, "B.I.C."); txt(78, bic_row, "BOUSFRPPXXX"); txt(180, bic_row, "I.B.A.N."); txt(233, bic_row, " ".join(iban[i : i + 4] for i in range(0, len(iban), 4)))
        txt(378, bic_row + 22, "MOUVEMENTS EN EUR")
        head = bic_row + 45
        txt(46, head, "Date"); txt(95, head + 6, "Libellé"); txt(381, head + 6, "Valeur"); txt(442, head + 6, "Débit"); txt(506, head + 6, "Crédit")
        txt(46, head + 12, "opération")
        return head + 32

    def footer() -> None:
        txt(83, 748, "A réception d'un extrait de compte vous disposez d'un délai de quatre mois pour formuler toute réclamation")
        txt(85, 773, "Boursorama - SA au capital de 53 576 889,20 - RCS Nanterre 351 058 151")

    # Lay the entries out page by page (about 24 lines of 11.4 pt fit under the header).
    pages: list[list[Entry]] = [[]]
    used = 3  # the opening balance row
    for e in spec.entries:
        need = len(e.lines) + 0.4
        if used + need > 24 and pages[-1]:
            pages.append([])
            used = 0
        pages[-1].append(e)
        used += need
    total = len(pages)

    for number, entries in enumerate(pages, 1):
        y = header(number, total, first=number == 1)
        if number == 1:
            txt(300, y, "SOLDE AU :")
            txt(375, y, d(spec.opening_date))
            txt(0, y, fr(spec.opening), right=546 if spec.opening >= 0 else 480)
            y += 33
        for e in entries:
            txt(46, y, d(e.day))
            txt(95, y, e.lines[0])
            txt(375, y, d(e.value_day or e.day))
            txt(0, y, fr(e.amount), right=546 if e.amount > 0 else 480)
            for extra in e.lines[1:]:
                y += 11.4
                txt(112, y, extra)
            y += 12
        if number == total:
            y = 683.0
            txt(145, y, "Nouveau solde en EUR :")
            closing = spec.wrong_closing if spec.wrong_closing is not None else spec.closing
            txt(0, y, fr(closing), right=546 if closing >= 0 else 480)
            txt(145, y + 14, "Montant frais bancaires* :")
        footer()
    return bytes(pdf.output())


def sample_entries() -> list[Entry]:
    """One of each kind of line the BoursoBank statement has."""
    D = Decimal
    return [
        Entry(date(2026, 9, 1), ["VIR Virement depuis LIVRET DEVELOPPE", "MENT DURABLE SOLID", "DUPONT JEAN"], D("150.00")),
        Entry(date(2026, 9, 1), ["VIR SEPA MARTIN Pauline", "cadeau anniversaire", "Rèf : SCT123450000000000000000001"], D("-50.00")),
        Entry(date(2026, 9, 1), ["PRLV SEPA EAUX DU NORD", "Facture EAUX", "1234567DV57ABCDE", "RUM S-1-0000000001-000000000-01"], D("-28.00")),
        Entry(date(2026, 9, 1), ["CARTE 31/08/26 TRAINS DE FRANCE CB*1234"], D("-71.00")),
        Entry(date(2026, 9, 2), ["*COTIS ASS.COMPTE 09/2026", "ASSURANCE COMPTE STANDARD"], D("-2.00")),
        Entry(date(2026, 9, 3), ["PRLV SEPA DIRECTION GENERALE DES FINANCE", "600231997960TLR2660421296524 TLRSEPA DGF", "IP IMPOT TF", "RUM NNFR46ZZZ005002266"], D("-1396.00")),
        Entry(date(2026, 9, 4), ["CARTE 02/09/26 UBER *EATS CB*1234"], D("-18.48")),
        Entry(date(2026, 9, 7), ["VIR SEPA Revolut", "achat recurrent crypto", "Rèf : SCT123450000000000000000002"], D("-25.00")),
        Entry(date(2026, 9, 7), ["ECH PRET:80000000000000000001"], D("-166.67")),
        Entry(date(2026, 9, 8), ["VIR SEPA ACME CONSULTING SAS", "Salaire septembre", "Réf 03620N0200000000000000"], D("2400.00")),
        Entry(date(2026, 9, 9), ["VIR INST DUPONT JEAN", "Virement depuis BoursoBank"], D("-200.00")),
        Entry(date(2026, 9, 10), ["VIR Virement de Jean DUPONT", "MME LEROY CLAIRE", "Rèf : SCT123450000000000000000003"], D("-35.00")),
        Entry(date(2026, 9, 12), ["VIR Virement depuis BOURSORAMA Claire", "LEROY CLAIRE"], D("60.00"), value_day=date(2026, 9, 13)),
        Entry(date(2026, 9, 14), ["CARTE 12/09/26 CARREFOUR CITY CB*1234"], D("-14.08")),
    ]


@dataclass
class SavingsSpec:
    entries: list[Entry]
    opening: Decimal = Decimal("3000.00")
    period: tuple[date, date] = (date(2026, 9, 1), date(2026, 9, 30))
    holder: list[str] = field(default_factory=lambda: ["DUPONT JEAN"])  # savings statements print no civility
    address: list[str] = field(default_factory=lambda: list(ADDRESS))
    account: str = "00099999999"
    product: str = "Plan Epargne Logement"
    title: str = ""

    @property
    def closing(self) -> Decimal:
        return self.opening + sum((e.amount for e in self.entries), Decimal(0))


def boursobank_savings_pdf(spec: SavingsSpec) -> bytes:
    """A savings extract (PEL...): product in the title, plain account number (no RIB, no IBAN), one page."""
    pdf = FPDF(unit="pt", format=(595, 842))
    pdf.set_auto_page_break(False)
    pdf.set_title(spec.title)
    pdf.set_creation_date(datetime(2026, 10, 1))
    pdf.set_font("Helvetica", size=9)
    pdf.add_page()
    t = pdf.text

    def right(edge: float, y: float, text: str) -> None:
        pdf.text(edge - pdf.get_string_width(text), y, text)

    t(288, 48, f"Extrait de votre {spec.product}")
    t(288, 62, "(Protégé par la Garantie des Dépôts www.garantiedesdepots.fr)")
    y = 198.0
    for line in [*spec.holder, *spec.address]:
        t(320, y, line)
        y += 12
    t(46, 318, "Date"); t(124, 318, "Numéro de compte"); t(273, 318, "Devise"); t(319, 318, "Période"); t(514, 318, "Page")
    t(59, 339, d(date(2026, 10, 1))); t(167, 339, spec.account); t(284, 339, "EUR"); t(359, 339, f"du {d(spec.period[0])} au {d(spec.period[1])}"); t(531, 339, "1")
    t(433, 353, "MOUVEMENTS EN EUR")
    t(46, 370, "Date"); t(68, 370, "opération"); t(201, 370, "Libellé"); t(343, 370, "Valeur"); t(423, 370, "Débit"); t(501, 370, "Crédit")
    t(231, 390, "SOLDE EN EUR"); t(295, 390, "AU :"); t(336, 390, d(spec.period[0])); right(544, 390, fr(spec.opening))
    y = 426.0
    for e in spec.entries:
        t(54, y, d(e.day)); t(115, y, e.lines[0]); t(336, y, d(e.value_day or e.day))
        right(544 if e.amount > 0 else 465, y, fr(e.amount))
        for extra in e.lines[1:]:
            y += 11.4
            t(115, y, extra)
        y += 14
    t(204, 677, "Nouveau solde en EUR"); right(545, 677, fr(spec.closing))
    t(100, 748, "A réception d'un extrait de compte vous disposez d'un délai d'un mois pour formuler toute réclamation")
    t(85, 773, "Boursorama - SA au capital de 53 576 889,20 - RCS Nanterre 351 058 151")
    return bytes(pdf.output())
