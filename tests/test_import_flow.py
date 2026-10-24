"""The drop-a-document flow end to end, on synthetic statements only (see fixtures.py)."""

import re
from datetime import date, datetime, timedelta
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from mymoney.adapters import Adapter, Detection, SourceDocument, identify
from mymoney.core import MyMoney
from mymoney.models import Account, Counterparty, Document, Statement, Transaction, utcnow
from mymoney.services import counterparties
from mymoney.web.app import create_app

from .conftest import PASSWORD
from .fixtures import ACCOUNT, BANK, BRANCH, IBAN, JOINT_ADDRESS, JOINT_HOLDER, Entry, SavingsSpec, Spec, boursobank_savings_pdf, boursobank_statement_pdf, fake_iban, sample_entries

D = Decimal


@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.vault.setup(PASSWORD)
    return app


def september(**kw) -> Spec:
    return Spec(sample_entries(), **kw)


def october(after: Spec, entries: list[Entry] | None = None, **kw) -> Spec:
    entries = entries if entries is not None else [
        Entry(date(2026, 10, 3), ["CARTE 01/10/26 BOULANGERIE CB*1234"], D("-4.20")),
        Entry(date(2026, 10, 8), ["VIR SEPA Pauline MARTIN", "remboursement", "Rèf : SCT123450000000000000000009"], D("-30.00")),
    ]
    return Spec(entries, opening=after.closing, opening_date=date(2026, 9, 30), period=(date(2026, 10, 1), date(2026, 10, 31)), **kw)


def drop(core: MyMoney, spec: Spec, name: str = "releve.pdf") -> tuple[int, bool, str]:
    data = boursobank_statement_pdf(spec)
    doc_id, existing = core.store_document(name, data)
    return doc_id, existing, core.analyze_document(doc_id, None, data)


def first_import(core: MyMoney, spec: Spec | None = None) -> tuple[int, int]:
    spec = spec or september()
    doc_id, _, _ = drop(core, spec)
    result = core.import_statement(doc_id, 0, {
        "account_id": "new", "name": "Compte courant", "import_transactions": "on", "update_balance": "on", "balance": str(spec.closing),
    })
    return doc_id, result["account_id"]


def transactions(core: MyMoney, account_id: int | None = None) -> list[Transaction]:
    with core.db.session() as s:
        query = select(Transaction).order_by(Transaction.date, Transaction.id)
        if account_id:
            query = query.where(Transaction.account_id == account_id)
        return list(s.scalars(query))


# ------------------------------------------------------------------------------------------ first import
def test_first_statement_creates_the_account_with_its_identity(core):
    spec = september()
    doc_id, existing, status = drop(core, spec)
    assert (existing, status) == (False, "parsed")
    review = core.review_document(doc_id)
    [plan] = review["plans"]
    assert plan.account is None and not plan.classified.exact and len(plan.classified.new) == len(spec.entries)
    assert core.auto_import(doc_id) is None  # an account to create: the user is asked

    _, account_id = first_import(core, spec)
    with core.db.session() as s:
        account = s.get(Account, account_id)
        assert account.institution == "BoursoBank" and account.account_type == "checking"
        assert account.holder_name == "M DUPONT JEAN" and account.holder_address == "12 RUE DES LILAS\n75011 PARIS"
        assert (account.iban, account.bic) == (IBAN, "BOUSFRPPXXX")
        assert account.rib == f"{BANK} {BRANCH} {ACCOUNT} {account.rib_key}"
        assert account.balance == spec.closing and account.balance_date.date() == date(2026, 9, 30)
        [statement] = s.scalars(select(Statement)).all()
        assert (statement.period_start, statement.period_end) == (date(2026, 9, 1), date(2026, 9, 30))
        assert (statement.opening_balance, statement.closing_balance, statement.added_count) == (D("1000.00"), spec.closing, len(spec.entries))
    rows = transactions(core)
    assert len(rows) == len(spec.entries)
    card = next(t for t in rows if t.kind == "card")
    assert card.counterparty_name == "TRAINS DE FRANCE" and card.details["card_last4"] == "1234"
    transfer = next(t for t in rows if t.reference == "SCT123450000000000000000001")
    assert transfer.counterparty_name == "MARTIN Pauline" and transfer.memo == "cadeau anniversaire" and transfer.raw_label


def test_the_original_file_is_encrypted_on_disk_and_downloadable(core, settings):
    data = boursobank_statement_pdf(september())
    doc_id, _ = core.store_document("releve.pdf", data)
    [blob] = list(settings.documents_dir.iterdir())
    stored = blob.read_bytes()
    assert b"%PDF" not in stored and b"BOURSOBANK" not in stored and b"DUPONT" not in stored
    assert blob.name != "releve.pdf" and "releve" not in blob.name
    assert core.read_document(doc_id) == ("releve.pdf", data)


def test_the_same_file_dropped_twice_is_reported_and_not_stored_twice(core):
    data = boursobank_statement_pdf(september())
    first, existing = core.store_document("a.pdf", data)
    again, existing_again = core.store_document("b.pdf", data)
    assert (existing, existing_again) == (False, True) and first == again
    with core.db.session() as s:
        assert len(s.scalars(select(Document)).all()) == 1


