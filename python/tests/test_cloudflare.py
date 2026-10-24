"""Cloudflare Access: the signed token names the visitor; nothing else does."""

import json
import re
import time

import jwt
import pytest
from cryptography.hazmat.primitives.asymmetric import rsa
from fastapi.testclient import TestClient
from jwt.algorithms import RSAAlgorithm

from mymoney.web.app import create_app
from mymoney.web.cloudflare import CloudflareAccess, team_host

from .conftest import ADMIN_EMAIL, PASSWORD

TEAM, AUD = "casa", "a" * 64
ISS = f"https://{TEAM}.cloudflareaccess.com"
BOB, BOB_PASSWORD = "bob@example.com", "Another-Strong-Pass-7"


class Signer:
    """What Cloudflare is, for the tests: a key pair, the published keys, and tokens signed with it."""

    def __init__(self, kid="key-1"):
        self.kid = kid
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        self.jwk = {**json.loads(RSAAlgorithm.to_jwk(self.key.public_key())), "kid": kid, "alg": "RS256", "use": "sig"}

    def token(self, email="admin@example.com", aud=AUD, iss=ISS, exp=300, kid=None, **extra):
        now = int(time.time())
        claims = {"aud": [aud], "iss": iss, "iat": now, "exp": now + exp, "type": "app", **extra}
        if email is not None:
            claims["email"] = email
        return jwt.encode(claims, self.key, algorithm="RS256", headers={"kid": kid or self.kid})


@pytest.fixture(scope="module")
def signer():
    return Signer()


def access(signer, **kw) -> CloudflareAccess:
    return CloudflareAccess(TEAM, AUD, fetcher=lambda url: [signer.jwk])


# ------------------------------------------------------------------------------------- the verification
def test_team_name_forms_are_equivalent():
    for value in ("casa", "Casa.cloudflareaccess.com", "https://casa.cloudflareaccess.com/", " casa "):
        assert team_host(value) == "casa.cloudflareaccess.com"
    assert not CloudflareAccess(None, AUD).enabled and not CloudflareAccess(TEAM, None).enabled and not CloudflareAccess("", "").enabled
    assert CloudflareAccess(TEAM, AUD).enabled


def test_a_valid_token_names_the_visitor(signer):
    cf = access(signer)
    assert cf.verify(signer.token("Admin@Example.com")) == "admin@example.com"
    assert cf.email_of({"cf-access-jwt-assertion": signer.token(BOB)}) == BOB


def test_everything_else_is_refused(signer):
    cf = access(signer)
    other = Signer("key-1")  # same key id, another key pair: a forged signature
    for token in (
        None, "", "garbage", "a.b.c",
        signer.token(aud="b" * 64),  # a token for another Access application
        signer.token(iss="https://evil.cloudflareaccess.com"),  # another team
        signer.token(exp=-3600),  # expired
        other.token(),  # not signed by Cloudflare's key
        signer.token(email=None),  # a service token: no person behind it
        signer.token(email="not-an-email"),
        signer.token(kid="unknown-key"),
    ):
        assert cf.verify(token) is None, token
    # The classic trick: sign with HMAC using the public key as the secret, hoping the server accepts "HS256".
    forged = jwt.encode({"aud": [AUD], "iss": ISS, "exp": int(time.time()) + 300, "email": ADMIN_EMAIL}, b"public-key-bytes", algorithm="HS256", headers={"kid": "key-1"})
    assert cf.verify(forged) is None
    # And an unsigned token.
    unsigned = jwt.encode({"aud": [AUD], "iss": ISS, "exp": int(time.time()) + 300, "email": ADMIN_EMAIL}, key=None, algorithm="none", headers={"kid": "key-1"})
    assert cf.verify(unsigned) is None


def test_disabled_means_every_token_is_ignored(signer):
    assert CloudflareAccess(None, None, fetcher=lambda url: [signer.jwk]).verify(signer.token()) is None


def test_keys_are_cached_refreshed_for_a_new_key_and_survive_an_outage(signer):
    calls = []
    new = Signer("key-2")
    published = [signer.jwk]

    def fetcher(url):
        calls.append(url)
        if url.startswith("https://down"):
            raise OSError("unreachable")
        return list(published)

    cf = CloudflareAccess(TEAM, AUD, fetcher=fetcher)
    assert cf.verify(signer.token()) and cf.verify(signer.token()) and len(calls) == 1  # cached
    assert calls[0] == f"{ISS}/cdn-cgi/access/certs"
    assert cf.verify(new.token()) is None  # key-2 is not published yet
    published.append(new.jwk)
    cf._fetched_at -= 60  # (the look-up for unknown keys is rate-limited)
    assert cf.verify(new.token()) == ADMIN_EMAIL and len(calls) == 2
    down = CloudflareAccess("down", AUD, fetcher=fetcher)
    assert down.verify(signer.token(iss="https://down.cloudflareaccess.com")) is None  # no crash: the password still works


