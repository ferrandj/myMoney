"""Synthetic loan documents for the tests: a made-up loan (amounts, number, borrower) with the real documents' layout.

`LoanSpec` describes one loan; from it the three documents are drawn: the offer (conditions + numbered amortization
table), the notice of a partial early repayment, and the dated amortization table printed after the repayment.
The schedules are computed here, independently of the code under test: interest = capital × rate / 12 rounded to the cent,
a constant payment, the last instalment clearing what is left.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import ROUND_HALF_UP, Decimal

from fpdf import FPDF

D = Decimal
CENT = D("0.01")
L_REF, L_PROJECT = "FT99000002", "FT99000001"
L_BORROWER = "DURAND PAUL"


def q(x: Decimal) -> Decimal:
    return x.quantize(CENT, ROUND_HALF_UP)


def month_add(day: date, n: int) -> date:
    index = day.year * 12 + day.month - 1 + n
    return date(index // 12, index % 12 + 1, day.day)


def money(x: Decimal) -> str:
    """122 082,74 : the thousands are separated by a space, which the PDF makes a separate word."""
    whole, cents = f"{abs(x):.2f}".split(".")
    groups = []
    while len(whole) > 3:
        whole, tail = whole[:-3], whole[-3:]
        groups.insert(0, tail)
    groups.insert(0, whole)
    return " ".join(groups) + "," + cents


@dataclass
class Line:
    capital: Decimal
    interest: Decimal
    insurance: Decimal
    total: Decimal
    remaining: Decimal
    day: date | None = None
    n: int | None = None


def amortize(capital: Decimal, rate: Decimal, payment: Decimal, insurance: Decimal, first_due: date | None = None, start_n: int = 1) -> list[Line]:
    lines: list[Line] = []
    k = 0
    while capital > 0:
        interest = q(capital * rate / 1200)
        principal = payment - insurance - interest
        if principal >= capital:
            principal = capital
        capital -= principal
        lines.append(Line(principal, interest, insurance, principal + interest + insurance, capital,
                          month_add(first_due, k) if first_due else None, start_n + k))
        k += 1
    return lines


@dataclass
class LoanSpec:
    principal: Decimal = D("60000.00")
    rate: Decimal = D("2.40")
    taeg: Decimal = D("2.95")
    months: int = 60
    first_due: date = date(2025, 3, 15)
    insurance: Decimal = D("12.00")
    insurance_optional_share: Decimal = D("0.75")  # of the insurance, the part that is not mandatory
    guarantee_fees: Decimal = D("900.00")
    reference: str = L_REF
    project: str = L_PROJECT
    borrower: str = L_BORROWER
    product: str = "Fiximmo"
    # The early repayment (partial): done after `repaid_after` instalments, for `repaid` euros.
    repaid_after: int = 15
    repaid: Decimal = D("10000.00")
    repayment_day: date = date(2026, 6, 6)
    new_payment_delta: Decimal = D("-0.50")  # the new instalment against the old one
    today: date = date(2026, 10, 24)
    wrong_interest_total: bool = False  # announce an interest total that the table does not reach
    broken_row: int | None = None  # corrupt this offer row (its total no longer adds up)
    brand: str = "Fortuneo"
    title: str = ""

    @property
    def payment(self) -> Decimal:
        r = self.rate / 1200
        return q(self.principal * r / (1 - (1 + r) ** -self.months)) + self.insurance

    @property
    def offer(self) -> list[Line]:
        lines = amortize(self.principal, self.rate, self.payment, self.insurance, None)
        # The bank spreads the rounding on the last instalment: it keeps the constant total.
        last = lines[-1]
        last.interest = self.payment - self.insurance - last.capital
        last.total = self.payment
        return lines

    @property
    def capital_before(self) -> Decimal:
        return self.offer[self.repaid_after - 1].remaining

    @property
    def capital_after(self) -> Decimal:
        return self.capital_before - self.repaid

    @property
    def last_due_before(self) -> date:
        return month_add(self.first_due, self.repaid_after - 1)

    @property
    def new_payment(self) -> Decimal:
        return self.payment + self.new_payment_delta

    @property
    def new_plan(self) -> list[Line]:
        """Every instalment after the repayment, dated."""
        return amortize(self.capital_after, self.rate, self.new_payment, self.insurance, month_add(self.last_due_before, 1))

    @property
    def schedule(self) -> list[Line]:
        """What the customer area prints on `today`: the instalments still to come."""
        return [x for x in self.new_plan if x.day > self.today]

    @property
    def end_before(self) -> date:
        return month_add(self.first_due, self.months - 1)

    @property
    def end_after(self) -> date:
        return self.new_plan[-1].day


def _pdf(title: str = "") -> FPDF:
    pdf = FPDF(unit="pt", format=(595, 842))
    pdf.core_fonts_encoding = "cp1252"
    pdf.set_auto_page_break(False)
    pdf.set_title(title)
    pdf.set_creation_date(datetime(2026, 10, 1))
    pdf.add_page()
    pdf.set_font("Helvetica", size=9)
    return pdf


def _t(pdf: FPDF, x: float, y: float, text: str, right: float | None = None) -> None:
    pdf.text(right - pdf.get_string_width(text) if right is not None else x, y, text)


def _rows(pdf: FPDF, lines: list[Line], dated: bool, top: float = 160) -> None:
    y = top
    for line in lines:
        if y > 780:
            pdf.add_page()
            pdf.set_font("Helvetica", size=9)
            _t(pdf, 43, 100, "N° Amortissement Intérêts Intérêts Assurances Total à payer Montant restant dû")
            _t(pdf, 96, 112, "Capital normaux différés après échéance")
            y = 140
        head = line.day.strftime("%d/%m/%Y") if dated else str(line.n)
        _t(pdf, 46, y, head)
        for x, value in ((140, line.capital), (210, line.interest), (270, D("0")), (330, line.insurance), (400, line.total), (520, line.remaining)):
            _t(pdf, 0, y, money(value), right=x + 40)
        y += 11.5


def offer_pdf(spec: LoanSpec) -> bytes:
    pdf = _pdf(spec.title)
    lines = spec.offer
    interest_total = sum((x.interest for x in lines), D(0))
    insurance_total = sum((x.insurance for x in lines), D(0))
    optional = q(insurance_total * spec.insurance_optional_share)
    mandatory = insurance_total - optional
    total_cost = interest_total + mandatory + spec.guarantee_fees
    announced = interest_total + (D("5.00") if spec.wrong_interest_total else D(0))
    _t(pdf, 186, 54, "OFFRE DE PRÊT(S)"); _t(pdf, 211, 81, "IMMOBILIER(S)")
    _t(pdf, 14, 780, f"{spec.brand} Emprunteur est un contrat d'assurance de groupe.")
    _t(pdf, 14, 133, "Les Conditions particulières et générales ci-dessous constituent l'offre de prêt immobilier.")
    _t(pdf, 18, 164, "CONDITIONS PARTICULIÈRES")
    _t(pdf, 14, 212, "Le Prêteur : (ci-après désigné « le Prêteur »)")
    _t(pdf, 14, 221, "EXEMPLE BANQUE, SOCIETE ANONYME, 1 PLACE DU TEST, 75000 PARIS")
    _t(pdf, 14, 238, "Le(s) Emprunteur(s) : (ci-après désigné « l'Emprunteur »)")
    _t(pdf, 14, 255, f"M. {spec.borrower} (12345678) né(e) le 01/01/1980 à Testville (00) et demeurant 8 RUE DES TILLEULS 69003 LYON")
    _t(pdf, 18, 291, f"ARTICLE II - CARACTÉRISTIQUES DU (DES) PRÊT(S) N° {spec.project}")
    _t(pdf, 14, 312, "Objet du financement : Achat ancien"); _t(pdf, 14, 322, "Destination : Résidence principale")
    _t(pdf, 14, 357, f"Prêt : {spec.product} N°{spec.reference}")
    _t(pdf, 18, 369, "Prêteur : EXEMPLE BANQUE"); _t(pdf, 211, 369, f"Montant (euros) : {money(spec.principal)}"); _t(pdf, 391, 369, f"Durée : {spec.months} mensualités")
    _t(pdf, 18, 383, f"Taux de base (% fixe) : {str(spec.rate).replace('.', ',')}"); _t(pdf, 211, 383, f"TAEG (%) : {str(spec.taeg).replace('.', ',')}")
    _t(pdf, 18, 413, f"Coût total du prêt (euros) : {money(total_cost)}"); _t(pdf, 211 + 90, 413, f"Dont : Intérêts (euros) : {money(announced)}")
    _t(pdf, 211, 429, "Frais de dossier (euros) : 0,00"); _t(pdf, 211, 444, "Frais de courtage (euros) : 0,00")
    _t(pdf, 211, 459, f"Assurance obligatoire (euros) : {money(mandatory)}")
    _t(pdf, 211, 473, f"Frais de garantie (cf. art II des Conditions générales) (euros) : {money(spec.guarantee_fees)}")
    _t(pdf, 14, 492, f"Coût des assurances facultatives : {money(optional)} euros")
    pdf.add_page(); pdf.set_font("Helvetica", size=9)
    _t(pdf, 32, 39, "TABLEAU D'AMORTISSEMENT")
    _t(pdf, 32, 75, f"EMPRUNTEUR {spec.borrower}  CO-EMPRUNTEUR")
    _t(pdf, 32, 93, f"PROJET N°{spec.project}  RÉFÉRENCE PRÊT {spec.reference}")
    _t(pdf, 43, 133, "N° Amortissement Intérêts Intérêts Assurances Total à payer Montant restant dû")
    _t(pdf, 96, 145, "Capital normaux différés après échéance")
    shown = [Line(x.capital, x.interest, x.insurance, x.total, x.remaining, None, x.n) for x in lines]
    if spec.broken_row:
        shown[spec.broken_row - 1].total += D("1.00")
    _rows(pdf, shown, dated=False, top=160)
    _t(pdf, 74, 810, f"{spec.brand} est une marque commerciale d'un établissement de test.")
    return bytes(pdf.output())


def prepayment_pdf(spec: LoanSpec, title: str = "") -> bytes:
    pdf = _pdf(title or spec.title)
    _t(pdf, 332, 171, f"M {spec.borrower}")
    _t(pdf, 461, 272, "Paris, le 08 Juin 2026")
    _t(pdf, 54, 291, f"L'(Les) Emprunteur(s) : {spec.borrower}")
    _t(pdf, 90, 338, f"Cher Monsieur {spec.borrower},")
    _t(pdf, 107, 372, "Suite à votre demande, nous vous informons avoir procédé au remboursement anticipé partiel de")
    _t(pdf, 107, 384, f"votre prêt. Vous trouverez ci-dessous le décompte arrêté au {spec.repayment_day.strftime('%d/%m/%Y')}.")
    _t(pdf, 107, 407, "Situation de votre prêt")
    _t(pdf, 107, 430, "AVANT REMBOURSEMENT ANTICIPE")
    _t(pdf, 112, 455, "Capital restant dû"); _t(pdf, 0, 455, money(spec.capital_before) + " €", right=546)
    _t(pdf, 112, 474, f"Dernière échéance au {spec.last_due_before.strftime('%d/%m/%Y')} (*)"); _t(pdf, 0, 474, money(spec.payment) + " €", right=546)
    _t(pdf, 112, 493, "Date de fin du prêt"); _t(pdf, 495, 493, spec.end_before.strftime("%d/%m/%Y"))
    _t(pdf, 108, 523, "APRES REMBOURSEMENT ANTICIPE")
    _t(pdf, 112, 552, "Capital restant dû"); _t(pdf, 0, 552, money(spec.capital_after) + " €", right=546)
    first_after = month_add(spec.last_due_before, 1)
    _t(pdf, 112, 571, f"Première échéance au {first_after.strftime('%d/%m/%Y')} (*)"); _t(pdf, 0, 571, money(spec.new_payment) + " €", right=546)
    _t(pdf, 112, 589, "Date de fin du prêt"); _t(pdf, 495, 589, spec.end_after.strftime("%d/%m/%Y"))
    _t(pdf, 108, 610, "(*) Assurances comprises.")
    _t(pdf, 469, 675, f"L'équipe {spec.brand}")
    _t(pdf, 144, 809, f"{spec.brand} est une marque commerciale d'un établissement de test.")
    return bytes(pdf.output())


def schedule_pdf(spec: LoanSpec, title: str = "") -> bytes:
    pdf = _pdf(title or spec.title)
    lines = spec.schedule
    balance = lines[0].remaining + lines[0].capital
    _t(pdf, 241, 29, "TABLEAU"); _t(pdf, 180, 56, "D'AMORTISSEMENT")
    _t(pdf, 363, 774, f"{spec.brand} est une marque commerciale d'un établissement de test.")
    _t(pdf, 45, 113, "VOS DONNÉES PERSONNELLES")
    _t(pdf, 43, 134, f"Emprunteur : {spec.borrower}"); _t(pdf, 43, 145, "Co emprunteur :"); _t(pdf, 43, 156, "Identifiant : 12345678")
    _t(pdf, 45, 176, "VOTRE PROJET")
    _t(pdf, 43, 197, f"Projet ACHAT D'ANCIEN SEUL N° {spec.project}")
    _t(pdf, 43, 208, f"Montant du financement {money(spec.principal)} €")
    _t(pdf, 43, 220, f"Date prévisionnelle de fin du prêt {spec.end_after.strftime('%d/%m/%Y')}")
    _t(pdf, 45, 240, "LISTE DU (DES) PRÊT(S) DU PROJET")
    _t(pdf, 46, 264, "Prêt"); _t(pdf, 200, 264, "Montant (euros)"); _t(pdf, 290, 264, "Marché"); _t(pdf, 372, 264, "Taux (%)"); _t(pdf, 429, 264, "Durée"); _t(pdf, 475, 264, "N° prêt")
    _t(pdf, 46, 292, f"{spec.product.upper()} EXEMPLE"); _t(pdf, 200, 292, money(balance)); _t(pdf, 290, 292, "PRET"); _t(pdf, 372, 292, str(spec.rate).replace(".", ","))
    _t(pdf, 429, 292, str(spec.repaid_after + len(spec.new_plan))); _t(pdf, 475, 292, spec.reference); _t(pdf, 290, 303, "IMMOBILIER")
    pdf.add_page(); pdf.set_font("Helvetica", size=9)
    _t(pdf, 45, 40, "TABLEAU D'AMORTISSEMENT")
    _t(pdf, 46, 64, f"EMPRUNTEUR {spec.borrower} CO-EMPRUNTEUR")
    _t(pdf, 46, 79, f"PRÊT {spec.product.upper()} EXEMPLE"); _t(pdf, 399, 79, f"MONTANT (euros) {money(balance)}")
    _t(pdf, 46, 93, f"PROJET N°{spec.project} RÉFÉRENCE PRÊT {spec.reference}")
    _t(pdf, 46, 108, f"TAUX DE BASE (% fixe) {str(spec.rate).replace('.', ',')} DURÉE {spec.repaid_after + len(spec.new_plan)} termes PÉRIODICITÉ Mensuelle")
    _t(pdf, 62, 143, "Date Amortissement Intérêts Intérêts Assurances Total à payer Montant restant")
    _t(pdf, 46, 154, "d'échéance Capital normaux différés dû après")
    _rows(pdf, lines, dated=True, top=179)
    return bytes(pdf.output())
