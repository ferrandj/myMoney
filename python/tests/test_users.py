"""Several users: key slots, ownership, sharing, and what each person can see (synthetic data only)."""

import re
from datetime import date
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from mymoney.core import MyMoney
from mymoney.models import Account, AccountShare, Document, Transaction, User
from mymoney.security.keystore import InvalidPassword, Keystore
from mymoney.services import access, ledger
from mymoney.services import users as user_service
from mymoney.web.app import create_app

from .conftest import ADMIN_EMAIL, PASSWORD
from .fixtures import IBAN, Entry, Spec, boursobank_statement_pdf, sample_entries

D = Decimal
BOB = "bob@example.com"
BOB_PASSWORD = "Another-Strong-Pass-7"
CAROL = "carol@example.com"
CAROL_PASSWORD = "Third-Strong-Pass-3!"


# ------------------------------------------------------------------------------------------------ key slots
def test_each_user_has_a_key_slot_to_the_same_vault(tmp_path):
    store = Keystore(tmp_path / "keys.json", 1, 8192)
    alice, bob = user_service.slot_id("alice@example.com"), user_service.slot_id(BOB)
    dek, recovery = store.create("Alice-Password-1!", alice)
    store.add_slot(dek, bob, BOB_PASSWORD)
    assert store.unlock("Alice-Password-1!", alice) == dek and store.unlock(BOB_PASSWORD, bob) == dek
    for password, slot in (("wrong", alice), (BOB_PASSWORD, alice), ("Alice-Password-1!", bob), ("x", "unknown-slot"), ("Alice-Password-1!", None)):
        with pytest.raises(InvalidPassword):
            store.unlock(password, slot)  # a wrong password, someone else's, an unknown user and a missing user all look the same
    store.set_password(dek, "Alice-New-Password-2!", alice)  # changing one password leaves the others alone
    assert store.unlock(BOB_PASSWORD, bob) == dek
    with pytest.raises(InvalidPassword):
        store.unlock("Alice-Password-1!", alice)
    store.remove_slot(bob)
    with pytest.raises(InvalidPassword):
        store.unlock(BOB_PASSWORD, bob)
    assert store.unlock_with_recovery(recovery) == dek and "Alice" not in store.path.read_text() and "alice" not in store.path.read_text()


def test_a_vault_from_before_users_keeps_its_password_for_the_first_user(tmp_path):
    store = Keystore(tmp_path / "keys.json", 1, 8192)
    dek, _ = store.create(PASSWORD)  # version 1 layout: a single password
    assert store.has_legacy_slot() and store.unlock(PASSWORD) == dek
    slot = user_service.slot_id(ADMIN_EMAIL)
    store.adopt_legacy_slot(slot)
    assert not store.has_legacy_slot() and store.unlock(PASSWORD, slot) == dek and store.slots() == [slot]


def test_emails_are_normalised_and_checked():
    assert user_service.normalize_email("  Bob@Example.COM ") == BOB
    assert user_service.slot_id("BOB@example.com") == user_service.slot_id(BOB)
    for bad in ("", "bob", "bob@", "@example.com", "a b@example.com", None):
        with pytest.raises(ValueError):
            user_service.normalize_email(bad)


# --------------------------------------------------------------------------------------- core level
@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.setup(ADMIN_EMAIL, "Alice", PASSWORD)
    with app.db.session() as s:
        user_service.add_with_password(s, app.vault, BOB, "Bob", BOB_PASSWORD)
        user_service.add_with_password(s, app.vault, CAROL, "Carol", CAROL_PASSWORD)
    return app


def identity(core: MyMoney, email: str) -> access.Identity:
    return core.identity_for_email(email)


def new_account(core: MyMoney, who: str, name: str, balance: str = "100") -> int:
    with access.as_user(identity(core, who)):
        return core.save_account(None, {"name": name, "account_type": "checking", "balance": balance, "iban": ""})


