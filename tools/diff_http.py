"""Differential test of the web interface: the Python version (python/) and the Rust version run side by side on
fresh vaults, receive exactly the same requests, and every answer is compared (status, redirect, content type and
the page itself, byte for byte once the per-server secrets are masked).

    python3 tools/diff_http.py [--rust target/release/mymoney] [--docs DIR ...] [--work DIR] [--keep]

The documents given with --docs are uploaded one by one, reviewed and validated the way a browser would (the form
defaults read from the page), then the whole app is exercised: accounts, holdings, contacts, categories, gaps,
duplicates, invalidation, rebuild, users and shares, settings, errors. Nothing leaves the machine: both servers are
pointed at a dead proxy, so price lookups fail the same way on both.
"""

from __future__ import annotations

import argparse
import difflib
import html.parser
import http.client
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import uuid
from pathlib import Path
from urllib.parse import urlencode, urlsplit, parse_qsl, unquote

ROOT = Path(__file__).resolve().parent.parent
EMAIL, PASSWORD = "admin@example.org", "Correct-Horse-Battery-9"
SECOND, SECOND_PASSWORD = "lea@example.org", "Another-Long-Passw0rd!"


# ------------------------------------------------------------------------------------------ servers
class Server:
    def __init__(self, name: str, cmd: list[str], cwd: Path, data: Path, port: int, extra_env: dict | None = None):
        self.name, self.port, self.data = name, port, data
        env = dict(os.environ)
        env.update(extra_env or {})
        env.update(
            MYMONEY_DATA_DIR=str(data), MYMONEY_ARGON2_MEMORY_KIB="8192", MYMONEY_ARGON2_TIME_COST="1", MYMONEY_VERSION="diff",
            HTTPS_PROXY="http://127.0.0.1:9", HTTP_PROXY="http://127.0.0.1:9", https_proxy="http://127.0.0.1:9", http_proxy="http://127.0.0.1:9",
            ALL_PROXY="http://127.0.0.1:9", NO_PROXY="", no_proxy="", MYMONEY_LOG_LEVEL="INFO", TZ=os.environ.get("TZ", "Europe/Paris"),
        )
        self.log_path = data.parent / f"{name}.log"
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(cmd + ["--port", str(port)], cwd=cwd, env=env, stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True)
        for _ in range(200):
            try:
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/healthz")
                if c.getresponse().status == 200:
                    break
            except OSError:
                time.sleep(0.1)
        else:
            raise SystemExit(f"{name} did not start, see {self.log_path}")

    def setup_token(self) -> str:
        text = self.log_path.read_text()
        return re.findall(r"code d'installation = (\S+)", text)[-1]

    def stop(self):
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        self.proc.wait(timeout=10)


PY_LAUNCHER = """
import os, sys
version = os.environ.get("DIFF_READER_VERSION")
if version:
    import mymoney.adapters, mymoney.adapters.registry, mymoney.core
    for module in (mymoney.adapters, mymoney.adapters.registry, mymoney.core):
        module.reader_version = lambda: version
    import mymoney.web.app
    mymoney.web.app.reader_version = lambda: version
from mymoney.cli import main
sys.argv = ["mymoney"] + sys.argv[1:]
main()
"""


class Client:
    """A browser: a cookie, the CSRF token of the last page, requests without following redirects."""

    def __init__(self, server: Server):
        self.server = server
        self.cookie: str | None = None
        self.csrf = ""
        self.last_html = ""

    def request(self, method: str, url: str, body: bytes | None = None, headers: dict | None = None):
        c = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=600)
        h = {"Host": f"127.0.0.1:{self.server.port}"}
        if self.cookie:
            h["Cookie"] = f"mm_session={self.cookie}"
        h.update(headers or {})
        c.request(method, url, body=body, headers=h)
        r = c.getresponse()
        data = r.read()
        hdrs = [(k.lower(), v) for k, v in r.getheaders()]
        for k, v in hdrs:
            if k == "set-cookie" and v.startswith("mm_session="):
                value = v.split(";", 1)[0].split("=", 1)[1]
                self.cookie = None if value in ('""', "") else value
        resp = Resp(r.status, hdrs, data)
        if resp.ctype.startswith("text/html"):
            self.last_html = resp.text
            m = re.search(r'name="csrf" value="([^"]+)"', resp.text)
            if m:
                self.csrf = m.group(1)
        return resp

    def get(self, url: str):
        return self.request("GET", url)

    def post(self, url: str, fields: list[tuple[str, str]] | dict, csrf: bool = True, files: list[tuple[str, str, bytes]] | None = None):
        pairs = list(fields.items()) if isinstance(fields, dict) else list(fields)
        if csrf:
            pairs = [("csrf", self.csrf)] + pairs
        if files is not None:
            boundary = "----diff" + uuid.uuid4().hex
            out = b""
            for k, v in pairs:
                out += f"--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n".encode() + v.encode() + b"\r\n"
            for k, name, data in files:
                quoted = name.replace('"', "%22")
                out += f"--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"; filename=\"{quoted}\"\r\nContent-Type: application/octet-stream\r\n\r\n".encode() + data + b"\r\n"
            out += f"--{boundary}--\r\n".encode()
            return self.request("POST", url, out, {"Content-Type": f"multipart/form-data; boundary={boundary}", "Origin": f"http://127.0.0.1:{self.server.port}"})
        body = urlencode(pairs).encode()
        return self.request("POST", url, body, {"Content-Type": "application/x-www-form-urlencoded", "Origin": f"http://127.0.0.1:{self.server.port}"})


class Resp:
    def __init__(self, status, headers, body):
        self.status, self.headers, self.body = status, headers, body

    def header(self, name):
        return next((v for k, v in self.headers if k == name), None)

    @property
    def ctype(self):
        return self.header("content-type") or ""

    @property
    def text(self):
        return self.body.decode("utf-8", "replace")

    @property
    def location(self):
        return self.header("location")


