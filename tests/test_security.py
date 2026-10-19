import pytest

from mymoney.core import MyMoney
from mymoney.models import Account
from mymoney.security import crypto
from mymoney.security.keystore import InvalidPassword
from mymoney.security.vault import VaultLocked, check_password_strength

from .conftest import PASSWORD


def test_encrypt_roundtrip_and_tamper():
    key = crypto.random_key()
    blob = crypto.encrypt(key, b"secret", b"aad")
    assert crypto.decrypt(key, blob, b"aad") == b"secret"
    with pytest.raises(crypto.DecryptionError):
        crypto.decrypt(key, blob, b"other-aad")
    with pytest.raises(crypto.DecryptionError):
        crypto.decrypt(key, blob[:-1] + bytes([blob[-1] ^ 1]), b"aad")


def test_weak_passwords_rejected():
    for weak in ("short", "alllowercaseletters", "Password1"):
        with pytest.raises(ValueError):
            check_password_strength(weak)
    check_password_strength(PASSWORD)
    check_password_strength("une phrase de passe assez longue")


def test_database_and_documents_are_encrypted_at_rest(settings):
    app = MyMoney(settings)
    recovery = app.vault.setup(PASSWORD)
    app.save_account(None, {"name": "Compte joint MARQUEUR", "account_type": "checking", "balance": "1234.56", "iban": "FR7630001007941234567890185"})
    doc_id = app.store_document("releve.csv", "Date;Libellé;Montant\n01/02/2026;LOYER MARQUEUR;-800,00\n".encode())
    app.vault.lock()

    raw_db = settings.db_path.read_bytes()
    assert not raw_db.startswith(b"SQLite format 3")
    assert b"MARQUEUR" not in raw_db and b"FR7630001007941234567890185" not in raw_db
    for blob in settings.documents_dir.iterdir():
        assert b"MARQUEUR" not in blob.read_bytes()
    assert PASSWORD.encode() not in settings.keystore_path.read_bytes()

    with pytest.raises(VaultLocked):
        with app.db.session():
            pass

    with pytest.raises(InvalidPassword):
        app.vault.unlock("Wrong-Password-1!")
    app.vault.unlock_with_recovery(recovery.lower().replace("-", " "))
    with app.db.session() as s:
        assert s.get(Account, 1).name == "Compte joint MARQUEUR"
    assert b"MARQUEUR" in app.read_document(doc_id)[1]


def test_change_password_keeps_data(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    app.save_account(None, {"name": "Livret", "account_type": "livret_a", "balance": "10"})
    app.vault.change_password(PASSWORD, "Another-Strong-Pass-2")
    app.vault.lock()
    with pytest.raises(InvalidPassword):
        app.vault.unlock(PASSWORD)
    app.vault.unlock("Another-Strong-Pass-2")
    with app.db.session() as s:
        assert s.get(Account, 1).name == "Livret"


def test_connection_secrets_double_encrypted(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    conn_id = app.create_connection("exchange", "Binance", {"exchange": "binance", "api_key": "KEY-MARQUEUR", "secret": "S", "password": ""})
    with app.db.session() as s:
        from mymoney.models import Connection

        conn = s.get(Connection, conn_id)
        assert b"KEY-MARQUEUR" not in conn.config_enc
        assert app.connection_config(conn)["api_key"] == "KEY-MARQUEUR"
    # Empty secret on update = unchanged.
    app.update_connection(conn_id, "Binance", {"exchange": "binance", "api_key": "", "secret": "", "password": ""})
    with app.db.session() as s:
        assert app.connection_config(s.get(Connection, conn_id))["api_key"] == "KEY-MARQUEUR"


def test_login_throttling(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    app.vault.lock()
    for _ in range(3):
        with pytest.raises(InvalidPassword):
            app.check_password("Wrong-Password-1!")
    with pytest.raises(PermissionError):
        app.check_password(PASSWORD)