# --------------------------------------------------------------------------------- following statements
def test_next_month_is_imported_by_itself_into_the_right_account(core):
    spec = september()
    _, account_id = first_import(core, spec)
    nxt = october(spec)
    doc_id, _, status = drop(core, nxt)
    assert status == "parsed"
    result = core.auto_import(doc_id)
    assert result == {"account_id": account_id, "added": 2, "skipped_exact": 0, "skipped_probable": 0}
    with core.db.session() as s:
        account = s.get(Account, account_id)
        assert account.balance == nxt.closing and account.balance_date.date() == date(2026, 10, 31)
        assert len(s.scalars(select(Statement)).all()) == 2


def test_same_statement_downloaded_again_is_flagged_as_a_duplicate(core):
    spec = september()
    _, account_id = first_import(core, spec)
    doc_id, existing, _ = drop(core, september(title="downloaded a second time"))
    assert existing is False  # another file (another hash) with the same content
    [plan] = core.review_document(doc_id)["plans"]
    assert plan.account.id == account_id
    assert plan.already_imported is not None
    assert len(plan.classified.exact) == len(spec.entries) and not plan.classified.new
    assert core.auto_import(doc_id) is None
    with pytest.raises(ValueError, match="déjà été importé"):
        core.import_statement(doc_id, 0, {"account_id": str(account_id), "import_transactions": "on"})
    result = core.import_statement(doc_id, 0, {"account_id": str(account_id), "import_transactions": "on", "force": "on"})
    assert result["added"] == 0 and result["skipped_exact"] == len(spec.entries)
    with core.db.session() as s:
        assert len(s.scalars(select(Statement)).all()) == 1
    assert len(transactions(core)) == len(spec.entries)


def test_overlapping_statement_only_adds_what_is_new(core):
    spec = september()
    _, account_id = first_import(core, spec)
    later = [e for e in sample_entries() if e.day >= date(2026, 9, 12)] + [Entry(date(2026, 10, 5), ["CARTE 03/10/26 CAFE DU COIN CB*1234"], D("-3.00"))]
    overlap = Spec(later, opening=D("500.00"), opening_date=date(2026, 9, 11), period=(date(2026, 9, 12), date(2026, 10, 15)))
    doc_id, _, _ = drop(core, overlap)
    [plan] = core.review_document(doc_id)["plans"]
    assert len(plan.overlaps) == 1 and plan.already_imported is None
    assert len(plan.classified.exact) == len(later) - 1 and len(plan.classified.new) == 1
    assert core.auto_import(doc_id) is None
    result = core.import_statement(doc_id, 0, {"account_id": str(account_id), "import_transactions": "on"})
    assert result["added"] == 1 and result["skipped_exact"] == len(later) - 1


def test_a_missing_period_is_reported(core):
    first_import(core)
    december = Spec(
        [Entry(date(2026, 12, 2), ["CARTE 01/12/26 CAFE CB*1234"], D("-3.00"))],
        opening=D("100.00"), opening_date=date(2026, 11, 30), period=(date(2026, 12, 1), date(2026, 12, 31)),
    )
    doc_id, _, _ = drop(core, december)
    [plan] = core.review_document(doc_id)["plans"]
    assert any("Il manque une période" in i and "01/10/2026" in i and "30/11/2026" in i for i in plan.issues)
    assert core.auto_import(doc_id) is None


def test_balances_must_chain_from_one_statement_to_the_next(core):
    spec = september()
    first_import(core, spec)
    nxt = october(spec)
    nxt.opening = spec.closing + D("12.00")  # does not continue the previous statement
    doc_id, _, _ = drop(core, nxt)
    [plan] = core.review_document(doc_id)["plans"]
    assert any("solde initial" in i for i in plan.issues)
    assert core.auto_import(doc_id) is None


def test_an_older_statement_does_not_overwrite_a_newer_balance(core):
    spec = september()
    _, account_id = first_import(core, spec)
    nxt = october(spec)
    core.auto_import(drop(core, nxt)[0])
    august = Spec(
        [Entry(date(2026, 8, 5), ["CARTE 04/08/26 CAFE CB*1234"], D("-3.00"))],
        opening=D("1003.00"), opening_date=date(2026, 7, 31), period=(date(2026, 8, 1), date(2026, 8, 31)),
    )
    doc_id, _, _ = drop(core, august)
    [plan] = core.review_document(doc_id)["plans"]
    assert plan.newer_balance is False
    core.import_statement(doc_id, 0, {"account_id": str(account_id), "import_transactions": "on"})
    with core.db.session() as s:
        assert s.get(Account, account_id).balance == nxt.closing


def test_lines_already_known_from_another_source_are_flagged_as_probable_duplicates(core):
    _, account_id = first_import(core)
    csv = f"IBAN;{IBAN}\nDate;Libellé;Montant\n14/09/2026;CB CARREFOUR 12/09;-14,08\n20/09/2026;AUTRE LIGNE;-1,00\n".encode()
    doc_id, _ = core.store_document("export.csv", csv)
    assert core.analyze_document(doc_id, None, csv) == "parsed"
    [plan] = core.review_document(doc_id)["plans"]
    assert plan.account.id == account_id
    assert [t.amount for t, _ in plan.classified.probable] == [D("-14.08")]
    assert plan.statuses() == ["probable", "new"]
    assert core.auto_import(doc_id) is None  # generic reading is never imported unseen
    result = core.import_statement(doc_id, 0, {"account_id": str(account_id), "import_transactions": "on"})
    assert (result["added"], result["skipped_probable"]) == (1, 1)
    doc_id2, _ = core.store_document("export2.csv", csv + b"\n")
    core.analyze_document(doc_id2, None, csv + b"\n")
    result = core.import_statement(doc_id2, 0, {"account_id": str(account_id), "import_transactions": "on", "import_probable": "on", "force": "on"})
    assert result["added"] == 1 and result["skipped_probable"] == 0  # the other probable line, explicitly kept