# ------------------------------------------------------------------------------------- comparing
def normalize(text: str) -> str:
    text = re.sub(r'(name="csrf" value=")[^"]+"', r'\1CSRF"', text)
    text = re.sub(r"(lecture )[0-9a-f]{6,}", r"\1VERSION", text)
    text = re.sub(r"(\d\d/\d\d/\d{4}) \d\d:\d\d", r"\1 HH:MM", text)
    text = re.sub(r'(<p class="recovery"><code>)[^<]+', r"\1RECOVERY", text)
    text = re.sub(r"(réessaie dans )\d+( s)", r"\1N\2", text)
    text = re.sub(r"--[0-9a-f]{26}", "--BOUNDARY", text)
    return text


def zip_entries(data: bytes):
    import io
    import zipfile
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        return [(i.filename, z.read(i.filename)) for i in z.infolist()]


TOLERANT = False  # set by --tolerant


class Checker:
    """Compares the two versions' answers. In `tolerant` mode (since the Rust version grew features the Python one does
    not have) differences are only listed, and a request fails only when Rust crashes (5xx) or refuses what Python
    answered."""

    def __init__(self, tolerant: bool | None = None):
        self.count = 0
        self.failures: list[str] = []
        self.tolerant = TOLERANT if tolerant is None else tolerant
        self.notes = 0

    def compare(self, label: str, a: Resp, b: Resp) -> bool:
        self.count += 1
        problems = []
        if a.status != b.status:
            problems.append(f"status {a.status} != {b.status}")
        if (a.location or "") != (b.location or ""):
            problems.append(f"location {a.location!r} != {b.location!r}")
        for h in ("content-type", "content-disposition", "allow", "cache-control", "service-worker-allowed", "strict-transport-security", "content-security-policy",
                  "x-frame-options", "content-range", "accept-ranges"):
            ha, hb = (re.sub(r"boundary=[0-9a-f]{26}", "boundary=B", x) if x else x for x in (a.header(h), b.header(h)))
            if ha != hb:
                problems.append(f"{h} {a.header(h)!r} != {b.header(h)!r}")
        def cookies(r):
            out = [v for k, v in r.headers if k == "set-cookie"]
            out = [re.sub(r"^mm_session=[A-Za-z0-9_-]{20,}", "mm_session=TOKEN", v) for v in out]
            return [re.sub(r"expires=[^;]+", "expires=DATE", v) for v in out]
        if cookies(a) != cookies(b):
            problems.append(f"set-cookie {cookies(a)} != {cookies(b)}")
        if a.ctype.startswith(("text/", "application/json", "application/manifest", "multipart/")):
            ta, tb = normalize(a.text), normalize(b.text)
            if ta != tb:
                diff = "\n".join(list(difflib.unified_diff(ta.splitlines(), tb.splitlines(), "python", "rust", lineterm="", n=1))[:60])
                problems.append("body differs:\n" + diff)
        elif a.ctype == "application/zip":
            if zip_entries(a.body) != zip_entries(b.body):
                problems.append(f"zip contents differ: {[n for n, _ in zip_entries(a.body)]} / {[n for n, _ in zip_entries(b.body)]}")
        elif not a.ctype.startswith("multipart/") and a.body != b.body:
            problems.append(f"binary body differs ({len(a.body)} / {len(b.body)} bytes)")
        if problems and self.tolerant:
            self.notes += 1
            if b.status >= 500 or (a.status < 400 <= b.status):
                self.failures.append(f"### {label}\nstatus {a.status} (python) / {b.status} (rust)\n" + "\n".join(problems))
                print(f"FAIL {label}: status {a.status} / {b.status}", flush=True)
                return False
            return True
        if problems:
            self.failures.append(f"### {label}\n" + "\n".join(problems))
            print(f"DIFF {label}: {problems[0][:200]}", flush=True)
            return False
        return True


# ---------------------------------------------------------------------------------------- forms
class FormReader(html.parser.HTMLParser):
    """The forms of a page with what a browser would send by default."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.forms: list[dict] = []
        self.current: dict | None = None
        self.select: dict | None = None
        self.textarea: dict | None = None

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "form":
            self.current = {"attrs": a, "fields": [], "selects": []}
            self.forms.append(self.current)
            return
        f = self.current
        if f is None:
            return
        if tag == "input":
            kind = (a.get("type") or "text").lower()
            if not a.get("name") or kind in ("submit", "file", "button"):
                return
            if kind in ("checkbox", "radio") and "checked" not in a:
                return
            value = a.get("value", "on" if kind in ("checkbox", "radio") else "")
            f["fields"].append([a["name"], value])
        elif tag == "select" and a.get("name"):
            self.select = {"name": a["name"], "options": [], "chosen": None}
            f["selects"].append(self.select)
        elif tag == "option" and self.select is not None:
            self.select["options"].append(a.get("value", ""))
            if "selected" in a:
                self.select["chosen"] = a.get("value", "")
        elif tag == "textarea" and a.get("name"):
            self.textarea = {"name": a["name"], "value": ""}

    def handle_data(self, data):
        if self.textarea is not None:
            self.textarea["value"] += data

    def handle_endtag(self, tag):
        if tag == "select" and self.select is not None:
            s = self.select
            value = s["chosen"] if s["chosen"] is not None else (s["options"][0] if s["options"] else "")
            self.current["fields"].append([s["name"], value])
            self.select = None
        elif tag == "textarea" and self.textarea is not None and self.current is not None:
            self.current["fields"].append([self.textarea["name"], self.textarea["value"]])
            self.textarea = None
        elif tag == "form":
            self.current = None


def forms_of(page: str) -> list[dict]:
    r = FormReader()
    r.feed(page)
    return r.forms


def form_with_action(page: str, pattern: str) -> list[dict]:
    return [f for f in forms_of(page) if re.search(pattern, f["attrs"].get("action", ""))]


# ------------------------------------------------------------------------------------------ the tour
class Migration:
    """Both servers open a copy of the same existing data (made by the Python version) and must show the same pages."""

    def __init__(self, py: Server, rs: Server, password: str):
        self.tour = Tour(py, rs, [])
        self.password = password

    def run(self):
        t = self.tour
        t.get("/login", "locked login")
        ra, rb = t.both("login", lambda c: c.post("/login", {"email": EMAIL, "password": self.password}, csrf=False))
        if ra.location != "/" or rb.location != "/":
            raise SystemExit(f"login failed: {ra.status} {ra.location} / {rb.status} {rb.location}")
        t.pages("migrated")
        t.analytics("migrated")
        ra, _ = t.get("/documents", "migrated library")
        for i in sorted({int(x) for x in re.findall(r'href="/documents/(\d+)"', ra.text)}):
            t.get(f"/documents/{i}", f"migrated document {i}")
        accounts = sorted({int(x) for x in re.findall(r'href="/accounts/(\d+)"', t.peek("/").text)})
        for i in accounts:
            t.get(f"/accounts/{i}", f"migrated account {i}")
            t.get(f"/accounts/{i}/edit", f"migrated account form {i}")
        ra, _ = t.get("/contacts", "migrated contacts")
        for i in sorted({int(x) for x in re.findall(r'href="/contacts/(\d+)"', ra.text)}):
            t.get(f"/contacts/{i}", f"migrated contact {i}")
        t.post("/documents/rebuild", {}, "migrated rebuild")
        t.walk_review()
        t.pages("migrated after rebuild")
        t.analytics("migrated after rebuild")


LEGACY_MAKER = """
import sys
from pathlib import Path
from mymoney.config import Settings
from mymoney.core import MyMoney
settings = Settings(data_dir=Path(sys.argv[1]), argon2_memory_kib=8192, argon2_time_cost=1)
core = MyMoney(settings)
core.vault.setup(sys.argv[2])  # the layout from before users: one master password, nobody signed up
core.save_account(None, {"name": "Ancien compte", "account_type": "checking", "balance": "42"})
for path in sys.argv[3:]:
    data = Path(path).read_bytes()
    doc_id, existing = core.store_document(Path(path).name, data)
    if existing:
        continue
    if core.analyze_document(doc_id, None, data) == "parsed" and core.check_duplicate(doc_id) is None:
        core.auto_import(doc_id)
