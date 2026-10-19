from datetime import date
from decimal import Decimal

from mymoney.services.rates import effective_rate, guess_account_type, reference_rate


def test_reference_rate_by_date():
    assert reference_rate("livret_a", date(2026, 3, 1)) == (Decimal("1.5"), date(2026, 2, 1))
    assert reference_rate("livret_a", date(2026, 10, 19)) == (Decimal("1.7"), date(2026, 8, 1))
    assert reference_rate("lep", date(2026, 10, 19))[0] == Decimal("2.5")
    assert reference_rate("livret_jeune", date(2026, 10, 19))[0] == Decimal("1.7")
    assert reference_rate("checking") is None


def test_override_wins():
    assert effective_rate("livret_a", Decimal("3"))[0] == Decimal("3")
    assert effective_rate("savings", None) == (None, "")


def test_guess_account_type():
    assert guess_account_type("LIVRET A") == "livret_a"
    assert guess_account_type("Livret Développement Durable et Solidaire") == "ldds"
    assert guess_account_type("Compte courant") == "checking"
    assert guess_account_type("Contrat assurance vie Fortuneo Vie") == "life_insurance"
    assert guess_account_type("PEA Bourse") == "pea"
    assert guess_account_type("Livret Cashbee") == "savings"
    assert guess_account_type("Livret avantage") == "savings"
