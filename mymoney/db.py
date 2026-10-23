"""Encrypted database (SQLCipher, AES-256) opened only while the vault is unlocked."""

from __future__ import annotations

from contextlib import contextmanager
from typing import Iterator

import sqlcipher3
from sqlalchemy import create_engine, event, inspect, text
from sqlalchemy.engine import Engine
from sqlalchemy.orm import Session, sessionmaker

from .security.vault import Vault, VaultLocked


class Database:
    def __init__(self, vault: Vault, path):
        self.vault = vault
        self.path = path
        self.engine: Engine | None = None
        self._sessionmaker: sessionmaker | None = None
        vault.on_unlock(self.open)
        vault.on_lock(self.close)

    def open(self) -> None:
        # Raw 256-bit key: SQLCipher skips its own KDF since the key already comes from Argon2id.
        hex_key = self.vault.subkey("database").hex()
        path = str(self.path)

        def connect():
            conn = sqlcipher3.connect(path, check_same_thread=False)
            conn.execute(f"PRAGMA key = \"x'{hex_key}'\"")
            conn.execute("PRAGMA cipher_memory_security = ON")
            # Fails here (instead of later) when the key is wrong or the file is corrupted.
            conn.execute("SELECT count(*) FROM sqlite_master").fetchone()
            return conn

        engine = create_engine("sqlite://", creator=connect, module=sqlcipher3)

        @event.listens_for(engine, "connect")
        def _pragmas(dbapi_conn, _record):
            cur = dbapi_conn.cursor()
            cur.execute("PRAGMA foreign_keys = ON")
            cur.execute("PRAGMA secure_delete = ON")
            cur.execute("PRAGMA journal_mode = WAL")
            cur.close()

        from .models import Base

        Base.metadata.create_all(engine)
        _add_missing_columns(engine, Base)
        self.engine = engine
        self._sessionmaker = sessionmaker(engine, expire_on_commit=False)

    def close(self) -> None:
        if self.engine is not None:
            self.engine.dispose()
        self.engine = None
        self._sessionmaker = None

    @contextmanager
    def session(self) -> Iterator[Session]:
        if self._sessionmaker is None:
            raise VaultLocked()
        session = self._sessionmaker()
        try:
            yield session
            session.commit()
        except Exception:
            session.rollback()
            raise
        finally:
            session.close()


def _add_missing_columns(engine: Engine, base) -> None:
    """Light schema upgrade: a database created by an older version gets the columns added since (all nullable)."""
    inspector = inspect(engine)
    with engine.begin() as conn:
        for table in base.metadata.sorted_tables:
            if not inspector.has_table(table.name):
                continue
            have = {c["name"] for c in inspector.get_columns(table.name)}
            for column in table.columns:
                if column.name not in have and column.nullable:
                    ddl = column.type.compile(dialect=engine.dialect)
                    conn.execute(text(f'ALTER TABLE "{table.name}" ADD COLUMN "{column.name}" {ddl}'))