# ------------------------------------------------------------------------------------------------- web
@pytest.fixture
def web(settings, signer):
    settings.cf_team_domain, settings.cf_audience = TEAM, AUD
    app = create_app(settings)
    app.state.cf._fetcher = lambda url: [signer.jwk]
    with TestClient(app) as client:
        client.app = app
        client.core = app.state.core
        client.signer = signer
        yield client


def csrf_of(html):
    return re.search(r'name="csrf" value="([^"]+)"', html).group(1)


def setup_vault(client):
    client.post("/setup", data={"setup_token": client.core.setup_token, "email": ADMIN_EMAIL, "name": "Alice", "password": PASSWORD, "confirm": PASSWORD})
    csrf = csrf_of(client.get("/settings").text)
    client.post("/settings/users", data={"csrf": csrf, "email": BOB, "name": "Bob", "password": BOB_PASSWORD})
    client.cookies.clear()
    return csrf


def via_cloudflare(client, email, **headers) -> TestClient:
    """A browser coming through Cloudflare: its own cookies, and the token Cloudflare adds to every request."""
    other = TestClient(client.app, headers={"cf-access-jwt-assertion": client.signer.token(email), **headers})
    return other


def test_a_visitor_vouched_for_by_cloudflare_enters_without_a_password(web):
    setup_vault(web)
    alice = via_cloudflare(web, ADMIN_EMAIL)
    home = alice.get("/", follow_redirects=False)
    assert home.status_code == 200 and "Alice" in home.text and "mm_session" in home.headers.get("set-cookie", "")
    assert alice.get("/login", follow_redirects=False).headers["location"] == "/"  # nothing to type
    bob = via_cloudflare(web, BOB)
    assert bob.get("/settings").status_code == 200
    assert "Ajouter une personne" not in bob.get("/settings").text and "Bob" in bob.get("/").text  # signed in as Bob, not as Alice


def test_the_identity_follows_the_token_not_an_old_session(web):
    setup_vault(web)
    browser = via_cloudflare(web, ADMIN_EMAIL)
    assert "Ajouter une personne" in browser.get("/settings").text
    browser.headers["cf-access-jwt-assertion"] = web.signer.token(BOB)  # someone else signs in on the same browser
    assert "Ajouter une personne" not in browser.get("/settings").text


def test_an_email_without_an_account_is_turned_away(web):
    setup_vault(web)
    stranger = via_cloudflare(web, "stranger@example.com")
    denied = stranger.get("/", follow_redirects=False)
    assert denied.status_code == 403 and "stranger@example.com" in denied.text and "pas de compte" in denied.text
    assert "mm_session" not in denied.headers.get("set-cookie", "")


def test_a_forged_header_or_a_wrong_token_signs_nobody_in(web):
    setup_vault(web)
    forged = TestClient(web.app, headers={"cf-access-authenticated-user-email": ADMIN_EMAIL})  # what anyone on the LAN can send
    assert forged.get("/", follow_redirects=False).headers["location"] == "/login"
    wrong_app = via_cloudflare(web, ADMIN_EMAIL)
    wrong_app.headers["cf-access-jwt-assertion"] = web.signer.token(ADMIN_EMAIL, aud="b" * 64)
    assert wrong_app.get("/", follow_redirects=False).headers["location"] == "/login"
    expired = TestClient(web.app, headers={"cf-access-jwt-assertion": web.signer.token(ADMIN_EMAIL, exp=-100)})
    assert expired.get("/", follow_redirects=False).headers["location"] == "/login"


def test_direct_access_on_the_lan_still_works_with_the_password(web):
    setup_vault(web)
    direct = TestClient(web.app)
    assert direct.post("/login", data={"email": ADMIN_EMAIL, "password": PASSWORD}, follow_redirects=False).status_code == 303
    assert direct.get("/").status_code == 200


