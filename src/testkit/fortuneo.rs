//! Synthetic Fortuneo documents for the tests (port of python/tests/fixtures_fortuneo.py): made-up holder, address,
//! IBAN and amounts, real-looking geometry. Three documents are drawn: the monthly "Relevé de Compte", the yearly
//! "Relevé de frais" and the FGDR letter, plus an insurer's arbitrage confirmation.

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;

use super::fixtures::{dec, fr, make_iban, rib_key, ymd};
use super::pdf::Pdf;

pub const F_BANK: &str = "30002";
pub const F_BRANCH: &str = "00999";
pub const F_ACCOUNT: &str = "00098765432";
pub const F_HOLDER: &str = "M MARTIN PAUL";

pub fn f_address() -> Vec<String> {
    vec!["8 RUE DES TILLEULS".into(), "69003 LYON".into()]
}

pub fn f_iban() -> String {
    make_iban(&format!("{F_BANK}{F_BRANCH}{F_ACCOUNT}{}", rib_key(F_BANK, F_BRANCH, F_ACCOUNT)), "FR")
}

const MONTHS: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];

/// Fortuneo prints thousands with a space ("1 500,00"), unlike the dots of the other banks.
pub fn fr_space(amount: Decimal) -> String {
    fr(amount).replace('.', " ")
}

pub fn long_date(value: NaiveDate) -> String {
    format!("{} {} {}", value.day(), MONTHS[value.month0() as usize], value.year())
}

#[derive(Clone)]
pub struct FEntry {
    pub day: NaiveDate,
    pub lines: Vec<String>,
    pub amount: Decimal,
    pub value_day: Option<NaiveDate>,
}

pub fn fentry(day: NaiveDate, lines: &[&str], amount: &str) -> FEntry {
    FEntry { day, lines: lines.iter().map(|s| s.to_string()).collect(), amount: dec(amount), value_day: None }
}

#[derive(Clone)]
pub struct FSpec {
    pub entries: Vec<FEntry>,
    pub opening: Decimal,
    pub period_end: NaiveDate,
    pub number: String,
    pub holder: String,
    pub address: Vec<String>,
    pub bank: String,
    pub branch: String,
    pub account: String,
    pub brand: String,
    pub wrong_totals: bool,
    pub wrong_closing: Option<Decimal>,
    pub title: String,
    pub livret: bool,             // a savings book: "Relevé Livret +", "Compte <client> 01", no RIB and no IBAN
    pub opening_on: Option<NaiveDate>, // date of the "ancien solde" (default: the last day of the month before the period)
    pub first: bool,              // the account's first statement: "ANCIEN SOLDE 0,00 €" with no date and no side
}

impl FSpec {
    pub fn new(entries: Vec<FEntry>) -> FSpec {
        FSpec {
            entries,
            opening: dec("500.00"),
            period_end: ymd(2026, 9, 30),
            number: "9 - 2026".into(),
            holder: F_HOLDER.into(),
            address: f_address(),
            bank: F_BANK.into(),
            branch: F_BRANCH.into(),
            account: F_ACCOUNT.into(),
            brand: "FORTUNEO".into(),
            wrong_totals: false,
            wrong_closing: None,
            title: String::new(),
            livret: false,
            opening_on: None,
            first: false,
        }
    }

    pub fn closing(&self) -> Decimal {
        self.opening + self.entries.iter().map(|e| e.amount).sum::<Decimal>()
    }
}

