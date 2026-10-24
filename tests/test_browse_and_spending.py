"""Documents tree and grouped download, spending by category, and "already known" contacts (synthetic data only)."""

import io
import re
import zipfile
from datetime import date
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from mymoney.core import MyMoney
from mymoney.models import Account, CategoryRule, Counterparty, Transaction, User
from mymoney.services import access, analytics, categories
from mymoney.services import counterparties as contact_service
from mymoney.services import users as user_service
from mymoney.web import charts
from mymoney.web.app import create_app, document_tree, zip_documents

from .conftest import ADMIN_EMAIL, PASSWORD
from .fixtures import Entry, Spec, boursobank_statement_pdf, sample_entries
from .fixtures_fortuneo import FeesSpec, fortuneo_fees_pdf, fortuneo_info_pdf, fortuneo_september, fortuneo_statement_pdf

D = Decimal


def tx(label, amount, kind="card", **kw) -> Transaction:
    t = Transaction(label=label, amount=D(str(amount)), kind=kind, date=date(2026, 9, 5), details=kw.pop("details", None), **kw)
    t.counterparty = None
    return t


# ------------------------------------------------------------------------------------------ categories
@pytest.mark.parametrize("label, amount, kind, expected", [
    ("CARTE CARREFOUR CITY", -10, "card", "courses"), ("LIDL 1234", -20, "card", "courses"), ("Boulangerie du coin", -3, "card", "restaurants"),
    ("UBER *EATS", -18, "card", "restaurants"), ("UBER TRIP", -12, "card", "transport"), ("TRAINS DE FRANCE", -71, "card", "transport"),
    ("PRLV SEPA EAUX DU NORD", -28, "direct_debit", "energie"), ("PRLV EDF CLIENTS", -62, "direct_debit", "energie"),
    ("PRLV SEPA DIRECTION GENERALE DES FINANCES", -1396, "direct_debit", "impots"), ("NETFLIX.COM", -13, "card", "abonnements"),
    ("ORANGE SA", -20, "direct_debit", "telecom"), ("PHARMACIE DU CENTRE", -12, "card", "sante"), ("AMAZON PAYMENTS", -30, "card", "shopping"),
    ("Google Play Apps", -1, "card", "loisirs"), ("ASSURANCE HABITATION MAIF", -25, "direct_debit", "assurances"),
    ("ASSURANCE VIE VERSEMENT PERIODIQUE", -80, "direct_debit", "epargne"), ("SUSHI SHOP", -25, "card", "restaurants"),
    ("ECH PRET 123", -166, "loan_payment", "credit"), ("COTIS CARTE", -2, "fee", "frais"), ("DISTRIBUTEUR", -40, "cash", "retraits"),
    ("MARTIN Pauline cadeau", -50, "transfer", "proches"), ("PRLV INCONNU", -9, "direct_debit", "prelevements"), ("MYSTERY SHOP", -9, "card", "autres"),
    ("ACME SAS Salaire septembre", 1500, "transfer", "salaire"), ("CAF PRESTATION", 120, "transfer", "aides"), ("Remboursement Doctolib", 20, "transfer", "remboursements"),
    ("VIREMENT DE JEAN", 100, "transfer", "recus"), ("INTERETS", 1, "interest", "interets"), ("Retour achat", 15, "card", "autres_entrees"),
])
def test_the_automatic_guess(label, amount, kind, expected):
    assert categories.categorize(tx(label, amount, kind), {}) == expected


def test_a_word_is_a_whole_word():
    assert categories.categorize(tx("BPCE PRELEVEMENT", -9, "direct_debit"), {}) == "prelevements"  # "bp" is not "bpce"
    assert categories.categorize(tx("BP STATION", -50), {}) == "transport"
    assert categories.categorize(tx("FREE MOBILE", -9, "direct_debit"), {}) == "telecom"
    assert categories.categorize(tx("Transport en commun", -2), {}) == "autres"  # "sport*" starts a word, "transport" does not


