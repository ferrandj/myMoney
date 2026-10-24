from __future__ import annotations

import hmac
import logging
import os
import re
from contextlib import asynccontextmanager
from datetime import date, datetime, timedelta
from decimal import Decimal
from pathlib import Path
from urllib.parse import quote, urlencode, urlparse

from fastapi import FastAPI, HTTPException, Query, Request
from markupsafe import Markup
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import HTMLResponse, RedirectResponse, Response
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from sqlalchemy import func, select
from sqlalchemy.orm import selectinload

from ..adapters import ADAPTERS, catalog, get_adapter, reader_version
from ..adapters.common import strip_accents
from ..config import Settings, settings as default_settings
from ..core import QUARANTINE_HOURS, MyMoney
from ..models import ACCOUNT_TYPES, CATEGORIES, COUNTERPARTY_KINDS, Account, Counterparty, Document, FeeSummary, Transaction, User
from ..security.keystore import InvalidPassword
from ..security.vault import VaultLocked
from ..services import access, analytics
from ..services import categories as category_service
from ..services import counterparties as contact_service
from ..services import coverage as coverage_service
from ..services import loans as loan_service
from ..services import statements as statement_service
from ..services import users as user_service
from ..services.access import AccessDenied
from ..services.rates import RATE_HISTORY, effective_rate
from ..services.valuation import Valuator
from . import charts, pwa, themes
from .cloudflare import CloudflareAccess
from .security import COOKIE, SECURITY_HEADERS, Sessions, verified_form

log = logging.getLogger(__name__)
HERE = Path(__file__).parent
PUBLIC_PATHS = ("/static/", "/themes.css", "/healthz", "/login", "/setup", "/manifest.webmanifest", "/sw.js", "/icons/")
KIND_LABELS = {
    "card": "Carte", "transfer": "Virement", "instant_transfer": "Virement instantané", "direct_debit": "Prélèvement",
    "loan_payment": "Prêt", "fee": "Frais", "cash": "Retrait", "cheque": "Chèque", "interest": "Intérêts",
}
ANALYTICS_VIEWS = {"evolution": "Évolution", "repartition": "Répartition", "flux": "Entrées / sorties", "depenses": "Dépenses"}
ANALYTICS_GROUPS = {
    "evolution": {"total": "Total", "compte": "Par compte", "categorie": "Par catégorie"},
    "repartition": {"categorie": "Par catégorie", "compte": "Par compte"},
}
DOCUMENT_STATUS = {
    "uploaded": "stocké", "needs_choice": "à préciser", "parsed": "à valider", "imported": "importé", "failed": "non reconnu",
    "duplicate": "doublon écarté", "copy": "copie",
}


# ----------------------------------------------------------------- formatting
RELIABILITY = {"high": "élevée", "medium": "moyenne", "low": "faible", "none": "non lu"}


def reliability(doc) -> dict:
    """How far to trust what was read from a document, for the lists: the weakest statement decides; a document the adapters
    could not read has no reliability yet."""
    result = doc.parse_result or {}
    statements = result.get("statements") or []
    if doc.status in ("uploaded", "needs_choice", "failed") or not statements:
        return {"code": "none", "label": RELIABILITY["none"], "detail": "Pas encore lu : ouvre-le pour choisir la lecture"}
    order = ["low", "medium", "high"]
    levels = [st.get("confidence") if st.get("confidence") in order else "medium" for st in statements]
    code = min(levels, key=order.index)
    warnings = sum(len(st.get("warnings") or []) for st in statements)
    detail = {
        "high": "Contrôles réussis (soldes et totaux qui concordent)",
        "medium": "Lecture plausible mais pas entièrement recoupée",
        "low": "Lecture douteuse : à vérifier ligne par ligne",
    }[code]
    if warnings:
        detail += f" · {warnings} avertissement(s)"
    if doc.status == "parsed" and result.get("decision") not in (None, "auto"):
        detail += " · type de document à confirmer"
    return {"code": code, "label": RELIABILITY[code], "detail": detail}


def money_text(value, currency: str = "EUR", decimals: int = 2) -> str:
    """An amount as plain text (for attributes and messages)."""
    if value is None:
        return "—"
    value = Decimal(value)
    sign = "-" if value < 0 else ""
    text = f"{abs(value):,.{decimals}f}".replace(",", " ").replace(".", ",")
    symbol = {"EUR": "€", "USD": "$", "GBP": "£"}.get(currency, currency)
    return f"{sign}{text} {symbol}"


def money(value, currency: str = "EUR", decimals: int = 2) -> Markup:
    """An amount in the page: marked as sensitive, so the "hide sensitive data" button can blur it."""
    return Markup('<span class="sensitive">{}</span>').format(money_text(value, currency, decimals))


def iban(value) -> str:
    """An IBAN in groups of four characters, the way banks print it."""
    compact = "".join(str(value or "").split())
    return " ".join(compact[i : i + 4] for i in range(0, len(compact), 4))


def identity_text(account) -> str:
    """What one sends to be paid: holder, IBAN, BIC and RIB, one per line."""
    lines = [
        f"Titulaire : {account.holder_name}" if account.holder_name else "",
        f"IBAN : {iban(account.iban)}" if account.iban else "",
        f"BIC : {account.bic}" if account.bic else "",
        f"RIB : {account.rib}" if account.rib else "",
    ]
    return "\n".join(line for line in lines if line)


def quantity(value) -> Markup:
    if value is None:
        return Markup("—")
    value = Decimal(value)
    text = f"{value:,.8f}".rstrip("0").rstrip(".")
    return Markup('<span class="sensitive">{}</span>').format(text.replace(",", " ").replace(".", ","))


def frdate(value) -> str:
    if not value:
        return "—"
    if isinstance(value, datetime):
        return value.strftime("%d/%m/%Y %H:%M")
    if isinstance(value, date):
        return value.strftime("%d/%m/%Y")
    if isinstance(value, str) and re.fullmatch(r"\d{4}-\d{2}-\d{2}", value):  # a date kept as text in a document's details
        return f"{value[8:]}/{value[5:7]}/{value[:4]}"
    return str(value)


def sparkline(points: list[tuple[date, Decimal]], width: int = 640, height: int = 140) -> str:
    """Server-side SVG of the net worth history (no JavaScript chart library needed)."""
    if len(points) < 2:
        return ""
    values = [float(v) for _, v in points]
    low, high = min(values), max(values)
    span = (high - low) or 1.0
    step = width / (len(values) - 1)
    coords = [(i * step, height - 8 - (v - low) / span * (height - 16)) for i, v in enumerate(values)]
    line = " ".join(f"{x:.1f},{y:.1f}" for x, y in coords)
    area = f"0,{height} {line} {width},{height}"
    return (
        f'<svg class="spark" viewBox="0 0 {width} {height}" preserveAspectRatio="none" role="img" aria-label="Évolution du patrimoine">'
        f'<polygon class="spark-area" points="{area}"/><polyline class="spark-line" points="{line}"/></svg>'
    )


# ------------------------------------------------------------------------ app
def safe_next(value: str | None, default: str) -> str:
    """Only local paths are accepted as a return address (no open redirect)."""
    return value if value and value.startswith("/") and not value.startswith("//") else default


DOC_KINDS = {
    "statement": "Relevé de compte", "savings_statement": "Relevé d'épargne", "fees_summary": "Récapitulatif des frais",
    "info": "Information", "loan_offer": "Offre de prêt", "loan_event": "Remboursement anticipé", "loan_schedule": "Tableau d'amortissement", "pdf": "Relevé (lecture générique)", "csv": "Relevé (CSV)", "ofx": "Relevé (OFX)", "qif": "Relevé (QIF)",
}