def names(core: MyMoney, who: str) -> list[str]:
    with access.as_user(identity(core, who)):
        return sorted(v.account.name for v in core.portfolio(False).accounts)


def test_first_user_is_the_administrator_and_login_needs_the_right_password(core):
    alice = core.authenticate(ADMIN_EMAIL, PASSWORD)
    assert alice.is_admin and alice.email == ADMIN_EMAIL and alice.name == "Alice"
    assert not core.authenticate(BOB, BOB_PASSWORD).is_admin
    for email, password in ((ADMIN_EMAIL, BOB_PASSWORD), (BOB, PASSWORD), ("nobody@example.com", PASSWORD), ("not-an-email", PASSWORD)):
        core._failed_logins, core._next_login_at = 0, 0.0
        with pytest.raises(InvalidPassword):
            core.authenticate(email, password)


def test_everyone_sees_their_own_accounts_only_until_something_is_shared(core):
    a = new_account(core, ADMIN_EMAIL, "Compte Alice")
    b = new_account(core, BOB, "Compte Bob")
    assert names(core, ADMIN_EMAIL) == ["Compte Alice"] and names(core, BOB) == ["Compte Bob"] and names(core, CAROL) == []
    with core.db.session() as s:
        assert s.get(Account, a).owner_id == identity(core, ADMIN_EMAIL).id and s.get(Account, b).owner_id == identity(core, BOB).id
    with access.as_user(identity(core, BOB)), core.db.session() as s:
        with pytest.raises(access.AccessDenied):
            access.check_read(s, a)
        assert access.readable_ids(s) == {b}


def test_a_share_gives_read_or_edit_and_only_the_owner_can_delete(core):
    a = new_account(core, ADMIN_EMAIL, "Compte Alice")
    with core.db.session() as s:
        user_service.share(s, s.get(Account, a), s.scalar(select(User).where(User.email == BOB)), can_edit=False)
        user_service.share(s, s.get(Account, a), s.scalar(select(User).where(User.email == CAROL)), can_edit=True)
    assert names(core, BOB) == ["Compte Alice"] and names(core, CAROL) == ["Compte Alice"]
    with access.as_user(identity(core, BOB)):
        with pytest.raises(access.AccessDenied, match="lecture seule"):
            core.save_account(a, {"name": "Renommé"})
        with pytest.raises(access.AccessDenied):
            core.add_holding(a, {"quantity": "1", "symbol": "BTC"})
        with pytest.raises(access.AccessDenied, match="propriétaire"):
            core.delete_account(a)
    with access.as_user(identity(core, CAROL)):
        core.save_account(a, {"name": "Compte Alice (corrigé)"})  # may edit...
        with pytest.raises(access.AccessDenied, match="propriétaire"):
            core.delete_account(a)  # ...but not delete
    with access.as_user(identity(core, ADMIN_EMAIL)):
        core.delete_account(a)
    assert names(core, BOB) == []


def test_sharing_rules(core):
    a = new_account(core, ADMIN_EMAIL, "Compte Alice")
    with core.db.session() as s:
        account, alice, bob = s.get(Account, a), s.scalar(select(User).where(User.email == ADMIN_EMAIL)), s.scalar(select(User).where(User.email == BOB))
        with pytest.raises(ValueError, match="déjà propriétaire"):
            user_service.share(s, account, alice, False)
        user_service.share(s, account, bob, False)
        user_service.share(s, account, bob, True)  # sharing again changes the rights, it does not add a second share
        assert [(sh.user_id, sh.can_edit) for sh in account.shares] == [(bob.id, True)]
        user_service.unshare(s, account, bob.id)
        assert account.shares == [] and s.scalars(select(AccountShare)).all() == []


