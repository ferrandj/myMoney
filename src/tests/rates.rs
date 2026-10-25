//! python/tests/test_rates.py

use crate::services::rates::{effective_rate, guess_account_type, reference_rate};
use crate::testkit::fixtures::*;

#[test]
fn test_reference_rate_by_date() {
    assert_eq!(reference_rate("livret_a", Some(ymd(2026, 3, 1))), Some((dec("1.5"), ymd(2026, 2, 1))));
    assert_eq!(reference_rate("livret_a", Some(ymd(2026, 10, 19))), Some((dec("1.7"), ymd(2026, 8, 1))));
    assert_eq!(reference_rate("lep", Some(ymd(2026, 10, 19))).unwrap().0, dec("2.5"));
    assert_eq!(reference_rate("livret_jeune", Some(ymd(2026, 10, 19))).unwrap().0, dec("1.7"));
    assert_eq!(reference_rate("checking", None), None);
}

#[test]
fn test_override_wins() {
    assert_eq!(effective_rate("livret_a", Some(dec("3")), None).0, Some(dec("3")));
    assert_eq!(effective_rate("savings", None, None), (None, String::new()));
}

#[test]
fn test_guess_account_type() {
    let guess = |label: &str| guess_account_type(label, "checking");
    assert_eq!(guess("LIVRET A"), "livret_a");
    assert_eq!(guess("Livret Développement Durable et Solidaire"), "ldds");
    assert_eq!(guess("Compte courant"), "checking");
    assert_eq!(guess("Contrat assurance vie Fortuneo Vie"), "life_insurance");
    assert_eq!(guess("PEA Bourse"), "pea");
    assert_eq!(guess("Livret Cashbee"), "savings");
    assert_eq!(guess("Livret avantage"), "savings");
}
