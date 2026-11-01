//! Synthetic Hello bank! documents for the tests: made-up holder, address, IBAN and amounts, the geometry of the real
//! "Relevé de compte chèques" (a BNP Paribas group layout, the first name printed before the last name).

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::fixtures::{dec, make_iban, rib_key, ymd};
use super::fortuneo::fr_space;
use super::pdf::Pdf;

pub const H_BANK: &str = "30004";
pub const H_BRANCH: &str = "00999";
pub const H_ACCOUNT: &str = "00012345678";
/// As the bank prints it: first name, then last name.
pub const H_PRINTED_HOLDER: &str = "M PAUL MARTIN";
/// As the application keeps it: last name, then first name.
pub const H_HOLDER: &str = "M MARTIN PAUL";

#[derive(Clone)]
pub struct HEntry {
    pub day: NaiveDate,
    pub value_day: Option<NaiveDate>,
    pub lines: Vec<String>,
    pub amount: Decimal,
}

pub fn hentry(day: NaiveDate, lines: &[&str], amount: &str) -> HEntry {
    HEntry { day, value_day: None, lines: lines.iter().map(|s| s.to_string()).collect(), amount: dec(amount) }
}

#[derive(Clone)]
pub struct HSpec {
    pub entries: Vec<HEntry>,
    pub opening: Decimal,
    pub opening_on: NaiveDate,
    pub period_end: NaiveDate,
    pub holder: String,
    pub account: String,
    pub wrong_totals: bool,
    pub title: String,
}

impl HSpec {
    pub fn new(entries: Vec<HEntry>) -> HSpec {
        HSpec {
            entries,
            opening: dec("500.00"),
            opening_on: ymd(2026, 9, 22),
            period_end: ymd(2026, 10, 15),
            holder: H_PRINTED_HOLDER.into(),
            account: H_ACCOUNT.into(),
            wrong_totals: false,
            title: String::new(),
        }
    }

    pub fn closing(&self) -> Decimal {
        self.opening + self.entries.iter().map(|e| e.amount).sum::<Decimal>()
    }
}

fn dotted(d: NaiveDate) -> String {
    d.format("%d.%m.%Y").to_string()
}

fn long_french(d: NaiveDate) -> String {
    const MONTHS: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
    format!("{} {} {}", d.format("%-d"), MONTHS[d.format("%m").to_string().parse::<usize>().unwrap() - 1], d.format("%Y"))
}