def test_documents_follow_the_same_rules(core):
    def drop(who: str, spec: Spec) -> int:
        data = boursobank_statement_pdf(spec)
        with access.as_user(identity(core, who)):
            doc_id, _ = core.store_document("releve.pdf", data)
            core.analyze_document(doc_id, None, data)
        return doc_id

    doc = drop(ADMIN_EMAIL, Spec(sample_entries()))
    with access.as_user(identity(core, BOB)):
        assert not core.document_visible(doc) and core.review_document(doc) is None
        for action in (lambda: core.read_document(doc), lambda: core.delete_document(doc), lambda: core.import_statement(doc, 0, {"account_id": "new"}), lambda: core.undo_import(doc)):
            with pytest.raises(access.AccessDenied):
                action()
    with access.as_user(identity(core, ADMIN_EMAIL)):
        account_id = core.import_statement(doc, 0, {"account_id": "new", "name": "Courant", "import_transactions": "on"})["account_id"]
    with core.db.session() as s:  # once the account is shared, its documents are readable (read only: not changeable)
        user_service.share(s, s.get(Account, account_id), s.scalar(select(User).where(User.email == BOB)), can_edit=False)
    with access.as_user(identity(core, BOB)):
        assert core.document_visible(doc) and core.read_document(doc)[0] == "releve.pdf"
        with pytest.raises(access.AccessDenied):
            core.delete_document(doc)
        with pytest.raises(access.AccessDenied):
            core.undo_import(doc)


def test_importing_into_someone_elses_account_is_refused_and_a_statement_of_it_is_not_matched_to_it(core):
    data = boursobank_statement_pdf(Spec(sample_entries()))
    with access.as_user(identity(core, ADMIN_EMAIL)):
        doc, _ = core.store_document("a.pdf", data)
        core.analyze_document(doc, None, data)
        account_id = core.import_statement(doc, 0, {"account_id": "new", "name": "Courant", "import_transactions": "on"})["account_id"]
    # Bob drops the next month of the same account (same IBAN): he has no right on it, so it is not offered as the target.
    next_month = boursobank_statement_pdf(Spec([Entry(date(2026, 10, 3), ["CARTE 01/10/26 BOULANGERIE CB*1234"], D("-4.20"))], opening=Spec(sample_entries()).closing,
                                               opening_date=date(2026, 9, 30), period=(date(2026, 10, 1), date(2026, 10, 31))))
    with access.as_user(identity(core, BOB)):
        doc2, _ = core.store_document("b.pdf", next_month)
        core.analyze_document(doc2, None, next_month)
        assert core.auto_import(doc2) is None  # nothing to attach to: it would be a new account, which he must decide
        review = core.review_document(doc2)
        assert review["plans"][0].account is None and review["accounts"] == []
        with pytest.raises(access.AccessDenied):
            core.import_statement(doc2, 0, {"account_id": str(account_id), "import_transactions": "on"})
    with core.db.session() as s:  # edit rights make it his target
        user_service.share(s, s.get(Account, account_id), s.scalar(select(User).where(User.email == BOB)), can_edit=True)
    with access.as_user(identity(core, BOB)):
        assert core.auto_import(doc2)["account_id"] == account_id


def test_two_people_accounts_are_not_my_own_accounts_for_transfers(core):
    """An opposite amount on the same day is "between my own accounts" only when both accounts have the same owner."""
    alice1, alice2 = new_account(core, ADMIN_EMAIL, "A1"), new_account(core, ADMIN_EMAIL, "A2")
    bob1 = new_account(core, BOB, "B1")
    with core.db.session() as s:
        for account_id, amount in ((alice1, D("-50")), (alice2, D("50")), (alice1, D("-70")), (bob1, D("70"))):
            s.add(Transaction(account_id=account_id, fingerprint=f"{account_id}{amount}", date=date(2026, 9, 5), label="VIR", amount=amount, kind="transfer"))
        s.flush()
        assert ledger.pair_transfers(s) == 1  # 50 between Alice's two accounts; the 70 from Alice to Bob is a real payment
        internal = {(t.account_id, t.amount): t.internal for t in s.scalars(select(Transaction))}
    assert internal == {(alice1, D("-50")): True, (alice2, D("50")): True, (alice1, D("-70")): False, (bob1, D("70")): False}