def test_undoing_an_import_removes_what_it_brought(core):
    doc_id, account_id = first_import(core)
    assert core.undo_import(doc_id) == len(sample_entries())
    assert transactions(core) == []
    with core.db.session() as s:
        assert s.scalars(select(Statement)).all() == []
        assert s.get(Document, doc_id).status == "parsed"
        assert s.get(Account, account_id) is not None


# ---------------------------------------------------------------------------------- choosing an adapter
class _Two(Adapter):
    id, institution, kind, label, formats = "test.a", "Acme", "statement", "Acme A", ("pdf",)

    @classmethod
    def detect(cls, doc):
        return Detection(0.85, ["looks like A"])

    def parse(self, doc):
        from mymoney.adapters import ParsedStatement

        return [ParsedStatement(source_format="pdf", institution="Acme", account_label="A")]


class _Three(_Two):
    id, label = "test.b", "Acme B"

    @classmethod
    def detect(cls, doc):
        return Detection(0.8, ["looks like B"])

    def parse(self, doc):
        from mymoney.adapters import ParsedStatement

        return [ParsedStatement(source_format="pdf", institution="Acme", account_label="B")]


def test_two_plausible_adapters_means_the_user_is_asked(core, monkeypatch):
    monkeypatch.setattr("mymoney.adapters.registry.ADAPTERS", [_Two, _Three])
    data = boursobank_statement_pdf(september())
    found = identify(SourceDocument("x.pdf", data))
    assert found.decision == "ask" and found.chosen is None and len(found.candidates) == 2

    doc_id, _ = core.store_document("x.pdf", data)
    assert core.analyze_document(doc_id, None, data) == "needs_choice"
    assert core.auto_import(doc_id) is None
    assert core.analyze_document(doc_id, "test.b", data) == "parsed"
    with core.db.session() as s:
        doc = s.get(Document, doc_id)
        assert doc.adapter_id == "test.b" and doc.parse_result["statements"][0]["account_label"] == "B"


def test_unknown_file_is_kept_but_reported(core):
    data = b"\x00\x01 not a statement"
    doc_id, _ = core.store_document("mystery.bin", data)
    assert core.analyze_document(doc_id, None, data) == "failed"
    with core.db.session() as s:
        assert "Format non reconnu" in s.get(Document, doc_id).parse_result["error"]
    assert core.read_document(doc_id)[1] == data


# --------------------------------------------------------------------------------------------- contacts
def pending(core, **kw):
    with core.db.session() as s:
        return counterparties.unresolved(s, **kw)


def test_people_and_organisations_are_listed_but_not_shops_nor_own_accounts(core):
    first_import(core)
    groups = pending(core)
    names = {g.name for g in groups}
    assert {"MARTIN Pauline", "ACME CONSULTING SAS", "EAUX DU NORD", "Revolut"} <= names
    # "LEROY CLAIRE" and "MME LEROY CLAIRE" are one person printed two ways: one line to identify, not two.
    [claire] = [g for g in groups if "LEROY" in (g.name or "")]
    assert claire.count == 2
    assert "TRAINS DE FRANCE" not in names and "DUPONT JEAN" not in names
    assert "TRAINS DE FRANCE" in {g.name for g in pending(core, include_shops=True)}


def test_a_named_contact_is_recognised_in_the_next_imports_whatever_the_spelling(core):
    spec = september()
    _, account_id = first_import(core, spec)
    with core.db.session() as s:
        cp = counterparties.identify(s, {"name": "Pauline", "kind": "person", "phone": "0600000000", "printed_name": "MARTIN Pauline"})
        contact_id = cp.id
    linked = [t for t in transactions(core) if t.counterparty_id == contact_id]
    assert [t.counterparty_name for t in linked] == ["MARTIN Pauline"]

    doc_id, _, _ = drop(core, october(spec))  # prints the same person as "Pauline MARTIN"
    core.auto_import(doc_id)
    linked = [t for t in transactions(core) if t.counterparty_id == contact_id]
    assert sorted(t.counterparty_name for t in linked) == ["MARTIN Pauline", "Pauline MARTIN"]
    with core.db.session() as s:
        assert s.get(Counterparty, contact_id).phone == "0600000000"
    assert "Pauline MARTIN" not in {g.name for g in pending(core)}


def test_an_iban_is_remembered_even_when_the_printed_name_changes(core):
    spec = september()
    garage = fake_iban(7)
    spec.entries.append(Entry(date(2026, 9, 20), ["VIR SEPA GARAGE DURAND", f"facture 123 {garage}", "Rèf : SCT123450000000000000000005"], D("-300.00")))
    first_import(core, spec)
    [group] = [g for g in pending(core) if g.iban == garage]
    assert (group.kind, group.name, group.count) == ("iban", "GARAGE DURAND", 1)

    with core.db.session() as s:
        cp = counterparties.identify(s, {"name": "Garage Durand", "kind": "company", "iban": garage, "printed_name": "GARAGE DURAND"})
        contact_id = cp.id
    other_spelling = [Entry(date(2026, 10, 9), ["VIR SEPA DURAND SARL", f"{garage} entretien"], D("-120.00"))]
    doc_id, _, _ = drop(core, october(spec, other_spelling))
    core.auto_import(doc_id)
    linked = [t for t in transactions(core) if t.counterparty_id == contact_id]
    assert {t.counterparty_name for t in linked} == {"GARAGE DURAND", "DURAND SARL"}