pub fn hello_statement_pdf(spec: &HSpec) -> Vec<u8> {
    let mut pdf = Pdf::new(&spec.title);
    let key = rib_key(H_BANK, H_BRANCH, &spec.account);
    let iban = make_iban(&format!("{H_BANK}{H_BRANCH}{}{key}", spec.account), "FR");
    let iban_spaced = iban.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ");
    pdf.text(205.0, 20.0, "RELEVE DE COMPTE CHEQUES");
    pdf.text(523.0, 27.0, "P. 1/1");
    pdf.text(264.0, 32.0, &format!("du {} au {}", long_french(spec.opening_on.succ_opt().unwrap()), long_french(spec.period_end)));
    pdf.text(354.0, 80.0, "03101 03166");
    pdf.text(17.0, 87.0, "Hello bank");
    pdf.text(17.0, 100.0, "Tél (1) : 01 00 00 00 00 (numéro non surtaxé)");
    pdf.text(17.0, 151.0, &format!("RIB : {H_BANK} {H_BRANCH} {} {key}", spec.account));
    pdf.text(354.0, 152.0, &spec.holder);
    pdf.text(17.0, 163.0, &format!("IBAN : {iban_spaced}"));
    pdf.text(354.0, 163.0, "8 RUE DES TILLEULS");
    pdf.text(17.0, 173.0, "BIC : BNPAFRPPXXX");
    pdf.text(354.0, 175.0, "69003 LYON");
    pdf.text(79.0, 240.0, "Vous trouverez les modifications des conditions tarifaires des produits et services liés à votre compte de dépôt.");
    pdf.text(79.0, 354.0, "Les sommes déposées sur ce compte sont éligibles à la garantie des dépôts du Fonds de Garantie des Dépôts.");
    pdf.text(450.0, 387.0, "Monnaie du compte : EURO");
    pdf.text(31.0, 406.0, "Date");
    pdf.text(316.0, 406.0, "Valeur");
    pdf.text(74.0, 407.0, "Nature des opérations");
    pdf.right(451.0, 407.0, "Débit");
    pdf.right(546.0, 407.0, "Crédit");

    let side = |amount: Decimal| if amount < Decimal::ZERO { 451.0 } else { 546.0 };
    let mut y = 426.0;
    let opening_word = if spec.opening >= Decimal::ZERO { "CREDITEUR" } else { "DEBITEUR" };
    pdf.text(74.0, y, &format!("SOLDE {opening_word} AU {}", dotted(spec.opening_on)));
    pdf.right(side(spec.opening), y, &fr_space(spec.opening.abs()));
    y += 15.0;
    for e in &spec.entries {
        pdf.text(31.0, y, &e.day.format("%d.%m").to_string());
        pdf.text(74.0, y, &e.lines[0]);
        pdf.text(320.0, y, &e.value_day.unwrap_or(e.day).format("%d.%m").to_string());
        pdf.right(side(e.amount), y, &fr_space(e.amount.abs()));
        for extra in &e.lines[1..] {
            y += 11.0;
            pdf.text(74.0, y, extra);
        }
        y += 15.0;
    }
    let mut debits: Decimal = spec.entries.iter().filter(|e| e.amount < Decimal::ZERO).map(|e| -e.amount).sum();
    let credits: Decimal = spec.entries.iter().filter(|e| e.amount > Decimal::ZERO).map(|e| e.amount).sum();
    if spec.wrong_totals {
        debits += dec("1.00");
    }
    y += 5.0;
    pdf.text(74.0, y, "TOTAL DES OPERATIONS");
    pdf.right(452.0, y, &fr_space(debits));
    pdf.right(546.0, y, &fr_space(credits));
    y += 17.0;
    let closing = spec.closing();
    let closing_word = if closing >= Decimal::ZERO { "CREDITEUR" } else { "DEBITEUR" };
    pdf.text(74.0, y, &format!("SOLDE {closing_word} AU {}", dotted(spec.period_end)));
    pdf.right(side(closing), y, &fr_space(closing.abs()));
    y += 22.0;
    pdf.text(40.0, y, &format!("Montant de votre autorisation de découvert en compte au {} : 0,00 Euro.", dotted(spec.period_end)));
    pdf.text(11.0, 794.0, "Hello bank! est une marque de BNP PARIBAS SA au capital de 2 000 000 000 € - Siège social fictif.");
    pdf.bytes()
}

/// A month of everything the statement can show: a credit, a card payment, a direct debit, a transfer to someone,
/// a transfer between the holder's own accounts, a cheque, a fee, and a long label on two lines.
pub fn hello_entries() -> Vec<HEntry> {
    let e = hentry;
    vec![
        e(ymd(2026, 9, 25), &["VIR SEPA RECU /FRM SOCIETE EXEMPLE SAS", "/MOTIF SALAIRE SEPTEMBRE /REF 20260925A"], "1800.00"),
        e(ymd(2026, 9, 28), &["CARTE X1234 27/09 BOULANGERIE TEST LYON"], "-3.50"),
        e(ymd(2026, 10, 1), &["PRLV SEPA OPERATEUR MOBILE EXEMPLE", "/MDT 000111 /MOTIF FACTURE OCTOBRE", "FR12ZZZ123456"], "-19.99"),
        e(ymd(2026, 10, 2), &["VIR SEPA EMIS /TO LEA DURAND", "/MOTIF REMBOURSEMENT RESTAURANT"], "-25.00"),
        e(ymd(2026, 10, 5), &["VIR SEPA RECU /FRM M MARTIN PAUL", "/EID /RNF VIREMENT DE PAUL MARTIN"], "200.00"),
        e(ymd(2026, 10, 8), &["CHEQUE 0001234"], "-60.00"),
        e(ymd(2026, 10, 12), &["COTISATION CARTE VISA CLASSIC"], "-2.50"),
        e(ymd(2026, 10, 14), &["RETRAIT DAB 13/10 AGENCE EXEMPLE"], "-40.00"),
    ]
}