def test_the_users_rule_wins_over_everything_and_internal_transfers_stay_apart():
    t = tx("CARREFOUR CITY", -10)
    key = categories.merchant(t)[0]
    assert categories.categorize(t, {key: "loisirs"}) == "loisirs"
    assert categories.categorize(tx("ECH PRET 1", -100, "loan_payment"), {categories.merchant(tx("ECH PRET 1", -100))[0]: "courses"}) == "courses"
    assert categories.categorize(t, {key: "nonsense"}) == "courses"  # a stale rule for a category that no longer exists is ignored
    assert categories.categorize(tx("VIR M DUPONT", -60, "transfer", details={"internal": True}), {}) == "interne"
    assert {o.id for o in categories.options("out")} >= {"courses", "autres", "interne"} and "salaire" not in {o.id for o in categories.options("out")}
    assert {o.id for o in categories.options("in")} >= {"salaire", "recus"}


@pytest.fixture
def core(settings):
    app = MyMoney(settings)
    app.setup(ADMIN_EMAIL, "Alice", PASSWORD)
    with app.db.session() as s:
        user_service.add_with_password(s, app.vault, "bob@example.com", "Bob", "Another-Strong-Pass-7")
    return app


def add_ops(core: MyMoney, who: str | None = None) -> int:
    ident = core.identity_for_email(who or ADMIN_EMAIL)
    with access.as_user(ident):
        account_id = core.save_account(None, {"name": "Courant", "account_type": "checking", "balance": "0"})
    rows = [
        (date(2026, 8, 3), "CARTE CARREFOUR CITY", "-40", "card"), (date(2026, 9, 2), "CARTE LIDL", "-60", "card"), (date(2026, 9, 9), "CARTE MYSTERY SHOP", "-15", "card"),
        (date(2026, 9, 10), "CARTE MYSTERY SHOP", "-5", "card"), (date(2026, 9, 12), "PRLV EDF", "-80", "direct_debit"), (date(2026, 9, 25), "ACME SAS Salaire", "2000", "transfer"),
        (date(2026, 9, 26), "VIR M DUPONT", "-300", "transfer"),
    ]
    with core.db.session() as s:
        for i, (day, label, amount, kind) in enumerate(rows):
            details = {"internal": True} if label.startswith("VIR M") else None
            s.add(Transaction(account_id=account_id, fingerprint=f"{account_id}-{i}", date=day, label=label, amount=D(amount), kind=kind, details=details, counterparty_name=label.replace("CARTE ", "")))
    return account_id


def report(core, account_id, **kw):
    args = dict(start=date(2026, 8, 1), end=date(2026, 9, 30), include_internal=False, direction="out", category=None)
    args.update(kw)
    with core.db.session() as s:
        return analytics.spending_report(s, [account_id], **args)


def test_the_report_adds_up_by_category_and_leaves_out_the_internal_transfers(core):
    a = add_ops(core)
    r = report(core, a)
    assert r.total == D("200") and r.total_in == D("2000") and r.saved == D("1800")  # the 300 between own accounts counts for nobody
    by = {c.id: c for c in r.categories}
    assert (by["courses"].total, by["courses"].count, by["energie"].total, by["autres"].total) == (D("100"), 2, D("80"), D("20"))
    assert sum(c.total for c in r.categories) == r.total and sum(c.share for c in r.categories) == pytest.approx(D(100))
    assert [c.id for c in r.categories] == ["courses", "energie", "autres"]  # biggest first
    assert [m for m, _ in r.months] == [date(2026, 8, 1), date(2026, 9, 1)]  # one entry per month of the period, even an empty one
    assert report(core, a, include_internal=True).total == D("500") and "interne" in {c.id for c in report(core, a, include_internal=True).categories}


def test_the_report_by_month_and_for_income(core):
    a = add_ops(core)
    sept = report(core, a, start=date(2026, 9, 1))
    assert sept.total == D("160") and dict(sept.months) == {date(2026, 9, 1): D("160")}
    whole = report(core, a)
    assert dict(whole.months) == {date(2026, 8, 1): D("40"), date(2026, 9, 1): D("160")}
    income = report(core, a, direction="in")
    assert income.total == D("2000") and [c.id for c in income.categories] == ["salaire"]


