"""Application services shared by the web UI and the CLI."""

from __future__ import annotations

import hashlib
import logging
import os
import secrets
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta
from decimal import Decimal
from typing import Any

from sqlalchemy import select

from .config import Settings
from .connectors import live
from .connectors.base import Context, NeedsAuth, NeedsInteraction, Prompt
from .connectors.registry import get_connector
from .connectors.woob_connector import WoobConnector
from .db import Database
from .importers import ParsedStatement, parse_document
from .models import ACCOUNT_TYPES, CATEGORIES, Account, Connection, Document, Holding, PriceCache, Setting, Snapshot, Transaction, utcnow
from .security.keystore import InvalidPassword, Keystore
from .security.vault import Vault, VaultLocked
from .services import sync as sync_service
from .services.prices import PriceService
from .services.valuation import AccountValue, Valuator

log = logging.getLogger(__name__)


@dataclass
class Portfolio:
    accounts: list[AccountValue]
    total: Decimal
    by_category: list[tuple[str, str, Decimal]]
    assets: Decimal
    debts: Decimal
    history: list[tuple[date, Decimal]] = field(default_factory=list)


class MyMoney:
    def __init__(self, settings: Settings):
        self.settings = settings
        settings.ensure_dirs()
        os.environ.setdefault("WOOB_WORKDIR", str(settings.woob_dir))
        WoobConnector.workdir = str(settings.woob_dir)
        self.keystore = Keystore(settings.keystore_path, settings.argon2_time_cost, settings.argon2_memory_kib)
        self.vault = Vault(self.keystore)
        self.db = Database(self.vault, settings.db_path)
        self.vault.on_lock(live.clear)
        self.vault.on_lock(self._on_lock)
        self.prompts: dict[int, Prompt] = {}
        self.running: set[int] = set()
        self._pending_prices: dict[str, str] = {}
        self._prices: PriceService | None = None
        self._executor = ThreadPoolExecutor(max_workers=2, thread_name_prefix="sync")
        self._last_auto_sync = time.monotonic()
        self._stop = threading.Event()
        self._failed_logins = 0
        self._next_login_at = 0.0
        self.setup_token = secrets.token_urlsafe(12)

    # ------------------------------------------------------------ security
    def _on_lock(self) -> None:
        self.prompts.clear()
        self._prices = None

    def check_password(self, password: str) -> None:
        """Verify the master password, with an exponential delay after failures (brute force)."""
        now = time.monotonic()
        if now < self._next_login_at:
            raise PermissionError(f"Trop d'essais : réessaie dans {int(self._next_login_at - now) + 1} s")
        try:
            if self.vault.unlocked:
                if self.keystore.unlock(password) != self.vault._master():
                    raise InvalidPassword()
            else:
                self.vault.unlock(password)
        except InvalidPassword:
            self._failed_logins += 1
            if self._failed_logins >= 3:
                self._next_login_at = time.monotonic() + min(300, 2 ** (self._failed_logins - 2))
            raise
        self._failed_logins = 0

    def auto_unlock(self) -> None:
        path = self.settings.unlock_password_file
        if path and self.vault.initialized and not self.vault.unlocked:
            with open(path) as fh:
                self.vault.unlock(fh.read().strip())
            log.warning("Coffre déverrouillé automatiquement depuis %s (voir SECURITY.md)", path)

    def start_background(self, web_sessions_active) -> None:
        def loop():
            while not self._stop.wait(30):
                try:
                    idle_limit = self.settings.auto_lock_minutes * 60
                    if (
                        self.vault.unlocked
                        and not self.settings.unlock_password_file
                        and not web_sessions_active()
                        and self.vault.idle_seconds() > idle_limit
                        and not self.running
                    ):
                        log.info("Verrouillage automatique après inactivité")
                        self.vault.lock()
                    hours = self.settings.auto_sync_hours
                    if self.vault.unlocked and hours and time.monotonic() - self._last_auto_sync > hours * 3600:
                        self._last_auto_sync = time.monotonic()
                        self.sync_all(background=True)
                except Exception:
                    log.exception("Tâche de fond")

        threading.Thread(target=loop, daemon=True, name="background").start()

    def stop(self) -> None:
        self._stop.set()
        self._executor.shutdown(wait=False, cancel_futures=True)

    # -------------------------------------------------------------- prices
    @property
    def prices(self) -> PriceService:
        if self._prices is None:
            def cache_get(key: str):
                try:
                    with self.db.session() as s:
                        row = s.get(PriceCache, key)
                        return (row.value, row.fetched_at.timestamp()) if row else None
                except VaultLocked:
                    return None

            def cache_set(key: str, value: str):
                self._pending_prices[key] = value

            self._prices = PriceService(coingecko_api_key=self.get_setting("coingecko_api_key") or None, cache_get=cache_get, cache_set=cache_set)
        return self._prices

    def _flush_prices(self) -> None:
        pending, self._pending_prices = self._pending_prices, {}
        if not pending:
            return
        with self.db.session() as s:
            for key, value in pending.items():
                row = s.get(PriceCache, key)
                if row is None:
                    s.add(PriceCache(key=key, value=value, fetched_at=datetime.now()))
                else:
                    row.value, row.fetched_at = value, datetime.now()

    # ------------------------------------------------------------ settings
    def get_setting(self, key: str, default: str = "") -> str:
        with self.db.session() as s:
            row = s.get(Setting, key)
            return row.value if row else default

    def set_setting(self, key: str, value: str) -> None:
        with self.db.session() as s:
            row = s.get(Setting, key)
            if row is None:
                s.add(Setting(key=key, value=value))
            else:
                row.value = value
        if key == "coingecko_api_key":
            self._prices = None

    # --------------------------------------------------------- connections
    def _aad(self, conn_id: int, what: str) -> str:
        return f"connection:{conn_id}:{what}"

    def connection_config(self, conn: Connection) -> dict:
        return self.vault.decrypt_json(conn.config_enc, self._aad(conn.id, "config"))

    def create_connection(self, connector_type: str, name: str, config: dict[str, str]) -> int:
        cls = get_connector(connector_type)
        missing = [f.label for f in cls.fields if f.required and not (config.get(f.name) or "").strip()]
        if missing:
            raise ValueError("Champs obligatoires manquants : " + ", ".join(missing))
        with self.db.session() as s:
            conn = Connection(name=name or cls.label, connector_type=connector_type)
            s.add(conn)
            s.flush()
            conn.config_enc = self.vault.encrypt_json(config, self._aad(conn.id, "config"))
            return conn.id

    def update_connection(self, conn_id: int, name: str, config: dict[str, str]) -> None:
        with self.db.session() as s:
            conn = s.get(Connection, conn_id)
            current = self.connection_config(conn)
            cls = get_connector(conn.connector_type)
            for f in cls.fields:
                value = config.get(f.name)
                # An empty secret field means "unchanged": secrets are never sent back to the browser.
                if f.secret and not value:
                    continue
                current[f.name] = value or ""
            conn.name = name or conn.name
            conn.config_enc = self.vault.encrypt_json(current, self._aad(conn.id, "config"))

    def delete_connection(self, conn_id: int, delete_accounts: bool) -> None:
        with self.db.session() as s:
            conn = s.get(Connection, conn_id)
            if conn is None:
                return
            for account in list(conn.accounts):
                if delete_accounts:
                    s.delete(account)
                else:
                    account.connection_id = None
            s.delete(conn)
        live.drop(conn_id)
        self.prompts.pop(conn_id, None)

    def _connector(self, conn: Connection):
        cls = get_connector(conn.connector_type)
        ctx = Context(
            connection_id=conn.id,
            redirect_url=f"{self.settings.public_url}/connections/callback",
            last_sync=conn.last_sync_at,
        )
        return cls(self.connection_config(conn), self.vault.decrypt_json(conn.state_enc, self._aad(conn.id, "state")), ctx)

    def _save_state(self, conn_id: int, connector, **fields: Any) -> None:
        with self.db.session() as s:
            conn = s.get(Connection, conn_id)
            conn.state_enc = self.vault.encrypt_json(connector.state, self._aad(conn_id, "state"))
            for key, value in fields.items():
                setattr(conn, key, value)

    def start_auth(self, conn_id: int) -> str:
        with self.db.session() as s:
            conn = s.get(Connection, conn_id)
            connector = self._connector(conn)
        url = connector.start_auth()
        self._save_state(conn_id, connector)
        return url

    def find_pending_auth(self, params: dict[str, str]) -> int | None:
        """Which connection a bank redirect belongs to (matched on the random state we sent)."""
        state = params.get("state") or params.get("ref")
        with self.db.session() as s:
            for conn in s.scalars(select(Connection).where(Connection.connector_type.in_(["enablebanking", "gocardless"]))):
                st = self.vault.decrypt_json(conn.state_enc, self._aad(conn.id, "state"))
                if state and state in (st.get("pending_state"), st.get("pending_ref")):
                    return conn.id
        return None

    def finish_auth(self, conn_id: int, params: dict[str, str]) -> None:
        with self.db.session() as s:
            connector = self._connector(s.get(Connection, conn_id))
        connector.finish_auth(params)
        self._save_state(conn_id, connector, status="new", last_error=None)

    def login(self, conn_id: int, answers: dict[str, str] | None) -> Prompt | None:
        with self.db.session() as s:
            connector = self._connector(s.get(Connection, conn_id))
        prompt = connector.login(answers)
        self._save_state(conn_id, connector)
        if prompt is None:
            self.prompts.pop(conn_id, None)
        else:
            self.prompts[conn_id] = prompt
        return prompt

    def sync(self, conn_id: int) -> dict:
        if conn_id in self.running:
            return {}
        self.running.add(conn_id)
        try:
            with self.db.session() as s:
                conn = s.get(Connection, conn_id)
                connector = self._connector(conn)
            try:
                accounts = connector.sync()
            except NeedsInteraction as exc:
                self.prompts[conn_id] = exc.prompt
                self._save_state(conn_id, connector, status="needs_input", last_error=exc.prompt.message)
                return {}
            except NeedsAuth as exc:
                self._save_state(conn_id, connector, status="needs_auth", last_error=str(exc))
                return {}
            except Exception as exc:
                log.warning("Synchro %s échouée : %s", conn_id, exc)
                self._save_state(conn_id, connector, status="error", last_error=str(exc)[:1000])
                return {}
            with self.db.session() as s:
                conn = s.get(Connection, conn_id)
                stats = sync_service.apply_accounts(s, conn, accounts)
            self.prompts.pop(conn_id, None)
            self._save_state(conn_id, connector, status="ok", last_error=None, last_sync_at=utcnow())
            return stats
        finally:
            self.running.discard(conn_id)

    def sync_async(self, conn_id: int) -> None:
        self._executor.submit(self.sync, conn_id)

    def sync_all(self, background: bool = False) -> None:
        with self.db.session() as s:
            ids = [c.id for c in s.scalars(select(Connection)) if not (background and c.status in ("needs_auth", "needs_input"))]
        for conn_id in ids:
            self.sync_async(conn_id)

    # ----------------------------------------------------------- portfolio
    def portfolio(self, with_history: bool = True) -> Portfolio:
        with self.db.session() as s:
            accounts = list(s.scalars(select(Account).where(Account.archived.is_(False)).order_by(Account.institution, Account.name)))
            for a in accounts:
                _ = a.holdings, a.connection
            valuator = Valuator(self.prices)
            valuator.prefetch(accounts)
            values = [valuator.account_value(a) for a in accounts]
            totals: dict[str, Decimal] = {}
            for v in values:
                totals[v.account.category] = totals.get(v.account.category, Decimal(0)) + v.total
                sync_service.record_snapshot(s, v.account.id, v.total)
            history = []
            if with_history:
                rows = s.execute(select(Snapshot.day, Snapshot.account_id, Snapshot.value).order_by(Snapshot.day)).all()
                # Accounts without a snapshot on a given day keep their last known value.
                by_day: dict[date, Decimal] = {}
                latest: dict[int, Decimal] = {}
                active = {a.id for a in accounts}
                for day, account_id, value in rows:
                    if account_id in active:
                        latest[account_id] = Decimal(value)
                        by_day[day] = sum(latest.values(), Decimal(0))
                history = sorted(by_day.items())
        self._flush_prices()
        by_category = [(code, label, totals[code]) for code, label in CATEGORIES.items() if code in totals]
        assets = sum((v.total for v in values if v.total > 0), Decimal(0))
        debts = sum((v.total for v in values if v.total < 0), Decimal(0))
        return Portfolio(values, assets + debts, by_category, assets, debts, history)

    # ------------------------------------------------------------ accounts
    def save_account(self, account_id: int | None, data: dict[str, str]) -> int:
        with self.db.session() as s:
            account = s.get(Account, account_id) if account_id else Account()
            if account_id is None:
                s.add(account)
            account.name = data.get("name") or account.name or "Compte"
            if data.get("account_type") in ACCOUNT_TYPES:
                account.account_type = data["account_type"]
            account.institution = data.get("institution") or None
            account.currency = (data.get("currency") or account.currency or "EUR").upper()
            if account.connection_id is None and data.get("balance") not in (None, ""):
                account.balance = Decimal(data["balance"].replace(",", ".").replace(" ", ""))
                account.balance_date = utcnow()
            rate = (data.get("interest_rate") or "").replace(",", ".").strip()
            account.interest_rate = Decimal(rate) if rate else None
            account.iban = (data.get("iban") or "").replace(" ", "").upper() or account.iban
            account.notes = data.get("notes") or None
            account.archived = data.get("archived") == "on"
            s.flush()
            return account.id

    def delete_account(self, account_id: int) -> None:
        with self.db.session() as s:
            account = s.get(Account, account_id)
            if account:
                s.delete(account)

    def add_holding(self, account_id: int, data: dict[str, str]) -> None:
        quantity = Decimal(data["quantity"].replace(",", ".").replace(" ", ""))
        symbol = (data.get("symbol") or "").strip().upper() or None
        isin = (data.get("isin") or "").strip().upper() or None
        value = (data.get("value") or "").replace(",", ".").replace(" ", "")
        asset_class = data.get("asset_class") or ("crypto" if symbol and not isin else "stock")
        with self.db.session() as s:
            s.add(
                Holding(
                    account_id=account_id, symbol=symbol, isin=isin, name=data.get("name") or symbol or isin,
                    asset_class=asset_class, quantity=quantity, value=Decimal(value) if value else None,
                    value_currency="EUR" if value else None, manual=True,
                    extra={"yahoo": data["yahoo"].strip()} if data.get("yahoo") else None,
                )
            )

    def delete_holding(self, holding_id: int) -> None:
        with self.db.session() as s:
            h = s.get(Holding, holding_id)
            if h:
                s.delete(h)

    # ----------------------------------------------------------- documents
    def store_document(self, filename: str, data: bytes) -> int:
        digest = hashlib.sha256(data).hexdigest()
        with self.db.session() as s:
            existing = s.scalar(select(Document).where(Document.sha256 == digest))
            if existing:
                return existing.id
        blob_name = secrets.token_hex(16) + ".bin"
        path = self.settings.documents_dir / blob_name
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as fh:
            fh.write(self.vault.encrypt_file(data, f"document:{blob_name}"))
        try:
            statements = parse_document(filename, data)
            status, result = "parsed", {"statements": [st.to_json() for st in statements]}
        except Exception as exc:
            status, result = "failed", {"error": str(exc), "statements": []}
        with self.db.session() as s:
            doc = Document(
                filename=os.path.basename(filename)[:300] or "document",
                mime=None,
                size=len(data),
                sha256=digest,
                blob_name=blob_name,
                status=status,
                parse_result=result,
            )
            s.add(doc)
            s.flush()
            return doc.id

    def read_document(self, doc_id: int) -> tuple[str, bytes]:
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            blob = (self.settings.documents_dir / doc.blob_name).read_bytes()
            return doc.filename, self.vault.decrypt_file(blob, f"document:{doc.blob_name}")

    def delete_document(self, doc_id: int) -> None:
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            if doc is None:
                return
            path = self.settings.documents_dir / doc.blob_name
            s.execute(Transaction.__table__.update().where(Transaction.document_id == doc_id).values(document_id=None))
            s.delete(doc)
        if path.exists():
            # Overwrite before unlinking (best effort: SSDs and copy-on-write filesystems may keep old blocks).
            size = path.stat().st_size
            with open(path, "r+b") as fh:
                fh.write(os.urandom(size))
            path.unlink()

    def suggest_account(self, st: ParsedStatement) -> int | None:
        with self.db.session() as s:
            if st.iban:
                found = s.scalar(select(Account).where(Account.iban == st.iban))
                if found:
                    return found.id
            if st.account_number:
                for account in s.scalars(select(Account)):
                    if account.iban and st.account_number in account.iban:
                        return account.id
                    if account.external_id and st.account_number in account.external_id:
                        return account.id
        return None

    def import_statement(self, doc_id: int, index: int, form: dict[str, str]) -> dict:
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            st = ParsedStatement.from_json(doc.parse_result["statements"][index])
            target = form.get("account_id") or "new"
            if target == "new":
                account = Account(
                    name=form.get("name") or st.account_label or doc.filename,
                    account_type=form.get("account_type") if form.get("account_type") in ACCOUNT_TYPES else (st.account_type or "checking"),
                    institution=form.get("institution") or st.institution,
                    currency=st.currency,
                    iban=st.iban,
                )
                s.add(account)
                s.flush()
            else:
                account = s.get(Account, int(target))
            if not account.iban and st.iban:
                account.iban = st.iban
            balance_raw = (form.get("balance") or "").replace(",", ".").replace(" ", "")
            if form.get("update_balance") == "on" and balance_raw:
                account.balance = Decimal(balance_raw)
                account.balance_date = datetime.combine(st.balance_date or date.today(), datetime.min.time())
            added = 0
            if form.get("import_transactions") == "on":
                added = sync_service.add_transactions(s, account, st.transactions, "import", document_id=doc.id)
            if st.holdings and form.get("import_holdings") == "on":
                for h in [h for h in account.holdings if h.manual]:
                    s.delete(h)
                for h in st.holdings:
                    s.add(Holding(account_id=account.id, symbol=h.symbol, isin=h.isin, name=h.name, asset_class=h.asset_class,
                                  quantity=h.quantity, value=h.value, value_currency=h.value_currency, manual=True))
            doc.status = "imported"
            doc.account_id = account.id
            return {"account_id": account.id, "transactions": added}