def fee_summaries(session, account) -> list[dict]:
    """The fees recaps of an account, each with what the imported statements say for the same period."""
    rows = []
    for summary in session.scalars(select(FeeSummary).where(FeeSummary.account_id == account.id).order_by(FeeSummary.period_start.desc())):
        spent = [
            t for t in session.scalars(
                select(Transaction).where(
                    Transaction.account_id == account.id, Transaction.kind == "fee",
                    Transaction.date >= summary.period_start, Transaction.date <= summary.period_end,
                )
            )
        ]
        rows.append({
            "summary": summary, "seen": sum((-t.amount for t in spent), Decimal(0)), "seen_count": len(spent),
            "complete": coverage_service.covers(session, account.id, summary.period_start, summary.period_end),
        })
    return rows


def document_group(d) -> tuple[str, str]:
    """(institution, type of document) as the library shows them."""
    institution = (d.parse_result or {}).get("institution") or "Autres documents"
    category = next((st.get("extra", {}).get("category") for st in (d.parse_result or {}).get("statements", []) if st.get("extra")), None)
    kind = category or DOC_KINDS.get(d.kind, "Document") if d.adapter_id else "Non reconnu"
    return institution, kind


def document_tree(documents: list) -> list[dict]:
    """Documents grouped for browsing: institution, then type of document, then year, newest first."""
    banks: dict[str, dict[str, list]] = {}
    for d in documents:
        institution, kind = document_group(d)
        banks.setdefault(institution, {}).setdefault(kind, []).append(d)

    def when(d) -> date:
        for st in (d.parse_result or {}).get("statements", []):
            for key in ("period_end", "balance_date"):
                if st.get(key):
                    return date.fromisoformat(st[key])
        return d.uploaded_at.date()

    tree = []
    plain = lambda text: strip_accents(text).lower()  # noqa: E731  (so that "Récapitulatif" sorts before "Relevé")
    for institution in sorted(banks, key=lambda b: (b == "Autres documents", plain(b))):
        kinds = []
        for kind, docs in sorted(banks[institution].items(), key=lambda kv: (kv[0] == "Non reconnu", plain(kv[0]))):
            docs = sorted(docs, key=lambda d: (when(d), d.id), reverse=True)
            years: list[tuple[int, list]] = []
            for d in docs:
                year = when(d).year
                if not years or years[-1][0] != year:
                    years.append((year, []))
                years[-1][1].append((d, when(d)))
            kinds.append({"label": kind, "count": len(docs), "imported": sum(1 for d in docs if d.status == "imported"), "years": years})
        tree.append({"label": institution, "count": sum(k["count"] for k in kinds), "imported": sum(k["imported"] for k in kinds), "kinds": kinds})
    return tree


def zip_documents(files: list[tuple[str, bytes]]) -> bytes:
    """A zip of (filename, content) pairs; two files with the same name keep both ("releve (2).pdf")."""
    import io
    import zipfile

    out = io.BytesIO()
    used: set[str] = set()
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as archive:
        for filename, data in files:
            name = filename.replace("/", "_").replace("\\", "_") or "document"
            stem, dot, ext = name.rpartition(".")
            candidate, n = name, 1
            while candidate.lower() in used:
                n += 1
                candidate = f"{stem} ({n}).{ext}" if dot else f"{name} ({n})"
            used.add(candidate.lower())
            archive.writestr(candidate, data)
    return out.getvalue()


