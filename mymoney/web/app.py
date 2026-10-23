from __future__ import annotations

import hmac
import logging
from contextlib import asynccontextmanager
from datetime import date, datetime
from decimal import Decimal
from pathlib import Path
from urllib.parse import quote, urlparse

from fastapi import FastAPI, HTTPException, Request
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import HTMLResponse, RedirectResponse, Response
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from sqlalchemy import func, select
from sqlalchemy.orm import selectinload

from ..adapters import ADAPTERS, catalog, get_adapter
from ..config import Settings, settings as default_settings
from ..core import MyMoney
from ..models import ACCOUNT_TYPES, CATEGORIES, COUNTERPARTY_KINDS, Account, Counterparty, Document, Transaction
from ..security.keystore import InvalidPassword
from ..security.vault import VaultLocked
from ..services import counterparties as contact_service
from ..services import statements as statement_service
from ..services.rates import RATE_HISTORY, effective_rate
from ..services.valuation import Valuator
from . import pwa
from .security import COOKIE, SECURITY_HEADERS, Sessions, verified_form

log = logging.getLogger(__name__)
HERE = Path(__file__).parent
PUBLIC_PATHS = ("/static/", "/healthz", "/login", "/setup", "/manifest.webmanifest", "/sw.js", "/icons/")
KIND_LABELS = {
    "card": "Carte", "transfer": "Virement", "instant_transfer": "Virement instantané", "direct_debit": "Prélèvement",
    "loan_payment": "Prêt", "fee": "Frais", "cash": "Retrait", "cheque": "Chèque", "interest": "Intérêts",
}
DOCUMENT_STATUS = {"uploaded": "stocké", "needs_choice": "à préciser", "parsed": "à valider", "imported": "importé", "failed": "non reconnu"}


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
def safe_next(value: str | None, default: str) -> str:
    """Only local paths are accepted as a return address (no open redirect)."""
    return value if value and value.startswith("/") and not value.startswith("//") else default


