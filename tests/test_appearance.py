"""Appearance: copy buttons, "hide sensitive data", colour themes and the logo."""

import io
import re
from decimal import Decimal

import pytest
from fastapi.testclient import TestClient

from mymoney.web import themes
from mymoney.web.app import create_app, iban, identity_text, money, money_text, quantity
from mymoney.web.pwa import render_icon

from .conftest import PASSWORD
from .fixtures import IBAN, Spec, boursobank_statement_pdf, sample_entries

D = Decimal


@pytest.fixture
def client(settings):
    app = create_app(settings)
    with TestClient(app) as c:
        c.core = app.state.core
        yield c


def csrf_of(html):
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def login(client):
    client.post("/setup", data={"setup_token": client.core.setup_token, "password": PASSWORD, "confirm": PASSWORD})
    return csrf_of(client.get("/documents").text)


def account_page(client) -> tuple[str, str]:
    csrf = login(client)
    review = client.post("/documents", data={"csrf": csrf}, files={"files": ("releve.pdf", boursobank_statement_pdf(Spec(sample_entries())), "application/pdf")})
    doc = re.search(r"/documents/(\d+)", str(review.url)).group(1)
    done = client.post(f"/documents/{doc}/import/0", data={"csrf": csrf, "account_id": "new", "name": "Compte courant", "import_transactions": "on"})
    return re.search(r"/accounts/(\d+)", str(done.url)).group(1), done.text


# ------------------------------------------------------------------------------------------------- themes
def luminance(color: str) -> float:
    r, g, b = (int(color.lstrip("#")[i : i + 2], 16) / 255 for i in (0, 2, 4))
    f = lambda c: c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)