def test_contacts_statistics_only_count_what_the_user_can_see(core):
    from mymoney.models import Counterparty
    from mymoney.services import counterparties

    a, b = new_account(core, ADMIN_EMAIL, "A"), new_account(core, BOB, "B")
    with core.db.session() as s:
        cp = Counterparty(name="Garage")
        s.add(cp)
        s.flush()
        for account_id, amount in ((a, D("-10")), (b, D("-25"))):
            s.add(Transaction(account_id=account_id, fingerprint=f"x{account_id}", date=date(2026, 9, 5), label="PRLV", amount=amount, kind="direct_debit", counterparty_id=cp.id))
        s.flush()
    for who, expected in ((ADMIN_EMAIL, D("10")), (BOB, D("25")), (CAROL, D("0"))):
        with access.as_user(identity(core, who)), core.db.session() as s:
            assert [c.total_out for c in counterparties.summaries(s) if c.contact.name == "Garage"] == [expected]


def test_removing_a_user_hands_their_accounts_to_the_administrator(core):
    a = new_account(core, BOB, "Compte Bob")
    with core.db.session() as s:
        user_service.share(s, s.get(Account, a), s.scalar(select(User).where(User.email == CAROL)), can_edit=False)
    with core.db.session() as s:
        alice, bob = s.scalar(select(User).where(User.email == ADMIN_EMAIL)), s.scalar(select(User).where(User.email == BOB))
        assert user_service.remove(s, core.vault, bob, alice) == 1
    with core.db.session() as s:
        carol = s.scalar(select(User).where(User.email == CAROL))
        assert s.get(Account, a).owner_id == alice.id and [sh.user_id for sh in s.get(Account, a).shares] == [carol.id]  # Carol keeps her share
        assert s.scalar(select(User).where(User.email == BOB)) is None
    with pytest.raises(InvalidPassword):
        core._failed_logins, core._next_login_at = 0, 0.0
        core.authenticate(BOB, BOB_PASSWORD)  # their key slot is gone too


def test_the_last_administrator_cannot_be_removed_nor_oneself(core):
    with core.db.session() as s:
        alice, bob = s.scalar(select(User).where(User.email == ADMIN_EMAIL)), s.scalar(select(User).where(User.email == BOB))
        with pytest.raises(ValueError, match="soi-même"):
            user_service.remove(s, core.vault, alice, alice)
        with pytest.raises(ValueError, match="au moins un administrateur"):
            user_service.remove(s, core.vault, alice, bob)


def test_adding_a_user_checks_the_password_and_the_email_before_writing_anything(core):
    with core.db.session() as s:
        with pytest.raises(ValueError, match="mot de passe"):
            user_service.add_with_password(s, core.vault, "dave@example.com", "Dave", "short")
        with pytest.raises(ValueError, match="déjà"):
            user_service.add_with_password(s, core.vault, BOB.upper(), "Bob again", BOB_PASSWORD)
    assert len(core.keystore.slots()) == 3 and core.has_users()


# --------------------------------------------------------------------- vault from before users existed
def test_upgrading_a_single_password_vault_gives_the_data_to_the_first_user(settings):
    old = MyMoney(settings)
    old.vault.setup(PASSWORD)  # the old layout: one master password, no user
    account_id = old.save_account(None, {"name": "Ancien compte", "account_type": "checking", "balance": "42"})
    data = boursobank_statement_pdf(Spec(sample_entries()))
    doc_id, _ = old.store_document("ancien.pdf", data)
    with old.db.session() as s:
        assert s.get(Account, account_id).owner_id is None and s.get(Document, doc_id).owner_id is None
    old.vault.lock()

    app = MyMoney(settings)
    assert app.legacy_login and not app.vault.unlocked
    app.check_password(PASSWORD)  # no e-mail yet: the master password opens it
    assert not app.has_users()
    me = app.create_first_user(" Admin@Example.com ", "Alice")
    assert me.is_admin and me.email == ADMIN_EMAIL and not app.legacy_login
    with access.as_user(me), app.db.session() as s:
        assert s.get(Account, account_id).owner_id == me.id and s.get(Document, doc_id).owner_id == me.id
        assert access.readable_ids(s) == {account_id}
    with pytest.raises(ValueError, match="déjà"):
        app.create_first_user("other@example.com", None)
    app.vault.lock()
    assert app.authenticate(ADMIN_EMAIL, PASSWORD).is_admin  # the same password, now through the user's slot


