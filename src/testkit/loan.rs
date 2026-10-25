//! Synthetic loan documents for the tests (port of python/tests/fixtures_loan.py): a made-up loan (amounts, number,
//! borrower) with the real documents' layout. From a `LoanSpec` the three documents are drawn: the offer
//! (conditions + numbered amortization table), the notice of a partial early repayment, and the dated amortization
//! table printed after the repayment. The schedules are computed here, independently of the code under test:
//! interest = capital x rate / 12 rounded to the cent, a constant payment, the last instalment clearing what is left.

use chrono::{Datelike, NaiveDate};
use rust_decimal::{Decimal, MathematicalOps, RoundingStrategy};

use super::fixtures::{dec, ymd};
use super::fortuneo::fr_space as money;
use super::pdf::Pdf;

pub const L_REF: &str = "FT99000002";
pub const L_PROJECT: &str = "FT99000001";
pub const L_BORROWER: &str = "DURAND PAUL";

pub fn q(x: Decimal) -> Decimal {
    x.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

pub fn month_add(day: NaiveDate, n: i32) -> NaiveDate {
    let index = day.year() * 12 + day.month0() as i32 + n;
    ymd(index.div_euclid(12), (index.rem_euclid(12) + 1) as u32, day.day())
}

#[derive(Clone, Debug)]
pub struct Line {
    pub capital: Decimal,
    pub interest: Decimal,
    pub insurance: Decimal,
    pub total: Decimal,
    pub remaining: Decimal,
    pub day: Option<NaiveDate>,
    pub n: Option<i32>,
}

pub fn amortize(mut capital: Decimal, rate: Decimal, payment: Decimal, insurance: Decimal, first_due: Option<NaiveDate>, start_n: i32) -> Vec<Line> {
    let mut lines = vec![];
    let mut k = 0;
    while capital > Decimal::ZERO {
        let interest = q(capital * rate / dec("1200"));
        let mut principal = payment - insurance - interest;
        if principal >= capital {
            principal = capital;
        }
        capital -= principal;
        lines.push(Line { capital: principal, interest, insurance, total: principal + interest + insurance, remaining: capital, day: first_due.map(|d| month_add(d, k)), n: Some(start_n + k) });
        k += 1;
    }
    lines
}

#[derive(Clone)]
pub struct LoanSpec {
    pub principal: Decimal,
    pub rate: Decimal,
    pub taeg: Decimal,
    pub months: i32,
    pub first_due: NaiveDate,
    pub insurance: Decimal,
    pub insurance_optional_share: Decimal, // of the insurance, the part that is not mandatory
    pub guarantee_fees: Decimal,
    pub reference: String,
    pub project: String,
    pub borrower: String,
    pub product: String,
    // The early repayment (partial): done after `repaid_after` instalments, for `repaid` euros.
    pub repaid_after: i32,
    pub repaid: Decimal,
    pub repayment_day: NaiveDate,
    pub new_payment_delta: Decimal, // the new instalment against the old one
    pub today: NaiveDate,
    pub wrong_interest_total: bool, // announce an interest total that the table does not reach
    pub broken_row: Option<usize>,  // corrupt this offer row (its total no longer adds up)
    pub brand: String,
    pub title: String,
}

impl LoanSpec {
    pub fn new() -> LoanSpec {
        LoanSpec {
            principal: dec("60000.00"),
            rate: dec("2.40"),
            taeg: dec("2.95"),
            months: 60,
            first_due: ymd(2025, 3, 15),
            insurance: dec("12.00"),
            insurance_optional_share: dec("0.75"),
            guarantee_fees: dec("900.00"),
            reference: L_REF.into(),
            project: L_PROJECT.into(),
            borrower: L_BORROWER.into(),
            product: "Fiximmo".into(),
            repaid_after: 15,
            repaid: dec("10000.00"),
            repayment_day: ymd(2026, 6, 6),
            new_payment_delta: dec("-0.50"),
            today: ymd(2026, 10, 24),
            wrong_interest_total: false,
            broken_row: None,
            brand: "Fortuneo".into(),
            title: String::new(),
        }
    }

    pub fn payment(&self) -> Decimal {
        let r = self.rate / dec("1200");
        let factor = Decimal::ONE / (Decimal::ONE + r).powi(self.months as i64);
        q(self.principal * r / (Decimal::ONE - factor)) + self.insurance
    }

    pub fn offer(&self) -> Vec<Line> {
        let mut lines = amortize(self.principal, self.rate, self.payment(), self.insurance, None, 1);
        // The bank spreads the rounding on the last instalment: it keeps the constant total.
        let payment = self.payment();
        let last = lines.last_mut().unwrap();
        last.interest = payment - self.insurance - last.capital;
        last.total = payment;
        lines
    }

    pub fn capital_before(&self) -> Decimal {
        self.offer()[self.repaid_after as usize - 1].remaining
    }

    pub fn capital_after(&self) -> Decimal {
        self.capital_before() - self.repaid
    }

    pub fn last_due_before(&self) -> NaiveDate {
        month_add(self.first_due, self.repaid_after - 1)
    }

    pub fn new_payment(&self) -> Decimal {
        self.payment() + self.new_payment_delta
    }

    /// Every instalment after the repayment, dated.
    pub fn new_plan(&self) -> Vec<Line> {
        amortize(self.capital_after(), self.rate, self.new_payment(), self.insurance, Some(month_add(self.last_due_before(), 1)), 1)
    }

    /// What the customer area prints on `today`: the instalments still to come.
    pub fn schedule(&self) -> Vec<Line> {
        self.new_plan().into_iter().filter(|x| x.day.unwrap() > self.today).collect()
    }

    pub fn end_before(&self) -> NaiveDate {
        month_add(self.first_due, self.months - 1)
    }

    pub fn end_after(&self) -> NaiveDate {
        self.new_plan().last().unwrap().day.unwrap()
    }
}

fn rows(pdf: &mut Pdf, lines: &[Line], dated: bool, top: f64) {
    let mut y = top;
    for line in lines {
        if y > 780.0 {
            pdf.add_page();
            pdf.text(43.0, 100.0, "N° Amortissement Intérêts Intérêts Assurances Total à payer Montant restant dû");
            pdf.text(96.0, 112.0, "Capital normaux différés après échéance");
            y = 140.0;
        }
        let head = if dated { line.day.unwrap().format("%d/%m/%Y").to_string() } else { line.n.unwrap().to_string() };
        pdf.text(46.0, y, &head);
        for (x, value) in [(140.0, line.capital), (210.0, line.interest), (270.0, Decimal::ZERO), (330.0, line.insurance), (400.0, line.total), (520.0, line.remaining)] {
            pdf.right(x + 40.0, y, &money(value));
        }
        y += 11.5;
    }
}

fn comma(d: Decimal) -> String {
    d.to_string().replace('.', ",")
}

pub fn offer_pdf(spec: &LoanSpec) -> Vec<u8> {
    let mut pdf = Pdf::new(&spec.title);
    let lines = spec.offer();
    let interest_total: Decimal = lines.iter().map(|x| x.interest).sum();
    let insurance_total: Decimal = lines.iter().map(|x| x.insurance).sum();
    let optional = q(insurance_total * spec.insurance_optional_share);
    let mandatory = insurance_total - optional;
    let total_cost = interest_total + mandatory + spec.guarantee_fees;
    let announced = interest_total + if spec.wrong_interest_total { dec("5.00") } else { Decimal::ZERO };
    pdf.text(186.0, 54.0, "OFFRE DE PRÊT(S)");
    pdf.text(211.0, 81.0, "IMMOBILIER(S)");
    pdf.text(14.0, 780.0, &format!("{} Emprunteur est un contrat d'assurance de groupe.", spec.brand));
    pdf.text(14.0, 133.0, "Les Conditions particulières et générales ci-dessous constituent l'offre de prêt immobilier.");
    pdf.text(18.0, 164.0, "CONDITIONS PARTICULIÈRES");
    pdf.text(14.0, 212.0, "Le Prêteur : (ci-après désigné « le Prêteur »)");
    pdf.text(14.0, 221.0, "EXEMPLE BANQUE, SOCIETE ANONYME, 1 PLACE DU TEST, 75000 PARIS");
    pdf.text(14.0, 238.0, "Le(s) Emprunteur(s) : (ci-après désigné « l'Emprunteur »)");
    pdf.text(14.0, 255.0, &format!("M. {} (12345678) né(e) le 01/01/1980 à Testville (00) et demeurant 8 RUE DES TILLEULS 69003 LYON", spec.borrower));
    pdf.text(18.0, 291.0, &format!("ARTICLE II - CARACTÉRISTIQUES DU (DES) PRÊT(S) N° {}", spec.project));
    pdf.text(14.0, 312.0, "Objet du financement : Achat ancien");
    pdf.text(14.0, 322.0, "Destination : Résidence principale");
    pdf.text(14.0, 357.0, &format!("Prêt : {} N°{}", spec.product, spec.reference));
    pdf.text(18.0, 369.0, "Prêteur : EXEMPLE BANQUE");
    pdf.text(211.0, 369.0, &format!("Montant (euros) : {}", money(spec.principal)));
    pdf.text(391.0, 369.0, &format!("Durée : {} mensualités", spec.months));
    pdf.text(18.0, 383.0, &format!("Taux de base (% fixe) : {}", comma(spec.rate)));
    pdf.text(211.0, 383.0, &format!("TAEG (%) : {}", comma(spec.taeg)));
    pdf.text(18.0, 413.0, &format!("Coût total du prêt (euros) : {}", money(total_cost)));
    pdf.text(211.0 + 90.0, 413.0, &format!("Dont : Intérêts (euros) : {}", money(announced)));
    pdf.text(211.0, 429.0, "Frais de dossier (euros) : 0,00");
    pdf.text(211.0, 444.0, "Frais de courtage (euros) : 0,00");
    pdf.text(211.0, 459.0, &format!("Assurance obligatoire (euros) : {}", money(mandatory)));
    pdf.text(211.0, 473.0, &format!("Frais de garantie (cf. art II des Conditions générales) (euros) : {}", money(spec.guarantee_fees)));
    pdf.text(14.0, 492.0, &format!("Coût des assurances facultatives : {} euros", money(optional)));
    pdf.add_page();
    pdf.text(32.0, 39.0, "TABLEAU D'AMORTISSEMENT");
    pdf.text(32.0, 75.0, &format!("EMPRUNTEUR {}  CO-EMPRUNTEUR", spec.borrower));
    pdf.text(32.0, 93.0, &format!("PROJET N°{}  RÉFÉRENCE PRÊT {}", spec.project, spec.reference));
    pdf.text(43.0, 133.0, "N° Amortissement Intérêts Intérêts Assurances Total à payer Montant restant dû");
    pdf.text(96.0, 145.0, "Capital normaux différés après échéance");
    let mut shown: Vec<Line> = lines.iter().map(|x| Line { day: None, ..x.clone() }).collect();
    if let Some(row) = spec.broken_row {
        shown[row - 1].total += dec("1.00");
    }
    rows(&mut pdf, &shown, false, 160.0);
    pdf.text(74.0, 810.0, &format!("{} est une marque commerciale d'un établissement de test.", spec.brand));
    pdf.bytes()
}

pub fn prepayment_pdf(spec: &LoanSpec, title: Option<&str>) -> Vec<u8> {
    let mut pdf = Pdf::new(title.unwrap_or(&spec.title));
    let eur = |d: Decimal| format!("{} €", money(d));
    pdf.text(332.0, 171.0, &format!("M {}", spec.borrower));
    pdf.text(461.0, 272.0, "Paris, le 08 Juin 2026");
    pdf.text(54.0, 291.0, &format!("L'(Les) Emprunteur(s) : {}", spec.borrower));
    pdf.text(90.0, 338.0, &format!("Cher Monsieur {},", spec.borrower));
    pdf.text(107.0, 372.0, "Suite à votre demande, nous vous informons avoir procédé au remboursement anticipé partiel de");
    pdf.text(107.0, 384.0, &format!("votre prêt. Vous trouverez ci-dessous le décompte arrêté au {}.", spec.repayment_day.format("%d/%m/%Y")));
    pdf.text(107.0, 407.0, "Situation de votre prêt");
    pdf.text(107.0, 430.0, "AVANT REMBOURSEMENT ANTICIPE");
    pdf.text(112.0, 455.0, "Capital restant dû");
    pdf.right(546.0, 455.0, &eur(spec.capital_before()));
    pdf.text(112.0, 474.0, &format!("Dernière échéance au {} (*)", spec.last_due_before().format("%d/%m/%Y")));
    pdf.right(546.0, 474.0, &eur(spec.payment()));
    pdf.text(112.0, 493.0, "Date de fin du prêt");
    pdf.text(495.0, 493.0, &spec.end_before().format("%d/%m/%Y").to_string());
    pdf.text(108.0, 523.0, "APRES REMBOURSEMENT ANTICIPE");
    pdf.text(112.0, 552.0, "Capital restant dû");
    pdf.right(546.0, 552.0, &eur(spec.capital_after()));
    let first_after = month_add(spec.last_due_before(), 1);
    pdf.text(112.0, 571.0, &format!("Première échéance au {} (*)", first_after.format("%d/%m/%Y")));
    pdf.right(546.0, 571.0, &eur(spec.new_payment()));
    pdf.text(112.0, 589.0, "Date de fin du prêt");
    pdf.text(495.0, 589.0, &spec.end_after().format("%d/%m/%Y").to_string());
    pdf.text(108.0, 610.0, "(*) Assurances comprises.");
    pdf.text(469.0, 675.0, &format!("L'équipe {}", spec.brand));
    pdf.text(144.0, 809.0, &format!("{} est une marque commerciale d'un établissement de test.", spec.brand));
    pdf.bytes()
}

pub fn schedule_pdf(spec: &LoanSpec, title: Option<&str>) -> Vec<u8> {
    let mut pdf = Pdf::new(title.unwrap_or(&spec.title));
    let lines = spec.schedule();
    let balance = lines[0].remaining + lines[0].capital;
    let terms = spec.repaid_after as usize + spec.new_plan().len();
    pdf.text(241.0, 29.0, "TABLEAU");
    pdf.text(180.0, 56.0, "D'AMORTISSEMENT");
    pdf.text(363.0, 774.0, &format!("{} est une marque commerciale d'un établissement de test.", spec.brand));
    pdf.text(45.0, 113.0, "VOS DONNÉES PERSONNELLES");
    pdf.text(43.0, 134.0, &format!("Emprunteur : {}", spec.borrower));
    pdf.text(43.0, 145.0, "Co emprunteur :");
    pdf.text(43.0, 156.0, "Identifiant : 12345678");
    pdf.text(45.0, 176.0, "VOTRE PROJET");
    pdf.text(43.0, 197.0, &format!("Projet ACHAT D'ANCIEN SEUL N° {}", spec.project));
    pdf.text(43.0, 208.0, &format!("Montant du financement {} €", money(spec.principal)));
    pdf.text(43.0, 220.0, &format!("Date prévisionnelle de fin du prêt {}", spec.end_after().format("%d/%m/%Y")));
    pdf.text(45.0, 240.0, "LISTE DU (DES) PRÊT(S) DU PROJET");
    for (x, t) in [(46.0, "Prêt"), (200.0, "Montant (euros)"), (290.0, "Marché"), (372.0, "Taux (%)"), (429.0, "Durée"), (475.0, "N° prêt")] {
        pdf.text(x, 264.0, t);
    }
    pdf.text(46.0, 292.0, &format!("{} EXEMPLE", spec.product.to_uppercase()));
    pdf.text(200.0, 292.0, &money(balance));
    pdf.text(290.0, 292.0, "PRET");
    pdf.text(372.0, 292.0, &comma(spec.rate));
    pdf.text(429.0, 292.0, &terms.to_string());
    pdf.text(475.0, 292.0, &spec.reference);
    pdf.text(290.0, 303.0, "IMMOBILIER");
    pdf.add_page();
    pdf.text(45.0, 40.0, "TABLEAU D'AMORTISSEMENT");
    pdf.text(46.0, 64.0, &format!("EMPRUNTEUR {} CO-EMPRUNTEUR", spec.borrower));
    pdf.text(46.0, 79.0, &format!("PRÊT {} EXEMPLE", spec.product.to_uppercase()));
    pdf.text(399.0, 79.0, &format!("MONTANT (euros) {}", money(balance)));
    pdf.text(46.0, 93.0, &format!("PROJET N°{} RÉFÉRENCE PRÊT {}", spec.project, spec.reference));
    pdf.text(46.0, 108.0, &format!("TAUX DE BASE (% fixe) {} DURÉE {terms} termes PÉRIODICITÉ Mensuelle", comma(spec.rate)));
    pdf.text(62.0, 143.0, "Date Amortissement Intérêts Intérêts Assurances Total à payer Montant restant");
    pdf.text(46.0, 154.0, "d'échéance Capital normaux différés dû après");
    rows(&mut pdf, &lines, true, 179.0);
    pdf.bytes()
}