def contrast(a: str, b: str) -> float:
    hi, lo = sorted((luminance(a), luminance(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def test_there_are_at_least_ten_light_and_ten_dark_themes():
    light = [t for t in themes.THEMES if t.kind == "light"]
    dark = [t for t in themes.THEMES if t.kind == "dark"]
    assert len(light) >= 10 and len(dark) >= 10
    ids = [t.id for t in themes.THEMES]
    assert len(ids) == len(set(ids)) and all(re.fullmatch(r"[a-z-]+", i) for i in ids)  # ids are safe in an attribute and in localStorage
    assert [title for title, _ in themes.groups()] == ["Clairs", "Sombres"]
    assert all(t.bg != u.bg or t.surface != u.surface for i, t in enumerate(themes.THEMES) for u in themes.THEMES[i + 1 :])  # no two identical


@pytest.mark.parametrize("theme", themes.THEMES, ids=lambda t: t.id)
def test_every_theme_is_readable(theme):
    assert theme.text and len(theme.series) == 8
    for background in (theme.bg, theme.surface):
        assert contrast(theme.text, background) >= 7, "text"
        assert contrast(theme.muted, background) >= 4.5, "muted text"
        for name in ("primary", "pos", "neg", "warn"):  # used as text colours (links, amounts, warnings)
            assert contrast(getattr(theme, name), background) >= 4.5, name
    assert contrast(theme.primary_text, theme.primary) >= 4.5  # text of the main buttons
    assert all(contrast(color, theme.surface) >= 3 for color in theme.series)  # chart colours on cards
    assert (luminance(theme.bg) < 0.2) == (theme.kind == "dark")


def test_the_stylesheet_has_one_rule_per_theme_and_is_public(client):
    css = client.get("/themes.css")  # served before login: the login page uses the saved style too
    assert css.status_code == 200 and css.headers["content-type"].startswith("text/css")
    for theme in themes.THEMES:
        rule = re.search(rf'\[data-theme="{theme.id}"\] \{{([^}}]*)\}}', css.text).group(1)
        for variable in ("bg", "surface", "text", "muted", "border", "primary", "primary-text", "pos", "neg", "warn", "s0", "s7", "cat-dettes"):
            assert f"--{variable}:" in rule
        assert f"color-scheme: {theme.kind}" in rule and theme.bg in rule
    assert "<" not in css.text and "url(" not in css.text and "@import" not in css.text


def test_pages_load_the_themes_and_the_scripts_without_anything_inline(client):
    login_page = client.get("/login")
    for page in (login_page, client.get("/setup")):
        assert '<link rel="stylesheet" href="/themes.css">' in page.text and '<script src="/static/theme.js"></script>' in page.text
    assert client.get("/static/theme.js").status_code == 200
    script = client.get("/static/theme.js").text
    assert "mymoney-theme" in script and "mymoney-privacy" in script
    csrf = login(client)
    home = client.get("/")
    assert " style=" not in home.text and "<script>" not in home.text  # strict Content-Security-Policy
    assert "style-src 'self'" in home.headers["content-security-policy"]
    for theme in themes.THEMES:
        assert f'data-set-theme="{theme.id}"' in home.text and theme.label in home.text
    assert 'data-set-theme="auto"' in home.text and 'id="privacy-toggle"' in home.text and csrf
    assert 'class="dots"' not in home.text and "<i></i>" not in home.text  # swatches are plain: no decorative dots


def test_app_js_keeps_the_choices_on_the_device(client):
    js = client.get("/static/app.js").text
    for expected in ("mymoney-theme", "mymoney-privacy", "data-set-theme", "classList.toggle(\"privacy\")", "navigator.clipboard", "execCommand(\"copy\")"):
        assert expected in js


# ----------------------------------------------------------------------------------- sensitive data
def test_amounts_are_marked_as_sensitive_in_the_page_but_not_in_attributes():
    assert str(money(D("1234.5"))) == '<span class="sensitive">1 234,50 €</span>'
    assert money_text(D("-1234.5")) == "-1 234,50 €" and str(money(None)) == '<span class="sensitive">—</span>'
    assert str(quantity(D("0.5"))) == '<span class="sensitive">0,5</span>'
    assert "<" not in money_text(D(1))


def test_the_privacy_mode_blurs_what_is_sensitive():
    css = open("mymoney/web/static/style.css", encoding="utf-8").read()
    rule = re.search(r"\.privacy \.sensitive, ([^{]*)\{([^}]*)\}", css)
    assert "filter: blur" in rule.group(2) and "user-select: none" in rule.group(2)
    for target in (".privacy code", ".privacy .chart-svg", ".privacy .donut-svg", ".privacy .spark"):
        assert target in rule.group(0)
    assert ".privacy .chart-svg .pop { display: none !important; }" in css  # the tooltips of the charts show amounts too


def test_identifiers_names_and_labels_carry_the_sensitive_class(client):
    account_id, _ = account_page(client)
    page = client.get(f"/accounts/{account_id}").text
    assert '<span class="sensitive">M DUPONT JEAN</span>' in page and '<span class="sensitive">12 RUE DES LILAS' in page
    assert page.count('class="sensitive"') >= 10  # amounts, holder, address, operation labels
    assert "<code>FR76" in page  # IBAN and RIB are in <code>, blurred by the privacy rule
    assert re.search(r'<td class="sensitive-cell"><span class="sensitive">', page)
    docs = client.get("/documents").text
    assert 'class="sensitive">releve.pdf' in docs


# ----------------------------------------------------------------------------------------- copy buttons
def test_iban_and_identity_text_helpers():
    assert iban("FR7612345678900001234567858") == "FR76 1234 5678 9000 0123 4567 858" and iban(None) == ""

    class Account:
        holder_name, iban, bic, rib = "M DUPONT JEAN", IBAN, "BOUSFRPPXXX", "12345 67890 00012345678 58"

    assert identity_text(Account) == f"Titulaire : M DUPONT JEAN\nIBAN : {iban(IBAN)}\nBIC : BOUSFRPPXXX\nRIB : 12345 67890 00012345678 58"


def test_the_account_page_has_a_copy_button_for_each_piece_of_identity(client):
    account_id, _ = account_page(client)
    page = client.get(f"/accounts/{account_id}").text
    copies = re.findall(r'data-copy="([^"]*)"', page)
    assert IBAN in copies  # the IBAN is copied without spaces: safe in any form
    assert "BOUSFRPPXXX" in copies and "12345 67890 00012345678 58" in copies and "M DUPONT JEAN" in copies
    assert any("12 RUE DES LILAS" in c and "75011 PARIS" in c for c in copies)
    everything = next(c for c in copies if c.startswith("Titulaire"))
    assert f"IBAN : {iban(IBAN)}" in everything and "BIC : BOUSFRPPXXX" in everything and "RIB : 12345" in everything
    assert "Tout copier" in page and "FR76 1234 5678 9000" in page  # displayed in groups of four
    assert page.count('class="btn small copy"') >= 6


def test_contacts_have_copy_buttons_for_their_iban(client):
    csrf = login(client)
    done = client.post("/contacts/new", data={"csrf": csrf, "name": "Pauline", "kind": "person", "iban": IBAN})
    assert done.status_code == 200
    assert f'data-copy="{IBAN}"' in done.text or f'data-copy="{IBAN}"' in client.get("/contacts").text


# ------------------------------------------------------------------------------------------------ logo
def test_the_logo_is_served_and_shown(client):
    logo = client.get("/static/logo.svg")
    assert logo.status_code == 200 and logo.headers["content-type"].startswith("image/svg") and "linearGradient" in logo.text
    assert client.get("/static/icon.svg").text == logo.text
    assert 'src="/static/logo.svg"' in client.get("/login").text.replace("\n", "") or 'src="/static/logo.svg"' in client.get("/setup").text
    login(client)
    home = client.get("/").text
    assert 'class="logo"' in home and '<span class="wordmark">my<b>Money</b></span>' in home


def test_the_app_icons_are_drawn_from_the_same_artwork():
    from PIL import Image

    for size, maskable in ((192, False), (512, False), (512, True)):
        image = Image.open(io.BytesIO(render_icon(size, maskable))).convert("RGBA")
        assert image.size == (size, size)
        top_left = image.getpixel((int(size * 0.2), int(size * 0.2)))
        bottom_right = image.getpixel((int(size * 0.9), int(size * 0.9))) if maskable else image.getpixel((int(size * 0.85), int(size * 0.85)))
        assert top_left[2] > top_left[0] and bottom_right[1] > top_left[1]  # indigo at the top left, teal at the bottom right
        centre_stroke = image.getpixel((int(size * 0.5), int(size * (0.7 if not maskable else 0.62))))
        assert min(centre_stroke[:3]) > 200  # the white stroke of the "m"
    assert render_icon(192, False)[:8] == b"\x89PNG\r\n\x1a\n"