def create_app(settings: Settings | None = None) -> FastAPI:
    settings = settings or default_settings
    core = MyMoney(settings)
    cf = CloudflareAccess(settings.cf_team_domain, settings.cf_audience)
    sessions = Sessions(settings.auto_lock_minutes or 24 * 60)
    core.vault.on_lock(sessions.clear)
    templates = Jinja2Templates(directory=str(HERE / "templates"))
    templates.env.filters.update(money=money, money_text=money_text, qty=quantity, frdate=frdate, iban=iban)
    templates.env.globals.update(
        ACCOUNT_TYPES=ACCOUNT_TYPES, CATEGORIES=CATEGORIES, COUNTERPARTY_KINDS=COUNTERPARTY_KINDS, KIND_LABELS=KIND_LABELS,
        DOCUMENT_STATUS=DOCUMENT_STATUS, DOC_KINDS=DOC_KINDS, LOAN_KINDS=loan_service.LOAN_KINDS,
        THEME_GROUPS=themes.groups(), identity_text=identity_text, reliability=reliability,
        APP_VERSION=os.environ.get("MYMONEY_VERSION", "dev"), READER_VERSION=reader_version(),
    )

    def contact_choices():
        with core.db.session() as session:
            return contact_service.choices(session)

    templates.env.globals.update(contact_choices=contact_choices)
    templates.env.globals.update(QUARANTINE_HOURS=QUARANTINE_HOURS)
    templates.env.filters.update(pct=lambda v: "—" if v is None else f"{Decimal(v):+.1f} %".replace(".", ","))

    @asynccontextmanager
    async def lifespan(_app):
        if not core.vault.initialized:
            # Whoever reaches the page first could otherwise create the vault: require a code from the logs.
            log.warning("Première installation : code d'installation = %s", core.setup_token)
        try:
            core.auto_unlock()
        except Exception:
            log.exception("Déverrouillage automatique impossible")
        core.start_background(sessions.active)
        yield
        core.stop()
        core.vault.lock()

    app = FastAPI(lifespan=lifespan, docs_url=None, redoc_url=None, openapi_url=None)
    app.state.core = core
    app.state.sessions = sessions
    app.state.cf = cf
    @app.get("/themes.css")
    async def themes_css():
        return Response(themes.stylesheet(), media_type="text/css")

    app.mount("/static", StaticFiles(directory=str(HERE / "static")), name="static")
    pwa.install(app)

    def render(request: Request, name: str, status_code: int = 200, **context) -> HTMLResponse:
        session = getattr(request.state, "session", None)
        context.update(
            request=request, csrf=session.csrf if session else "", unlocked=core.vault.unlocked, flash=request.query_params.get("msg"),
            me=session.identity if session else None, cf_active=cf.enabled and bool(getattr(request.state, "cf_email", None)),
        )
        return templates.TemplateResponse(request, name, context, status_code=status_code)

    def redirect(url: str, msg: str | None = None) -> RedirectResponse:
        if msg:
            url, _, fragment = url.partition("#")  # the message goes in the query, before any #anchor
            url += ("&" if "?" in url else "?") + "msg=" + quote(msg) + ("#" + fragment if fragment else "")
        return RedirectResponse(url, status_code=303)

    def trusted_origin(request: Request) -> bool:
        """A form post must come from this very site (or from the public address of the tunnel in front of it)."""
        origin = request.headers.get("origin")
        if not origin or origin == "null" or urlparse(origin).netloc == request.headers.get("host"):
            return True
        return origin.rstrip("/") in {o.strip().rstrip("/") for o in settings.trusted_origins.split(",") if o.strip()}

    def https_in_front(request: Request) -> bool:
        """The browser talks HTTPS even when the tunnel talks plain http to the app: the cookie can be marked Secure."""
        forwarded = request.headers.get("x-forwarded-proto", "").split(",")[0].strip().lower()
        return request.url.scheme == "https" or forwarded == "https" or '"https"' in request.headers.get("cf-visitor", "")

    @app.middleware("http")
    async def guard(request: Request, call_next):
        path = request.url.path
        if request.method == "POST" and not trusted_origin(request):
            return Response("Origine refusée", status_code=403)
        request.state.session = sessions.get(request.cookies.get(COOKIE))
        request.state.cf_email = None
        public = path.startswith(PUBLIC_PATHS)
        new_token = None
        identity = request.state.session.identity if request.state.session else None

        # Cloudflare Access: a valid signed token names the visitor, no password needed once the vault is open.
        if cf.enabled and core.vault.initialized and not path.startswith(("/static/", "/icons/", "/themes.css", "/healthz")):
            request.state.cf_email = await run_in_threadpool(cf.email_of, request.headers)
            email = request.state.cf_email
            if email and core.vault.unlocked and (identity is None or identity.email != email):
                found = await run_in_threadpool(core.identity_for_email, email)
                if found is not None:
                    new_token = sessions.create(found)
                    request.state.session = sessions.get(new_token)
                    identity = found
                elif await run_in_threadpool(core.has_users):
                    response = render(request, "denied.html", 403, message=f"Ton adresse ({email}) n'a pas de compte sur myMoney : demande à un administrateur de la créer.", cf_email=email)
                    for key, value in SECURITY_HEADERS.items():
                        response.headers.setdefault(key, value)
                    return response
                # else: a vault from before users existed, whose first user is being named (/first-user)

        if not public:
            if not core.vault.initialized:
                return RedirectResponse("/setup", status_code=303)
            if request.state.session is None or not core.vault.unlocked:
                if request.state.session is not None:
                    sessions.drop(request.cookies.get(COOKIE))
                return RedirectResponse("/login", status_code=303)
            if identity is None and path not in ("/first-user", "/logout"):  # a vault from before users: name its first user
                return RedirectResponse("/first-user", status_code=303)
            core.vault.touch()
        try:
            with access.as_user(identity):
                response = await call_next(request)
        except VaultLocked:
            response = RedirectResponse("/login", status_code=303)
        if new_token:
            response.set_cookie(COOKIE, new_token, httponly=True, samesite="strict", secure=settings.cookie_secure or https_in_front(request), path="/")
        for key, value in SECURITY_HEADERS.items():
            response.headers.setdefault(key, value)
        if settings.cookie_secure or https_in_front(request):
            response.headers.setdefault("Strict-Transport-Security", "max-age=31536000")
        return response

    @app.exception_handler(AccessDenied)
    async def access_denied(request: Request, exc: AccessDenied):
        return render(request, "denied.html", 404, message=str(exc))

    def start_session(request: Request, response: Response, identity: access.Identity | None) -> None:
        response.set_cookie(
            COOKIE, sessions.create(identity), httponly=True, samesite="strict", secure=settings.cookie_secure or https_in_front(request), path="/"
        )

    def admin_only(request: Request) -> access.Identity:
        me = request.state.session.identity if request.state.session else None
        if me is None or not me.is_admin:
            raise HTTPException(403, "Réservé aux administrateurs")
        return me

    # ------------------------------------------------------------- auth
    @app.get("/healthz")
    async def healthz():
        return {"ok": True}

    @app.get("/setup")
    async def setup_page(request: Request):
        if core.vault.initialized:
            return redirect("/login")
        return render(request, "setup.html")

    @app.post("/setup")
    async def setup(request: Request):
        if core.vault.initialized:
            return redirect("/login")
        form = await request.form()
        token = str(form.get("setup_token", "")).strip()
        if not hmac.compare_digest(token, core.setup_token):
            return render(request, "setup.html", 403, error="Code d'installation incorrect : il est affiché dans les logs du conteneur")
        password, confirm = str(form.get("password", "")), str(form.get("confirm", ""))
        if password != confirm:
            return render(request, "setup.html", 400, error="Les deux mots de passe ne correspondent pas")
        try:
            recovery = await run_in_threadpool(core.setup, str(form.get("email", "")), str(form.get("name", "")), password)
        except ValueError as exc:
            return render(request, "setup.html", 400, error=str(exc))
        identity = await run_in_threadpool(core.identity_for_email, str(form.get("email", "")))
        request.state.session = None
        response = render(request, "recovery.html", recovery=recovery)
        start_session(request, response, identity)
        return response

    @app.get("/login")
    async def login_page(request: Request):
        if not core.vault.initialized:
            return redirect("/setup")
        session = request.state.session
        if session is not None and session.identity is not None and core.vault.unlocked:
            return redirect("/")
        return render(request, "login.html", legacy=core.legacy_login, cf_email=request.state.cf_email, locked=not core.vault.unlocked)

    @app.post("/login")
    async def login(request: Request):
        form = await request.form()
        email = str(form.get("email", "")).strip()
        identity = None
        try:
            if form.get("recovery"):
                identity = await run_in_threadpool(core.authenticate_recovery, str(form["recovery"]))
            elif core.legacy_login and not email:
                await run_in_threadpool(core.check_password, str(form.get("password", "")))
            else:
                identity = await run_in_threadpool(core.authenticate, email, str(form.get("password", "")))
        except InvalidPassword:
            return render(request, "login.html", 401, error="E-mail ou mot de passe incorrect", legacy=core.legacy_login, cf_email=request.state.cf_email, locked=not core.vault.unlocked)
        except PermissionError as exc:
            return render(request, "login.html", 429, error=str(exc), legacy=core.legacy_login, cf_email=request.state.cf_email, locked=not core.vault.unlocked)
        if identity is None:
            target = "/first-user"
        else:
            target = "/settings?recovered=1" if form.get("recovery") else "/"
        response = redirect(target)
        start_session(request, response, identity)
        return response

    @app.get("/first-user")
    async def first_user_page(request: Request):
        if await run_in_threadpool(core.has_users):
            return redirect("/")
        return render(request, "first_user.html", cf_email=request.state.cf_email or "")

    @app.post("/first-user")
    async def first_user(request: Request):
        form = await verified_form(request)
        try:
            identity = await run_in_threadpool(core.create_first_user, form.get("email", ""), form.get("name"))
        except ValueError as exc:
            return render(request, "first_user.html", 400, error=str(exc), cf_email=form.get("email", ""))
        request.state.session.identity = identity
        return redirect("/", "Compte administrateur créé : les comptes et documents existants sont à toi")

    @app.post("/logout")
    async def logout(request: Request):
        form = await verified_form(request)
        me = request.state.session.identity
        sessions.drop(request.cookies.get(COOKIE))
        lock = form.get("lock") == "1" and me is not None and me.is_admin and not settings.unlock_password_file
        if lock:
            core.vault.lock()
        if request.state.cf_email and not lock:  # signed in by Cloudflare: end that session too, or the next page signs back in
            response = RedirectResponse(cf.logout_url, status_code=303)
        else:
            response = redirect("/login", "Coffre verrouillé" if lock else "Déconnecté")
        response.delete_cookie(COOKIE, path="/")
        return response

    # -------------------------------------------------------- dashboard
    @app.get("/")
    async def dashboard(request: Request):
        portfolio = await run_in_threadpool(core.portfolio)
        with core.db.session() as s:
            to_identify = len(contact_service.unresolved(s))
            gaps = coverage_service.all_gaps(s)
        groups: dict[str, list] = {}
        for value in portfolio.accounts:
            groups.setdefault(value.account.category, []).append(value)
        return render(
            request,
            "dashboard.html",
            portfolio=portfolio,
            groups=[(code, CATEGORIES[code], groups[code]) for code in CATEGORIES if code in groups],
            chart=sparkline(portfolio.history),
            to_identify=to_identify,
            gaps=gaps,
        )

    # -------------------------------------------------------- missing statements
    @app.post("/gaps/acknowledge")
    async def gap_acknowledge(request: Request):
        form = await verified_form(request)
        target = safe_next(form.get("next"), "/")
        try:
            with core.db.session() as s:
                coverage_service.acknowledge(
                    s, int(form["account_id"]), date.fromisoformat(form["start"]), date.fromisoformat(form["end"]), form.get("note")
                )
        except (KeyError, ValueError) as exc:
            return redirect(target, str(exc) if isinstance(exc, ValueError) and str(exc) else "Période invalide")
        return redirect(target, "Noté : ce trou n'est plus signalé (tu peux le remettre en alerte depuis la page du compte)")

    @app.post("/gaps/{ack_id}/restore")
    async def gap_restore(request: Request, ack_id: int):
        form = await verified_form(request)
        with core.db.session() as s:
            coverage_service.restore(s, ack_id)
        return redirect(safe_next(form.get("next"), "/"), "Ce trou est de nouveau signalé")

    # -------------------------------------------------------- analytics
    @app.get("/analytics")
    async def analytics_page(
        request: Request, vue: str = "evolution", periode: str = "1a", par: str = "", c: list[int] = Query(default=[]), interne: int = 0,
        sens: str = "sorties", cat: str = "", mois: str = "",
    ):
        vue = vue if vue in ANALYTICS_VIEWS else "evolution"
        periode = periode if periode in analytics.PERIODS else "1a"
        sens = sens if sens in ("sorties", "entrees") else "sorties"
        direction = "out" if sens == "sorties" else "in"
        cat = cat if cat in category_service.ALL and vue == "depenses" else ""
        month_range = None
        try:
            year, month = (int(x) for x in mois.split("-"))
            month_range = (date(year, month, 1), date(year + month // 12, month % 12 + 1, 1) - timedelta(days=1))
        except ValueError:
            mois = ""
        groups = ANALYTICS_GROUPS.get(vue, {})
        par = par if par in groups else next(iter(groups), "")
        portfolio = await run_in_threadpool(core.portfolio, False)
        values = {v.account.id: v for v in portfolio.accounts}
        accounts = {i: v.account for i, v in values.items()}
        selected = [i for i in c if i in accounts] or list(accounts)
        everything = set(selected) == set(accounts)
        today = date.today()

        def url(**over) -> str:
            params = {"vue": vue, "periode": periode, "par": par, "interne": interne or "", "sens": "" if sens == "sorties" else sens, "cat": cat, "mois": mois, **over}
            pairs = [(k, str(v)) for k, v in params.items() if v not in (None, "")]
            pairs += [] if everything else [("c", str(i)) for i in selected]
            return "/analytics?" + urlencode(pairs)

        with core.db.session() as s:
            histories = {i: analytics.account_history(s, accounts[i], values[i].total, today) for i in selected}
            start = analytics.period_start(periode, today, analytics.earliest_day(histories, selected, today))
            total_points = analytics.build_series(histories, accounts, selected, "total", start, today)
            flows = analytics.monthly_flows(s, selected, start, today, bool(interne)) if vue == "flux" else []
            report = None
            if vue == "depenses":
                report_start, report_end = month_range if month_range else (start, today)
                report = analytics.spending_report(s, selected, report_start, report_end, bool(interne), direction, cat or None)
        sel_values = [values[i] for i in selected]
        assets = sum((v.total for v in sel_values if v.total > 0), Decimal(0))
        debts = sum((v.total for v in sel_values if v.total < 0), Decimal(0))

        rows = []
        for v in sel_values:
            history = histories[v.account.id]
            before = history.get(start)
            rows.append({
                "account": v.account, "total": v.total, "share": (v.total / assets * 100) if assets and v.total > 0 else None,
                "delta": (v.total - before) if before is not None else None,
                "pct": ((v.total - before) / abs(before) * 100) if before else None,
            })
        # Like for like: only accounts that already existed at the start count towards the variation, so adding an account
        # (or importing its first statement) is not mistaken for growth.
        compared = [r for r in rows if r["delta"] is not None]
        base = sum((r["total"] - r["delta"] for r in compared), Decimal(0))
        variation = {"delta": sum((r["delta"] for r in compared), Decimal(0)), "pct": None} if compared else None
        if variation and base:
            variation["pct"] = variation["delta"] / abs(base) * 100

        content: dict = {}
        if vue == "evolution":
            series = analytics.build_series(histories, accounts, selected, par, start, today)
            content = {"chart": charts.line_chart(series), "series": series, "stats": {s.key: analytics.stats(s.points) for s in series}}
        elif vue == "repartition":
            items = analytics.repartition(sel_values, par)
            content = {"chart": charts.donut(items), "items": items, "ring_total": sum((x[2] for x in items), Decimal(0))}
        elif vue == "flux":
            content = {
                "chart": charts.flow_chart(flows), "flows": flows,
                "flow_links": {f.month: url(vue="depenses", par="", cat="", sens="", mois=f.month.strftime("%Y-%m")) for f in flows},
            }
        else:
            palette_size = charts.PALETTE_SIZE
            top = report.categories[: palette_size - 1]
            rest = sum((x.total for x in report.categories[palette_size - 1 :]), Decimal(0))
            ring = [(x.id, x.label, x.total) for x in top] + ([("reste", "Autres catégories", rest)] if rest else [])
            content = {
                "report": report, "sens": sens, "cat": cat, "mois": mois, "ring": ring, "donut": charts.donut(ring, "Répartition par catégorie") if not cat else "",
                "month_chart": charts.month_bars(report.months, "Par mois", "neg" if direction == "out" else "pos"),
                "options": category_service.options(direction),
                "category_links": {x.id: url(cat=x.id) for x in report.categories},
                "sens_links": [("sorties", "Dépenses", url(sens="", cat="")), ("entrees", "Revenus", url(sens="entrees", cat=""))],
                "all_link": url(cat=""), "month_off_link": url(mois=""),
                "month_label": month_range[0].strftime("%m/%Y") if month_range else "",
                "this_url": url(),
            }

        return render(
            request, "analytics.html", vue=vue, periode=periode, par=par, interne=interne, accounts=list(accounts.values()), selected=set(selected),
            everything=everything, rows=rows, assets=assets, debts=debts, total=assets + debts, variation=variation, start=start, today=today,
            views=[(code, label, url(vue=code, par="", cat="", mois="", sens="")) for code, label in ANALYTICS_VIEWS.items()],
            periods=[(code, label, url(periode=code, mois="")) for code, (label, _) in analytics.PERIODS.items()],
            group_options=[(code, label, url(par=code)) for code, label in groups.items()], palette=charts.PALETTE_SIZE, **content,
        )

    @app.post("/analytics/categorize")
    async def analytics_categorize(request: Request):
        """"This merchant goes in that category, from now on": the user's rule wins over the automatic guess."""
        form = await verified_form(request)
        target = safe_next(form.get("next"), "/analytics?vue=depenses")
        try:
            with core.db.session() as s:
                category_service.set_rule(s, form.get("key", ""), form.get("category", ""))
        except ValueError as exc:
            return redirect(target, str(exc))
        what = form.get("category")
        label = form.get("label") or "Ce bénéficiaire"
        if what == "auto":
            return redirect(target, f"« {label} » : classement automatique rétabli")
        return redirect(target, f"« {label} » est maintenant classé dans « {category_service.ALL[what].label} » (toutes ses opérations, y compris les prochaines)")

    # --------------------------------------------------------- accounts
    @app.get("/accounts/new")
    async def account_new(request: Request):
        return render(request, "account_form.html", account=None)

    @app.post("/accounts/new")
    async def account_create(request: Request):
        form = await verified_form(request)
        try:
            account_id = core.save_account(None, form)
        except Exception as exc:
            return render(request, "account_form.html", 400, account=None, error=f"Valeur invalide : {exc}")
        return redirect(f"/accounts/{account_id}", "Compte créé")

    @app.get("/accounts/{account_id}")
    async def account_detail(request: Request, account_id: int, page: int = 1):
        with core.db.session() as s:
            account = s.get(Account, account_id)
            if account is None:
                raise HTTPException(404)
            my_role = access.role(s, account)  # AccessDenied (404) for an account the user cannot see
            shares = [(sh.user, sh.can_edit) for sh in account.shares]
            owner = account.owner
            sharable = [u for u in user_service.all_users(s) if u.id != account.owner_id and all(sh.user_id != u.id for sh in account.shares)] if my_role == "owner" else []
            _ = account.holdings
            per_page = 100
            total_tx = s.scalar(select(func.count()).select_from(Transaction).where(Transaction.account_id == account_id))
            transactions = list(
                s.scalars(
                    select(Transaction).where(Transaction.account_id == account_id).options(selectinload(Transaction.counterparty))
                    .order_by(Transaction.date.desc(), Transaction.id.desc()).offset((page - 1) * per_page).limit(per_page)
                )
            )
            fee_rows = fee_summaries(s, account)
            rules = category_service.load_rules(s)
            tx_categories = {t.id: category_service.ALL[category_service.categorize(t, rules)].label for t in transactions}
            loan, loan_plan = account.loan, None
            if loan is not None:
                loan_plan = loan_service.rebuild(s, loan)
                _ = (loan.events, loan.installments)
                loan_docs = {d.id: d for d in s.scalars(select(Document).where(Document.id.in_(
                    [x for x in [loan.offer_document_id, loan.schedule_document_id, *[e.document_id for e in loan.events]] if x])))}
            else:
                loan_docs = {}
            statements, chain_issues = statement_service.coverage(s, account_id)
            all_gaps = coverage_service.account_gaps(s, account, include_acknowledged=True)
            to_identify = contact_service.unresolved(s, account_id=account_id)
        valuator = Valuator(core.prices)
        value = await run_in_threadpool(lambda: (valuator.prefetch([account]), valuator.account_value(account))[1])
        rate, rate_source = effective_rate(account.account_type, account.interest_rate)
        if loan is not None and loan.rate is not None:
            rate, rate_source = loan.rate, "lu dans les documents du prêt"
        return render(
            request, "account.html", account=account, value=value, transactions=transactions, rate=rate, rate_source=rate_source,
            page=page, pages=max(1, -(-total_tx // per_page)), total_tx=total_tx, statements=statements, chain_issues=chain_issues, fee_rows=fee_rows,
            open_gaps=[g for g in all_gaps if not g.acknowledged], acked_gaps=[g for g in all_gaps if g.acknowledged],
            to_identify=to_identify, loan=loan, loan_plan=loan_plan, loan_docs=loan_docs,
            role=my_role, shares=shares, owner=owner, sharable=sharable, tx_categories=tx_categories,
        )

    @app.post("/accounts/{account_id}/shares")
    async def share_add(request: Request, account_id: int):
        """The owner shares the account with one other user, read only or with the right to edit."""
        form = await verified_form(request)
        try:
            with core.db.session() as s:
                access.check_owner(s, account_id)
                account = s.get(Account, account_id)
                target = s.get(User, int(form.get("user_id") or 0))
                if target is None:
                    raise ValueError("Choisis la personne avec qui partager")
                user_service.share(s, account, target, form.get("mode") == "edit")
                label = target.label
        except ValueError as exc:
            return redirect(f"/accounts/{account_id}#partage", str(exc))
        return redirect(f"/accounts/{account_id}#partage", f"Compte partagé avec {label}")

    @app.post("/accounts/{account_id}/shares/{user_id}/delete")
    async def share_remove(request: Request, account_id: int, user_id: int):
        """The owner stops sharing; the person it is shared with can also leave a share."""
        await verified_form(request)
        me = request.state.session.identity
        with core.db.session() as s:
            access.check_read(s, account_id)
            if user_id != me.id:
                access.check_owner(s, account_id)
            account = s.get(Account, account_id)
            user_service.unshare(s, account, user_id)
        if user_id == me.id:
            return redirect("/", "Tu n'as plus accès à ce compte")
        return redirect(f"/accounts/{account_id}#partage", "Partage retiré")

    @app.post("/accounts/{account_id}/loan/first-due")
    async def loan_first_due(request: Request, account_id: int):
        """The offer only numbers the instalments: the user gives the date of the first one when no document does."""
        form = await verified_form(request)
        try:
            day = date.fromisoformat(form.get("first_due", ""))
        except ValueError:
            return redirect(f"/accounts/{account_id}", "Date invalide")
        with core.db.session() as s:
            account = s.get(Account, account_id)
            if account is None or account.loan is None:
                raise HTTPException(404)
            access.check_edit(s, account_id)
            loan_service.set_first_due(s, account.loan, day)
        return redirect(f"/accounts/{account_id}", "Date de la première échéance enregistrée")

    @app.get("/accounts/{account_id}/edit")
    async def account_edit(request: Request, account_id: int):
        with core.db.session() as s:
            account = s.get(Account, account_id)
            if account is None:
                raise HTTPException(404)
            access.check_edit(s, account_id)
        return render(request, "account_form.html", account=account)

    @app.post("/accounts/{account_id}/edit")
    async def account_update(request: Request, account_id: int):
        form = await verified_form(request)
        try:
            core.save_account(account_id, form)
        except Exception as exc:
            return redirect(f"/accounts/{account_id}/edit", f"Valeur invalide : {exc}")
        return redirect(f"/accounts/{account_id}", "Compte mis à jour")

    @app.post("/accounts/{account_id}/delete")
    async def account_delete(request: Request, account_id: int):
        await verified_form(request)
        core.delete_account(account_id)
        return redirect("/", "Compte supprimé")

    @app.post("/accounts/{account_id}/holdings")
    async def holding_add(request: Request, account_id: int):
        form = await verified_form(request)
        try:
            core.add_holding(account_id, form)
        except Exception as exc:
            return redirect(f"/accounts/{account_id}", f"Position invalide : {exc}")
        return redirect(f"/accounts/{account_id}", "Position ajoutée")

    @app.post("/holdings/{holding_id}/delete")
    async def holding_delete(request: Request, holding_id: int):
        form = await verified_form(request)
        core.delete_holding(holding_id)
        return redirect(f"/accounts/{int(form.get('account_id', 0))}", "Position supprimée")

    # -------------------------------------------------------- documents
    @app.get("/documents")
    async def documents_page(request: Request, account_id: int | None = None, start: str = "", end: str = ""):
        await run_in_threadpool(core.purge_quarantine)
        missing = None
        if account_id and start and end:
            try:
                with core.db.session() as s:
                    account = s.get(Account, account_id)
                    if account is not None and account.id in (access.readable_ids(s) if access.restricted() else {account.id}):
                        missing = {"account": account.name, "label": coverage_service.describe(date.fromisoformat(start), date.fromisoformat(end))}
            except ValueError:
                missing = None
        await run_in_threadpool(core.refresh_stale_waiting)  # waiting documents are shown with the current reading
        with core.db.session() as s:
            everything = access.visible_documents(s)
            names = {d.id: d.filename for d in everything}
        documents = [d for d in everything if d.status != "duplicate"]
        aside = [(d, names.get(d.duplicate_of)) for d in everything if d.status == "duplicate"]
        return render(
            request, "documents.html", documents=documents, aside=aside, missing=missing, max_mb=settings.max_upload_mb, catalog=catalog(),
            tree=document_tree(documents), pending=[d for d in documents if d.status == "parsed"],
            reasons=await run_in_threadpool(core.pending_reasons) if any(d.status == "parsed" for d in documents) else {},
            imported_count=sum(1 for d in documents if d.status == "imported"),
        )

    @app.post("/documents/invalidate")
    async def documents_invalidate(request: Request):
        """Cancel imports: the ticked documents, everything, one institution, or one type of document of an institution."""
        form = await request.form()
        session = request.state.session
        token = form.get("csrf")
        if not session or not isinstance(token, str) or not hmac.compare_digest(token, session.csrf):
            raise HTTPException(403, "Jeton CSRF invalide")
        scope = str(form.get("scope") or "")
        with core.db.session() as s:
            imported = [d for d in access.visible_documents(s) if d.status == "imported"]
            groups = {d.id: document_group(d) for d in imported}
        if scope == "all":
            ids = [d.id for d in imported]
        elif scope.startswith("bank:"):
            ids = [i for i, (bank, _) in groups.items() if bank == scope[5:]]
        elif scope.startswith("kind:"):
            bank, _, kind = scope[5:].partition("::")
            ids = [i for i, g in groups.items() if g == (bank, kind)]
        else:
            chosen = {int(x) for x in form.getlist("ids") if str(x).isdigit()}
            ids = [i for i in groups if i in chosen]
            if not chosen:
                return redirect("/documents", "Coche au moins un document à invalider")
        count = await run_in_threadpool(core.invalidate_documents, ids)
        return redirect("/documents", f"{count} import(s) invalidé(s) : les fichiers sont conservés et reviennent dans « À valider »" if count else "Aucun import à invalider")

    @app.post("/documents/download")
    async def documents_download(request: Request):
        """Download the ticked documents: one file as it is, several as a zip."""
        form = await request.form()
        session = request.state.session
        token = form.get("csrf")
        if not session or not isinstance(token, str) or not hmac.compare_digest(token, session.csrf):
            raise HTTPException(403, "Jeton CSRF invalide")
        ids = []
        for raw in form.getlist("ids"):
            if isinstance(raw, str) and raw.isdigit() and int(raw) not in ids:
                ids.append(int(raw))
        if not ids:
            return redirect("/documents", "Coche au moins un document à télécharger")
        files = [await run_in_threadpool(core.read_document, i) for i in ids]  # AccessDenied (404) for a document one cannot open
        if len(files) == 1:
            filename, data = files[0]
            return Response(data, media_type="application/octet-stream", headers={"Content-Disposition": f"attachment; filename*=UTF-8''{quote(filename)}"})
        if sum(len(data) for _, data in files) > 400 * 1024 * 1024:
            return redirect("/documents", "Sélection trop volumineuse : télécharge-la en plusieurs fois")
        archive = await run_in_threadpool(zip_documents, files)
        name = f"myMoney-documents-{date.today():%Y%m%d}.zip"
        return Response(archive, media_type="application/zip", headers={"Content-Disposition": f"attachment; filename*=UTF-8''{quote(name)}"})

    @app.post("/documents")
    async def documents_upload(request: Request):
        form = await request.form()
        session = request.state.session
        token = form.get("csrf")
        if not session or not isinstance(token, str) or not hmac.compare_digest(token, session.csrf):
            raise HTTPException(403, "Jeton CSRF invalide")
        await run_in_threadpool(core.purge_quarantine)
        outcomes: list[tuple[int, str, dict | None]] = []  # (document id, "existing" | "duplicate" | "differs" | "imported" | status, result)
        for upload in form.getlist("files"):
            if not hasattr(upload, "read"):
                continue
            data = await upload.read(settings.max_upload_mb * 1024 * 1024 + 1)
            if len(data) > settings.max_upload_mb * 1024 * 1024:
                return redirect("/documents", f"{upload.filename} dépasse {settings.max_upload_mb} Mo")
            doc_id, existing = await run_in_threadpool(core.store_document, upload.filename or "document", data)
            if existing:
                if not await run_in_threadpool(core.document_visible, doc_id):
                    return redirect("/documents", "Ce fichier est déjà dans le coffre (déposé par quelqu'un d'autre) : rien n'est ajouté")
                outcomes.append((doc_id, "existing", None))
                continue
            status = await run_in_threadpool(core.analyze_document, doc_id, None, data)
            verdict = await run_in_threadpool(core.check_duplicate, doc_id) if status == "parsed" else None
            imported = await run_in_threadpool(core.auto_import, doc_id) if status == "parsed" and verdict is None else None
            state = "duplicate" if verdict == "identical" else "differs" if verdict else "imported" if imported else status
            outcomes.append((doc_id, state, imported))
        if not outcomes:
            return redirect("/documents", "Aucun fichier reçu")
        if len(outcomes) == 1:
            doc_id, state, imported = outcomes[0]
            if state == "existing":
                return redirect(f"/documents/{doc_id}", "Ce fichier est strictement identique à un document déjà déposé : rien n'est ajouté")
            if state == "duplicate":
                return redirect(f"/documents/{doc_id}", "Doublon : aucune différence avec le fichier déjà importé")
            if state == "differs":
                return redirect(f"/documents/{doc_id}", "Ce relevé existe déjà mais le fichier diffère : choisis quoi en faire")
            if imported:
                return redirect(after_import(imported, doc_id), import_message(imported, auto=True))
            return redirect(f"/documents/{doc_id}")
        counts = {k: sum(1 for _, state, _ in outcomes if state == k) for k in ("imported", "existing", "duplicate", "differs")}
        review = len(outcomes) - sum(counts.values())
        parts = [f"{len(outcomes)} fichier(s)", f"{counts['imported']} importé(s) automatiquement"]
        if review:
            parts.append(f"{review} à valider : bouton « Tout valider » ci-dessous")
        if counts["duplicate"]:
            parts.append(f"{counts['duplicate']} doublon(s) sans différence écarté(s)")
        if counts["differs"]:
            parts.append(f"{counts['differs']} déjà importé(s) mais différent(s) : à trancher")
        if counts["existing"]:
            parts.append(f"{counts['existing']} strictement identique(s) à un fichier déjà déposé")
        return redirect("/documents", " · ".join(parts))

    def after_import(result: dict, doc_id: int) -> str:
        return f"/accounts/{result['account_id']}" if result.get("account_id") else f"/documents/{doc_id}"

    def import_message(result: dict, auto: bool = False) -> str:
        if result.get("kind") == "info":
            return "Document classé avec tes papiers : il ne contient aucune opération"
        if result.get("kind") in loan_service.LOAN_KINDS:
            return ("Classé automatiquement : " if auto else "") + {
                "loan_offer": "offre de prêt enregistrée (montant, taux, coût et échéances du contrat)",
                "loan_event": "remboursement anticipé enregistré, plan du prêt mis à jour",
                "loan_schedule": "tableau d'amortissement enregistré, plan du prêt mis à jour",
            }[result["kind"]]
        if result.get("kind") == "fees_summary":
            return ("Classé automatiquement : " if auto else "") + f"récapitulatif des frais {result['year']} rattaché au compte"
        text = f"{result['added']} opération(s) ajoutée(s)"
        if result["skipped_exact"]:
            text += f" · {result['skipped_exact']} doublon(s) exact(s) ignoré(s)"
        if result["skipped_probable"]:
            text += f" · {result['skipped_probable']} doublon(s) probable(s) ignoré(s)"
        return ("Importé automatiquement : " if auto else "Import terminé : ") + text

    @app.post("/documents/rebuild")
    async def documents_rebuild(request: Request):
        """Cancel every import and read all the files again; what no longer passes is shown, never validated silently."""
        await verified_form(request)
        outcome = await run_in_threadpool(core.rebuild_imports)
        return render(request, "rebuild.html", outcome=outcome)

    @app.get("/documents/review")
    async def documents_review(request: Request, after: int | None = None):
        """Walk through the unvalidated documents one after the other (oldest period first)."""
        nxt, waiting = await run_in_threadpool(core.next_pending, after)
        if nxt is None:
            done = "Revue terminée" + (f" : {waiting} document(s) restent à valider" if waiting else " : plus rien à valider")
            return redirect("/documents", done)
        return redirect(f"/documents/{nxt}?review=1")

    async def after_step(doc_id: int, form, fallback: str, message: str | None = None):
        """In review mode, the next document follows whatever was just done; otherwise the usual page."""
        if form.get("review"):
            nxt, waiting = await run_in_threadpool(core.next_pending, doc_id)
            if nxt is not None:
                return redirect(f"/documents/{nxt}?review=1", message)
            return redirect("/documents", (message + " · " if message else "") + "Revue terminée" + (f" : {waiting} document(s) restent à valider" if waiting else " : plus rien à valider"))
        return redirect(fallback, message)

    @app.get("/documents/{doc_id}")
    async def document_review(request: Request, doc_id: int, review: int = 0):
        review = await run_in_threadpool(core.review_document, doc_id)
        if review is None:
            raise HTTPException(404)
        doc = review["doc"]
        result = doc.parse_result or {}
        candidates = []
        for c in result.get("candidates", []):
            try:
                candidates.append((get_adapter(c["adapter"]), c))
            except ValueError:
                continue
        with core.db.session() as s:
            to_identify = contact_service.unresolved(s, document_id=doc_id) if doc.status == "imported" else []
            original = s.get(Document, doc.duplicate_of) if doc.duplicate_of else None
        queue = None
        if review and doc.status == "parsed":
            nxt, waiting = await run_in_threadpool(core.next_pending, doc_id)
            queue = {"waiting": waiting, "next": nxt}
        return render(
            request, "document.html", doc=doc, result=result, current_reading=core.reading_is_current(doc), plans=review["plans"], accounts=review["accounts"],
            candidates=candidates, adapters=ADAPTERS, to_identify=to_identify, original=original, queue=queue,
        )

    @app.post("/documents/{doc_id}/analyze")
    async def document_analyze(request: Request, doc_id: int):
        form = await verified_form(request)
        adapter_id = form.get("adapter") or None
        try:
            if adapter_id:
                get_adapter(adapter_id)
            if await run_in_threadpool(core.analyze_document, doc_id, adapter_id) == "parsed":
                await run_in_threadpool(core.check_duplicate, doc_id)
        except Exception as exc:
            return redirect(f"/documents/{doc_id}", f"Analyse impossible : {exc}")
        return redirect(f"/documents/{doc_id}")

    @app.post("/documents/{doc_id}/import/{index}")
    async def document_import(request: Request, doc_id: int, index: int):
        form = await verified_form(request)
        try:
            result = await run_in_threadpool(core.import_statement, doc_id, index, form)
        except Exception as exc:
            return redirect(f"/documents/{doc_id}" + ("?review=1" if form.get("review") else ""), f"Import impossible : {exc}")
        return await after_step(doc_id, form, after_import(result, doc_id), import_message(result))

    @app.post("/documents/{doc_id}/dedup/{index}")
    async def document_dedup(request: Request, doc_id: int, index: int):
        """What to do with a file for a period that was already imported and that differs from what is stored."""
        form = await verified_form(request)
        action = form.get("action")
        try:
            if action == "replace":
                result = await run_in_threadpool(core.replace_document, doc_id, index)
                return await after_step(
                    doc_id, form, f"/accounts/{result['account_id']}",
                    f"« {result['replaced']} » remplacé : {result['removed']} opération(s) retirée(s), {result['added']} ajoutée(s) depuis le nouveau fichier",
                )
            if action == "keep":
                result = await run_in_threadpool(core.import_default, doc_id, index, True)
                return await after_step(doc_id, form, f"/accounts/{result['account_id']}", "Les deux fichiers sont gardés. " + import_message(result))
            if action == "copy":
                await run_in_threadpool(core.keep_duplicate, doc_id)
                return await after_step(doc_id, form, f"/documents/{doc_id}", "Copie conservée à côté de l'original (rien n'est importé)")
            if action == "discard":
                await run_in_threadpool(core.delete_document, doc_id)
                return await after_step(doc_id, form, "/documents", "Nouveau fichier écarté : l'ancien reste en place")
        except Exception as exc:
            return redirect(f"/documents/{doc_id}", f"Impossible : {exc}")
        return redirect(f"/documents/{doc_id}", "Action inconnue")

    @app.post("/documents/{doc_id}/keep")
    async def document_keep(request: Request, doc_id: int):
        await verified_form(request)
        await run_in_threadpool(core.keep_duplicate, doc_id)
        return redirect(f"/documents/{doc_id}", "Copie conservée à côté de l'original (rien n'est importé)")

    @app.post("/documents/{doc_id}/undo")
    async def document_undo(request: Request, doc_id: int):
        await verified_form(request)
        removed = await run_in_threadpool(core.undo_import, doc_id)
        return redirect(f"/documents/{doc_id}", f"Import annulé : {removed} opération(s) retirée(s)")

    @app.get("/documents/{doc_id}/download")
    async def document_download(request: Request, doc_id: int):
        filename, data = await run_in_threadpool(core.read_document, doc_id)
        safe = quote(filename)
        return Response(
            data,
            media_type="application/octet-stream",
            headers={"Content-Disposition": f"attachment; filename*=UTF-8''{safe}"},
        )

    @app.post("/documents/{doc_id}/delete")
    async def document_delete(request: Request, doc_id: int):
        await verified_form(request)
        core.delete_document(doc_id)
        return redirect("/documents", "Document supprimé")

    # --------------------------------------------------------- contacts
    @app.get("/contacts")
    async def contacts_page(request: Request, all: int = 0):
        with core.db.session() as s:
            summaries = contact_service.summaries(s)
            pending = contact_service.unresolved(s, include_shops=bool(all))
        return render(request, "contacts.html", summaries=summaries, pending=pending, include_shops=bool(all))

    @app.get("/contacts/identify")
    async def contact_identify_page(request: Request, name: str = "", iban: str = "", next: str = "/contacts"):
        with core.db.session() as s:
            contacts = list(s.scalars(select(Counterparty).order_by(Counterparty.name)))
        return render(request, "contact_identify.html", printed=name, iban=iban, next=safe_next(next, "/contacts"), contacts=contacts)

    @app.post("/contacts/identify")
    async def contact_identify(request: Request):
        form = await verified_form(request)
        target = safe_next(form.get("next"), "/contacts")
        try:
            with core.db.session() as s:
                cp = contact_service.identify(s, form)
                name = cp.name
        except ValueError as exc:
            return redirect(f"/contacts/identify?name={quote(form.get('printed_name', ''))}&iban={quote(form.get('iban', ''))}&next={quote(target)}", str(exc))
        return redirect(target, f"« {name} » enregistré : les prochains imports le reconnaîtront")

    @app.get("/contacts/new")
    async def contact_new(request: Request):
        return render(request, "contact.html", contact=None, transactions=[], others=[])

    @app.post("/contacts/new")
    async def contact_create(request: Request):
        form = await verified_form(request)
        try:
            with core.db.session() as s:
                cp = contact_service.save_contact(s, None, form)
                contact_service.add_identifier(s, cp, "iban", form.get("iban"))
                contact_service.add_identifier(s, cp, "name", form.get("printed_name"), form.get("printed_name"))
                contact_service.relink(s)
                contact_id = cp.id
        except ValueError as exc:
            return render(request, "contact.html", 400, contact=None, transactions=[], others=[], error=str(exc))
        return redirect(f"/contacts/{contact_id}", "Contact créé")

    @app.get("/contacts/{contact_id}")
    async def contact_detail(request: Request, contact_id: int):
        with core.db.session() as s:
            contact = s.get(Counterparty, contact_id)
            if contact is None:
                raise HTTPException(404)
            _ = contact.identifiers
            visible = access.readable_ids(s)
            transactions = [
                t for t in s.scalars(
                    select(Transaction).where(Transaction.counterparty_id == contact_id)
                    .order_by(Transaction.date.desc(), Transaction.id.desc()).limit(2000)
                ) if visible is None or t.account_id in visible
            ][:200]
            account_names = {a.id: a.name for a in access.only_readable(s, list(s.scalars(select(Account))))}
            others = list(s.scalars(select(Counterparty).where(Counterparty.id != contact_id).order_by(Counterparty.name)))
        return render(request, "contact.html", contact=contact, transactions=transactions, others=others, account_names=account_names)

    @app.post("/contacts/{contact_id}")
    async def contact_update(request: Request, contact_id: int):
        form = await verified_form(request)
        with core.db.session() as s:
            contact_service.save_contact(s, contact_id, form)
        return redirect(f"/contacts/{contact_id}", "Contact enregistré")

    @app.post("/contacts/{contact_id}/identifiers")
    async def contact_add_identifier(request: Request, contact_id: int):
        form = await verified_form(request)
        try:
            with core.db.session() as s:
                cp = s.get(Counterparty, contact_id)
                added = contact_service.add_identifier(s, cp, form.get("kind", "iban"), form.get("value"), form.get("value"))
                linked = contact_service.relink(s) if added else 0
        except ValueError as exc:
            return redirect(f"/contacts/{contact_id}", str(exc))
        return redirect(f"/contacts/{contact_id}", f"Identifiant ajouté : {linked} opération(s) rattachée(s)" if added else "Rien à ajouter")

    @app.post("/contacts/{contact_id}/identifiers/{identifier_id}/delete")
    async def contact_remove_identifier(request: Request, contact_id: int, identifier_id: int):
        await verified_form(request)
        with core.db.session() as s:
            contact_service.remove_identifier(s, identifier_id)
        return redirect(f"/contacts/{contact_id}", "Identifiant retiré")

    @app.post("/contacts/{contact_id}/merge")
    async def contact_merge(request: Request, contact_id: int):
        form = await verified_form(request)
        target = int(form.get("target") or 0)
        if not target:
            return redirect(f"/contacts/{contact_id}", "Choisis le contact à conserver")
        with core.db.session() as s:
            contact_service.merge(s, contact_id, target)
        return redirect(f"/contacts/{target}", "Contacts fusionnés")

    @app.post("/contacts/{contact_id}/delete")
    async def contact_delete(request: Request, contact_id: int):
        await verified_form(request)
        with core.db.session() as s:
            contact_service.delete_contact(s, contact_id)
        return redirect("/contacts", "Contact supprimé (les opérations gardent le nom imprimé par la banque)")

    # --------------------------------------------------------- settings
    @app.get("/settings")
    async def settings_page(request: Request):
        rates = sorted(RATE_HISTORY, key=lambda r: r[0], reverse=True)
        me = request.state.session.identity
        with core.db.session() as s:
            people = user_service.all_users(s) if me.is_admin else []
            owned = {u.id: s.scalar(select(func.count()).select_from(Account).where(Account.owner_id == u.id)) for u in people}
        return render(
            request, "settings.html", rates=rates, settings=settings, people=people, owned=owned,
            coingecko_key=bool(core.get_setting("coingecko_api_key")), recovered=request.query_params.get("recovered"),
            cf_enabled=cf.enabled, cf_host=cf.host,
        )

    @app.post("/settings/password")
    async def settings_password(request: Request):
        """Everyone changes their own password: it is the key slot of this user, the others are not touched."""
        form = await verified_form(request)
        me = request.state.session.identity
        if form.get("new") != form.get("confirm"):
            return redirect("/settings", "Les deux nouveaux mots de passe ne correspondent pas")
        try:
            await run_in_threadpool(core.vault.change_password, form.get("current", ""), form.get("new", ""), user_service.slot_id(me.email))
        except InvalidPassword:
            return redirect("/settings", "Mot de passe actuel incorrect")
        except ValueError as exc:
            return redirect("/settings", str(exc))
        return redirect("/settings", "Mot de passe changé")

    @app.post("/settings/recovery")
    async def settings_recovery(request: Request):
        form = await verified_form(request)
        me = admin_only(request)
        try:
            await run_in_threadpool(core.check_password, form.get("current", ""), me.email)
        except (InvalidPassword, PermissionError):
            return redirect("/settings", "Mot de passe incorrect")
        recovery = await run_in_threadpool(core.vault.regenerate_recovery)
        return render(request, "recovery.html", recovery=recovery)

    @app.post("/settings/users")
    async def settings_user_add(request: Request):
        """An administrator gives someone access: their e-mail (the one Cloudflare vouches for) and a first password."""
        form = await verified_form(request)
        admin_only(request)
        try:
            with core.db.session() as s:
                user = user_service.add_with_password(
                    s, core.vault, form.get("email", ""), form.get("name"), form.get("password", ""), form.get("is_admin") == "on"
                )
                label = user.label
        except ValueError as exc:
            return redirect("/settings#utilisateurs", str(exc))
        return redirect("/settings#utilisateurs", f"{label} peut maintenant se connecter (par Cloudflare ou avec son mot de passe)")

    @app.post("/settings/users/{user_id}/delete")
    async def settings_user_delete(request: Request, user_id: int):
        await verified_form(request)
        me = admin_only(request)
        try:
            with core.db.session() as s:
                user, heir = s.get(User, user_id), s.get(User, me.id)
                if user is None:
                    raise ValueError("Utilisateur introuvable")
                label = user.label
                moved = user_service.remove(s, core.vault, user, heir)
        except ValueError as exc:
            return redirect("/settings#utilisateurs", str(exc))
        sessions.drop_user(user_id)
        return redirect("/settings#utilisateurs", f"{label} supprimé" + (f" : ses {moved} compte(s) sont maintenant à toi" if moved else ""))

    @app.post("/settings/users/{user_id}/role")
    async def settings_user_role(request: Request, user_id: int):
        form = await verified_form(request)
        me = admin_only(request)
        make_admin = form.get("is_admin") == "on"
        with core.db.session() as s:
            user = s.get(User, user_id)
            if user is None:
                raise HTTPException(404)
            if user.is_admin and not make_admin and sum(1 for u in user_service.all_users(s) if u.is_admin) <= 1:
                return redirect("/settings#utilisateurs", "Il doit rester au moins un administrateur")
            user.is_admin = make_admin
            identity = user_service.identity(user)
        sessions.update_identity(identity)
        return redirect("/settings#utilisateurs", "Rôle modifié")

    @app.post("/settings/users/{user_id}/password")
    async def settings_user_password(request: Request, user_id: int):
        """Forgotten password: the administrator sets a new one for that user's key slot."""
        form = await verified_form(request)
        admin_only(request)
        try:
            from ..security.vault import check_password_strength

            check_password_strength(form.get("password", ""))
            with core.db.session() as s:
                user = s.get(User, user_id)
                if user is None:
                    raise HTTPException(404)
                core.keystore.set_password(core.vault._master(), form["password"], user_service.slot_id(user.email))
                label = user.label
        except ValueError as exc:
            return redirect("/settings#utilisateurs", str(exc))
        return redirect("/settings#utilisateurs", f"Nouveau mot de passe défini pour {label}")

    @app.post("/settings/general")
    async def settings_general(request: Request):
        form = await verified_form(request)
        admin_only(request)
        if form.get("coingecko_api_key"):
            core.set_setting("coingecko_api_key", form["coingecko_api_key"].strip())
        if form.get("clear_coingecko") == "on":
            core.set_setting("coingecko_api_key", "")
        return redirect("/settings", "Paramètres enregistrés")

    return app