def test_one_category_gives_its_merchants_operations_and_months(core):
    a = add_ops(core)
    r = report(core, a, category="courses")
    assert r.category.total == D("100") and [m.label for m in r.merchants] == ["LIDL", "CARREFOUR CITY"]
    assert [(t.label, name) for t, name in r.operations] == [("CARTE LIDL", "Courant"), ("CARTE CARREFOUR CITY", "Courant")]  # newest first
    assert dict(r.months) == {date(2026, 8, 1): D("40"), date(2026, 9, 1): D("60")}  # the months of this category only
    other = report(core, a, category="autres")
    assert [(m.label, m.count) for m in other.merchants] == [("MYSTERY SHOP", 2)] and not other.merchants[0].ruled


def test_choosing_a_category_for_a_merchant_moves_all_its_operations_and_is_personal(core):
    a = add_ops(core)
    alice, bob = core.identity_for_email(ADMIN_EMAIL), core.identity_for_email("bob@example.com")
    key = report(core, a, category="autres").merchants[0].key
    with access.as_user(alice), core.db.session() as s:
        categories.set_rule(s, key, "loisirs")
    with access.as_user(alice):
        r = report(core, a)
        assert {c.id: c.total for c in r.categories}["loisirs"] == D("20") and "autres" not in {c.id for c in r.categories}
        assert report(core, a, category="loisirs").merchants[0].ruled
    with core.db.session() as s:  # Bob's own view of the same data (nothing is shared with him: use the rule table directly)
        assert [r.owner_id for r in s.scalars(select(CategoryRule))] == [alice.id]
    with access.as_user(bob), core.db.session() as s:
        assert categories.load_rules(s) == {}  # Alice's choice is not Bob's
    with access.as_user(alice), core.db.session() as s:
        categories.set_rule(s, key, "auto")  # forget it
        assert categories.load_rules(s) == {}
        with pytest.raises(ValueError, match="inconnue"):
            categories.set_rule(s, key, "nonsense")


def test_month_bars_are_drawn_with_a_tooltip():
    svg = charts.month_bars([(date(2026, 8, 1), D("40")), (date(2026, 9, 1), D("160"))], "Par mois", "neg")
    assert svg.startswith("<svg") and svg.count('class="bar-neg"') == 2 and 'class="pop"' in svg and "09/2026" in svg and "160 €" in svg
    assert charts.month_bars([(date(2026, 8, 1), D("0"))]) == "" and charts.month_bars([]) == ""


# ------------------------------------------------------------------------------------------------- web
@pytest.fixture
def web(settings):
    app = create_app(settings)
    with TestClient(app) as client:
        client.core = app.state.core
        yield client


def csrf_of(html):
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def sign_up(client):
    client.post("/setup", data={"setup_token": client.core.setup_token, "email": ADMIN_EMAIL, "name": "Alice", "password": PASSWORD, "confirm": PASSWORD})
    return csrf_of(client.get("/documents").text)


