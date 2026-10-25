//! Synthetic BoursoBank documents for the tests (port of python/tests/fixtures.py). Everything here is made up: no
//! real name, address, IBAN or amount. `boursobank_statement_pdf` draws a statement with the same layout as a real
//! monthly extract, so the adapter is exercised on realistic geometry without a single personal byte.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::str::FromStr;

use super::pdf::Pdf;
use crate::adapters::common::rib_key as rib_key_of;

pub fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

pub fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

pub fn rib_key(bank: &str, branch: &str, account: &str) -> String {
    rib_key_of(bank, branch, account).unwrap()
}

pub const BANK: &str = "12345";
pub const BRANCH: &str = "67890";
pub const ACCOUNT: &str = "00012345678";

pub fn holder() -> Vec<String> {
    vec!["M DUPONT JEAN".into()]
}

pub fn address() -> Vec<String> {
    vec!["12 RUE DES LILAS".into(), "75011 PARIS".into()]
}

/// A joint account: the bank cuts the holder line at about 29 characters, and the address has a second line.
pub fn joint_holder() -> Vec<String> {
    vec!["M DUPONT OU MME LEROY CLAIRE".into()]
}

pub fn joint_address() -> Vec<String> {
    vec!["12 RUE DES LILAS".into(), "BAT B APPT 12".into(), "75011 PARIS".into()]
}

pub fn make_iban(bban: &str, country: &str) -> String {
    let mut digits = String::new();
    for c in format!("{bban}{country}00").chars() {
        digits.push_str(&c.to_digit(36).unwrap().to_string());
    }
    let rem = digits.bytes().fold(0u64, |r, b| (r * 10 + (b - b'0') as u64) % 97);
    format!("{country}{:02}{bban}", 98 - rem)
}

/// A valid IBAN that belongs to nobody (other bank code, account derived from the seed).
pub fn fake_iban(seed: u64) -> String {
    let account = format!("{}", 90000000000u64 + seed);
    make_iban(&format!("20041{}{account}{}", "01005", rib_key("20041", "01005", &account)), "FR")
}

pub fn iban() -> String {
    make_iban(&format!("{BANK}{BRANCH}{ACCOUNT}{}", rib_key(BANK, BRANCH, ACCOUNT)), "FR")
}

#[derive(Clone)]
pub struct Entry {
    pub day: NaiveDate,
    pub lines: Vec<String>, // first line, then the detail lines
    pub amount: Decimal,    // negative = debit
    pub value_day: Option<NaiveDate>,
}

pub fn entry(day: NaiveDate, lines: &[&str], amount: &str) -> Entry {
    Entry { day, lines: lines.iter().map(|s| s.to_string()).collect(), amount: dec(amount), value_day: None }
}

#[derive(Clone)]
pub struct Spec {
    pub entries: Vec<Entry>,
    pub opening: Decimal,
    pub opening_date: NaiveDate,
    pub period: (NaiveDate, NaiveDate),
    pub holder: Vec<String>,
    pub address: Vec<String>,
    pub bank: String,
    pub branch: String,
    pub account: String,
    pub garble: bool,                  // fonts whose accents come out as other glyphs when the text is extracted
    pub wrong_closing: Option<Decimal>, // a closing balance that does not match, to test the consistency check
    pub title: String,                 // document metadata only: gives the same statement a different file hash
    pub product: String,               // savings accounts print "Compte d'épargne : <product>" under the title
}

impl Spec {
    pub fn new(entries: Vec<Entry>) -> Spec {
        Spec {
            entries,
            opening: dec("1000.00"),
            opening_date: ymd(2026, 8, 31),
            period: (ymd(2026, 9, 1), ymd(2026, 9, 30)),
            holder: holder(),
            address: address(),
            bank: BANK.into(),
            branch: BRANCH.into(),
            account: ACCOUNT.into(),
            garble: false,
            wrong_closing: None,
            title: String::new(),
            product: String::new(),
        }
    }

    pub fn closing(&self) -> Decimal {
        self.opening + self.entries.iter().map(|e| e.amount).sum::<Decimal>()
    }
}

/// 1.234,56 : the dots separate the thousands.
pub fn fr(amount: Decimal) -> String {
    let s = format!("{:.2}", amount.abs());
    let (whole, cents) = s.split_once('.').unwrap();
    let mut whole = whole.to_string();
    let mut groups = vec![];
    while whole.len() > 3 {
        let tail = whole.split_off(whole.len() - 3);
        groups.insert(0, tail);
    }
    groups.insert(0, whole);
    format!("{},{}", groups.join("."), cents)
}