def test_an_iban_cannot_be_stolen_from_another_contact(core):
    first_import(core)
    iban = fake_iban(3)
    with core.db.session() as s:
        counterparties.identify(s, {"name": "Marie", "iban": iban})
    with pytest.raises(ValueError, match="Déjà associé"):
        with core.db.session() as s:
            counterparties.identify(s, {"name": "Autre", "iban": iban})
    with pytest.raises(ValueError, match="IBAN invalide"):
        with core.db.session() as s:
            counterparties.identify(s, {"name": "Autre", "iban": "FR7612345678901234567890123"})
    with core.db.session() as s:
        assert [c.name for c in s.scalars(select(Counterparty))] == ["Marie"]  # refused identifiers leave no half-made contact


def test_merging_and_deleting_contacts_keeps_the_operations(core):
    first_import(core)
    iban = fake_iban(5)
    with core.db.session() as s:
        a = counterparties.identify(s, {"name": "Claire", "printed_name": "LEROY CLAIRE"}).id
        b = counterparties.identify(s, {"name": "Claire L.", "iban": iban, "phone": "0700000000"}).id
    # "LEROY CLAIRE" and "MME LEROY CLAIRE" are the same key: both printed names land on the first contact.
    leroy = [t for t in transactions(core) if t.counterparty_name and "LEROY" in t.counterparty_name]
    assert len(leroy) == 2 and {t.counterparty_id for t in leroy} == {a}
    with core.db.session() as s:
        counterparties.merge(s, b, a)
    with core.db.session() as s:
        merged = s.get(Counterparty, a)
        assert s.get(Counterparty, b) is None
        assert {i.kind for i in merged.identifiers} == {"name", "iban"} and merged.phone == "0700000000"
    with core.db.session() as s:
        counterparties.delete_contact(s, a)
    leroy = [t for t in transactions(core) if t.counterparty_name and "LEROY" in t.counterparty_name]
    assert len(leroy) == 2 and all(t.counterparty_id is None for t in leroy)


# ------------------------------------------------------------------------------------------------- web
@pytest.fixture
def client(settings):
    app = create_app(settings)
    with TestClient(app) as c:
        c.core = app.state.core
        yield c


def csrf_of(html: str) -> str:
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def login(client) -> str:
    client.post("/setup", data={"setup_token": client.core.setup_token, "password": PASSWORD, "confirm": PASSWORD})
    return csrf_of(client.get("/documents").text)


def upload(client, csrf: str, data: bytes, name: str = "releve.pdf"):
    return client.post("/documents", data={"csrf": csrf}, files={"files": (name, data, "application/pdf")})


def test_web_drop_review_import_then_next_month_automatically(client):
    csrf = login(client)
    spec = september()
    page = upload(client, csrf, boursobank_statement_pdf(spec))
    assert page.status_code == 200
    for expected in ("M DUPONT JEAN", "12 RUE DES LILAS", IBAN, "BOUSFRPPXXX", f"{BANK} {BRANCH} {ACCOUNT}", "Valider l'import", "BoursoBank · extrait de compte mensuel"):
        assert expected in page.text, expected
    assert "les 14 opérations lues sont complètes" in page.text
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))

    done = client.post(f"/documents/{doc_id}/import/0", data={
        "csrf": csrf, "account_id": "new", "name": "Compte courant", "institution": "BoursoBank", "account_type": "checking",
        "update_balance": "on", "balance": str(spec.closing), "import_transactions": "on",
    })
    assert done.status_code == 200 and "Import terminé" in done.text
    for expected in ("Titulaire et coordonnées", "M DUPONT JEAN", f"{BANK} {BRANCH} {ACCOUNT}", "Relevés importés", "TRAINS DE FRANCE"):
        assert expected in done.text, expected

    auto = upload(client, csrf, boursobank_statement_pdf(october(spec)))
    assert "Importé automatiquement" in auto.text and "2 opération(s) ajoutée(s)" in auto.text

    again = upload(client, csrf, boursobank_statement_pdf(october(spec)), "copie.pdf")  # another download of the same statement
    assert "Relevé déjà importé" in again.text and "doublon(s) exact(s)" in again.text


def test_web_same_file_twice_and_download(client):
    csrf = login(client)
    data = boursobank_statement_pdf(september())
    first = upload(client, csrf, data)
    doc_id = int(re.search(r"/documents/(\d+)/import/0", first.text).group(1))
    second = upload(client, csrf, data, "autre-nom.pdf")
    assert "strictement identique" in second.text and f"/documents/{doc_id}/import/0" in second.text
    download = client.get(f"/documents/{doc_id}/download")
    assert download.content == data and "attachment" in download.headers["content-disposition"]


def test_web_contacts_flow(client):
    csrf = login(client)
    spec = september()
    page = upload(client, csrf, boursobank_statement_pdf(spec))
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))
    done = client.post(f"/documents/{doc_id}/import/0", data={"csrf": csrf, "account_id": "new", "name": "CC", "import_transactions": "on"})
    account_url = f"/accounts/{int(re.search(r'/accounts/(\d+)', str(done.url)).group(1))}"

    contacts = client.get("/contacts")
    assert "À identifier" in contacts.text and "MARTIN Pauline" in contacts.text
    r = client.post("/contacts/identify", data={
        "csrf": csrf, "next": account_url, "name": "Pauline", "kind": "person", "phone": "0600000000", "printed_name": "MARTIN Pauline", "iban": "",
    })
    assert r.status_code == 200 and "les prochains imports le reconnaîtront" in r.text
    assert 'href="/contacts/' in r.text and ">Pauline<" in r.text

    contact_id = re.search(r'href="/contacts/(\d+)"', r.text).group(1)
    detail = client.get(f"/contacts/{contact_id}")
    assert "0600000000" in detail.text and "MARTIN Pauline" in detail.text and "cadeau anniversaire" in detail.text  # the linked operation
    page = client.get("/contacts/identify?name=MARTIN%20Pauline&iban=&next=//evil.example")
    assert page.status_code == 200 and "MARTIN Pauline" in page.text and "evil.example" not in page.text  # only local return addresses