def test_web_spending_by_category_with_drill_down_and_recategorising(web):
    csrf = sign_up(web)
    a = add_ops(web.core)
    page = web.get("/analytics?vue=depenses&periode=tout")
    assert "Où est parti" in page.text and "par catégorie" in page.text and "Courses" in page.text and "Énergie et eau" in page.text and "Autres dépenses" in page.text
    assert "cat=courses" in page.text and "Plus gros bénéficiaires" in page.text and "Dépenses par mois" in page.text
    assert "Revenus" in page.text and "Solde de la période" in page.text
    # Inside a category: who, when, and a way to change it.
    drill = web.get("/analytics?vue=depenses&periode=tout&cat=autres")
    assert "Toutes les catégories" in drill.text and "MYSTERY SHOP" in drill.text and 'name="category"' in drill.text and "Changer" in drill.text
    done = web.post("/analytics/categorize", data={"csrf": csrf, "key": categories.merchant(tx("MYSTERY SHOP", -1, counterparty_name="MYSTERY SHOP"))[0], "label": "MYSTERY SHOP", "category": "loisirs", "next": "/analytics?vue=depenses&periode=tout"})
    assert "classé dans" in done.text and "Loisirs et culture" in done.text and "cat=autres" not in done.text
    again = web.get("/analytics?vue=depenses&periode=tout&cat=loisirs")
    assert "à ta façon" in again.text and "↺ Automatique" in again.text
    bad = web.post("/analytics/categorize", data={"csrf": csrf, "key": "x", "category": "nonsense", "next": "//evil.example"})
    assert "inconnue" in bad.text and "evil.example" not in str(bad.url)
    assert web.post("/analytics/categorize", data={"key": "x", "category": "loisirs"}).status_code == 403  # CSRF


def test_web_income_view_months_and_links_from_the_flows(web):
    sign_up(web)
    add_ops(web.core)
    income = web.get("/analytics?vue=depenses&periode=tout&sens=entrees")
    assert "il vient, par catégorie" in income.text and "Salaire et revenus" in income.text and "Reçu" in income.text
    flows = web.get("/analytics?vue=flux&periode=tout")
    assert "vue=depenses" in flows.text and "mois=2026-09" in flows.text and "où est parti" in flows.text.lower()
    month = web.get("/analytics?vue=depenses&periode=tout&mois=2026-09")
    assert "Mois : 09/2026" in month.text and "CARREFOUR" not in month.text and "160,00" in month.text  # the August purchase is out
    assert "Mois :" not in web.get("/analytics?vue=depenses&periode=tout&mois=nonsense").text
    empty = web.get("/analytics?vue=depenses&periode=tout&mois=2020-01")
    assert empty.status_code == 200 and "Aucune opération" in empty.text
    assert web.get("/analytics?vue=depenses&cat=nonsense").status_code == 200  # an unknown category is just ignored


def test_the_account_page_names_the_category_of_each_operation(web):
    sign_up(web)
    a = add_ops(web.core)
    page = web.get(f"/accounts/{a}")
    assert "Courses" in page.text and "Énergie et eau" in page.text


# ----------------------------------------------------------------------------------------- documents
class Doc:
    def __init__(self, id, name, institution, adapter, kind, day, size=2048, status="imported"):
        from datetime import datetime

        self.id, self.filename, self.adapter_id, self.kind, self.size, self.status = id, name, adapter, kind, size, status
        self.parse_result = {"institution": institution, "statements": [{"period_end": day.isoformat()}] if day else []}
        self.uploaded_at = datetime(2026, 10, 1)


def test_documents_are_grouped_by_bank_then_type_then_year_newest_first():
    docs = [
        Doc(1, "a.pdf", "BoursoBank", "x", "statement", date(2026, 9, 30)), Doc(2, "b.pdf", "BoursoBank", "x", "statement", date(2025, 12, 31)),
        Doc(3, "c.pdf", "BoursoBank", "x", "savings_statement", date(2026, 6, 30)), Doc(4, "d.pdf", "Fortuneo", "x", "fees_summary", date(2023, 12, 31)),
        Doc(5, "e.pdf", None, None, "statement", None), Doc(6, "f.pdf", "Fortuneo", "x", "statement", date(2026, 10, 31)),
        Doc(7, "g.pdf", "BoursoBank", "x", "statement", date(2026, 3, 31)), Doc(8, "h.pdf", "Fortuneo", None, "statement", None),
    ]
    tree = document_tree(docs)
    assert [b["label"] for b in tree] == ["BoursoBank", "Fortuneo", "Autres documents"] and [b["count"] for b in tree] == [4, 3, 1]
    bourso = tree[0]
    assert [k["label"] for k in bourso["kinds"]] == ["Relevé d'épargne", "Relevé de compte"]
    statements = bourso["kinds"][1]
    assert [(y, [d.id for d, _ in rows]) for y, rows in statements["years"]] == [(2026, [1, 7]), (2025, [2])]
    assert [k["label"] for k in tree[1]["kinds"]] == ["Récapitulatif des frais", "Relevé de compte", "Non reconnu"]  # unrecognised files last
    assert tree[2]["kinds"][0]["label"] == "Non reconnu"


