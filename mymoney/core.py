"""Application services shared by the web UI and the CLI."""

from __future__ import annotations

import hashlib
import logging
import os
import secrets
import threading
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta
from decimal import Decimal

from sqlalchemy import select

from .adapters import ParsedStatement, SourceDocument, get_adapter, identify, parse_with
from .config import Settings
from .db import Database
from .models import ACCOUNT_TYPES, CATEGORIES, Account, Document, Holding, PriceCache, Setting, Snapshot, Transaction, User, utcnow
from .security.keystore import InvalidPassword, Keystore
from .security.vault import Vault, VaultLocked
from .services import access, ledger
from .services import users as user_service
from .services import loans as loan_service
from .services import statements as statement_service
from .services.prices import PriceService
from .services.valuation import AccountValue, Valuator

log = logging.getLogger(__name__)
QUARANTINE_HOURS = 24  # how long a duplicate set aside waits for the user's decision


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
        self.keystore = Keystore(settings.keystore_path, settings.argon2_time_cost, settings.argon2_memory_kib)
        self.vault = Vault(self.keystore)
        self.db = Database(self.vault, settings.db_path)
        self.vault.on_lock(self._on_lock)
        self._pending_prices: dict[str, str] = {}
        self._prices: PriceService | None = None
        self._stop = threading.Event()
        self._failed_logins = 0
        self._next_login_at = 0.0
        self.setup_token = secrets.token_urlsafe(12)

    # ------------------------------------------------------------ security
    def _on_lock(self) -> None:
        self._prices = None

    def _throttle(self) -> None:
        now = time.monotonic()
        if now < self._next_login_at:
            raise PermissionError(f"Trop d'essais : réessaie dans {int(self._next_login_at - now) + 1} s")

    def _failed(self) -> None:
        self._failed_logins += 1
        if self._failed_logins >= 3:
            self._next_login_at = time.monotonic() + min(300, 2 ** (self._failed_logins - 2))

    def check_password(self, password: str, email: str | None = None) -> None:
        """Verify a password (a user's, or the legacy master password before users exist), with an exponential delay after
        failures (brute force). Opens the vault when it is locked: everyone's data is behind the same key."""
        self._throttle()
        try:
            slot = user_service.slot_id(email) if email else None
        except ValueError:
            self._failed()
            raise InvalidPassword()
        try:
            if self.vault.unlocked:
                if self.keystore.unlock(password, slot) != self.vault._master():
                    raise InvalidPassword()
            else:
                self.vault.unlock(password, slot)
        except InvalidPassword:
            self._failed()
            raise
        self._failed_logins = 0

    @property
    def legacy_login(self) -> bool:
        """The vault predates users: it still has a single password and nobody has an account yet."""
        return self.vault.initialized and self.keystore.has_legacy_slot()

    def setup(self, email: str, name: str | None, password: str) -> str:
        """First installation: the vault, and its first user (the administrator). Returns the recovery code."""
        email = user_service.normalize_email(email)
        recovery = self.vault.setup(password, user_service.slot_id(email))
        with self.db.session() as s:
            user_service.create(s, email, name, is_admin=True)
        return recovery

    def authenticate(self, email: str, password: str) -> access.Identity:
        """Sign a user in with their own password. Raises InvalidPassword for a wrong password or an unknown user."""
        self.check_password(password, email)
        with self.db.session() as s:
            user = user_service.get_by_email(s, email)
            if user is None:
                raise InvalidPassword()
            user.last_login = utcnow()
            return user_service.identity(user)

    def authenticate_recovery(self, code: str) -> access.Identity | None:
        """The recovery code opens the vault and signs in the first administrator (None while nobody has an account)."""
        self._throttle()
        try:
            self.vault.unlock_with_recovery(code)
        except InvalidPassword:
            self._failed()
            raise
        self._failed_logins = 0
        with self.db.session() as s:
            admin = user_service.first_admin(s)
            return user_service.identity(admin) if admin else None

    def identity_for_email(self, email: str | None) -> access.Identity | None:
        """The user an e-mail address belongs to (the vault must be open)."""
        with self.db.session() as s:
            user = user_service.get_by_email(s, email)
            if user is None:
                return None
            user.last_login = utcnow()
            return user_service.identity(user)

    def identity_by_id(self, user_id: int | None) -> access.Identity | None:
        if user_id is None:
            return None
        with self.db.session() as s:
            user = s.get(User, user_id)
            return user_service.identity(user) if user else None

    def has_users(self) -> bool:
        with self.db.session() as s:
            return user_service.count(s) > 0

    def create_first_user(self, email: str, name: str | None) -> access.Identity:
        """A vault from before users existed: its password becomes the administrator's, and the data becomes theirs."""
        email = user_service.normalize_email(email)
        with self.db.session() as s:
            if user_service.count(s):
                raise ValueError("Il existe déjà des utilisateurs")
            user = user_service.create(s, email, name, is_admin=True)
            user_service.claim_legacy_data(s, user)
            self.keystore.adopt_legacy_slot(user_service.slot_id(email))
            user.last_login = utcnow()
            return user_service.identity(user)

    def auto_unlock(self) -> None:
        path = self.settings.unlock_password_file
        if path and self.vault.initialized and not self.vault.unlocked:
            with open(path) as fh:
                password = fh.read().strip()
            email = self.settings.unlock_email
            if email:
                self.vault.unlock(password, user_service.slot_id(email))
            elif self.keystore.has_legacy_slot():
                self.vault.unlock(password)
            else:  # no e-mail given: the password belongs to one of the slots
                for slot in self.keystore.slots():
                    try:
                        self.vault.unlock(password, slot)
                        break
                    except InvalidPassword:
                        continue
                else:
                    raise InvalidPassword()
            log.warning("Coffre déverrouillé automatiquement depuis %s (voir SECURITY.md)", path)

    def start_background(self, web_sessions_active) -> None:
        def loop():
            while not self._stop.wait(30):
                try:
                    if (
                        self.vault.unlocked
                        and self.settings.auto_lock_minutes > 0
                        and not self.settings.unlock_password_file
                        and not web_sessions_active()
                        and self.vault.idle_seconds() > self.settings.auto_lock_minutes * 60
                    ):
                        log.info("Verrouillage automatique après inactivité")
                        self.vault.lock()
                except Exception:
                    log.exception("Tâche de fond")

        threading.Thread(target=loop, daemon=True, name="background").start()

    def stop(self) -> None:
        self._stop.set()

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

    # ----------------------------------------------------------- portfolio
    def portfolio(self, with_history: bool = True) -> Portfolio:
        with self.db.session() as s:
            loan_service.refresh_all(s)  # the capital owed moves with every instalment
            accounts = access.only_readable(s, list(s.scalars(select(Account).where(Account.archived.is_(False)).order_by(Account.institution, Account.name))))
            for a in accounts:
                _ = (a.holdings, a.owner)
            valuator = Valuator(self.prices)
            valuator.prefetch(accounts)
            values = [valuator.account_value(a) for a in accounts]
            totals: dict[str, Decimal] = {}
            for v in values:
                totals[v.account.category] = totals.get(v.account.category, Decimal(0)) + v.total
                ledger.record_snapshot(s, v.account.id, v.total)
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
            if account_id:
                access.check_edit(s, account_id)
            account = s.get(Account, account_id) if account_id else Account()
            if account_id is None:
                account.owner_id = access.owner_for_new(s)
                s.add(account)
            account.name = data.get("name") or account.name or "Compte"
            if data.get("account_type") in ACCOUNT_TYPES:
                account.account_type = data["account_type"]
            account.institution = data.get("institution") or None
            account.currency = (data.get("currency") or account.currency or "EUR").upper()
            if data.get("balance") not in (None, ""):
                account.balance = Decimal(data["balance"].replace(",", ".").replace(" ", ""))
                account.balance_date = utcnow()
            rate = (data.get("interest_rate") or "").replace(",", ".").strip()
            account.interest_rate = Decimal(rate) if rate else None
            account.iban = (data.get("iban") or "").replace(" ", "").upper() or None
            for attr in ("bic", "bank_code", "branch_code", "account_number", "rib_key"):
                if attr in data:
                    setattr(account, attr, "".join(data[attr].split()).upper() or None)
            for attr in ("holder_name", "holder_address"):
                if attr in data:
                    setattr(account, attr, data[attr].strip() or None)
            account.notes = data.get("notes") or None
            account.archived = data.get("archived") == "on"
            s.flush()
            return account.id

    def delete_account(self, account_id: int) -> None:
        with self.db.session() as s:
            access.check_owner(s, account_id)
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
            access.check_edit(s, account_id)
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
                access.check_edit(s, h.account_id)
                s.delete(h)

    # ----------------------------------------------------------- documents
    def store_document(self, filename: str, data: bytes) -> tuple[int, bool]:
        """Encrypt and keep the file. Returns (document id, True if the very same file was already stored)."""
        digest = hashlib.sha256(data).hexdigest()
        with self.db.session() as s:
            existing = s.scalar(select(Document).where(Document.sha256 == digest))
            if existing:
                return existing.id, True  # (the web layer checks that the user may open it)
        blob_name = secrets.token_hex(16) + ".bin"
        path = self.settings.documents_dir / blob_name
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as fh:
            fh.write(self.vault.encrypt_file(data, f"document:{blob_name}"))
        with self.db.session() as s:
            doc = Document(
                filename=os.path.basename(filename)[:300] or "document", size=len(data), sha256=digest, blob_name=blob_name, status="uploaded",
                owner_id=access.owner_for_new(s),
            )
            s.add(doc)
            s.flush()
            return doc.id, False

    def analyze_document(self, doc_id: int, adapter_id: str | None = None, data: bytes | None = None) -> str:
        """Find the adapter for a stored document (or use the one the user chose) and parse it. Returns its status."""
        with self.db.session() as s:
            self._doc(s, doc_id)
        filename, data = (self.read_document(doc_id) if data is None else (self._filename(doc_id), data))
        source = SourceDocument(filename, data)
        found = identify(source)
        result: dict = {
            "decision": "chosen" if adapter_id else found.decision,
            "message": "" if adapter_id else found.message,
            "institution": found.institution,
            "candidates": [c.to_json() for c in found.candidates],
            "statements": [],
        }
        chosen = adapter_id or (found.chosen.adapter.id if found.chosen else None)
        status, kind = "failed", "statement"
        if chosen is None and found.decision == "ask":
            status = "needs_choice"
        elif chosen is None:
            result["error"] = found.message
        else:
            try:
                statements = parse_with(get_adapter(chosen).id, source)
                result.update(adapter=chosen, statements=[st.to_json() for st in statements])
                status, kind = "parsed", get_adapter(chosen).kind
            except Exception as exc:
                log.warning("Lecture de %s avec %s impossible : %s", doc_id, chosen, exc)
                result.update(adapter=chosen, error=str(exc))
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            doc.status, doc.parse_result, doc.adapter_id, doc.kind = status, result, chosen, kind
        return status

    def _doc(self, s, doc_id: int, edit: bool = True) -> Document:
        """The document, if the current user may open it (or change it, with `edit`)."""
        return access.check_document(s, s.get(Document, doc_id), edit=edit)

    def document_visible(self, doc_id: int) -> bool:
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            return doc is not None and access.document_visible(s, doc)

    def _filename(self, doc_id: int) -> str:
        with self.db.session() as s:
            return s.get(Document, doc_id).filename

    def read_document(self, doc_id: int) -> tuple[str, bytes]:
        with self.db.session() as s:
            doc = self._doc(s, doc_id, edit=False)
            blob = (self.settings.documents_dir / doc.blob_name).read_bytes()
            return doc.filename, self.vault.decrypt_file(blob, f"document:{doc.blob_name}")

    def delete_document(self, doc_id: int) -> None:
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            if doc is None:
                return
            self._doc(s, doc_id)
            path = self.settings.documents_dir / doc.blob_name
            s.execute(Transaction.__table__.update().where(Transaction.document_id == doc_id).values(document_id=None))
            s.delete(doc)
        self._wipe(path)

    @staticmethod
    def _wipe(path) -> None:
        if path.exists():
            # Overwrite before unlinking (best effort: SSDs and copy-on-write filesystems may keep old blocks).
            size = path.stat().st_size
            with open(path, "r+b") as fh:
                fh.write(os.urandom(size))
            path.unlink()

    # ------------------------------------------------------------- duplicate files
    def check_duplicate(self, doc_id: int) -> str | None:
        """Does this file repeat one already imported (same account, same period)? Returns "identical" when it changes
        nothing (the file is set aside), "differs" when it has extra or missing lines or other balances, None otherwise."""
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            result = doc.parse_result or {}
            if doc.status != "parsed" or len(result.get("statements", [])) != 1:
                return None
            st = ParsedStatement.from_json(result["statements"][0])
            if st.document_kind in statement_service.DOCUMENT_ONLY_KINDS:
                return None  # a fees recap or an information letter: nothing to compare operation by operation
            found = statement_service.find_accounts(s, st)
            if len(found) != 1:
                return None
            dedup = statement_service.plan_import(s, st, found[0]).dedup
            if dedup is None:
                return None
            doc.duplicate_of = dedup.document.id if dedup.document else None
            if dedup.identical:
                doc.status = "duplicate"
                return "identical"
            return "differs"

    def keep_duplicate(self, doc_id: int) -> None:
        """Keep a duplicate that was set aside, as a copy next to the original (nothing is imported from it)."""
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            if doc.status in ("duplicate", "parsed"):
                doc.status = "copy"

    def purge_quarantine(self) -> int:
        """Delete the duplicates that were set aside and not kept within a day."""
        limit = utcnow() - timedelta(hours=QUARANTINE_HOURS)
        with access.unrestricted():  # housekeeping for everyone's files
            with self.db.session() as s:
                old = list(s.scalars(select(Document.id).where(Document.status == "duplicate", Document.uploaded_at < limit)))
            for doc_id in old:
                self.delete_document(doc_id)
        return len(old)

    def import_default(self, doc_id: int, index: int, force: bool = False) -> dict:
        """The one-click import (matching account, new lines, balance if newer), even if the period was imported before."""
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            st = ParsedStatement.from_json(doc.parse_result["statements"][index])
            found = statement_service.find_accounts(s, st)
            if len(found) != 1:
                raise ValueError("Compte introuvable ou ambigu : choisis-le dans le formulaire d'import")
            form = statement_service.default_import_form(statement_service.plan_import(s, st, found[0]))
            return statement_service.import_statement(s, doc, st, {**form, "force": "on" if force else ""})

    def replace_document(self, doc_id: int, index: int) -> dict:
        """Replace the file already imported for this period by this one: the old file and what it brought are removed,
        this one is imported in full."""
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            st = ParsedStatement.from_json(doc.parse_result["statements"][index])
            found = statement_service.find_accounts(s, st)
            if len(found) != 1:
                raise ValueError("Compte introuvable ou ambigu")
            dedup = statement_service.plan_import(s, st, found[0]).dedup
            if dedup is None:
                raise ValueError("Ce relevé ne remplace rien : aucune période identique n'a été importée")
            if dedup.document is None:
                raise ValueError("L'ancien fichier n'existe plus : impossible de le remplacer (garde les deux)")
            old = dedup.document
            old_name, old_path = old.filename, self.settings.documents_dir / old.blob_name
            removed = statement_service.undo_import(s, old)
            s.delete(old)
            s.flush()
            form = statement_service.default_import_form(statement_service.plan_import(s, st, found[0]))
            result = statement_service.import_statement(s, doc, st, form)
            doc.duplicate_of = None
            doc.parse_result = {**doc.parse_result, "replaced": {"filename": old_name, "removed": removed}}
            result["replaced"], result["removed"] = old_name, removed
        self._wipe(old_path)
        return result

    def review_document(self, doc_id: int) -> dict | None:
        """Everything the review page shows: what was understood, and what importing it would do."""
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            if doc is None or not access.document_visible(s, doc):
                return None
            statements = [ParsedStatement.from_json(item) for item in (doc.parse_result or {}).get("statements", [])]
            accounts = access.only_editable(s, list(s.scalars(select(Account).order_by(Account.institution, Account.name))))
            plans = []
            for st in statements:
                found = statement_service.find_accounts(s, st)
                plans.append(statement_service.plan_import(s, st, found[0] if len(found) == 1 else None))
            return {"doc": doc, "plans": plans, "accounts": accounts}

    def import_statement(self, doc_id: int, index: int, form: dict[str, str]) -> dict:
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            st = ParsedStatement.from_json(doc.parse_result["statements"][index])
            return statement_service.import_statement(s, doc, st, form)

    def auto_import(self, doc_id: int) -> dict | None:
        """Import without asking when there is nothing to ask: one clear adapter, one matching account, balances check out,
        nothing suspicious. Anything else is left for the user to review."""
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            result = doc.parse_result or {}
            if doc.status != "parsed" or result.get("decision") != "auto" or len(result.get("statements", [])) != 1:
                return None
            st = ParsedStatement.from_json(result["statements"][0])
            if st.document_kind == "info":  # nothing to ask: it is filed
                return statement_service.import_statement(s, doc, st, {})
            found = statement_service.find_accounts(s, st)
            if len(found) != 1:
                return None
            plan = statement_service.plan_import(s, st, found[0])
            if not plan.clean:
                return None
            return statement_service.import_statement(s, doc, st, statement_service.default_import_form(plan))

    def pending_documents(self) -> list[Document]:
        """Documents read but not yet validated (the ones the user still has to look at), oldest period first."""
        with self.db.session() as s:
            docs = sorted((d for d in access.visible_documents(s) if d.status == "parsed"), key=lambda d: (_period_key(d), d.id))
            for d in docs:
                s.expunge(d)
        return docs

    def validate_pending(self) -> dict[str, int]:
        """Validate in one go every document whose import needs no decision: the account is the one the document points to,
        or a new account because nothing matches yet. What needs a person (cut holder names, several possible accounts,
        a statement that repeats another, inconsistent balances, warnings) is left as it is. Returns counts."""
        done, left = 0, 0
        todo = [d.id for d in self.pending_documents()]
        for _ in range(3):  # a document can wait for the account another one creates (a fees recap, a loan notice)
            progress, remaining = False, []
            for doc_id in todo:
                try:
                    if self._validate_if_obvious(doc_id):
                        done += 1
                        progress = True
                    else:
                        remaining.append(doc_id)
                except Exception:
                    remaining.append(doc_id)
            todo = remaining
            if not progress or not todo:
                break
        return {"imported": done, "left": len(todo)}

    def _validate_if_obvious(self, doc_id: int) -> bool:
        with self.db.session() as s:
            doc = self._doc(s, doc_id)
            result = doc.parse_result or {}
            if doc.status != "parsed" or len(result.get("statements", [])) != 1:
                return False
            st = ParsedStatement.from_json(result["statements"][0])
            if st.document_kind == "info":
                statement_service.import_statement(s, doc, st, {})
                return True
            found = statement_service.find_accounts(s, st)
            if len(found) > 1:
                return False
            plan = statement_service.plan_import(s, st, found[0] if found else None)
            if plan.dedup is not None or plan.ask_holder:
                return False
            if plan.account is None:
                creating = st.document_kind not in statement_service.DOCUMENT_ONLY_KINDS
                if not (creating and st.confidence == "high" and not st.warnings and not plan.issues):
                    return False
            elif not plan.clean:
                return False
            statement_service.import_statement(s, doc, st, statement_service.default_import_form(plan))
            return True

    def undo_import(self, doc_id: int) -> int:
        with self.db.session() as s:
            doc = s.get(Document, doc_id)
            if doc is None:
                return 0
            self._doc(s, doc_id)
            return statement_service.undo_import(s, doc)


def _period_key(doc: Document):
    statements = (doc.parse_result or {}).get("statements") or [{}]
    return str(statements[0].get("period_end") or statements[0].get("period_start") or "")
