from __future__ import annotations

import hmac
import logging
from contextlib import asynccontextmanager
from datetime import date, datetime
from decimal import Decimal
from pathlib import Path
from urllib.parse import parse_qsl, quote, urlparse

from fastapi import FastAPI, HTTPException, Request
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import HTMLResponse, RedirectResponse, Response
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from sqlalchemy import func, select

from ..config import Settings, settings as default_settings
from ..connectors.registry import CONNECTORS, get_connector
from ..core import MyMoney
from ..importers import ParsedStatement
from ..models import ACCOUNT_TYPES, CATEGORIES, Account, Connection, Document, Transaction
from ..security.keystore import InvalidPassword
from ..security.vault import VaultLocked
from ..services.rates import RATE_HISTORY, effective_rate
from ..services.valuation import Valuator
from .security import COOKIE, SECURITY_HEADERS, Sessions, verified_form

log = logging.getLogger(__name__)
HERE = Path(__file__).parent
PUBLIC_PATHS = ("/static/", "/healthz", "/login", "/setup", "/connections/callback")


# ----------------------------------------------------------------- formatting
def money(value, currency: str = "EUR", decimals: int = 2) -> str:
    if value is None:
        return "—"
    value = Decimal(value)
    sign = "-" if value < 0 else ""
    text = f"{abs(value):,.{decimals}f}".replace(",", " ").replace(".", ",")
    symbol = {"EUR": "€", "USD": "$", "GBP": "£"}.get(currency, currency)
    return f"{sign}{text} {symbol}"


def quantity(value) -> str:
    if value is None:
        return "—"
    value = Decimal(value)
    text = f"{value:,.8f}".rstrip("0").rstrip(".")
    return text.replace(",", " ").replace(".", ",")