def drop_documents(web, csrf):
    ids = {}
    from mymoney.models import Document

    for name, data in (("sept.pdf", boursobank_statement_pdf(Spec(sample_entries()))), ("fortuneo.pdf", fortuneo_statement_pdf(fortuneo_september())),
                       ("frais.pdf", fortuneo_fees_pdf(FeesSpec())), ("fgdr.pdf", fortuneo_info_pdf())):
        web.post("/documents", data={"csrf": csrf}, files={"files": (name, data, "application/pdf")})
        with web.core.db.session() as s:
            ids[name] = s.scalar(select(Document.id).where(Document.filename == name))
        if name in ("sept.pdf", "fortuneo.pdf"):
            web.post(f"/documents/{ids[name]}/import/0", data={"csrf": csrf, "account_id": "new", "name": name, "import_transactions": "on"})
    return ids


def test_web_documents_page_is_a_tree_you_can_tick(web):
    csrf = sign_up(web)
    ids = drop_documents(web, csrf)
    page = web.get("/documents").text
    assert page.count('class="doc-bank"') == 2 and "BoursoBank" in page and "Fortuneo" in page
    assert "Récapitulatif des frais" in page and "Information" in page and "Relevé de compte" in page
    assert page.count('name="ids"') == 4 and 'id="download-selected"' in page and "data-check-group" in page
    assert "2026" in page and 'action="/documents/download"' in page
    assert re.search(r'<details class="doc-bank" open>', page)  # the first bank is open, the others folded


def test_web_download_one_file_as_it_is_or_several_as_a_zip(web):
    csrf = sign_up(web)
    ids = drop_documents(web, csrf)
    one = web.post("/documents/download", data={"csrf": csrf, "ids": [str(ids["sept.pdf"])]})
    assert one.status_code == 200 and one.content.startswith(b"%PDF") and "sept.pdf" in one.headers["content-disposition"]
    many = web.post("/documents/download", data={"csrf": csrf, "ids": [str(ids["sept.pdf"]), str(ids["frais.pdf"]), str(ids["sept.pdf"]), "junk"]})
    assert many.headers["content-type"] == "application/zip" and ".zip" in many.headers["content-disposition"]
    archive = zipfile.ZipFile(io.BytesIO(many.content))
    assert sorted(archive.namelist()) == ["frais.pdf", "sept.pdf"] and all(archive.read(n).startswith(b"%PDF") for n in archive.namelist())  # asked twice, sent once
    assert "au moins un document" in web.post("/documents/download", data={"csrf": csrf}).text
    assert web.post("/documents/download", data={"ids": [str(ids["sept.pdf"])]}).status_code == 403  # CSRF
    assert web.post("/documents/download", data={"csrf": csrf, "ids": ["99999"]}).status_code == 404


def test_web_download_refuses_documents_one_may_not_open(settings):
    app = create_app(settings)
    with TestClient(app) as alice:
        alice.core = app.state.core
        csrf = sign_up(alice)
        ids = drop_documents(alice, csrf)
        alice.post("/settings/users", data={"csrf": csrf, "email": "bob@example.com", "name": "Bob", "password": "Another-Strong-Pass-7"})
        bob = TestClient(app)
        bob.post("/login", data={"email": "bob@example.com", "password": "Another-Strong-Pass-7"})
        bob_csrf = csrf_of(bob.get("/documents").text)
        assert bob.post("/documents/download", data={"csrf": bob_csrf, "ids": [str(ids["sept.pdf"])]}).status_code == 404
        assert bob.post("/documents/download", data={"csrf": bob_csrf, "ids": [str(ids["sept.pdf"]), str(ids["frais.pdf"])]}).status_code == 404  # nothing partial