def test_web_unknown_file_and_choice_page(client):
    csrf = login(client)
    r = upload(client, csrf, b"nothing useful here", "notes.bin")
    assert r.status_code == 200 and "Format non reconnu" in r.text and "Changer de lecture" in r.text
    doc_id = int(re.search(r"/documents/(\d+)/analyze", r.text).group(1))
    r = client.post(f"/documents/{doc_id}/analyze", data={"csrf": csrf, "adapter": "generic.csv"})
    assert r.status_code == 200 and "Lecture impossible" in r.text


def test_web_requires_login_for_contacts_and_documents(client):
    for path in ("/contacts", "/documents", "/contacts/identify"):
        assert client.get(path, follow_redirects=False).status_code == 303


def test_web_account_identity_can_be_corrected_by_hand(client):
    csrf = login(client)
    page = upload(client, csrf, boursobank_statement_pdf(september()))
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))
    done = client.post(f"/documents/{doc_id}/import/0", data={"csrf": csrf, "account_id": "new", "name": "CC", "import_transactions": "on"})
    account_id = int(re.search(r"/accounts/(\d+)", str(done.url)).group(1))
    form = client.get(f"/accounts/{account_id}/edit")
    assert form.status_code == 200 and "M DUPONT JEAN" in form.text and "75011 PARIS" in form.text and 'name="rib_key"' in form.text
    r = client.post(f"/accounts/{account_id}/edit", data={
        "csrf": csrf, "name": "Compte courant", "account_type": "checking", "holder_name": "M DUPONT JEAN ET MME DUPONT ANNE",
        "holder_address": "3 PLACE DU MARCHE\n69001 LYON", "iban": IBAN, "bic": "BOUSFRPPXXX",
    })
    assert r.status_code == 200 and "M DUPONT JEAN ET MME DUPONT ANNE" in r.text and "69001 LYON" in r.text


def test_web_other_pages_render(client):
    csrf = login(client)
    assert client.get("/").status_code == 200 and "Déposer un document" in client.get("/").text
    assert client.get("/settings").status_code == 200
    assert "Documents pris en charge" in client.get("/documents").text and "boursobank.statement_pdf" not in client.get("/documents").text
    assert client.get("/contacts/new").status_code == 200
    created = client.post("/contacts/new", data={"csrf": csrf, "name": "Claire", "kind": "person", "phone": "0700000000", "iban": fake_iban(9)})
    assert created.status_code == 200 and "Contact créé" in created.text and fake_iban(9) in created.text
    assert client.get("/connections", follow_redirects=False).status_code == 404  # connectors are gone


def test_web_asks_which_reading_to_use_when_two_fit(client, monkeypatch):
    monkeypatch.setattr("mymoney.adapters.registry.ADAPTERS", [_Two, _Three])
    csrf = login(client)
    r = upload(client, csrf, boursobank_statement_pdf(september()))
    assert "Quelle lecture choisir ?" in r.text and "Acme A" in r.text and "Acme B" in r.text and "looks like B" in r.text
    doc_id = int(re.search(r"/documents/(\d+)/analyze", r.text).group(1))
    r = client.post(f"/documents/{doc_id}/analyze", data={"csrf": csrf, "adapter": "test.b"})
    assert r.status_code == 200 and "Valider l'import" in r.text and "Lu avec" in r.text and "Acme B" in r.text


def test_a_savings_account_without_iban_is_recognised_by_its_account_number(core):
    first_import(core)  # a current account already exists: the savings plan must not be mistaken for it
    spec = SavingsSpec([Entry(date(2026, 9, 28), ["VIR DUPONT JEAN"], D("45.00"))])
    data = boursobank_savings_pdf(spec)
    doc_id, _ = core.store_document("pel.pdf", data)
    assert core.analyze_document(doc_id, None, data) == "parsed"
    [plan] = core.review_document(doc_id)["plans"]
    assert plan.account is None and plan.candidates == []  # first time: a new account is proposed
    assert core.auto_import(doc_id) is None
    result = core.import_statement(doc_id, 0, {"account_id": "new", "name": "PEL", "account_type": "pel", "import_transactions": "on", "update_balance": "on", "balance": str(spec.closing)})
    with core.db.session() as s:
        pel = s.get(Account, result["account_id"])
        assert (pel.account_type, pel.account_number, pel.iban, pel.holder_name) == ("pel", "00099999999", None, "DUPONT JEAN")
        assert pel.institution == "BoursoBank" and pel.balance == D("3045.00")

    nxt = SavingsSpec([Entry(date(2026, 10, 28), ["VIR DUPONT JEAN"], D("45.00"))], opening=spec.closing,
                      period=(date(2026, 10, 1), date(2026, 10, 31)))
    data2 = boursobank_savings_pdf(nxt)
    doc2, _ = core.store_document("pel2.pdf", data2)
    core.analyze_document(doc2, None, data2)
    assert core.auto_import(doc2) == {"account_id": result["account_id"], "added": 1, "skipped_exact": 0, "skipped_probable": 0}
    with core.db.session() as s:
        assert s.get(Account, result["account_id"]).balance == nxt.closing
        assert len(s.scalars(select(Account)).all()) == 2