def test_with_the_vault_locked_cloudflare_cannot_open_it_but_prefills_the_login(web):
    setup_vault(web)
    web.core.vault.lock()
    alice = via_cloudflare(web, ADMIN_EMAIL)
    assert alice.get("/", follow_redirects=False).headers["location"] == "/login"
    page = alice.get("/login")
    assert "Cloudflare te reconnaît" in page.text and f'value="{ADMIN_EMAIL}"' in page.text and "Coffre verrouillé" in page.text
    assert alice.post("/login", data={"email": ADMIN_EMAIL, "password": PASSWORD}, follow_redirects=False).status_code == 303
    assert web.core.vault.unlocked
    # Once open, the others come in by themselves.
    assert via_cloudflare(web, BOB).get("/").status_code == 200


def test_logout_through_cloudflare_ends_the_access_session_too(web):
    setup_vault(web)
    bob = via_cloudflare(web, BOB)
    csrf = csrf_of(bob.get("/settings").text)
    out = bob.post("/logout", data={"csrf": csrf}, follow_redirects=False)
    assert out.status_code == 303 and out.headers["location"] == "/cdn-cgi/access/logout"
    direct = TestClient(web.app)  # not through Cloudflare: back to the login page
    direct.post("/login", data={"email": BOB, "password": BOB_PASSWORD})
    assert direct.post("/logout", data={"csrf": csrf_of(direct.get("/settings").text)}, follow_redirects=False).headers["location"].startswith("/login")


def test_cookie_is_secure_behind_an_https_tunnel_and_the_public_origin_is_accepted(web):
    setup_vault(web)
    # The tunnel talks plain http to the app but tells it the browser used https.
    tunnel = via_cloudflare(web, BOB, **{"x-forwarded-proto": "https"})
    cookie = tunnel.get("/").headers["set-cookie"].lower()
    assert "secure" in cookie and "httponly" in cookie and "samesite=strict" in cookie
    assert "strict-transport-security" in tunnel.get("/").headers
    plain = via_cloudflare(web, BOB)  # plain http on the LAN: no Secure flag, or the browser would drop the cookie
    assert "secure" not in plain.get("/").headers["set-cookie"].lower()
    # A form posted from the public address is accepted even though the Host the app sees is another one.
    web.core.settings.trusted_origins = "https://mymoney.example.com"
    browser = TestClient(web.app, base_url="https://testserver", headers={"cf-access-jwt-assertion": web.signer.token(BOB)})
    csrf = csrf_of(browser.get("/settings").text)
    form = {"csrf": csrf, "current": BOB_PASSWORD, "new": BOB_PASSWORD, "confirm": BOB_PASSWORD}
    assert browser.post("/settings/password", data=form, headers={"origin": "https://mymoney.example.com"}).status_code == 200
    assert browser.post("/settings/password", data=form, headers={"origin": "https://evil.example"}).status_code == 403


def test_without_cloudflare_settings_tokens_are_ignored(settings, signer):
    app = create_app(settings)  # CF not configured
    with TestClient(app) as client:
        client.post("/setup", data={"setup_token": app.state.core.setup_token, "email": ADMIN_EMAIL, "password": PASSWORD, "confirm": PASSWORD})
        stranger = TestClient(app, headers={"cf-access-jwt-assertion": signer.token(ADMIN_EMAIL)})
        assert stranger.get("/", follow_redirects=False).headers["location"] == "/login"
        assert "Désactivé" in client.get("/settings").text


def test_a_vault_from_before_users_can_be_upgraded_through_cloudflare(settings, signer):
    from mymoney.core import MyMoney

    old = MyMoney(settings)
    old.vault.setup(PASSWORD)
    old.save_account(None, {"name": "Ancien compte", "account_type": "checking", "balance": "42"})
    old.vault.lock()
    settings.cf_team_domain, settings.cf_audience = TEAM, AUD
    app = create_app(settings)
    app.state.cf._fetcher = lambda url: [signer.jwk]
    with TestClient(app, headers={"cf-access-jwt-assertion": signer.token(ADMIN_EMAIL)}) as browser:
        assert "Mot de passe maître" in browser.get("/login").text
        assert browser.post("/login", data={"password": PASSWORD}, follow_redirects=False).headers["location"] == "/first-user"
        page = browser.get("/first-user")  # not turned away: nobody has an account yet, and Cloudflare's e-mail is offered
        assert page.status_code == 200 and f'value="{ADMIN_EMAIL}"' in page.text
        done = browser.post("/first-user", data={"csrf": csrf_of(page.text), "email": ADMIN_EMAIL, "name": "Alice"})
        assert "Ancien compte" in done.text
        assert via_cloudflare_for(app, signer, ADMIN_EMAIL).get("/").status_code == 200  # and from now on, Cloudflare is enough


def via_cloudflare_for(app, signer, email):
    return TestClient(app, headers={"cf-access-jwt-assertion": signer.token(email)})