def test_zip_keeps_two_files_with_the_same_name():
    data = zipfile.ZipFile(io.BytesIO(zip_documents([("releve.pdf", b"1"), ("releve.pdf", b"2"), ("RELEVE.PDF", b"3"), ("sans-extension", b"4"), ("a/b.pdf", b"5")])))
    assert sorted(data.namelist()) == ["RELEVE (3).PDF", "a_b.pdf", "releve (2).pdf", "releve.pdf", "sans-extension"]


# ------------------------------------------------------------------------------------------- contacts
def make_contact(core, name="Pauline") -> int:
    with core.db.session() as s:
        cp = contact_service.save_contact(s, None, {"name": name, "kind": "person"})
        contact_service.add_identifier(s, cp, "name", "MARTIN Pauline", "MARTIN Pauline")
        return cp.id


def test_identify_with_an_existing_contact_adds_the_new_name_and_iban_to_it(core):
    from .fixtures import fake_iban

    a = add_ops(core)
    iban = fake_iban(3)
    with core.db.session() as s:
        s.add(Transaction(account_id=a, fingerprint="x1", date=date(2026, 9, 20), label="VIR SEPA PAULINE M", amount=D("-30"), kind="transfer", counterparty_name="PAULINE M", counterparty_iban=iban))
        s.add(Transaction(account_id=a, fingerprint="x2", date=date(2026, 9, 21), label="VIR SEPA PAULINE M", amount=D("-10"), kind="transfer", counterparty_name="PAULINE M", counterparty_iban=iban))
    contact_id = make_contact(core)
    with core.db.session() as s:
        assert any(g.iban == iban for g in contact_service.unresolved(s))
        cp = contact_service.identify(s, {"contact_id": str(contact_id), "iban": iban, "printed_name": "PAULINE M"})
        assert cp.id == contact_id
        kinds = {i.kind for i in cp.identifiers}
        assert kinds == {"iban", "name"} and len(cp.identifiers) == 3  # her old printed name, the new one, and the IBAN
        assert not any(g.iban == iban for g in contact_service.unresolved(s))  # both operations are linked now
        assert len(s.scalars(select(Transaction).where(Transaction.counterparty_id == contact_id)).all()) == 2
    with core.db.session() as s:
        with pytest.raises(ValueError, match="nom, ou choisis un contact"):
            contact_service.identify(s, {"iban": "", "printed_name": "SOMEONE", "name": "  "})
        with pytest.raises(ValueError, match="introuvable"):
            contact_service.identify(s, {"contact_id": "99999", "printed_name": "X"})
        assert [name for _, name in contact_service.choices(s)] == ["Pauline"]


def test_web_quick_form_offers_the_contacts_already_known(web):
    csrf = sign_up(web)
    a = add_ops(web.core)
    first = web.get("/contacts").text
    assert 'data-inline-contact' not in first and "Nouveau contact…" not in first  # nobody known yet: only the new-contact fields
    contact_id = make_contact(web.core, "Pauline Martin")
    page = web.get("/contacts").text
    assert 'data-inline-contact' in page and f'<option value="{contact_id}">Pauline Martin</option>' in page and "Nouveau contact…" in page
    assert page.count("data-inline-contact") >= 2
    # The same person under another name: pick her instead of creating a second contact.
    done = web.post("/contacts/identify", data={"csrf": csrf, "next": "/contacts", "contact_id": str(contact_id), "iban": "", "printed_name": "MYSTERY SHOP"})
    assert "enregistré" in done.text and "Pauline Martin" in done.text
    detail = web.get(f"/contacts/{contact_id}").text
    assert "MYSTERY SHOP".lower() in detail.lower() or "mystery" in detail.lower()
    nothing = web.post("/contacts/identify", data={"csrf": csrf, "next": "/contacts", "iban": "", "printed_name": "ACME SAS", "name": ""})
    assert "choisis un contact déjà enregistré" in nothing.text