def test_joint_account_asks_for_the_full_names_once_and_recognises_transfers_between_my_accounts(core):
    D_ = D
    to_joint = Entry(date(2026, 9, 1), ["VIR Payer les factures", "M DUPONT OU MME LEROY CLAIRE"], D_("-50.00"))  # printed as the bank cuts it
    to_partner = Entry(date(2026, 9, 2), ["VIR SEPA LEROY CLAIRE", "cadeau"], D_("-20.00"))
    _, checking_id = first_import(core, Spec([to_joint, to_partner]))
    assert not any(t.internal for t in transactions(core))  # the joint account is not known yet

    joint = Spec(
        [Entry(date(2026, 9, 1), ["VIR Payer les factures", "DUPONT JEAN"], D_("50.00")),
         Entry(date(2026, 9, 3), ["VIR Virement depuis BoursoBank", "LEROY CLAIRE"], D_("200.00"))],
        holder=list(JOINT_HOLDER), address=list(JOINT_ADDRESS), bank=BANK, branch="80999", account="00077777777",
    )
    doc_id, _, _ = drop(core, joint)
    [plan] = core.review_document(doc_id)["plans"]
    assert plan.account is None and plan.ask_holder and not plan.clean
    result = core.import_statement(doc_id, 0, {
        "account_id": "new", "name": "Compte joint", "import_transactions": "on", "update_balance": "on", "balance": str(joint.closing),
        "holder_name": "M JEAN DUPONT OU MME CLAIRE LEROY",
    })
    with core.db.session() as s:
        account = s.get(Account, result["account_id"])
        assert account.holder_name == "M JEAN DUPONT OU MME CLAIRE LEROY"  # what the user typed
        assert account.holder_alias == "M DUPONT OU MME LEROY CLAIRE"  # what the bank prints, kept to recognise it elsewhere
        assert account.holder_address == "12 RUE DES LILAS\nBAT B APPT 12\n75011 PARIS"

    by_name = {(t.account_id, t.counterparty_name): t for t in transactions(core)}
    assert by_name[(checking_id, "M DUPONT OU MME LEROY CLAIRE")].internal  # the earlier transfer to the joint account is now "mine"
    assert not by_name[(checking_id, "LEROY CLAIRE")].internal  # the partner's name is not the user's
    assert by_name[(result["account_id"], "DUPONT JEAN")].internal  # "me" on the joint account, known from the other account's holder
    assert not by_name[(result["account_id"], "LEROY CLAIRE")].internal

    # Next month: no question any more, the full names survive, and it imports by itself.
    nxt = Spec([Entry(date(2026, 10, 2), ["VIR SEPA ACME CONSULTING SAS", "Salaire"], D_("100.00"))], opening=joint.closing,
               opening_date=date(2026, 9, 30), period=(date(2026, 10, 1), date(2026, 10, 31)), holder=list(JOINT_HOLDER),
               address=list(JOINT_ADDRESS), bank=BANK, branch="80999", account="00077777777")
    doc2, _, _ = drop(core, nxt)
    [plan2] = core.review_document(doc2)["plans"]
    assert plan2.account.id == result["account_id"] and not plan2.ask_holder
    assert core.auto_import(doc2)["added"] == 1
    with core.db.session() as s:
        assert s.get(Account, result["account_id"]).holder_name == "M JEAN DUPONT OU MME CLAIRE LEROY"


def test_opposite_transfers_on_two_of_my_accounts_are_paired_whatever_the_names(core):
    D_ = D
    out = Entry(date(2026, 9, 5), ["VIR Payer les factures", "COMPTE COMMUN"], D_("-50.00"))  # the other account is named by its label
    stranger = Entry(date(2026, 9, 6), ["VIR SEPA PLOMBIER SARL", "facture"], D_("-90.00"))
    _, checking_id = first_import(core, Spec([out, stranger]))
    other = Spec(
        [Entry(date(2026, 9, 6), ["VIR Virement depuis BoursoBank", "UN AUTRE NOM"], D_("50.00")),  # a day later: still the same transfer
         Entry(date(2026, 9, 7), ["VIR SEPA GARAGISTE", "pneus"], D_("-90.00"))],  # same amount as the plumber but outgoing: not a pair
        holder=["MME LEROY CLAIRE"], bank=BANK, branch="80999", account="00077777777",
    )
    doc_id, _, _ = drop(core, other)
    result = core.import_statement(doc_id, 0, {"account_id": "new", "name": "Autre", "import_transactions": "on"})
    rows = {(t.account_id, t.amount): t for t in transactions(core)}
    sent, received = rows[(checking_id, D_("-50.00"))], rows[(result["account_id"], D_("50.00"))]
    assert sent.internal and received.internal
    assert sent.details["paired_with"] == received.id and received.details["paired_with"] == sent.id
    assert not rows[(checking_id, D_("-90.00"))].internal and not rows[(result["account_id"], D_("-90.00"))].internal

    core.undo_import(doc_id)  # the other half leaves: the transfer is no longer "internal" nor paired
    freed = next(t for t in transactions(core) if t.id == sent.id)
    assert not freed.internal and "paired_with" not in (freed.details or {})