# ------------------------------------------------------------------------------------------------- web
@pytest.fixture
def web(settings):
    app = create_app(settings)
    with TestClient(app) as alice:
        alice.app = app
        alice.core = app.state.core
        yield alice


def csrf_of(html):
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def sign_up(client):
    client.post("/setup", data={"setup_token": client.core.setup_token, "email": ADMIN_EMAIL, "name": "Alice", "password": PASSWORD, "confirm": PASSWORD})
    return csrf_of(client.get("/settings").text)


def add_user(client, csrf, email, name, password, admin=False):
    data = {"csrf": csrf, "email": email, "name": name, "password": password}
    if admin:
        data["is_admin"] = "on"
    return client.post("/settings/users", data=data)


def second_browser(web, email, password) -> TestClient:
    other = TestClient(web.app)
    response = other.post("/login", data={"email": email, "password": password}, follow_redirects=False)
    assert response.status_code == 303, response.text
    return other


def create_account(client, csrf, name, balance="1000"):
    done = client.post("/accounts/new", data={"csrf": csrf, "name": name, "account_type": "checking", "balance": balance})
    return int(re.search(r"/accounts/(\d+)", str(done.url)).group(1))


def test_web_each_user_logs_in_with_email_and_password_and_sees_only_their_accounts(web):
    csrf = sign_up(web)
    assert "Compte administrateur" not in web.get("/").text
    created = add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    assert "Bob peut maintenant se connecter" in created.text
    a = create_account(web, csrf, "Compte Alice")
    bob = second_browser(web, BOB, BOB_PASSWORD)
    bob_csrf = csrf_of(bob.get("/settings").text)
    b = create_account(bob, bob_csrf, "Compte Bob")
    assert "Compte Alice" in web.get("/").text and "Compte Bob" not in web.get("/").text
    assert "Compte Bob" in bob.get("/").text and "Compte Alice" not in bob.get("/").text
    # What Bob cannot see does not exist for him.
    for url in (f"/accounts/{a}", f"/accounts/{a}/edit"):
        assert bob.get(url).status_code == 404
    assert bob.post(f"/accounts/{a}/delete", data={"csrf": bob_csrf}).status_code == 404
    assert web.get(f"/accounts/{b}").status_code == 404
    assert bob.post("/login", data={"email": BOB, "password": "wrong"}).status_code == 401
    assert bob.post("/login", data={"email": "ghost@example.com", "password": BOB_PASSWORD}).status_code == 401