core.vault.lock()
"""


class Legacy:
    """A vault from before users existed: the master password opens it, then the first user is named."""

    def __init__(self, py: Server, rs: Server, password: str):
        self.tour = Tour(py, rs, [])
        self.password = password

    def run(self):
        t = self.tour
        t.get("/login", "legacy login page")
        t.get("/", "legacy /")
        t.both("legacy login wrong", lambda c: c.post("/login", {"password": "nope"}, csrf=False))
        time.sleep(1.2)
        ra, rb = t.both("legacy login", lambda c: c.post("/login", {"password": self.password}, csrf=False))
        if ra.location != "/first-user" or rb.location != "/first-user":
            raise SystemExit(f"legacy login failed: {ra.status} {ra.location} / {rb.status} {rb.location}")
        for url in ("/", "/documents", "/settings", "/first-user", "/login"):
            t.get(url, f"before first user {url}")
        t.post("/first-user", {"email": "pas-un-email", "name": "Alice"}, "first user bad email")
        ra, rb = t.post("/first-user", {"email": " Admin@Example.org ", "name": "Alice"}, "first user")
        t.follow(ra, rb, "after first user")
        t.post("/first-user", {"email": "other@example.org"}, "first user twice")
        t.get("/first-user", "first user page after")
        t.pages("legacy")
        t.analytics("legacy")
        accounts = sorted({int(x) for x in re.findall(r'href="/accounts/(\d+)"', t.peek("/").text)})
        for i in accounts:
            t.get(f"/accounts/{i}", f"legacy account {i}")
        ra, _ = t.get("/documents", "legacy library")
        for i in sorted({int(x) for x in re.findall(r'href="/documents/(\d+)"', ra.text)}):
            t.get(f"/documents/{i}", f"legacy document {i}")
        ra, rb = t.post("/logout", {}, "legacy logout")
        time.sleep(1.2)
        ra, rb = t.both("login as the first user", lambda c: c.post("/login", {"email": EMAIL, "password": self.password}, csrf=False))
        t.follow(ra, rb, "first user signed in")
        t.pages("first user")


class Tour:
    def __init__(self, py: Server, rs: Server, docs: list[Path]):
        self.py, self.rs = py, rs
        self.check = Checker()
        self.docs = docs
        self.a = {"py": Client(py), "rs": Client(rs)}

    def both(self, label: str, fn, compare=True) -> tuple[Resp, Resp]:
        ra = fn(self.a["py"])
        rb = fn(self.a["rs"])
        if compare:
            self.check.compare(label, ra, rb)
        self.network(label)
        return ra, rb

    def network(self, label: str):
        """Both versions must call the price services the same number of times (free tiers are rate-limited)."""
        counts = []
        for server in (self.py, self.rs):
            server.log.flush()
            text = server.log_path.read_text(errors="replace")
            counts.append(len(re.findall(r"CoinGecko indisponible|FX \S+ indisponible", text)))
        last = getattr(self, "_last_counts", [0, 0])
        if os.environ.get("DIFF_NETWORK_LOG") and counts != last:
            with open(os.environ["DIFF_NETWORK_LOG"], "a") as fh:
                fh.write(f"{counts[0] - last[0]} {counts[1] - last[1]} {label}\n")
        # After « invalidate all » the Rust version removes the accounts an import made (the Python version keeps them and
        # re-imports into them): it then has fewer positions to price. Fewer calls is the intended difference.
        pruned = getattr(self, "rust_prunes", False) and counts[1] <= counts[0]
        if counts[0] != counts[1] and counts != getattr(self, "_last_counts", None) and not pruned:
            self.check.failures.append(f"### {label}\nprice calls {counts[0]} != {counts[1]}")
            print(f"DIFF {label}: price calls {counts[0]} != {counts[1]}", flush=True)
        self._last_counts = counts

    def peek(self, url: str):
        """A page read to find what to do next: asked of both servers (and compared), Python's answer returned."""
        return self.get(url, f"peek {url}")[0]

    def get(self, url: str, label: str | None = None):
        return self.both(label or f"GET {url}", lambda c: c.get(url))

    def post(self, url: str, fields, label: str | None = None, **kw):
        return self.both(label or f"POST {url} {fields if not isinstance(fields, dict) else list(fields.items())}"[:160], lambda c: c.post(url, fields, **kw))

    def follow(self, ra: Resp, rb: Resp, label: str):
        """GET the page both redirects point to (the flash message included)."""
        if ra.status in (302, 303, 307) and ra.location and ra.location == rb.location and not ra.location.startswith("http"):
            return self.get(ra.location, f"{label} → {ra.location[:80]}")
        return ra, rb

    def pages(self, label: str):
        for url in ["/", "/documents", "/contacts", "/contacts?all=1", "/settings"]:
            self.get(url, f"{label} {url}")

    def analytics(self, label: str):
        base = ["vue=evolution", "vue=evolution&par=compte", "vue=evolution&par=categorie", "vue=evolution&periode=tout", "vue=evolution&periode=1m",
                "vue=repartition", "vue=repartition&par=compte", "vue=flux", "vue=flux&interne=1", "vue=flux&periode=tout",
                "vue=depenses", "vue=depenses&periode=tout", "vue=depenses&sens=entrees&periode=tout", "vue=depenses&periode=tout&interne=1",
                "vue=depenses&periode=6m&cat=autres", "vue=depenses&periode=tout&cat=frais", "vue=depenses&periode=tout&cat=courses",
                "vue=depenses&mois=2024-02&periode=tout", "vue=depenses&mois=2024-13", "vue=depenses&mois=x", "vue=xx&periode=yy&sens=zz&par=ww",
                "vue=evolution&c=1", "vue=evolution&c=1&c=2&c=999", "vue=depenses&c=2&periode=tout", "c=abc", "interne=x"]
        for q in base:
            self.get(f"/analytics?{q}", f"{label} analytics {q}")

    def run(self):
        g, p = self.get, self.post
        # -------------------------------------------------------------- before setup
        for url in ["/", "/login", "/setup", "/healthz", "/themes.css", "/manifest.webmanifest", "/sw.js", "/static/style.css",
                    "/static/app.js", "/static/theme.js", "/static/logo.svg", "/static/icon.svg", "/static/nope.css", "/icons/icon-192.png",
                    "/icons/icon-512.png", "/icons/maskable-512.png", "/icons/nope.png", "/nope", "/documents/", "/settings/"]:
            g(url)
        self.both("HEAD /static/style.css", lambda c: c.request("HEAD", "/static/style.css"))
        self.both("POST /static/style.css", lambda c: c.request("POST", "/static/style.css", b"", {"Content-Type": "application/x-www-form-urlencoded"}))
        for rng in ("bytes=0-9", "bytes=-5", "bytes=10-", "bytes=0-1,5-7", "bytes=0-5,3-9", "bytes=5-1", "bytes=99999-", "bytes=abc", "items=0-1", "bytes=", "bytes=0-1," * 101):
            self.both(f"GET /static/style.css Range {rng[:40]}", lambda c, r=rng: c.request("GET", "/static/style.css", None, {"Range": r}))
        self.both("Range with a stale If-Range", lambda c: c.request("GET", "/static/style.css", None, {"Range": "bytes=0-9", "If-Range": '"stale"'}))
        p("/setup", {"setup_token": "wrong", "email": EMAIL, "name": "Admin", "password": PASSWORD, "confirm": PASSWORD}, csrf=False)
        self.both("POST /setup mismatch", lambda c: c.post("/setup", {"setup_token": c.server.setup_token(), "email": EMAIL, "password": PASSWORD, "confirm": "x"}, csrf=False))
        self.both("POST /setup weak", lambda c: c.post("/setup", {"setup_token": c.server.setup_token(), "email": EMAIL, "password": "short", "confirm": "short"}, csrf=False))
        self.both("POST /setup bad email", lambda c: c.post("/setup", {"setup_token": c.server.setup_token(), "email": "nope", "password": PASSWORD, "confirm": PASSWORD}, csrf=False))
        ra, rb = self.both("POST /setup", lambda c: c.post("/setup", {"setup_token": c.server.setup_token(), "email": EMAIL, "name": "Admin", "password": PASSWORD, "confirm": PASSWORD}, csrf=False))
        self.recovery = {k: re.search(r'<p class="recovery"><code>([^<]+)', r.text).group(1) for k, r in (("py", ra), ("rs", rb)) if "recovery" in r.text}
        g("/setup", "GET /setup after setup")
        g("/login", "GET /login signed in")
        # -------------------------------------------------------------- empty app
        self.pages("empty")
        self.analytics("empty")
        for url in ["/accounts/new", "/contacts/new", "/contacts/identify?name=Jean%20Dupont&iban=FR7630004000031234567890143&next=/contacts",
                    "/contacts/identify?next=//evil", "/accounts/1", "/accounts/abc", "/accounts/1/edit", "/documents/1", "/documents/abc",
                    "/documents/download", "/documents/review", "/contacts/1", "/first-user", "/documents?account_id=1&start=2024-01-01&end=2024-01-31"]:
            g(url)
        # -------------------------------------------------------------- csrf, origin, methods
        p("/accounts/new", {"name": "X"}, "POST without csrf", csrf=False)
        self.both("POST bad origin", lambda c: c.request("POST", "/accounts/new", b"name=x", {"Content-Type": "application/x-www-form-urlencoded", "Origin": "https://evil.example"}))
        self.both("DELETE /accounts/new", lambda c: c.request("DELETE", "/accounts/new"))
        self.both("POST /healthz", lambda c: c.request("POST", "/healthz", b"", {"Content-Type": "application/x-www-form-urlencoded"}))
        self.both("HEAD /", lambda c: c.request("HEAD", "/"))
        # -------------------------------------------------------------- manual accounts
        accounts = [
            {"name": "Livret A", "account_type": "livret_a", "institution": "Banque Postale", "balance": "1 234,56", "currency": "eur", "interest_rate": "", "holder_name": "Jean Exemple", "iban": "FR76 3000 4000 0312 3456 7890 143", "bic": "PSSTFRPPXXX", "bank_code": "30004", "branch_code": "00031", "account_number": "23456789012", "rib_key": "43", "notes": "Mon livret\nligne 2"},
            {"name": "Crypto", "account_type": "crypto_wallet", "institution": "", "balance": "0", "currency": "EUR"},
            {"name": "Appartement", "account_type": "real_estate", "balance": "250000", "currency": "EUR", "notes": "<b>pas de html</b>"},
            {"name": "Compte USD", "account_type": "checking", "balance": "1000.50", "currency": "usd"},
            {"name": "Prêt auto", "account_type": "loan", "balance": "-5000", "currency": "EUR", "interest_rate": "3,5"},
            {"name": "", "account_type": "nope", "balance": "abc"},
        ]
        for a in accounts:
            ra, rb = p("/accounts/new", a)
            self.follow(ra, rb, "account created")
        g("/accounts/1")
        g("/accounts/1/edit")
        ra, rb = p("/accounts/1/edit", {"name": "Livret A perso", "account_type": "livret_a", "balance": "2000", "currency": "EUR", "archived": "on"})
        self.follow(ra, rb, "edit")
        p("/accounts/1/edit", {"name": "Livret A perso", "account_type": "livret_a", "balance": "2000", "currency": "EUR"})
        ra, rb = p("/accounts/1/edit", {"balance": "1,2,3"})
        self.follow(ra, rb, "bad edit")
        for h in [{"symbol": "BTC", "quantity": "0,5", "asset_class": "crypto", "name": "Bitcoin"},
                  {"isin": "FR0000120271", "quantity": "10", "asset_class": "stock"},
                  {"symbol": "XYZ", "quantity": "3", "asset_class": "other", "value": "1500"},
                  {"symbol": "", "quantity": "abc"}]:
            ra, rb = p("/accounts/2/holdings", h)
            self.follow(ra, rb, "holding")
        g("/accounts/2")
        p("/holdings/1/delete", {"account_id": "2"})
        g("/accounts/2")
        # behind a reverse proxy that talks HTTPS to the browser
        for headers in ({"Host": "nas.local:8000"}, {"Host": "nas.local", "X-Forwarded-Proto": "https"}, {"Host": "nas.local", "X-Forwarded-Proto": "https, http"},
                        {"Host": "nas.local", "Cf-Visitor": '{"scheme":"https"}'}, {"Host": "nas.local", "X-Forwarded-Proto": "HTTPS"}):
            for url in ("/documents/", "/accounts/2/", "/", "/healthz"):
                self.both(f"GET {url} {headers}", lambda c, u=url, h=headers: c.request("GET", u, None, h))
        g("/accounts/3")
        g("/accounts/4")
        g("/accounts/5")
        self.pages("manual")
        self.analytics("manual")
        # -------------------------------------------------------------- documents
        big = os.urandom(26 * 1024 * 1024)
        ra, rb = self.both("upload too big", lambda c: c.post("/documents", [], files=[("files", "enorme.pdf", big)]))
        self.follow(ra, rb, "after too big")
        uploaded = 0
        for path in self.docs:
            data = path.read_bytes()
            ra, rb = self.both(f"upload {path.name}", lambda c: c.post("/documents", [], files=[("files", path.name, data)]))
            uploaded += 1
            self.follow(ra, rb, f"after upload {path.name}")
            m = re.match(r"/documents/(\d+)", ra.location or "")
            if m:
                self.review(int(m.group(1)), path.name)
            if uploaded % 10 == 0:
                self.pages(f"after {uploaded} uploads")
        # several files at once, and nothing at all
        if len(self.docs) >= 3:
            files = [("files", d.name, d.read_bytes()) for d in self.docs[:3]]
            ra, rb = self.both("upload three again", lambda c: c.post("/documents", [], files=files))
            self.follow(ra, rb, "after multi upload")
        ra, rb = self.both("upload nothing", lambda c: c.post("/documents", [], files=[]))
        self.follow(ra, rb, "after empty upload")
        self.both("upload empty file", lambda c: c.post("/documents", [], files=[("files", "", b"")]))
        self.walk_review()
        self.pages("after documents")
        self.analytics("after documents")
        self.accounts_and_documents()
        self.contacts()
        self.categories()
        self.gaps()
        self.dedup_and_invalidate()
        self.users_and_shares()
        self.settings()
        self.analytics("final")
        self.pages("final")
        self.logout_and_login()

    # ------------------------------------------------------------------------------------- reviews
    def review(self, doc_id: int, name: str):
        """Open a document, then validate it with the form defaults (as clicking "Valider" would)."""
        ra, rb = self.get(f"/documents/{doc_id}?review=1", f"review {name}")
        page = ra.text
        for idx_form in form_with_action(page, rf"/documents/{doc_id}/import/\d+$"):
            action = idx_form["attrs"]["action"]
            fields = [(k, v) for k, v in idx_form["fields"] if k != "csrf"]
            ra, rb = self.post(action, fields, f"import {name} {action}")
            self.follow(ra, rb, f"after import {name}")
            break
        else:
            for f in form_with_action(page, rf"/documents/{doc_id}/analyze$"):
                choice = [(k, v) for k, v in f["fields"] if k == "adapter"]
                if choice:
                    ra, rb = self.post(f"/documents/{doc_id}/analyze", choice, f"choose reading {name}")
                    self.follow(ra, rb, f"after choice {name}")
                break

    def walk_review(self):
        ra, rb = self.get("/documents/review", "review start")
        seen = 0
        while ra.location and ra.location.startswith("/documents/") and "?review=1" in ra.location and seen < 400:
            seen += 1
            doc = int(re.match(r"/documents/(\d+)", ra.location).group(1))
            pa, pb = self.get(ra.location, f"review page {doc}")
            forms = form_with_action(pa.text, rf"/documents/{doc}/import/\d+$")
            if forms:
                fields = [(k, v) for k, v in forms[0]["fields"] if k != "csrf"]
                ra, rb = self.post(forms[0]["attrs"]["action"], fields, f"review import {doc}")
            else:
                ra, rb = self.get(f"/documents/review?after={doc}", f"review skip {doc}")
        self.follow(ra, rb, "review end")

    # ---------------------------------------------------------------------------------- accounts
    def accounts_and_documents(self):
        ra, _ = self.get("/documents", "library")
        ids = sorted({int(x) for x in re.findall(r'href="/documents/(\d+)"', ra.text)})
        for i in ids:
            self.get(f"/documents/{i}", f"document {i}")
        accounts = sorted({int(x) for x in re.findall(r'href="/accounts/(\d+)"', self.peek("/").text)})
        for i in accounts:
            for page in (1, 2):
                self.get(f"/accounts/{i}?page={page}", f"account {i} p{page}")
        for i in accounts[:3]:
            self.get(f"/accounts/{i}?page=x", f"account {i} bad page")
        if ids:
            first = ids[0]
            self.get(f"/documents/{first}/download")
            self.post("/documents/download", [("ids", str(ids[0]))], "download one", csrf=True)
            self.post("/documents/download", [("ids", str(i)) for i in ids[:4]] + [("ids", "abc"), ("ids", str(ids[0]))], "download zip")
            self.post("/documents/download", [], "download none")
            self.post("/documents/download", [("ids", "1")], "download bad csrf", csrf=False)
        # loan first due date
        for i in accounts:
            if "/loan/first-due" in self.peek(f"/accounts/{i}").text:
                ra, rb = self.post(f"/accounts/{i}/loan/first-due", {"first_due": "2023-05-05"}, f"first due {i}")
                self.follow(ra, rb, "first due")
                self.post(f"/accounts/{i}/loan/first-due", {"first_due": "bad"}, f"first due bad {i}")

    # ---------------------------------------------------------------------------------- contacts
    def contacts(self):
        ra, _ = self.get("/contacts?all=1", "contacts all")
        forms = form_with_action(ra.text, r"^/contacts/identify$")
        for n, f in enumerate(forms[:6]):
            fields = [(k, v) for k, v in f["fields"] if k != "csrf"]
            fields = [(k, (f"Contact {n}" if k == "name" else v)) for k, v in fields]
            ra2, rb2 = self.post("/contacts/identify", fields, f"identify {n}")
            self.follow(ra2, rb2, f"after identify {n}")
        p = self.post
        ra, rb = p("/contacts/new", {"name": "Marie Exemple", "kind": "person", "phone": "0600000000", "email": "marie@example.org", "iban": "FR1420041010050500013M02606", "printed_name": "MME MARIE EXEMPLE", "notes": "amie"})
        self.follow(ra, rb, "contact created")
        p("/contacts/new", {"name": "", "kind": "person"}, "contact without name")
        ra, _ = self.get("/contacts", "contacts")
        ids = sorted({int(x) for x in re.findall(r'href="/contacts/(\d+)"', ra.text)})
        for i in ids:
            self.get(f"/contacts/{i}", f"contact {i}")
        if ids:
            c = ids[-1]
            ra, rb = p(f"/contacts/{c}", {"name": "Marie E.", "kind": "company", "phone": "", "email": "", "notes": ""})
            self.follow(ra, rb, "contact saved")
            for kind, value in (("iban", "FR7630006000011234567890189"), ("name", "MARIE E"), ("iban", "pas un iban"), ("iban", "")):
                ra, rb = p(f"/contacts/{c}/identifiers", {"kind": kind, "value": value}, f"identifier {kind} {value}")
                self.follow(ra, rb, "identifier")
            page = self.peek(f"/contacts/{c}").text
            idents = re.findall(rf"/contacts/{c}/identifiers/(\d+)/delete", page)
            if idents:
                p(f"/contacts/{c}/identifiers/{idents[0]}/delete", {}, "identifier removed")
            if len(ids) >= 2:
                p(f"/contacts/{ids[0]}/merge", {"target": ""}, "merge without target")
                ra, rb = p(f"/contacts/{ids[0]}/merge", {"target": str(c)}, "merge")
                self.follow(ra, rb, "merged")
            ra, rb = p(f"/contacts/{c}/delete", {}, "contact deleted")
            self.follow(ra, rb, "contact deleted")
        self.get("/contacts/9999", "contact missing")

    def categories(self):
        ra, _ = self.get("/analytics?vue=depenses&periode=tout&cat=autres", "categories page")
        forms = form_with_action(ra.text, r"^/analytics/categorize$")
        for n, f in enumerate(forms[:4]):
            fields = [(k, v) for k, v in f["fields"] if k != "csrf"]
            fields = [(k, ("courses" if k == "category" else v)) for k, v in fields]
            ra2, rb2 = self.post("/analytics/categorize", fields, f"categorize {n}")
            self.follow(ra2, rb2, f"after categorize {n}")
        self.post("/analytics/categorize", {"key": "x", "category": "nope", "next": "/analytics"}, "categorize unknown")
        ra2, rb2 = self.post("/analytics/categorize", {"key": "x", "category": "auto", "label": "", "next": "//evil"}, "categorize auto")
        self.follow(ra2, rb2, "after auto")
        self.analytics("after categories")

    def gaps(self):
        ra, _ = self.get("/", "dashboard gaps")
        for f in form_with_action(ra.text, r"^/gaps/acknowledge$")[:2]:
            fields = [(k, v) for k, v in f["fields"] if k != "csrf"]
            ra2, rb2 = self.post("/gaps/acknowledge", fields, "gap acknowledged")
            self.follow(ra2, rb2, "after gap")
        self.post("/gaps/acknowledge", {"account_id": "1", "start": "2024-02-30", "end": "2024-03-01", "next": "/"}, "gap bad date")
        self.post("/gaps/acknowledge", {"start": "2024-01-01"}, "gap missing")
        self.post("/gaps/acknowledge", {"account_id": "1", "start": "2024-03-01", "end": "2024-01-01", "next": "/accounts/1"}, "gap reversed")
        accounts = sorted({int(x) for x in re.findall(r'href="/accounts/(\d+)"', self.peek("/").text)})
        for i in accounts:
            page = self.peek(f"/accounts/{i}").text
            for ack in re.findall(r'/gaps/(\d+)/restore', page)[:1]:
                ra2, rb2 = self.post(f"/gaps/{ack}/restore", {"next": f"/accounts/{i}"}, f"gap restored {ack}")
                self.follow(ra2, rb2, "after restore")

    def dedup_and_invalidate(self):
        ra, _ = self.get("/documents", "library before invalidation")
        page = ra.text
        dups = re.findall(r'/documents/(\d+)/keep', page)
        for d in dups[:1]:
            self.post(f"/documents/{d}/keep", {}, f"keep {d}")
        for d in dups[1:2]:
            self.post(f"/documents/{d}/delete", {}, f"delete dup {d}")
        ids = sorted({int(x) for x in re.findall(r'name="ids" value="(\d+)"', page)})
        for i in ids:
            text = self.peek(f"/documents/{i}").text
            for action in ("replace", "keep", "copy", "discard"):
                if f'value="{action}"' in text:
                    ra2, rb2 = self.post(f"/documents/{i}/dedup/0", {"action": action}, f"dedup {action} {i}")
                    self.follow(ra2, rb2, f"after dedup {i}")
                    break
        self.post(f"/documents/{ids[0] if ids else 1}/dedup/0", {"action": "nope"}, "dedup unknown")
        if ids:
            ra2, rb2 = self.post(f"/documents/{ids[0]}/undo", {}, "undo")
            self.follow(ra2, rb2, "after undo")
            self.post(f"/documents/{ids[0]}/analyze", {"adapter": "nope"}, "analyze unknown adapter")
            self.post(f"/documents/{ids[0]}/analyze", {"adapter": ""}, "analyze auto")
            self.post(f"/documents/{ids[0]}/import/7", {}, "import bad index")
            self.post("/documents/invalidate", [("ids", str(i)) for i in ids[1:3]], "invalidate ticked")
            self.post("/documents/invalidate", [], "invalidate none")
        ra, _ = self.get("/documents", "library after invalidation")
        for scope in re.findall(r'name="scope" value="([^"]+)"', ra.text)[1:3]:
            self.post("/documents/invalidate", {"scope": scope}, f"invalidate {scope}")
        ra2, rb2 = self.post("/documents/rebuild", {}, "rebuild")
        self.walk_review()
        self.post("/documents/invalidate", {"scope": "all"}, "invalidate all")
        self.rust_prunes = True
        self.post("/documents/rebuild", {}, "rebuild after all")
        self.walk_review()
        self.pages("after rebuild")
        if ids:
            self.post(f"/documents/{ids[-1]}/delete", {}, "delete document")

    # ------------------------------------------------------------------------------------ users
    def users_and_shares(self):
        p = self.post
        p("/settings/users", {"email": "bad", "name": "", "password": SECOND_PASSWORD}, "user bad email")
        p("/settings/users", {"email": SECOND, "name": "Léa", "password": "short"}, "user weak password")
        ra, rb = p("/settings/users", {"email": SECOND, "name": "Léa", "password": SECOND_PASSWORD}, "user added")
        self.follow(ra, rb, "after user")
        p("/settings/users", {"email": SECOND, "name": "Léa", "password": SECOND_PASSWORD}, "user twice")
        self.get("/settings", "settings with users")
        accounts = sorted({int(x) for x in re.findall(r'href="/accounts/(\d+)"', self.peek("/").text)})
        if accounts:
            ra, rb = p(f"/accounts/{accounts[0]}/shares", {"user_id": "2", "mode": "read"}, "share read")
            self.follow(ra, rb, "after share")
            if len(accounts) > 1:
                p(f"/accounts/{accounts[1]}/shares", {"user_id": "2", "mode": "edit"}, "share edit")
            p(f"/accounts/{accounts[0]}/shares", {"user_id": "", "mode": "read"}, "share nobody")
            p(f"/accounts/{accounts[0]}/shares", {"user_id": "99", "mode": "read"}, "share unknown")
            self.get(f"/accounts/{accounts[0]}", "shared account")
        # the second user signs in on both servers
        b = {"py": Client(self.py), "rs": Client(self.rs)}
        def as_b(label, fn):
            ra, rb = fn(b["py"]), fn(b["rs"])
            self.check.compare(label, ra, rb)
            return ra, rb
        as_b("B login page", lambda c: c.get("/login"))
        as_b("B login wrong", lambda c: c.post("/login", {"email": SECOND, "password": "nope"}, csrf=False))
        time.sleep(1.2)
        as_b("B login", lambda c: c.post("/login", {"email": SECOND, "password": SECOND_PASSWORD}, csrf=False))
        for url in ["/", "/documents", "/contacts", "/settings", "/analytics?vue=depenses&periode=tout"] + [f"/accounts/{i}" for i in accounts[:3]] + ["/documents/1", "/accounts/1/edit"]:
            as_b(f"B {url}", lambda c, u=url: c.get(u))
        as_b("B settings admin only", lambda c: c.post("/settings/users", {"email": "x@example.org", "password": SECOND_PASSWORD}))
        if len(accounts) > 1:
            as_b("B leaves share", lambda c: c.post(f"/accounts/{accounts[1]}/shares/2/delete", {}))
            as_b("B delete account denied", lambda c: c.post(f"/accounts/{accounts[0]}/delete", {}))
        as_b("B password mismatch", lambda c: c.post("/settings/password", {"current": SECOND_PASSWORD, "new": "Abcdefgh-12345", "confirm": "x"}))
        as_b("B password wrong", lambda c: c.post("/settings/password", {"current": "wrong", "new": "Abcdefgh-12345!", "confirm": "Abcdefgh-12345!"}))
        as_b("B logout", lambda c: c.post("/logout", {}))
        # the administrator manages roles and passwords
        p("/settings/users/2/role", {"is_admin": "on"}, "make admin")
        p("/settings/users/2/role", {}, "remove admin")
        p("/settings/users/1/role", {}, "last admin")
        p("/settings/users/99/role", {}, "role unknown")
        p("/settings/users/2/password", {"password": "short"}, "reset weak")
        p("/settings/users/2/password", {"password": "Brand-New-Password-42"}, "reset")
        p("/settings/users/99/password", {"password": "Brand-New-Password-42"}, "reset unknown")
        if accounts:
            p(f"/accounts/{accounts[0]}/shares/2/delete", {}, "unshare")
        p("/settings/users/99/delete", {}, "delete unknown user")
        ra, rb = p("/settings/users/2/delete", {}, "delete user")
        self.follow(ra, rb, "after delete user")

    def settings(self):
        p = self.post
        p("/settings/general", {"coingecko_api_key": " CG-test "}, "coingecko key")
        self.get("/settings", "settings with key")
        p("/settings/general", {"clear_coingecko": "on"}, "coingecko cleared")
        p("/settings/password", {"current": PASSWORD, "new": "x", "confirm": "y"}, "password mismatch")
        p("/settings/password", {"current": "wrong", "new": "Abcdefgh-12345!", "confirm": "Abcdefgh-12345!"}, "password wrong")
        time.sleep(1.2)
        p("/settings/password", {"current": PASSWORD, "new": "weak", "confirm": "weak"}, "password weak")
        p("/settings/recovery", {"current": "wrong"}, "recovery wrong")
        time.sleep(2.5)
        ra, rb = p("/settings/recovery", {"current": PASSWORD}, "recovery regenerated")
        for k, r in (("py", ra), ("rs", rb)):
            m = re.search(r'<p class="recovery"><code>([^<]+)', r.text)
            if m:
                self.recovery[k] = m.group(1)

    def logout_and_login(self):
        ra, rb = self.post("/logout", {}, "logout")
        self.follow(ra, rb, "after logout")
        self.get("/", "after logout /")
        time.sleep(4.5)
        for n in range(4):  # the third failure in a row starts slowing down every attempt
            self.both(f"login wrong {n}", lambda c: c.post("/login", {"email": EMAIL, "password": "nope"}, csrf=False))
        time.sleep(9)
        self.both("login wrong", lambda c: c.post("/login", {"email": EMAIL, "password": "nope"}, csrf=False))
        time.sleep(4.5)
        self.both("login https", lambda c: c.request("POST", "/login", urlencode({"email": EMAIL, "password": PASSWORD}).encode(),
                                                     {"Content-Type": "application/x-www-form-urlencoded", "X-Forwarded-Proto": "https"}))
        self.both("logout https", lambda c: c.request("POST", "/logout", urlencode({"csrf": c.csrf or ""}).encode(),
                                                      {"Content-Type": "application/x-www-form-urlencoded", "X-Forwarded-Proto": "https"}))
        time.sleep(1.2)
        self.both("login", lambda c: c.post("/login", {"email": EMAIL, "password": PASSWORD}, csrf=False))
        self.get("/", "after login /")
        ra, rb = self.post("/logout", {"lock": "1"}, "lock")
        self.follow(ra, rb, "after lock")
        self.get("/", "locked /")
        self.get("/login", "locked login page")
        if "py" in self.recovery and "rs" in self.recovery:
            self.both("login recovery", lambda c: c.post("/login", {"recovery": self.recovery["py" if c is self.a["py"] else "rs"]}, csrf=False))
            self.get("/settings?recovered=1", "recovered settings")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rust", default=str(ROOT / "target/release/mymoney"))
    ap.add_argument("--docs", nargs="*", default=[])
    ap.add_argument("--work", default=None)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--report", default=None)
    ap.add_argument("--existing", default=None, help="data dir made by the Python version: both servers open a copy")
    ap.add_argument("--password", default=PASSWORD)
    ap.add_argument("--legacy", action="store_true", help="a vault from before users, made by the Python version with --docs")
    ap.add_argument("--port", type=int, default=18601, help="the Python server's port (the Rust one gets the next)")
    ap.add_argument("--tolerant", action="store_true", help="only fail when Rust crashes or refuses what Python answered (the Rust version has more features)")
    args = ap.parse_args()
    global TOLERANT
    TOLERANT = args.tolerant
    work = Path(args.work or f"/tmp/diff_http_{os.getpid()}")
    shutil.rmtree(work, ignore_errors=True)
    (work / "py").mkdir(parents=True)
    (work / "rs").mkdir(parents=True)
    docs: list[Path] = []
    for d in args.docs:
        p = Path(d)
        docs += sorted(x for x in p.rglob("*") if x.is_file()) if p.is_dir() else [p]
    if args.legacy:
        made = work / "legacy"
        subprocess.run([sys.executable, "-c", LEGACY_MAKER, str(made), args.password] + [str(d) for d in docs], cwd=ROOT / "python", check=True,
                       env=dict(os.environ, HTTPS_PROXY="http://127.0.0.1:9", HTTP_PROXY="http://127.0.0.1:9"))
        args.existing = str(made)
    if args.existing:
        for side in ("py", "rs"):
            shutil.copytree(args.existing, work / side / "data")
    rs = Server("rust", [args.rust, "serve"], ROOT, work / "rs" / "data", args.port + 1)
    version = re.search(r"lecture ([0-9a-f]+)", Client(rs).get("/login").text)
    extra = {"DIFF_READER_VERSION": version.group(1)} if (args.existing and version) else {}
    py = Server("python", [sys.executable, "-c", PY_LAUNCHER, "serve"], ROOT / "python", work / "py" / "data", args.port, extra)
    runner = Legacy(py, rs, args.password) if args.legacy else Migration(py, rs, args.password) if args.existing else Tour(py, rs, docs)
    tour = runner.tour if args.existing else runner
    try:
        runner.run()
    finally:
        py.stop()
        rs.stop()
    report = "\n\n".join(tour.check.failures)
    if args.report:
        Path(args.report).write_text(report)
    if tour.check.tolerant:
        print(f"{tour.check.count - len(tour.check.failures)}/{tour.check.count} réponses acceptables ({tour.check.notes} avec des différences voulues)")
    else:
        print(f"{tour.check.count - len(tour.check.failures)}/{tour.check.count} réponses identiques")
    if not args.keep:
        shutil.rmtree(work, ignore_errors=True)
    sys.exit(1 if tour.check.failures else 0)


if __name__ == "__main__":
    main()