def test_web_review_asks_for_the_full_names_of_a_joint_account(client):
    csrf = login(client)
    spec = Spec(sample_entries()[:3], holder=list(JOINT_HOLDER), address=list(JOINT_ADDRESS))
    page = upload(client, csrf, boursobank_statement_pdf(spec))
    assert "Noms complets des titulaires" in page.text and "Nom du titulaire coupé par la banque" in page.text
    assert 'name="holder_name"' in page.text and "Compte joint BoursoBank" in page.text
    sole = upload(client, csrf, boursobank_statement_pdf(Spec(sample_entries()[:3], account="00099999999")))
    assert 'name="holder_name"' not in sole.text  # a name that is not cut is not asked


# ---------------------------------------------------------------------------------- duplicate files
def blobs(settings) -> list:
    return sorted(settings.documents_dir.iterdir())


def with_extra_line(**kw) -> Spec:
    return Spec(sample_entries() + [Entry(date(2026, 9, 20), ["CARTE 18/09/26 LIBRAIRIE DU COIN CB*1234"], D("-12.50"))], **kw)


def test_a_file_with_no_difference_is_set_aside_and_can_be_kept_or_dropped(core, settings):
    doc1, _ = first_import(core)
    doc2, existing, status = drop(core, september(title="downloaded again"))
    assert (existing, status) == (False, "parsed")  # another file (another hash), the same statement
    assert core.check_duplicate(doc2) == "identical"
    with core.db.session() as s:
        d = s.get(Document, doc2)
        assert d.status == "duplicate" and d.duplicate_of == doc1
    assert core.auto_import(doc2) is None
    assert len(transactions(core)) == len(sample_entries())

    core.keep_duplicate(doc2)  # "keep it anyway": a copy next to the original, nothing imported
    with core.db.session() as s:
        assert s.get(Document, doc2).status == "copy"
    assert len(transactions(core)) == len(sample_entries())

    doc3, _, _ = drop(core, september(title="and again"))
    assert core.check_duplicate(doc3) == "identical"
    before = len(blobs(settings))
    core.delete_document(doc3)  # dropped
    with core.db.session() as s:
        assert s.get(Document, doc3) is None
    assert len(blobs(settings)) == before - 1


def test_duplicates_set_aside_are_deleted_after_a_day_unless_kept(core, settings):
    first_import(core)
    old, _, _ = drop(core, september(title="old"))
    recent, _, _ = drop(core, september(title="recent"))
    kept, _, _ = drop(core, september(title="kept"))
    for doc_id in (old, recent, kept):
        assert core.check_duplicate(doc_id) == "identical"
    core.keep_duplicate(kept)
    with core.db.session() as s:
        for doc_id in (old, kept):
            s.get(Document, doc_id).uploaded_at = utcnow() - timedelta(hours=25)
    files = len(blobs(settings))
    assert core.purge_quarantine() == 1  # only the old one that nobody kept
    with core.db.session() as s:
        assert s.get(Document, old) is None and s.get(Document, recent).status == "duplicate" and s.get(Document, kept).status == "copy"
    assert len(blobs(settings)) == files - 1


def test_a_file_with_extra_lines_is_reported_and_can_be_stored_with_the_old_one(core):
    first, account_id = first_import(core)
    n = len(sample_entries())
    new_spec = with_extra_line()
    doc2, _, _ = drop(core, new_spec)
    assert core.check_duplicate(doc2) == "differs"
    [plan] = core.review_document(doc2)["plans"]
    dd = plan.dedup
    assert dd.document.id == first and dd.can_replace and not dd.identical
    assert [t.label for t in dd.extra] == ["LIBRAIRIE DU COIN"] and dd.new_count == 1 and dd.probable_count == 0
    assert dd.missing == [] and dd.opening is None and dd.closing == (september().closing, new_spec.closing)
    assert core.auto_import(doc2) is None
    with core.db.session() as s:
        assert s.get(Document, doc2).status == "parsed" and s.get(Document, doc2).duplicate_of == first  # kept for the user's decision

    result = core.import_default(doc2, 0, force=True)  # "store with the old one": both files stay, only the new line is imported
    assert (result["added"], result["skipped_exact"]) == (1, n)
    with core.db.session() as s:
        assert s.get(Document, first) is not None and s.get(Document, doc2) is not None
        assert len(s.scalars(select(Statement)).all()) == 1  # still one statement for the period
        assert s.get(Account, account_id).balance == new_spec.closing
    assert len(transactions(core)) == n + 1


def test_replacing_the_old_file_swaps_it_and_what_it_brought(core, settings):
    first, account_id = first_import(core)
    n = len(sample_entries())
    new_spec = with_extra_line()
    doc2, _, _ = drop(core, new_spec)
    core.check_duplicate(doc2)
    files = len(blobs(settings))
    result = core.replace_document(doc2, 0)
    assert (result["replaced"], result["removed"], result["added"]) == ("releve.pdf", n, n + 1)
    with core.db.session() as s:
        assert s.get(Document, first) is None  # the old file is gone...
        assert s.get(Document, doc2).status == "imported" and s.get(Document, doc2).duplicate_of is None
        assert [st.document_id for st in s.scalars(select(Statement))] == [doc2]  # ...and so is its statement record
        assert s.get(Account, account_id).balance == new_spec.closing
    assert len(blobs(settings)) == files - 1
    assert len(transactions(core)) == n + 1 and all(t.document_id == doc2 for t in transactions(core))