def test_web_sharing_read_only_then_edit_then_stop(web):
    csrf = sign_up(web)
    add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    a = create_account(web, csrf, "Compte Alice", "2500")
    bob = second_browser(web, BOB, BOB_PASSWORD)
    bob_csrf = csrf_of(bob.get("/settings").text)
    page = web.get(f"/accounts/{a}")
    assert "Partager avec" in page.text and "Bob" in page.text

    shared = web.post(f"/accounts/{a}/shares", data={"csrf": csrf, "user_id": _user_id(web, BOB), "mode": "read"})
    assert "Compte partagé avec Bob" in shared.text and "lecture seule" in shared.text
    view = bob.get(f"/accounts/{a}")
    assert view.status_code == 200 and "partagé en lecture seule" in view.text and "Quitter ce partage" in view.text
    assert f'href="/accounts/{a}/edit"' not in view.text and "Partager avec" not in view.text
    home = bob.get("/")
    assert "Compte Alice" in home.text and "partagé par" in home.text
    assert bob.get(f"/accounts/{a}/edit").status_code == 404 or "lecture seule" in bob.get(f"/accounts/{a}/edit").text
    refused = bob.post(f"/accounts/{a}/edit", data={"csrf": bob_csrf, "name": "Piraté"})
    assert "lecture seule" in refused.text or refused.status_code == 404
    assert "Piraté" not in web.get(f"/accounts/{a}").text
    assert bob.post(f"/accounts/{a}/shares", data={"csrf": bob_csrf, "user_id": _user_id(web, BOB), "mode": "edit"}).status_code == 404  # he cannot widen his own share

    web.post(f"/accounts/{a}/shares", data={"csrf": csrf, "user_id": _user_id(web, BOB), "mode": "edit"})
    view = bob.get(f"/accounts/{a}")
    assert "partagé en modification" in view.text and f'href="/accounts/{a}/edit"' in view.text
    done = bob.post(f"/accounts/{a}/edit", data={"csrf": bob_csrf, "name": "Compte commun"})
    assert "Compte mis à jour" in done.text and "Compte commun" in web.get("/").text
    assert bob.post(f"/accounts/{a}/delete", data={"csrf": bob_csrf}).status_code == 404

    web.post(f"/accounts/{a}/shares/{_user_id(web, BOB)}/delete", data={"csrf": csrf})
    assert bob.get(f"/accounts/{a}").status_code == 404 and "Compte commun" not in bob.get("/").text


def test_web_a_shared_account_can_be_left_by_the_person_it_is_shared_with(web):
    csrf = sign_up(web)
    add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    a = create_account(web, csrf, "Compte Alice")
    web.post(f"/accounts/{a}/shares", data={"csrf": csrf, "user_id": _user_id(web, BOB), "mode": "read"})
    bob = second_browser(web, BOB, BOB_PASSWORD)
    left = bob.post(f"/accounts/{a}/shares/{_user_id(web, BOB)}/delete", data={"csrf": csrf_of(bob.get("/settings").text)})
    assert "plus accès à ce compte" in left.text and bob.get(f"/accounts/{a}").status_code == 404


def _user_id(client, email) -> int:
    with client.core.db.session() as s:
        return s.scalar(select(User.id).where(User.email == email))


def test_web_documents_are_private_to_their_owner_and_to_those_the_account_is_shared_with(web):
    csrf = sign_up(web)
    add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    bob = second_browser(web, BOB, BOB_PASSWORD)
    bob_csrf = csrf_of(bob.get("/settings").text)
    upload = web.post("/documents", data={"csrf": csrf}, files={"files": ("secret.pdf", boursobank_statement_pdf(Spec(sample_entries())), "application/pdf")})
    doc = int(re.search(r"/documents/(\d+)", str(upload.url)).group(1))
    account = int(re.search(r"/accounts/(\d+)", str(web.post(f"/documents/{doc}/import/0", data={"csrf": csrf, "account_id": "new", "name": "Courant", "import_transactions": "on"}).url)).group(1))
    assert "secret.pdf" in web.get("/documents").text and "secret.pdf" not in bob.get("/documents").text
    assert bob.get(f"/documents/{doc}").status_code == 404 and bob.get(f"/documents/{doc}/download").status_code == 404
    assert bob.post(f"/documents/{doc}/delete", data={"csrf": bob_csrf}).status_code == 404
    # The same file dropped by Bob is not handed over to him.
    again = bob.post("/documents", data={"csrf": bob_csrf}, files={"files": ("copy.pdf", boursobank_statement_pdf(Spec(sample_entries())), "application/pdf")})
    assert "déjà dans le coffre" in again.text and f"/documents/{doc}" not in str(again.url)
    web.post(f"/accounts/{account}/shares", data={"csrf": csrf, "user_id": _user_id(web, BOB), "mode": "read"})
    assert bob.get(f"/documents/{doc}").status_code == 200 and bob.get(f"/documents/{doc}/download").status_code == 200
    assert bob.post(f"/documents/{doc}/delete", data={"csrf": bob_csrf}).status_code == 404  # read only