pub fn fdate(value: NaiveDate) -> String {
    value.format("%d/%m/%Y").to_string()
}

fn spaced(iban: &str) -> String {
    iban.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

fn garbled(text: &str) -> String {
    text.replace('é', "Ø").replace('è', "(cid:232)").replace('à', "(cid:224)").replace('°', "(cid:176)")
}

pub fn boursobank_statement_pdf(spec: &Spec) -> Vec<u8> {
    let mut pdf = Pdf::new(&spec.title);
    let key = rib_key(&spec.bank, &spec.branch, &spec.account);
    let iban = make_iban(&format!("{}{}{}{key}", spec.bank, spec.branch, spec.account), "FR");

    let txt = |pdf: &mut Pdf, x: f64, y: f64, text: &str| {
        let t = if spec.garble { garbled(text) } else { text.to_string() };
        pdf.text(x, y, &t);
    };
    let txt_right = |pdf: &mut Pdf, right: f64, y: f64, text: &str| {
        let t = if spec.garble { garbled(text) } else { text.to_string() };
        pdf.right(right, y, &t);
    };

    let header = |pdf: &mut Pdf, page: usize, total: usize, first: bool| -> f64 {
        if page > 1 {
            pdf.add_page();
        }
        let top = if first { 0.0 } else { -190.0 };
        if first {
            txt(pdf, 320.0, 40.0, "Extrait de votre compte en EUR");
            txt(pdf, 320.0, 52.0, "(Protégé par la Garantie des Dépôts www.garantiedesdepots.fr)");
            txt(pdf, 322.0, 62.0, "BOURSOBANK");
            if !spec.product.is_empty() {
                txt(pdf, 320.0, 74.0, &format!("Compte d'épargne : {}", spec.product));
            }
            let mut y = 198.0;
            for line in spec.holder.iter().chain(spec.address.iter()) {
                txt(pdf, 320.0, y, line);
                y += 12.0;
            }
        }
        let banner = 305.0 + top;
        for (x, t) in [(46.0, "Date"), (94.0, "N° de RIB"), (262.0, "Devise"), (293.0, "Période"), (412.0, "Montant DA*"), (472.0, "TAEG*"), (524.0, "Page")] {
            txt(pdf, x, banner, t);
        }
        for (x, t) in [(94.0, "Banque"), (128.0, "Guichet"), (162.0, "N° de compte"), (227.0, "Clé RIB")] {
            txt(pdf, x, banner + 12.0, t);
        }
        let row = banner + 23.0;
        txt(pdf, 46.0, row, &fdate(ymd(2026, 10, 1)));
        txt(pdf, 97.0, row, &spec.bank);
        txt(pdf, 131.0, row, &spec.branch);
        txt(pdf, 166.0, row, &spec.account);
        txt(pdf, 236.0, row, &key);
        txt(pdf, 266.0, row, "EUR");
        txt(pdf, 295.0, row, &format!("du {} au {}", fdate(spec.period.0), fdate(spec.period.1)));
        txt(pdf, 426.0, row, "0,00");
        txt(pdf, 473.0, row, "0,000000 %");
        txt(pdf, 531.0, row, &format!("{page}/{total}"));
        let bic_row = banner + 58.0;
        txt(pdf, 43.0, bic_row, "B.I.C.");
        txt(pdf, 78.0, bic_row, "BOUSFRPPXXX");
        txt(pdf, 180.0, bic_row, "I.B.A.N.");
        txt(pdf, 233.0, bic_row, &spaced(&iban));
        txt(pdf, 378.0, bic_row + 22.0, "MOUVEMENTS EN EUR");
        let head = bic_row + 45.0;
        txt(pdf, 46.0, head, "Date");
        txt(pdf, 95.0, head + 6.0, "Libellé");
        txt(pdf, 381.0, head + 6.0, "Valeur");
        txt(pdf, 442.0, head + 6.0, "Débit");
        txt(pdf, 506.0, head + 6.0, "Crédit");
        txt(pdf, 46.0, head + 12.0, "opération");
        head + 32.0
    };

    let footer = |pdf: &mut Pdf| {
        txt(pdf, 83.0, 748.0, "A réception d'un extrait de compte vous disposez d'un délai de quatre mois pour formuler toute réclamation");
        txt(pdf, 85.0, 773.0, "Boursorama - SA au capital de 53 576 889,20 - RCS Nanterre 351 058 151");
    };

    // Lay the entries out page by page (about 24 lines of 11.4 pt fit under the header).
    let mut pages: Vec<Vec<&Entry>> = vec![vec![]];
    let mut used = 3.0;
    for e in &spec.entries {
        let need = e.lines.len() as f64 + 0.4;
        if used + need > 24.0 && !pages.last().unwrap().is_empty() {
            pages.push(vec![]);
            used = 0.0;
        }
        pages.last_mut().unwrap().push(e);
        used += need;
    }
    let total = pages.len();

    for (index, entries) in pages.iter().enumerate() {
        let number = index + 1;
        let mut y = header(&mut pdf, number, total, number == 1);
        if number == 1 {
            txt(&mut pdf, 300.0, y, "SOLDE AU :");
            txt(&mut pdf, 375.0, y, &fdate(spec.opening_date));
            txt_right(&mut pdf, if spec.opening >= Decimal::ZERO { 546.0 } else { 480.0 }, y, &fr(spec.opening));
            y += 33.0;
        }
        for e in entries {
            txt(&mut pdf, 46.0, y, &fdate(e.day));
            txt(&mut pdf, 95.0, y, &e.lines[0]);
            txt(&mut pdf, 375.0, y, &fdate(e.value_day.unwrap_or(e.day)));
            txt_right(&mut pdf, if e.amount > Decimal::ZERO { 546.0 } else { 480.0 }, y, &fr(e.amount));
            for extra in &e.lines[1..] {
                y += 11.4;
                txt(&mut pdf, 112.0, y, extra);
            }
            y += 12.0;
        }
        if number == total {
            let y = 683.0;
            txt(&mut pdf, 145.0, y, "Nouveau solde en EUR :");
            let closing = spec.wrong_closing.unwrap_or_else(|| spec.closing());
            txt_right(&mut pdf, if closing >= Decimal::ZERO { 546.0 } else { 480.0 }, y, &fr(closing));
            txt(&mut pdf, 145.0, y + 14.0, "Montant frais bancaires* :");
        }
        footer(&mut pdf);
    }
    pdf.bytes()
}

/// One of each kind of line the BoursoBank statement has.
pub fn sample_entries() -> Vec<Entry> {
    let e = entry;
    vec![
        e(ymd(2026, 9, 1), &["VIR Virement depuis LIVRET DEVELOPPE", "MENT DURABLE SOLID", "DUPONT JEAN"], "150.00"),
        e(ymd(2026, 9, 1), &["VIR SEPA MARTIN Pauline", "cadeau anniversaire", "Rèf : SCT123450000000000000000001"], "-50.00"),
        e(ymd(2026, 9, 1), &["PRLV SEPA EAUX DU NORD", "Facture EAUX", "1234567DV57ABCDE", "RUM S-1-0000000001-000000000-01"], "-28.00"),
        e(ymd(2026, 9, 1), &["CARTE 31/08/26 TRAINS DE FRANCE CB*1234"], "-71.00"),
        e(ymd(2026, 9, 2), &["*COTIS ASS.COMPTE 09/2026", "ASSURANCE COMPTE STANDARD"], "-2.00"),
        e(ymd(2026, 9, 3), &["PRLV SEPA DIRECTION GENERALE DES FINANCE", "600231997960TLR2660421296524 TLRSEPA DGF", "IP IMPOT TF", "RUM NNFR46ZZZ005002266"], "-1396.00"),
        e(ymd(2026, 9, 4), &["CARTE 02/09/26 UBER *EATS CB*1234"], "-18.48"),
        e(ymd(2026, 9, 7), &["VIR SEPA Revolut", "achat recurrent crypto", "Rèf : SCT123450000000000000000002"], "-25.00"),
        e(ymd(2026, 9, 7), &["ECH PRET:80000000000000000001"], "-166.67"),
        e(ymd(2026, 9, 8), &["VIR SEPA ACME CONSULTING SAS", "Salaire septembre", "Réf 03620N0200000000000000"], "2400.00"),
        e(ymd(2026, 9, 9), &["VIR INST DUPONT JEAN", "Virement depuis BoursoBank"], "-200.00"),
        e(ymd(2026, 9, 10), &["VIR Virement de Jean DUPONT", "MME LEROY CLAIRE", "Rèf : SCT123450000000000000000003"], "-35.00"),
        Entry { value_day: Some(ymd(2026, 9, 13)), ..e(ymd(2026, 9, 12), &["VIR Virement depuis BOURSORAMA Claire", "LEROY CLAIRE"], "60.00") },
        e(ymd(2026, 9, 14), &["CARTE 12/09/26 CARREFOUR CITY CB*1234"], "-14.08"),
    ]
}

pub fn september() -> Spec {
    Spec::new(sample_entries())
}

#[derive(Clone)]
pub struct SavingsSpec {
    pub entries: Vec<Entry>,
    pub opening: Decimal,
    pub period: (NaiveDate, NaiveDate),
    pub holder: Vec<String>, // savings statements print no civility
    pub address: Vec<String>,
    pub account: String,
    pub product: String,
    pub title: String,
}

impl SavingsSpec {
    pub fn new(entries: Vec<Entry>) -> SavingsSpec {
        SavingsSpec {
            entries,
            opening: dec("3000.00"),
            period: (ymd(2026, 9, 1), ymd(2026, 9, 30)),
            holder: vec!["DUPONT JEAN".into()],
            address: address(),
            account: "00099999999".into(),
            product: "Plan Epargne Logement".into(),
            title: String::new(),
        }
    }

    pub fn closing(&self) -> Decimal {
        self.opening + self.entries.iter().map(|e| e.amount).sum::<Decimal>()
    }
}

/// A savings extract (PEL...): product in the title, plain account number (no RIB, no IBAN), one page.
pub fn boursobank_savings_pdf(spec: &SavingsSpec) -> Vec<u8> {
    let mut pdf = Pdf::new(&spec.title);
    pdf.text(288.0, 48.0, &format!("Extrait de votre {}", spec.product));
    pdf.text(288.0, 62.0, "(Protégé par la Garantie des Dépôts www.garantiedesdepots.fr)");
    let mut y = 198.0;
    for line in spec.holder.iter().chain(spec.address.iter()) {
        pdf.text(320.0, y, line);
        y += 12.0;
    }
    for (x, t) in [(46.0, "Date"), (124.0, "Numéro de compte"), (273.0, "Devise"), (319.0, "Période"), (514.0, "Page")] {
        pdf.text(x, 318.0, t);
    }
    pdf.text(59.0, 339.0, &fdate(ymd(2026, 10, 1)));
    pdf.text(167.0, 339.0, &spec.account);
    pdf.text(284.0, 339.0, "EUR");
    pdf.text(359.0, 339.0, &format!("du {} au {}", fdate(spec.period.0), fdate(spec.period.1)));
    pdf.text(531.0, 339.0, "1");
    pdf.text(433.0, 353.0, "MOUVEMENTS EN EUR");
    for (x, t) in [(46.0, "Date"), (68.0, "opération"), (201.0, "Libellé"), (343.0, "Valeur"), (423.0, "Débit"), (501.0, "Crédit")] {
        pdf.text(x, 370.0, t);
    }
    pdf.text(231.0, 390.0, "SOLDE EN EUR");
    pdf.text(295.0, 390.0, "AU :");
    pdf.text(336.0, 390.0, &fdate(spec.period.0));
    pdf.right(544.0, 390.0, &fr(spec.opening));
    let mut y = 426.0;
    for e in &spec.entries {
        pdf.text(54.0, y, &fdate(e.day));
        pdf.text(115.0, y, &e.lines[0]);
        pdf.text(336.0, y, &fdate(e.value_day.unwrap_or(e.day)));
        pdf.right(if e.amount > Decimal::ZERO { 544.0 } else { 465.0 }, y, &fr(e.amount));
        for extra in &e.lines[1..] {
            y += 11.4;
            pdf.text(115.0, y, extra);
        }
        y += 14.0;
    }
    pdf.text(204.0, 677.0, "Nouveau solde en EUR");
    pdf.right(545.0, 677.0, &fr(spec.closing()));
    pdf.text(100.0, 748.0, "A réception d'un extrait de compte vous disposez d'un délai d'un mois pour formuler toute réclamation");
    pdf.text(85.0, 773.0, "Boursorama - SA au capital de 53 576 889,20 - RCS Nanterre 351 058 151");
    pdf.bytes()
}