def test_a_corrected_file_that_lost_a_line_shows_it_as_missing_and_replacing_drops_it(core):
    first_import(core)
    n = len(sample_entries())
    corrected = Spec(sample_entries()[:-1])  # the bank removed the last line
    doc2, _, _ = drop(core, corrected)
    assert core.check_duplicate(doc2) == "differs"
    [plan] = core.review_document(doc2)["plans"]
    assert [t.label for t in plan.dedup.missing] == ["CARREFOUR CITY"] and plan.dedup.extra == []
    core.replace_document(doc2, 0)
    assert len(transactions(core)) == n - 1 and not any(t.label == "CARREFOUR CITY" for t in transactions(core))


def test_nothing_to_replace_when_the_old_file_was_deleted_but_both_can_still_be_kept(core):
    first, _ = first_import(core)
    core.delete_document(first)  # the statement record stays, its file does not
    doc2, _, _ = drop(core, with_extra_line())
    assert core.check_duplicate(doc2) == "differs"
    [plan] = core.review_document(doc2)["plans"]
    assert plan.dedup.document is None and not plan.dedup.can_replace
    with pytest.raises(ValueError, match="n'existe plus"):
        core.replace_document(doc2, 0)
    assert core.import_default(doc2, 0, force=True)["added"] == 1


def test_a_new_period_or_another_account_is_not_a_duplicate(core):
    spec = september()
    first_import(core, spec)
    nxt, _, _ = drop(core, october(spec))
    assert core.check_duplicate(nxt) is None
    other_account = Spec(sample_entries(), bank=BANK, branch="80999", account="00077777777")
    doc, _, _ = drop(core, other_account)
    assert core.check_duplicate(doc) is None  # same period but another account: nothing to compare with


def test_web_identical_download_is_set_aside_and_listed(client):
    csrf = login(client)
    spec = september()
    page = upload(client, csrf, boursobank_statement_pdf(spec))
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))
    client.post(f"/documents/{doc_id}/import/0", data={"csrf": csrf, "account_id": "new", "name": "CC", "import_transactions": "on"})

    again = upload(client, csrf, boursobank_statement_pdf(september(title="second download")), "copie.pdf")
    assert "Doublon : aucune différence" in again.text and "Le garder quand même" in again.text and "Valider l'import" not in again.text
    dup_id = int(re.search(r"/documents/(\d+)/keep", again.text).group(1))
    listing = client.get("/documents")
    assert "Doublons écartés" in listing.text and "copie.pdf" in listing.text
    assert listing.text.count("copie.pdf") == 1  # shown in the set-aside section, not in the library table

    kept = client.post(f"/documents/{dup_id}/keep", data={"csrf": csrf})
    assert "Copie conservée" in kept.text
    assert "Doublons écartés" not in client.get("/documents").text
    third = upload(client, csrf, boursobank_statement_pdf(september(title="third download")), "troisieme.pdf")
    dropped = client.post(f"/documents/{re.search(r'/documents/(\d+)/keep', third.text).group(1)}/delete", data={"csrf": csrf})
    assert "troisieme.pdf" not in dropped.text


def test_web_file_with_differences_offers_replace_keep_or_discard(client):
    csrf = login(client)
    page = upload(client, csrf, boursobank_statement_pdf(september()))
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))
    client.post(f"/documents/{doc_id}/import/0", data={"csrf": csrf, "account_id": "new", "name": "CC", "import_transactions": "on"})

    diff = upload(client, csrf, boursobank_statement_pdf(with_extra_line()))
    for expected in ("diffère", "1 opération(s) en plus", "LIBRAIRIE DU COIN", "Remplacer l'ancien", "Stocker avec l'ancien", "Écarter ce fichier", "Solde final"):
        assert expected in diff.text, expected
    new_id = int(re.search(r"/documents/(\d+)/dedup/0", diff.text).group(1))

    replaced = client.post(f"/documents/{new_id}/dedup/0", data={"csrf": csrf, "action": "replace"})
    assert "remplacé" in replaced.text and "LIBRAIRIE DU COIN" in replaced.text
    names = client.get("/documents").text
    assert names.count("releve.pdf") == 1  # one file left: the new one took the old one's place

    # Discarding a differing file leaves the old one untouched.
    other = upload(client, csrf, boursobank_statement_pdf(Spec(sample_entries()[:-1])))
    discard_id = int(re.search(r"/documents/(\d+)/dedup/0", other.text).group(1))
    out = client.post(f"/documents/{discard_id}/dedup/0", data={"csrf": csrf, "action": "discard"})
    assert "Nouveau fichier écarté" in out.text and "LIBRAIRIE DU COIN" in client.get(f"/accounts/{re.search(r'/accounts/(\d+)', str(replaced.url)).group(1)}").text


def test_web_upload_summary_counts_duplicates(client):
    csrf = login(client)
    page = upload(client, csrf, boursobank_statement_pdf(september()))
    doc_id = int(re.search(r"/documents/(\d+)/import/0", page.text).group(1))
    client.post(f"/documents/{doc_id}/import/0", data={"csrf": csrf, "account_id": "new", "name": "CC", "import_transactions": "on"})
    r = client.post("/documents", data={"csrf": csrf}, files=[
        ("files", ("a.pdf", boursobank_statement_pdf(september(title="a")), "application/pdf")),
        ("files", ("b.pdf", boursobank_statement_pdf(with_extra_line(title="b")), "application/pdf")),
    ])
    assert "1 doublon(s) sans différence écarté(s)" in r.text and "1 déjà importé(s) mais différent(s)" in r.text