def test_web_settings_are_split_between_everyone_and_administrators(web):
    csrf = sign_up(web)
    add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    bob = second_browser(web, BOB, BOB_PASSWORD)
    bob_csrf = csrf_of(bob.get("/settings").text)
    page = bob.get("/settings").text
    assert "Mon mot de passe" in page and "Ajouter une personne" not in page and "Code de secours" not in page and "Clé API CoinGecko" not in page
    admin_page = web.get("/settings").text
    assert "Ajouter une personne" in admin_page and "Code de secours" in admin_page and "Cloudflare Access" in admin_page
    for url, data in (("/settings/users", {"email": CAROL, "password": CAROL_PASSWORD}), (f"/settings/users/{_user_id(web, ADMIN_EMAIL)}/delete", {}),
                      ("/settings/recovery", {"current": BOB_PASSWORD}), ("/settings/general", {"coingecko_api_key": "x"}),
                      (f"/settings/users/{_user_id(web, ADMIN_EMAIL)}/password", {"password": "Hacked-Password-123!"})):
        assert bob.post(url, data={"csrf": bob_csrf, **data}).status_code == 403
    # Bob's "lock" only signs him out: the vault stays open for the others.
    bob.post("/logout", data={"csrf": bob_csrf, "lock": "1"})
    assert web.core.vault.unlocked and web.get("/").status_code == 200
    # Alice's lock closes it for everyone.
    web.post("/logout", data={"csrf": csrf, "lock": "1"})
    assert not web.core.vault.unlocked


def test_web_changing_your_password_only_changes_yours_and_an_admin_can_reset_another(web):
    csrf = sign_up(web)
    add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    bob = second_browser(web, BOB, BOB_PASSWORD)
    bob_csrf = csrf_of(bob.get("/settings").text)
    new = "Bob-Brand-New-Pass-9!"
    assert "Mot de passe changé" in bob.post("/settings/password", data={"csrf": bob_csrf, "current": BOB_PASSWORD, "new": new, "confirm": new}).text
    assert "incorrect" in bob.post("/settings/password", data={"csrf": bob_csrf, "current": "wrong", "new": new, "confirm": new}).text
    web.core._failed_logins, web.core._next_login_at = 0, 0.0
    assert TestClient(web.app).post("/login", data={"email": BOB, "password": BOB_PASSWORD}).status_code == 401
    web.core._failed_logins, web.core._next_login_at = 0, 0.0
    assert TestClient(web.app).post("/login", data={"email": BOB, "password": new}, follow_redirects=False).status_code == 303
    assert TestClient(web.app).post("/login", data={"email": ADMIN_EMAIL, "password": PASSWORD}, follow_redirects=False).status_code == 303  # Alice unaffected
    # Bob forgot it: the administrator sets another one.
    reset = "Reset-By-Admin-Pass-4!"
    assert "Nouveau mot de passe défini" in web.post(f"/settings/users/{_user_id(web, BOB)}/password", data={"csrf": csrf, "password": reset}).text
    assert TestClient(web.app).post("/login", data={"email": BOB, "password": reset}, follow_redirects=False).status_code == 303