fn iban_line(iban: &str) -> String {
    iban.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

fn bank_block(pdf: &mut Pdf, brand: &str, y: f64) {
    pdf.text(45.0, y, brand);
    pdf.text(45.0, y + 9.0, "TOUR EXEMPLE");
    pdf.text(45.0, y + 19.0, "1 PLACE DU TEST");
    pdf.text(45.0, y + 28.0, "75000 PARIS");
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

pub fn fortuneo_statement_pdf(spec: &FSpec) -> Vec<u8> {
    let mut pdf = Pdf::new(&spec.title);
    let end = spec.period_end;
    let key = rib_key(&spec.bank, &spec.branch, &spec.account);
    let iban = make_iban(&format!("{}{}{}{key}", spec.bank, spec.branch, spec.account), "FR");
    let opening_date = spec.opening_on.unwrap_or_else(|| ymd(end.year(), end.month(), 1).pred_opt().unwrap());
    let holder_chars: Vec<char> = spec.holder.chars().collect();
    let (h1, h2): (String, String) = (holder_chars.iter().take(30).collect(), holder_chars.iter().skip(30).collect());

    if spec.livret {
        pdf.text(264.0, 32.0, "Relevé Livret +");
        pdf.text(531.0, 35.0, &format!("N° {}", spec.number));
        pdf.text(264.0, 48.0, &format!("Compte {} 01", &spec.account[..8]));
        pdf.text(541.0, 48.0, "Page 1/1");
    } else {
        pdf.text(264.0, 32.0, "Relevé de Compte");
        pdf.text(531.0, 35.0, &format!("N° {}", spec.number));
        pdf.text(264.0, 48.0, "COMPTE COURANT");
        pdf.text(541.0, 48.0, "Page 1/1");
    }
    pdf.text(264.0, 68.0, &format!("Arrêté au {}", long_date(end)));
    bank_block(&mut pdf, &spec.brand, 118.0);
    if holder_chars.len() > 30 {
        // a joint account: the bank cuts the holder line at 30 characters, in the middle of a word
        pdf.text(323.0, 165.0, &h1);
        pdf.text(323.0, 176.0, &h2);
    } else {
        pdf.text(323.0, 176.0, &spec.holder);
    }
    for (i, line) in spec.address.iter().enumerate() {
        pdf.text(323.0, 189.0 + 10.0 * i as f64, line);
    }
    pdf.text(45.0, 189.0, &format!("SITE INTERNET www.{}.fr", spec.brand.to_lowercase()));
    pdf.text(45.0, 208.0, "SERVICE CLIENTS TEL 0 800 000 000");
    pdf.text(45.0, 221.0, "SERVICE CLIENTS FAX 0 800 000 001");
    pdf.text(58.0, 267.0, &format!("Titulaire(s) du compte : {h1}"));
    if holder_chars.len() > 30 {
        pdf.text(146.0, 277.0, &h2);
    }
    if !spec.livret {
        pdf.text(58.0, 291.0, &format!("NUMÉRO DE COMPTE {} {} {} {key}", spec.bank, spec.branch, spec.account));
        pdf.text(58.0, 301.0, &format!("IBAN {}", iban_line(&iban)));
        pdf.text(235.0, 301.0, "BIC FTNOFRP1XXX");
    }

    pdf.text(57.0, 357.0, "Date");
    pdf.text(84.0, 357.0, "Date de Valeur");
    pdf.text(236.0, 357.0, "Opération");
    pdf.text(407.0, 357.0, "Débit");
    pdf.text(495.0, 357.0, "Crédit");
    let mut y = 375.0;
    pdf.text(54.0, y, &if spec.first { "ANCIEN SOLDE".to_string() } else { format!("ANCIEN SOLDE CRÉDITEUR AU {}", long_date(opening_date).to_uppercase()) });
    pdf.right(if spec.opening >= Decimal::ZERO { 546.0 } else { 463.0 }, y, &fr_space(spec.opening));
    y += 22.0;
    for (n, e) in spec.entries.iter().enumerate() {
        if n == 1 {
            pdf.text(29.0, y, "simÉ"); // the mirrored margin watermark ("Émis le...") lands on the same row as an operation
        }
        pdf.text(54.0, y, &e.day.format("%d/%m").to_string());
        pdf.text(88.0, y, &e.value_day.unwrap_or(e.day).format("%d/%m/%Y").to_string());
        pdf.text(136.0, y, &e.lines[0]);
        pdf.right(if e.amount > Decimal::ZERO { 546.0 } else { 463.0 }, y, &fr_space(e.amount));
        for extra in &e.lines[1..] {
            y += 11.0;
            pdf.text(136.0, y, extra);
        }
        y += 11.0;
    }
    let mut debits: Decimal = spec.entries.iter().filter(|e| e.amount < Decimal::ZERO).map(|e| -e.amount).sum();
    let credits: Decimal = spec.entries.iter().filter(|e| e.amount > Decimal::ZERO).map(|e| e.amount).sum();
    if spec.wrong_totals {
        debits += dec("1.00");
    }
    y += 11.0;
    pdf.text(55.0, y, "TOTAL DES OPÉRATIONS DU RELEVÉ");
    if debits != Decimal::ZERO {
        pdf.right(463.0, y, &fr_space(debits));
    }
    if credits != Decimal::ZERO {
        pdf.right(546.0, y, &fr_space(credits));
    }
    y += 24.0;
    let closing = spec.wrong_closing.unwrap_or_else(|| spec.closing());
    pdf.text(29.0, y, "simÉ");
    pdf.text(54.0, y, &format!("NOUVEAU SOLDE {} AU {}", if closing >= Decimal::ZERO { "CRÉDITEUR" } else { "DÉBITEUR" }, long_date(end).to_uppercase()));
    pdf.right(if closing >= Decimal::ZERO { 546.0 } else { 463.0 }, y, &fr_space(closing));
    pdf.text(50.0, y + 40.0, "Garantie de vos dépôts");
    pdf.text(50.0, y + 55.0, "Total de vos services et frais bancaires");
    pdf.text(50.0, y + 68.0, "Services et frais facturés");
    pdf.text(439.0, y + 68.0, "0,00 €");
    pdf.text(320.0, y + 80.0, "TOTAL facturé");
    pdf.text(439.0, y + 80.0, "0,00 €");
    pdf.text(74.0, 763.0, &format!("{} est une marque commerciale d'un établissement de test.", capitalize(&spec.brand)));
    pdf.bytes()
}

/// One of each kind of line the statement has (September 2026).
pub fn fortuneo_entries() -> Vec<FEntry> {
    let e = fentry;
    vec![
        e(ymd(2026, 9, 1), &["VIR M MARTIN PAUL"], "700.00"),
        e(ymd(2026, 9, 3), &["VIR M MARTIN PAUL"], "-60.00"),
        e(ymd(2026, 9, 9), &["VIR M MARTIN PAUL"], "200.00"),
        FEntry { value_day: Some(ymd(2026, 9, 13)), ..e(ymd(2026, 9, 14), &["CARTE 12/09 Boulangerie Test Lyon"], "-3.50") },
        FEntry { value_day: Some(ymd(2026, 9, 15)), ..e(ymd(2026, 9, 14), &["ECH PRET 0000TEST00000001"], "-250.00") },
        e(ymd(2026, 9, 21), &["PRLV SEPA ASSURANCE EXEMPLE", "VERSEMENT PERIODIQUE - CONTRAT 99999999", "FR00ZZZ000000", "ABCD25139000000003485321", "V260918007125"], "-80.00"),
        e(ymd(2026, 9, 22), &["VIR SEPA ACME CONSULTING SAS", "Salaire septembre", "SCT000000000000000000000001"], "1500.00"),
        e(ymd(2026, 9, 25), &["VIR SEPA MARTIN Pauline", "cadeau", "SCT000000000000000000000002"], "-40.00"),
    ]
}

pub fn fortuneo_september() -> FSpec {
    FSpec::new(fortuneo_entries())
}

// ------------------------------------------------------------------------------------------- fees recap
#[derive(Clone)]
pub struct Service {
    pub category: String,
    pub name: String,
    pub used: i64,
    pub unit: String,
    pub billed: i64,
    pub total: String,
}

pub fn service(category: &str, name: &str, used: i64, unit: &str, billed: i64, total: &str) -> Service {
    Service { category: category.into(), name: name.into(), used, unit: unit.into(), billed, total: total.into() }
}

#[derive(Clone)]
pub struct FeesSpec {
    pub total: Decimal,
    pub year: i32,
    pub holder: String,
    pub address: Vec<String>,
    pub bank: String,
    pub branch: String,
    pub account: String,
    pub brand: String,
    pub services: Vec<Service>,
    pub interest_paid: Decimal,
    pub title: String,
}

impl FeesSpec {
    pub fn new() -> FeesSpec {
        FeesSpec {
            total: dec("9.00"),
            year: 2023,
            holder: F_HOLDER.into(),
            address: f_address(),
            bank: F_BANK.into(),
            branch: F_BRANCH.into(),
            account: F_ACCOUNT.into(),
            brand: "Fortuneo".into(),
            services: vec![],
            interest_paid: dec("0.00"),
            title: String::new(),
        }
    }
}

pub fn fortuneo_fees_pdf(spec: &FeesSpec) -> Vec<u8> {
    let mut pdf = Pdf::new(&spec.title);
    let key = rib_key(&spec.bank, &spec.branch, &spec.account);
    let iban = make_iban(&format!("{}{}{}{key}", spec.bank, spec.branch, spec.account), "FR");
    pdf.text(241.0, 59.0, "Relevé de frais");
    pdf.text(36.0, 111.0, &spec.brand);
    pdf.text(36.0, 124.0, "TOUR EXEMPLE");
    pdf.text(36.0, 137.0, "1 PLACE DU TEST");
    pdf.text(36.0, 150.0, "75000 PARIS");
    pdf.text(36.0, 176.0, &format!("SITE INTERNET www.{}.fr", spec.brand.to_lowercase()));
    pdf.text(36.0, 189.0, "SERVICE CLIENTS TEL 0 800 000 000");
    pdf.text(36.0, 203.0, "SERVICE CLIENTS FAX 0 800 000 001");
    pdf.text(36.0, 285.0, &spec.holder);
    for (i, line) in spec.address.iter().enumerate() {
        pdf.text(36.0, 298.0 + 13.0 * i as f64, line);
    }
    pdf.text(40.0, 333.0, "Intitulé du compte");
    pdf.text(186.0, 333.0, "COMPTE COURANT");
    pdf.text(40.0, 353.0, "Identification du compte");
    pdf.text(186.0, 353.0, "BIC");
    pdf.text(338.0, 353.0, "FTNOFRP1XXX");
    pdf.text(186.0, 366.0, "IBAN");
    pdf.text(338.0, 366.0, &iban_line(&iban));
    pdf.text(186.0, 379.0, "NUMERO DE COMPTE");
    pdf.text(338.0, 379.0, &format!("{} {} {} {key}", spec.bank, spec.branch, spec.account));
    pdf.text(186.0, 392.0, "TITULAIRE(S) DU COMPTE");
    pdf.text(338.0, 392.0, &spec.holder);
    pdf.text(40.0, 412.0, "Période");
    pdf.text(186.0, 412.0, &format!("Du 1er Janvier au 31 Décembre {}", spec.year));
    pdf.text(40.0, 432.0, "Date");
    pdf.text(186.0, 432.0, &format!("31/12/{}", spec.year));
    pdf.text(36.0, 570.0, "Récapitulatif des frais et intérêts");
    pdf.text(40.0, 608.0, "Total des frais payés (frais afférents aux offres groupées de");
    pdf.right(564.0, 608.0, &format!("{} €", fr(spec.total)));
    pdf.text(40.0, 622.0, "services et tous autres frais)");
    pdf.text(40.0, 653.0, "Total des intérêts payés");
    pdf.right(564.0, 653.0, &format!("{} €", fr(spec.interest_paid)));
    pdf.text(40.0, 684.0, "Total des intérêts acquis");
    pdf.text(437.0, 681.0, "Intérêts non applicables");
    if !spec.services.is_empty() {
        pdf.add_page();
        pdf.text(40.0, 60.0, "Décompte des frais prélevés sur le compte");
        for (x, t) in [(40.0, "Service"), (250.0, "Utilisé"), (320.0, "Prix"), (400.0, "Facturé"), (480.0, "Total")] {
            pdf.text(x, 80.0, t);
        }
        let mut y = 100.0;
        let mut category: Option<&str> = None;
        for s in &spec.services {
            if category != Some(s.category.as_str()) {
                pdf.text(40.0, y, &s.category);
                category = Some(&s.category);
                y += 13.0;
            }
            if let Some((first, second)) = s.name.split_once('\n') {
                // a long name wraps above and below the row of figures
                pdf.text(52.0, y, first);
                y += 6.7;
                pdf.text(250.0, y, &s.used.to_string());
                pdf.text(320.0, y, &s.unit);
                pdf.text(400.0, y, &s.billed.to_string());
                pdf.text(480.0, y, &s.total);
                pdf.text(52.0, y + 6.7, second);
                y += 6.7 + 13.0;
                continue;
            }
            pdf.text(40.0, y, &s.name);
            pdf.text(250.0, y, &s.used.to_string());
            pdf.text(320.0, y, &s.unit);
            pdf.text(400.0, y, &s.billed.to_string());
            pdf.text(480.0, y, &s.total);
            y += 13.0;
        }
        pdf.text(40.0, y + 10.0, "Total des frais payés");
        pdf.right(564.0, y + 10.0, &format!("{} €", fr(spec.total)));
    }
    pdf.bytes()
}

// ------------------------------------------------------------------------------------------- FGDR letter
pub fn fortuneo_info_pdf(year: i32, holder: &str, address: Option<&[String]>, brand: &str, title: &str, ceiling: &str) -> Vec<u8> {
    let mut pdf = Pdf::new(title);
    pdf.text(323.0, 53.0, &format!("Le 01 janvier {year},"));
    pdf.text(323.0, 176.0, holder);
    let default = f_address();
    for (i, line) in address.unwrap_or(&default).iter().enumerate() {
        pdf.text(323.0, 187.0 + 12.0 * i as f64, line);
    }
    pdf.text(50.0, 267.0, "Monsieur,");
    pdf.text(50.0, 293.0, &format!("Vous détenez un ou plusieurs comptes chez {brand} éligible(s) à la garantie du Fonds de Garantie"));
    pdf.text(50.0, 305.0, "des Dépôts et de Résolution (FGDR).");
    pdf.text(50.0, 430.0, &format!("L'Équipe {brand}"));
    pdf.text(149.0, 468.0, "INFORMATIONS GÉNÉRALES SUR LA PROTECTION DES DÉPÔTS");
    pdf.text(53.0, 480.0, "La protection des dépôts Fonds de garantie des dépôts et de résolution (FGDR)");
    pdf.text(54.0, 514.0, &format!("Plafond de la protection : {ceiling} par déposant et par établissement de crédit (1)"));
    pdf.bytes()
}

/// The default FGDR letter.
pub fn fortuneo_info() -> Vec<u8> {
    fortuneo_info_pdf(2026, F_HOLDER, None, "Fortuneo", "", "100 000 €")
}

/// The insurer's confirmation of an arbitrage on a Fortuneo Vie contract: the letter loses most of its spaces.
pub fn fortuneo_arbitrage_pdf(net: &str, bought_total: &str, title: &str) -> Vec<u8> {
    let mut pdf = Pdf::new(title);
    pdf.text(40.0, 60.0, "Références:");
    pdf.text(40.0, 72.0, "Contrat:FORTUNEO VIE");
    pdf.text(40.0, 84.0, "N°:000123456789012345");
    pdf.text(40.0, 110.0, "Objet:Confirmation d’arbitrage");
    pdf.text(40.0, 130.0, F_HOLDER);
    pdf.text(40.0, 150.0, "FORTUNEO");
    pdf.text(40.0, 190.0, "Monsieur,");
    pdf.text(40.0, 205.0, "Le21septembre2026");
    pdf.text(40.0, 220.0, "Nous avons le plaisir de vous communiquer, ci-dessous, l’état récapitulatif de l’arbitrage réalisé sur votre contrat");
    pdf.text(40.0, 232.0, "FORTUNEO VIEdanslecadredumandatd’arbitrage.");
    pdf.text(40.0, 250.0, "l Datedel’arbitrage 16septembre2026");
    pdf.text(40.0, 262.0, &format!("Montantbrutdel’arbitrage: {net} €"));
    pdf.text(40.0, 274.0, "Fraisd’arbitrage: 0,00 €");
    pdf.text(40.0, 286.0, "Fraisdegestion : 0,00 €");
    pdf.text(40.0, 298.0, &format!("Montantnet: {net} €"));
    pdf.text(40.0, 320.0, "l Désinvestissement:");
    pdf.text(40.0, 332.0, "GESTIONNAIREEXEMPLE");
    pdf.text(40.0, 344.0, "FONDSACTIONSEXEMPLEA FR0000000001 17/09/2026 1000,00 0,0500 50,00");
    pdf.text(40.0, 356.0, "FONDSOBLIGATIONSEXEMPLEB LU0000000002 17/09/2026 400,00 0,1250 50,00");
    pdf.text(40.0, 380.0, "l Réinvestissement:");
    pdf.text(40.0, 392.0, "SURAVENIR");
    pdf.text(40.0, 404.0, "FDSEUROSEXEMPLERENDEMENT 17/09/2026 1,00 40,0000 40,00");
    pdf.text(40.0, 416.0, &format!("FONDSDIVERSEXEMPLEC IE0000000003 17/09/2026 120,00 0,5000 {}", bought_total.replace("100,00", "60,00")));
    pdf.bytes()
}

/// One fund of the annual statement of a life-insurance contract: name, ISIN, units, unit value (amounts as printed).
pub struct LifeFund {
    pub name: &'static str,
    pub isin: &'static str,
    pub units: &'static str,
    pub price: &'static str,
    pub amount: &'static str,
}

pub struct LifeAnnualSpec {
    pub year: i32,
    /// the euro fund (guaranteed capital), printed without units
    pub euro: &'static str,
    pub funds: Vec<LifeFund>,
    /// the value of the contract the year before ("Pour rappel..."); the first statement has none
    pub previous: Option<&'static str>,
    /// the value printed for the contract: the sum of the funds unless set
    pub declared: Option<&'static str>,
    pub payments: &'static str,
    /// the contract number as printed (digits only; grouped by the fixture)
    pub contract: &'static str,
}

impl LifeAnnualSpec {
    pub fn with_contract(self, contract: &'static str) -> Self {
        LifeAnnualSpec { contract, ..self }
    }

    fn contract_printed(&self) -> String {
        let c = self.contract;
        format!("{} {} {} {}", &c[..6], &c[6..14], &c[14..16], &c[16..])
    }

    pub fn new(year: i32) -> Self {
        LifeAnnualSpec {
            year,
            euro: "400,00",
            funds: vec![
                LifeFund { name: "FONDS ACTIONS EXEMPLE A", isin: "FR0000000001", units: "0,1500", price: "400,00", amount: "60,00" },
                LifeFund { name: "FONDS OBLIGATIONS EXEMPLE B", isin: "LU0000000002", units: "1,2500", price: "100,00", amount: "125,00" },
                LifeFund { name: "FONDS DIVERS EXEMPLE C", isin: "IE0000000003", units: "0,0100", price: "1 500,00", amount: "15,00" },
            ],
            previous: Some("440,00"),
            declared: None,
            payments: "100,00",
            contract: "000123456789012345",
        }
    }

    pub fn total(&self) -> Decimal {
        std::iter::once(self.euro).chain(self.funds.iter().map(|f| f.amount)).map(|a| dec(&a.replace(' ', "").replace(',', "."))).sum()
    }
}

/// The yearly "Relevé d'information annuel" of a Fortuneo Vie contract: a letter, then the situation of the contract
/// (funds in a table whose last column is only found by its position) and the movements of the year.
pub fn fortuneo_life_annual_pdf(spec: &LifeAnnualSpec) -> Vec<u8> {
    let mut pdf = Pdf::new("");
    let y = spec.year;
    let value = spec.declared.map(str::to_string).unwrap_or_else(|| fr_space(spec.total()));
    pdf.text(324.0, 60.0, &format!("Brest, le 19 février {}", y + 1));
    pdf.text(36.0, 84.0, &format!("N° Contrat : {}", spec.contract_printed()));
    pdf.text(31.0, 110.0, "Votre conseiller :");
    pdf.text(31.0, 122.0, "FORTUNEO");
    pdf.text(31.0, 134.0, "TSA 41707");
    pdf.text(323.0, 146.0, "M. MARTIN PAUL");
    pdf.text(323.0, 158.0, "8 RUE DES TILLEULS");
    pdf.text(323.0, 170.0, "69003 LYON");
    pdf.text(36.0, 200.0, &format!("N/Réf : Contrat FORTUNEO VIE N° : {}", spec.contract_printed()));
    pdf.text(36.0, 212.0, "Objet : Relevé d’information Annuelle");
    pdf.text(37.0, 240.0, &format!("J’ai le plaisir de vous adresser, au nom de Suravenir, le relevé d’information annuelle de votre contrat arrêté au 31 décembre {y}."));

    pdf.add_page();
    pdf.text(152.0, 60.0, "VOTRE RELEVE D'INFORMATION ANNUELLE");
    pdf.text(37.0, 90.0, &format!("Les taux de revalorisation des supports en euros, nets de frais de gestion pour l’année {y} ont été de :"));
    pdf.text(37.0, 108.0, "n");
    pdf.text(54.0, 108.0, "2,50% pour le FDS EUROS EXEMPLE RENDEMENT (dont 0,40% de bonification sur encours). Votre montant d'intérêts net");
    pdf.text(37.0, 120.0, "est de 5,96 € .");
    pdf.text(37.0, 136.0, "n");
    pdf.text(54.0, 136.0, "3,00% pour le FDS EUROS EXEMPLE OPPORTUNITES.");
    pdf.text(180.0, 160.0, &format!("Valeur (*) de votre contrat au 31 décembre {y} : {value} €"));
    pdf.text(171.0, 190.0, &format!("LA SITUATION DE VOTRE CONTRAT AU 31 DÉCEMBRE {y}"));
    pdf.text(92.0, 210.0, "Support d'investissement");
    pdf.text(280.0, 210.0, "Code ISIN");
    pdf.text(354.0, 210.0, "Nombre de");
    pdf.text(441.0, 210.0, "Valeur");
    pdf.text(510.0, 210.0, "Valeur en €");
    pdf.text(346.0, 222.0, "parts détenues");
    pdf.text(421.0, 222.0, "liquidative en €");
    let row = |pdf: &mut Pdf, top: f64, f: &LifeFund| {
        pdf.text(39.0, top, f.name);
        pdf.text(270.0, top, f.isin);
        pdf.right(411.0, top, f.units);
        pdf.right(492.0, top, f.price);
        pdf.right(570.0, top, f.amount);
    };
    pdf.text(39.0, 250.0, "FDS EUROS EXEMPLE RENDEMENT (1)");
    pdf.right(570.0, 250.0, spec.euro);
    for (i, f) in spec.funds.iter().take(2).enumerate() {
        row(&mut pdf, 270.0 + 20.0 * i as f64, f);
    }

    pdf.add_page();
    pdf.text(156.0, 60.0, &format!("LA SITUATION DE VOTRE CONTRAT AU 31 DÉCEMBRE {y} (suite)"));
    pdf.text(92.0, 90.0, "Support d'investissement");
    pdf.text(281.0, 90.0, "Code ISIN");
    for (i, f) in spec.funds.iter().skip(2).enumerate() {
        row(&mut pdf, 120.0 + 20.0 * i as f64, f);
    }
    pdf.text(37.0, 180.0, "(1) Fonds en euros présentant une garantie en capital");
    if let Some(p) = spec.previous {
        pdf.text(34.0, 200.0, &format!("Pour rappel, la valeur de votre contrat au 31 décembre {} était de {p} € .", y - 1));
    }
    pdf.text(133.0, 230.0, &format!("SYNTHÈSE DES MOUVEMENTS INTERVENUS SUR VOTRE CONTRAT EN {y}"));
    pdf.text(101.0, 250.0, "Libellé opération");
    pdf.text(269.0, 250.0, "Débit (en € )");
    pdf.text(376.0, 250.0, "Crédit (en € )");
    pdf.text(485.0, 250.0, "Frais(en € ) (4)");
    pdf.text(38.0, 270.0, "Versements");
    pdf.right(455.0, 270.0, spec.payments);
    pdf.text(38.0, 284.0, "Arbitrages");
    pdf.right(345.0, 284.0, "50,00");
    pdf.right(455.0, 284.0, "50,00");
    pdf.text(38.0, 298.0, "Revalorisation annuelle des fonds en euros");
    pdf.right(455.0, 298.0, "5,96");
    pdf.text(38.0, 312.0, "Prélèvements sociaux sur la revalorisation");
    pdf.right(345.0, 326.0, "1,03");
    pdf.text(38.0, 340.0, "annuelle des fonds en euros");
    pdf.text(38.0, 360.0, "(4) Montant des frais sur opération");
    pdf.text(37.0, 400.0, &format!("Le montant total des frais imputés sur votre contrat pour l’année {y} est de 4,48 €."));
    pdf.bytes()
}

/// The "Profil investisseur et profil extra-financier" form: personal answers first, then the result.
pub fn fortuneo_investor_profile_pdf(established: &str, profile: &str) -> Vec<u8> {
    let mut pdf = Pdf::new("");
    pdf.text(60.0, 60.0, "FORMULAIRE VOS BESOINS, NOS CONSEILS");
    pdf.text(60.0, 72.0, "Profil investisseur et profil extra-financier");
    pdf.text(28.0, 100.0, "VOS DONNEES PERSONNELLES");
    pdf.text(28.0, 120.0, "Nom de naissance : MARTIN");
    pdf.text(28.0, 132.0, "Prénom(s) : PAUL");
    pdf.text(28.0, 144.0, "Situation familiale : Union libre");
    pdf.text(28.0, 156.0, "Situation professionnelle : Profession exemple");
    pdf.text(28.0, 190.0, "QUESTIONNAIRE INVESTISSEUR");
    pdf.text(28.0, 202.0, "Vous épargnez principalement pour ? faire fructifier votre argent");
    pdf.text(60.0, 740.0, "FORTUNEO VIE est un contrat collectif d'assurance-vie multisupport géré par Suravenir.");
    pdf.add_page();
    pdf.text(28.0, 60.0, "2. RESULTAT PROFIL INVESTISSEUR ET RESULTAT PROFIL EXTRA-FINANCIER");
    pdf.text(28.0, 76.0, "2.1 RESULTAT PROFIL INVESTISSEUR");
    pdf.text(28.0, 96.0, "VOTRE PROJET : faire fructifier votre argent");
    pdf.text(28.0, 110.0, "VOTRE HORIZON DE PLACEMENT : plus de 8 ans");
    pdf.text(28.0, 124.0, &format!("VOUS AVEZ UN PROFIL INVESTISSEUR : {profile}"));
    pdf.text(28.0, 140.0, &format!("Ce profil a été établi par vos soins le {established}"));
    pdf.text(28.0, 170.0, "2.2 RESULTAT PROFIL EXTRA-FINANCIER");
    pdf.text(28.0, 190.0, "VOUS AVEZ UN PROFIL EXTRA-FINANCIER : SANS PREFERENCE");
    pdf.text(28.0, 204.0, &format!("Ce profil a été établi par vos soins le {established}"));
    pdf.bytes()
}

/// The other wording of the arbitrage confirmation ("Unités de comptes initiales / cibles", a management fee, then the
/// whole portfolio after the arbitrage), with the spaces kept inside the names of the funds.
pub fn fortuneo_arbitrage_v2_pdf(net: &str, initial_total: &str) -> Vec<u8> {
    fortuneo_arbitrage_v2_named_pdf(net, initial_total, "Contrat : FORTUNEO VIE")
}

/// Same letter, with the contract line as given (some letters print "Contrat : FORTUNEOVIE").
pub fn fortuneo_arbitrage_v2_named_pdf(net: &str, initial_total: &str, contract_line: &str) -> Vec<u8> {
    let mut pdf = Pdf::new("");
    pdf.text(40.0, 60.0, "Références :");
    pdf.text(40.0, 72.0, contract_line);
    pdf.text(40.0, 84.0, "N°: 000123 45678901 23 45");
    pdf.text(40.0, 110.0, "Objet : Confirmation d'arbitrage");
    pdf.text(40.0, 130.0, "M. MARTIN PAUL");
    pdf.text(40.0, 190.0, "Monsieur,");
    pdf.text(40.0, 205.0, "Le 17 avril 2026");
    pdf.text(40.0, 232.0, "FORTUNEO VIE dans le cadre du mandat d'arbitrage.");
    pdf.text(40.0, 250.0, "l Date de l'arbitrage 14 avril 2026");
    pdf.text(40.0, 262.0, "Montant brut de l'arbitrage : 100,02 €");
    pdf.text(40.0, 274.0, "Frais d’arbitrage : 0,00 €");
    pdf.text(40.0, 286.0, "Frais de gestion (*) : 0,02 €");
    pdf.text(40.0, 298.0, &format!("Montant net : {net} €"));
    pdf.text(40.0, 320.0, "l Unités de comptes initiales :");
    pdf.text(40.0, 332.0, "Valeurs");
    pdf.text(40.0, 344.0, "SURAVENIR");
    pdf.text(40.0, 356.0, &format!("FDS EUROS EXEMPLE RENDEMENT 15/04/2026 1,00 {initial_total} {initial_total}"));
    pdf.text(40.0, 368.0, "CARMIGNAC");
    pdf.text(40.0, 380.0, "FONDS ACTIONS EXEMPLE A FR0000000001 15/04/2026 400,00 0,2500 100,00");
    pdf.text(40.0, 410.0, "l Unités de comptes cibles :");
    pdf.text(40.0, 422.0, "FDS EUROS EXEMPLE RENDEMENT 15/04/2026 1,00 40,0000 40,00");
    pdf.text(40.0, 434.0, "FONDS DIVERS EXEMPLE C IE0000000003 15/04/2026 120,00 0,5000 60,00");
    pdf.text(40.0, 464.0, "l Répartition des unités de comptes après arbitrage : (*)");
    pdf.text(40.0, 476.0, "FDS EUROS EXEMPLE RENDEMENT 16/04/2026 1,00 340,0000 340,00");
    pdf.text(40.0, 488.0, "FONDS DIVERS EXEMPLE C IE0000000003 16/04/2026 120,00 0,5000 60,00");
    pdf.bytes()
}

/// The "Opérations" export of a Fortuneo Vie contract: two years of payments, the revaluation of the euro fund with the
/// social contributions taken on it, a distribution, arbitrages (not operations), set-up lines without an amount and the
/// fees of the running year (still without a state).
pub fn fortuneo_life_operations_csv() -> Vec<u8> {
    "Date;Libellé;Type;État;Montant (€)\n\
2026-10-26;Frais année 2026;Frais;;-5.38\n\
2026-09-15;Versement programmé;Versement;Traité;80.00\n\
2026-08-14;Arbitrage mandat d'arbitrage;Arbitrage;Traité;50.00\n\
2026-07-15;Versement programmé;Versement;Traité;80.00\n\
2026-05-14;Distribution Fonds Exemple;Distribution;Traité;0.05\n\
2026-01-24;Revalorisation fonds euros Exemple Rendement;Revalorisation;Traité;5.96\n\
2026-01-24;Prélèvement / revalo fonds euros Exemple Rendement;Prélèvement;Traité;-1.03\n\
2025-12-31;Frais année 2025;Frais;;-3.01\n\
2025-05-19;Mise en place de VP;Paramétrage;Traité;\n\
2025-01-20;Versement;Versement;Traité;100.00\n\
2024-09-20;Adhésion;Versement;Traité;300.00\n"
        .as_bytes()
        .to_vec()
}