def frdate(value) -> str:
    if not value:
        return "—"
    if isinstance(value, datetime):
        return value.strftime("%d/%m/%Y %H:%M")
    if isinstance(value, date):
        return value.strftime("%d/%m/%Y")
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
def create_app(settings: Settings | None = None) -> FastAPI:
    settings = settings or default_settings
    core = MyMoney(settings)
    sessions = Sessions(settings.auto_lock_minutes)
    core.vault.on_lock(sessions.clear)
    templates = Jinja2Templates(directory=str(HERE / "templates"))
    templates.env.filters.update(money=money, qty=quantity, frdate=frdate)
    templates.env.globals.update(ACCOUNT_TYPES=ACCOUNT_TYPES, CATEGORIES=CATEGORIES, CONNECTORS=CONNECTORS)

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
    app.mount("/static", StaticFiles(directory=str(HERE / "static")), name="static")

    def render(request: Request, name: str, status_code: int = 200, **context) -> HTMLResponse:
        session = getattr(request.state, "session", None)
        context.update(request=request, csrf=session.csrf if session else "", unlocked=core.vault.unlocked, flash=request.query_params.get("msg"))
        return templates.TemplateResponse(request, name, context, status_code=status_code)

    def redirect(url: str, msg: str | None = None) -> RedirectResponse:
        if msg:
            url += ("&" if "?" in url else "?") + "msg=" + quote(msg)
        return RedirectResponse(url, status_code=303)

    @app.middleware("http")
    async def guard(request: Request, call_next):
        path = request.url.path
        if request.method == "POST":
            origin = request.headers.get("origin")
            if origin and origin != "null" and urlparse(origin).netloc != request.headers.get("host"):
                return Response("Origine refusée", status_code=403)
        request.state.session = sessions.get(request.cookies.get(COOKIE))
        public = path.startswith(PUBLIC_PATHS)
        if not public:
            if not core.vault.initialized:
                return RedirectResponse("/setup", status_code=303)
            if request.state.session is None or not core.vault.unlocked:
                if request.state.session is not None:
                    sessions.drop(request.cookies.get(COOKIE))
                return RedirectResponse("/login", status_code=303)
            core.vault.touch()
        try:
            response = await call_next(request)
        except VaultLocked:
            response = RedirectResponse("/login", status_code=303)
        for key, value in SECURITY_HEADERS.items():
            response.headers.setdefault(key, value)
        if settings.cookie_secure:
            response.headers.setdefault("Strict-Transport-Security", "max-age=31536000")
        return response

    def start_session(response: Response) -> None:
        response.set_cookie(COOKIE, sessions.create(), httponly=True, samesite="strict", secure=settings.cookie_secure, path="/")

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
            recovery = await run_in_threadpool(core.vault.setup, password)
        except ValueError as exc:
            return render(request, "setup.html", 400, error=str(exc))
        response = render(request, "recovery.html", recovery=recovery)
        start_session(response)
        return response

    @app.get("/login")
    async def login_page(request: Request):
        if not core.vault.initialized:
            return redirect("/setup")
        return render(request, "login.html")

    @app.post("/login")
    async def login(request: Request):
        form = await request.form()
        try:
            if form.get("recovery"):
                await run_in_threadpool(core.vault.unlock_with_recovery, str(form["recovery"]))
            else:
                await run_in_threadpool(core.check_password, str(form.get("password", "")))
        except InvalidPassword:
            return render(request, "login.html", 401, error="Mot de passe incorrect")
        except PermissionError as exc:
            return render(request, "login.html", 429, error=str(exc))
        response = redirect("/settings?recovered=1" if form.get("recovery") else "/")
        start_session(response)
        return response

    @app.post("/logout")
    async def logout(request: Request):
        form = await verified_form(request)
        sessions.drop(request.cookies.get(COOKIE))
        if form.get("lock") == "1" and not settings.unlock_password_file:
            core.vault.lock()
        response = redirect("/login", "Coffre verrouillé" if form.get("lock") == "1" else "Déconnecté")
        response.delete_cookie(COOKIE, path="/")
        return response

    # -------------------------------------------------------- dashboard
    @app.get("/")
    async def dashboard(request: Request):
        portfolio = await run_in_threadpool(core.portfolio)
        with core.db.session() as s:
            connections = list(s.scalars(select(Connection).order_by(Connection.name)))
        groups: dict[str, list] = {}
        for value in portfolio.accounts:
            groups.setdefault(value.account.category, []).append(value)
        return render(
            request,
            "dashboard.html",
            portfolio=portfolio,
            groups=[(code, CATEGORIES[code], groups[code]) for code in CATEGORIES if code in groups],
            chart=sparkline(portfolio.history),
            connections=connections,
            running=core.running,
        )

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
            _ = account.holdings, account.connection
            per_page = 100
            total_tx = s.scalar(select(func.count()).select_from(Transaction).where(Transaction.account_id == account_id))
            transactions = list(
                s.scalars(
                    select(Transaction).where(Transaction.account_id == account_id)
                    .order_by(Transaction.date.desc(), Transaction.id.desc()).offset((page - 1) * per_page).limit(per_page)
                )
            )
        valuator = Valuator(core.prices)
        value = await run_in_threadpool(lambda: (valuator.prefetch([account]), valuator.account_value(account))[1])
        rate, rate_source = effective_rate(account.account_type, account.interest_rate)
        return render(
            request, "account.html", account=account, value=value, transactions=transactions, rate=rate, rate_source=rate_source,
            page=page, pages=max(1, -(-total_tx // per_page)), total_tx=total_tx,
        )

    @app.get("/accounts/{account_id}/edit")
    async def account_edit(request: Request, account_id: int):
        with core.db.session() as s:
            account = s.get(Account, account_id)
            if account is None:
                raise HTTPException(404)
            _ = account.connection
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

    # ------------------------------------------------------ connections
    @app.get("/connections")
    async def connections_page(request: Request):
        with core.db.session() as s:
            connections = list(s.scalars(select(Connection).order_by(Connection.name)))
            for c in connections:
                _ = c.accounts
        return render(request, "connections.html", connections=connections, running=core.running, prompts=core.prompts)

    @app.get("/connections/new")
    async def connection_new(request: Request, type: str | None = None):
        if not type:
            return render(request, "connection_types.html")
        return render(request, "connection_form.html", cls=get_connector(type), connection=None, values={})

    @app.post("/connections/new")
    async def connection_create(request: Request):
        form = await verified_form(request)
        cls = get_connector(form.get("connector_type", ""))
        try:
            conn_id = core.create_connection(cls.type, form.get("name", ""), {f.name: form.get(f.name, "") for f in cls.fields})
        except ValueError as exc:
            values = {f.name: form.get(f.name, "") for f in cls.fields if not f.secret}
            return render(request, "connection_form.html", 400, cls=cls, connection=None, values=values, error=str(exc))
        return redirect(f"/connections/{conn_id}/connect")

    @app.get("/connections/{conn_id}/edit")
    async def connection_edit(request: Request, conn_id: int):
        with core.db.session() as s:
            conn = s.get(Connection, conn_id)
            if conn is None:
                raise HTTPException(404)
            cls = get_connector(conn.connector_type)
            config = core.connection_config(conn)
        values = {f.name: config.get(f.name, "") for f in cls.fields if not f.secret}
        return render(request, "connection_form.html", cls=cls, connection=conn, values=values)

    @app.post("/connections/{conn_id}/edit")
    async def connection_update(request: Request, conn_id: int):
        form = await verified_form(request)
        with core.db.session() as s:
            cls = get_connector(s.get(Connection, conn_id).connector_type)
        core.update_connection(conn_id, form.get("name", ""), {f.name: form.get(f.name, "") for f in cls.fields})
        return redirect("/connections", "Connexion mise à jour")

    @app.post("/connections/{conn_id}/delete")
    async def connection_delete(request: Request, conn_id: int):
        form = await verified_form(request)
        core.delete_connection(conn_id, form.get("delete_accounts") == "on")
        return redirect("/connections", "Connexion supprimée")

    @app.get("/connections/{conn_id}/connect")
    async def connection_connect(request: Request, conn_id: int):
        """Next step after creating a connection: bank login, 2FA prompt, or a first sync."""
        with core.db.session() as s:
            conn = s.get(Connection, conn_id)
            if conn is None:
                raise HTTPException(404)
            cls = get_connector(conn.connector_type)
        if cls.auth_flow == "redirect":
            return render(request, "connect_redirect.html", connection=conn, cls=cls, redirect_url=f"{settings.public_url}/connections/callback")
        if cls.auth_flow == "interactive" and conn.connector_type == "traderepublic":
            return redirect(f"/connections/{conn_id}/login")
        core.sync_async(conn_id)
        return redirect("/connections", "Synchronisation lancée")

    @app.get("/connections/{conn_id}/authorize")
    async def connection_authorize(request: Request, conn_id: int):
        try:
            url = await run_in_threadpool(core.start_auth, conn_id)
        except Exception as exc:
            return redirect("/connections", f"Impossible de démarrer la connexion : {exc}")
        return RedirectResponse(url, status_code=303)

    @app.get("/connections/callback")
    async def connection_callback(request: Request):
        # The bank redirects here cross-site: the SameSite=Strict cookie is not sent, so the request
        # is matched to a connection through the random `state` we generated instead.
        params = dict(request.query_params)
        if not core.vault.unlocked:
            return render(request, "callback.html", ok=False, message="Le coffre est verrouillé : déverrouille-le puis colle l'URL de retour dans la page de la connexion.")
        conn_id = core.find_pending_auth(params)
        if conn_id is None:
            return render(request, "callback.html", ok=False, message="Réponse de la banque inconnue ou expirée.")
        try:
            await run_in_threadpool(core.finish_auth, conn_id, params)
        except Exception as exc:
            return render(request, "callback.html", ok=False, message=str(exc))
        core.sync_async(conn_id)
        return render(request, "callback.html", ok=True, message="Banque connectée, première synchronisation en cours.")

    @app.post("/connections/{conn_id}/callback-paste")
    async def connection_callback_paste(request: Request, conn_id: int):
        form = await verified_form(request)
        params = dict(parse_qsl(urlparse(form.get("url", "")).query))
        try:
            await run_in_threadpool(core.finish_auth, conn_id, params)
        except Exception as exc:
            return redirect(f"/connections/{conn_id}/connect", f"Échec : {exc}")
        core.sync_async(conn_id)
        return redirect("/connections", "Banque connectée, synchronisation lancée")

    @app.get("/connections/{conn_id}/login")
    async def connection_login_page(request: Request, conn_id: int):
        with core.db.session() as s:
            conn = s.get(Connection, conn_id)
        if conn is None:
            raise HTTPException(404)
        prompt = core.prompts.get(conn_id)
        if prompt is None and conn.connector_type == "traderepublic":
            try:
                prompt = await run_in_threadpool(core.login, conn_id, None)
            except Exception as exc:
                return redirect("/connections", f"Connexion impossible : {exc}")
        if prompt is None:
            return redirect("/connections")
        return render(request, "login_step.html", connection=conn, prompt=prompt)

    @app.post("/connections/{conn_id}/login")
    async def connection_login(request: Request, conn_id: int):
        form = await verified_form(request)
        answers = {k: v for k, v in form.items() if k != "csrf"}
        try:
            prompt = await run_in_threadpool(core.login, conn_id, answers)
        except Exception as exc:
            core.prompts.pop(conn_id, None)
            return redirect("/connections", f"Échec de la connexion : {exc}")
        if prompt is not None:
            return redirect(f"/connections/{conn_id}/login")
        core.sync_async(conn_id)
        return redirect("/connections", "Connecté, synchronisation lancée")

    @app.post("/connections/{conn_id}/sync")
    async def connection_sync(request: Request, conn_id: int):
        await verified_form(request)
        core.sync_async(conn_id)
        return redirect("/connections", "Synchronisation lancée")

    @app.post("/connections/sync-all")
    async def connections_sync_all(request: Request):
        await verified_form(request)
        core.sync_all()
        return redirect("/connections", "Synchronisation de toutes les connexions lancée")

    @app.get("/connections/{conn_id}/banks")
    async def connection_banks(request: Request, conn_id: int):
        with core.db.session() as s:
            connector = core._connector(s.get(Connection, conn_id))
        try:
            banks = await run_in_threadpool(connector.list_banks)
        except Exception as exc:
            return redirect(f"/connections/{conn_id}/connect", f"Liste indisponible : {exc}")
        return render(request, "banks.html", banks=banks, conn_id=conn_id)

    # -------------------------------------------------------- documents
    @app.get("/documents")
    async def documents_page(request: Request):
        with core.db.session() as s:
            documents = list(s.scalars(select(Document).order_by(Document.uploaded_at.desc())))
        return render(request, "documents.html", documents=documents, max_mb=settings.max_upload_mb)

    @app.post("/documents")
    async def documents_upload(request: Request):
        form = await request.form()
        session = request.state.session
        token = form.get("csrf")
        if not session or not isinstance(token, str) or not hmac.compare_digest(token, session.csrf):
            raise HTTPException(403, "Jeton CSRF invalide")
        ids = []
        for upload in form.getlist("files"):
            if not hasattr(upload, "read"):
                continue
            data = await upload.read(settings.max_upload_mb * 1024 * 1024 + 1)
            if len(data) > settings.max_upload_mb * 1024 * 1024:
                return redirect("/documents", f"{upload.filename} dépasse {settings.max_upload_mb} Mo")
            ids.append(await run_in_threadpool(core.store_document, upload.filename or "document", data))
        if len(ids) == 1:
            return redirect(f"/documents/{ids[0]}")
        return redirect("/documents", f"{len(ids)} document(s) ajouté(s)")

    @app.get("/documents/{doc_id}")
    async def document_review(request: Request, doc_id: int):
        with core.db.session() as s:
            doc = s.get(Document, doc_id)
            if doc is None:
                raise HTTPException(404)
            accounts = list(s.scalars(select(Account).order_by(Account.institution, Account.name)))
        statements = [ParsedStatement.from_json(item) for item in (doc.parse_result or {}).get("statements", [])]
        suggestions = [core.suggest_account(st) for st in statements]
        return render(request, "document.html", doc=doc, statements=statements, suggestions=suggestions, accounts=accounts)

    @app.post("/documents/{doc_id}/import/{index}")
    async def document_import(request: Request, doc_id: int, index: int):
        form = await verified_form(request)
        try:
            result = core.import_statement(doc_id, index, form)
        except Exception as exc:
            return redirect(f"/documents/{doc_id}", f"Import impossible : {exc}")
        return redirect(f"/accounts/{result['account_id']}", f"Import terminé : {result['transactions']} opération(s) ajoutée(s)")

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

    # --------------------------------------------------------- settings
    @app.get("/settings")
    async def settings_page(request: Request):
        rates = sorted(RATE_HISTORY, key=lambda r: r[0], reverse=True)
        return render(
            request, "settings.html", rates=rates, settings=settings,
            coingecko_key=bool(core.get_setting("coingecko_api_key")), recovered=request.query_params.get("recovered"),
        )

    @app.post("/settings/password")
    async def settings_password(request: Request):
        form = await verified_form(request)
        if form.get("new") != form.get("confirm"):
            return redirect("/settings", "Les deux nouveaux mots de passe ne correspondent pas")
        try:
            await run_in_threadpool(core.vault.change_password, form.get("current", ""), form.get("new", ""))
        except InvalidPassword:
            return redirect("/settings", "Mot de passe actuel incorrect")
        except ValueError as exc:
            return redirect("/settings", str(exc))
        return redirect("/settings", "Mot de passe changé")

    @app.post("/settings/recovery")
    async def settings_recovery(request: Request):
        form = await verified_form(request)
        try:
            await run_in_threadpool(core.check_password, form.get("current", ""))
        except (InvalidPassword, PermissionError):
            return redirect("/settings", "Mot de passe incorrect")
        recovery = await run_in_threadpool(core.vault.regenerate_recovery)
        return render(request, "recovery.html", recovery=recovery)

    @app.post("/settings/general")
    async def settings_general(request: Request):
        form = await verified_form(request)
        if form.get("coingecko_api_key"):
            core.set_setting("coingecko_api_key", form["coingecko_api_key"].strip())
        if form.get("clear_coingecko") == "on":
            core.set_setting("coingecko_api_key", "")
        return redirect("/settings", "Paramètres enregistrés")

    @app.post("/settings/woob-update")
    async def settings_woob(request: Request):
        await verified_form(request)
        from ..connectors.woob_connector import update_modules

        try:
            await run_in_threadpool(update_modules, str(settings.woob_dir))
        except Exception as exc:
            return redirect("/settings", f"Mise à jour Woob impossible : {exc}")
        return redirect("/settings", "Modules Woob mis à jour")

    return app