def test_web_admin_manages_users(web):
    csrf = sign_up(web)
    add_user(web, csrf, BOB, "Bob", BOB_PASSWORD)
    assert "a déjà un utilisateur" in add_user(web, csrf, BOB, "Bob 2", BOB_PASSWORD).text
    assert "Adresse e-mail invalide" in add_user(web, csrf, "nope", "X", BOB_PASSWORD).text
    assert "mot de passe" in add_user(web, csrf, CAROL, "Carol", "short").text.lower()
    bob_id = _user_id(web, BOB)
    assert "Rôle modifié" in web.post(f"/settings/users/{bob_id}/role", data={"csrf": csrf, "is_admin": "on"}).text
    bob = second_browser(web, BOB, BOB_PASSWORD)
    assert "Ajouter une personne" in bob.get("/settings").text  # his open session knows at once
    web.post(f"/settings/users/{bob_id}/role", data={"csrf": csrf})  # Bob is a plain user again
    assert "au moins un administrateur" in web.post(f"/settings/users/{_user_id(web, ADMIN_EMAIL)}/role", data={"csrf": csrf}).text
    # Bob owns an account, then is deleted: it goes to Alice, and his open session is closed.
    bob_csrf = csrf_of(bob.get("/settings").text)
    b = create_account(bob, bob_csrf, "Compte Bob")
    gone = web.post(f"/settings/users/{bob_id}/delete", data={"csrf": csrf})
    assert "Bob supprimé" in gone.text and "1 compte(s) sont maintenant à toi" in gone.text
    assert "Compte Bob" in web.get("/").text
    assert bob.get("/", follow_redirects=False).headers["location"] == "/login"


def test_web_a_vault_from_before_users_asks_for_its_first_user_after_login(settings):
    old = MyMoney(settings)
    old.vault.setup(PASSWORD)
    old.save_account(None, {"name": "Ancien compte", "account_type": "checking", "balance": "42"})
    old.vault.lock()
    app = create_app(settings)
    with TestClient(app) as client:
        login_page = client.get("/login")
        assert 'name="email"' not in login_page.text and "Mot de passe maître" in login_page.text
        done = client.post("/login", data={"password": PASSWORD}, follow_redirects=False)
        assert done.status_code == 303 and done.headers["location"] == "/first-user"
        assert client.get("/", follow_redirects=False).headers["location"] == "/first-user"  # nothing else opens before that
        page = client.get("/first-user")
        assert "multi-utilisateurs" in page.text
        bad = client.post("/first-user", data={"csrf": csrf_of(page.text), "email": "nope", "name": "X"})
        assert bad.status_code == 400 and "e-mail invalide" in bad.text
        ok = client.post("/first-user", data={"csrf": csrf_of(page.text), "email": ADMIN_EMAIL, "name": "Alice"})
        assert "Compte administrateur créé" in ok.text and "Ancien compte" in ok.text
        assert client.get("/first-user", follow_redirects=False).headers["location"] == "/"
        assert 'name="email"' in TestClient(app).get("/login").text  # from now on the login asks for the e-mail


def test_a_database_from_before_users_gets_its_new_columns():
    """An existing NAS database has no owner columns: they are added when it opens, and the rows stay."""
    from sqlalchemy import create_engine, inspect, text

    from mymoney.db import _add_missing_columns
    from mymoney.models import Base

    engine = create_engine("sqlite://")
    with engine.begin() as conn:  # the tables as an earlier version created them: no owner column
        conn.execute(text("CREATE TABLE accounts (id INTEGER PRIMARY KEY, name VARCHAR(200), balance TEXT)"))
        conn.execute(text("CREATE TABLE documents (id INTEGER PRIMARY KEY, filename VARCHAR(300))"))
        conn.execute(text("INSERT INTO accounts (id, name, balance) VALUES (1, 'Ancien compte', '42')"))
    Base.metadata.create_all(engine)  # creates the new tables (users, account_shares), leaves the old ones alone
    _add_missing_columns(engine, Base)
    inspector = inspect(engine)
    assert "owner_id" in {c["name"] for c in inspector.get_columns("accounts")} and "owner_id" in {c["name"] for c in inspector.get_columns("documents")}
    assert inspector.has_table("users") and inspector.has_table("account_shares")
    with engine.connect() as conn:
        assert conn.execute(text("SELECT name, owner_id FROM accounts")).all() == [("Ancien compte", None)]