def create_app(settings: Settings | None = None) -> FastAPI:
    settings = settings or default_settings
    core = MyMoney(settings)
    sessions = Sessions(settings.auto_lock_minutes)
    core.vault.on_lock(sessions.clear)
    templates = Jinja2Templates(directory=str(HERE / "templates"))
    templates.env.filters.update(money=money, qty=quantity, frdate=frdate)
    templates.env.globals.update(
        ACCOUNT_TYPES=ACCOUNT_TYPES, CATEGORIES=CATEGORIES, COUNTERPARTY_KINDS=COUNTERPARTY_KINDS, KIND_LABELS=KIND_LABELS,
        DOCUMENT_STATUS=DOCUMENT_STATUS,
    )

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
    pwa.install(app)

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
            to_identify = len(contact_service.unresolved(s))
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
            _ = account.holdings
            per_page = 100
            total_tx = s.scalar(select(func.count()).select_from(Transaction).where(Transaction.account_id == account_id))
            transactions = list(
                s.scalars(
                    select(Transaction).where(Transaction.account_id == account_id).options(selectinload(Transaction.counterparty))
                    .order_by(Transaction.date.desc(), Transaction.id.desc()).offset((page - 1) * per_page).limit(per_page)
                )
            )
            statements, gaps = statement_service.coverage(s, account_id)
            to_identify = contact_service.unresolved(s, account_id=account_id)
        valuator = Valuator(core.prices)
        value = await run_in_threadpool(lambda: (valuator.prefetch([account]), valuator.account_value(account))[1])
        rate, rate_source = effective_rate(account.account_type, account.interest_rate)
        return render(
            request, "account.html", account=account, value=value, transactions=transactions, rate=rate, rate_source=rate_source,
            page=page, pages=max(1, -(-total_tx // per_page)), total_tx=total_tx, statements=statements, gaps=gaps,
            to_identify=to_identify,
        )

    @app.get("/accounts/{account_id}/edit")
    async def account_edit(request: Request, account_id: int):
        with core.db.session() as s:
            account = s.get(Account, account_id)
            if account is None:
                raise HTTPException(404)
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
    async def documents_page(request: Request):
        with core.db.session() as s:
            documents = list(s.scalars(select(Document).order_by(Document.uploaded_at.desc())))
        return render(request, "documents.html", documents=documents, max_mb=settings.max_upload_mb, catalog=catalog())

    @app.post("/documents")
    async def documents_upload(request: Request):
        form = await request.form()
        session = request.state.session
        token = form.get("csrf")
        if not session or not isinstance(token, str) or not hmac.compare_digest(token, session.csrf):
            raise HTTPException(403, "Jeton CSRF invalide")
        outcomes: list[tuple[int, str, dict | None]] = []  # (document id, "existing" | "imported" | status, import result)
        for upload in form.getlist("files"):
            if not hasattr(upload, "read"):
                continue
            data = await upload.read(settings.max_upload_mb * 1024 * 1024 + 1)
            if len(data) > settings.max_upload_mb * 1024 * 1024:
                return redirect("/documents", f"{upload.filename} dépasse {settings.max_upload_mb} Mo")
            doc_id, existing = await run_in_threadpool(core.store_document, upload.filename or "document", data)
            if existing:
                outcomes.append((doc_id, "existing", None))
                continue
            status = await run_in_threadpool(core.analyze_document, doc_id, None, data)
            imported = await run_in_threadpool(core.auto_import, doc_id) if status == "parsed" else None
            outcomes.append((doc_id, "imported" if imported else status, imported))
        if not outcomes:
            return redirect("/documents", "Aucun fichier reçu")
        if len(outcomes) == 1:
            doc_id, state, imported = outcomes[0]
            if state == "existing":
                return redirect(f"/documents/{doc_id}", "Ce fichier a déjà été déposé : il n'est pas ajouté une seconde fois")
            if imported:
                return redirect(f"/accounts/{imported['account_id']}", import_message(imported, auto=True))
            return redirect(f"/documents/{doc_id}")
        counts = {k: sum(1 for _, state, _ in outcomes if state == k) for k in ("imported", "existing")}
        review = len(outcomes) - counts["imported"] - counts["existing"]
        parts = [f"{len(outcomes)} fichier(s)", f"{counts['imported']} importé(s) automatiquement"]
        if review:
            parts.append(f"{review} à vérifier")
        if counts["existing"]:
            parts.append(f"{counts['existing']} déjà présent(s)")
        return redirect("/documents", " · ".join(parts))

    def import_message(result: dict, auto: bool = False) -> str:
        text = f"{result['added']} opération(s) ajoutée(s)"
        if result["skipped_exact"]:
            text += f" · {result['skipped_exact']} doublon(s) exact(s) ignoré(s)"
        if result["skipped_probable"]:
            text += f" · {result['skipped_probable']} doublon(s) probable(s) ignoré(s)"
        return ("Importé automatiquement : " if auto else "Import terminé : ") + text

    @app.get("/documents/{doc_id}")
    async def document_review(request: Request, doc_id: int):
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
        return render(
            request, "document.html", doc=doc, result=result, plans=review["plans"], accounts=review["accounts"],
            candidates=candidates, adapters=ADAPTERS, to_identify=to_identify,
        )

    @app.post("/documents/{doc_id}/analyze")
    async def document_analyze(request: Request, doc_id: int):
        form = await verified_form(request)
        adapter_id = form.get("adapter") or None
        try:
            if adapter_id:
                get_adapter(adapter_id)
            await run_in_threadpool(core.analyze_document, doc_id, adapter_id)
        except Exception as exc:
            return redirect(f"/documents/{doc_id}", f"Analyse impossible : {exc}")
        return redirect(f"/documents/{doc_id}")

    @app.post("/documents/{doc_id}/import/{index}")
    async def document_import(request: Request, doc_id: int, index: int):
        form = await verified_form(request)
        try:
            result = await run_in_threadpool(core.import_statement, doc_id, index, form)
        except Exception as exc:
            return redirect(f"/documents/{doc_id}", f"Import impossible : {exc}")
        return redirect(f"/accounts/{result['account_id']}", import_message(result))

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
            transactions = list(
                s.scalars(
                    select(Transaction).where(Transaction.counterparty_id == contact_id)
                    .order_by(Transaction.date.desc(), Transaction.id.desc()).limit(200)
                )
            )
            account_names = {a.id: a.name for a in s.scalars(select(Account))}
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

    return app
