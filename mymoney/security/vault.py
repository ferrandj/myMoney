"""In-memory vault: holds the data key only while the app is unlocked.

When locked (at start-up, on demand, or after inactivity) the key is wiped and the
encrypted database is closed, so nothing on the NAS disk is readable.
"""

from __future__ import annotations

import json
import threading
import time
from typing import Any, Callable

from . import crypto
from .keystore import Keystore

MIN_PASSWORD_LENGTH = 12


class VaultLocked(Exception):
    pass


class Vault:
    def __init__(self, keystore: Keystore):
        self.keystore = keystore
        self._dek: bytearray | None = None
        self._lock = threading.RLock()
        self._last_activity = 0.0
        self._on_unlock: list[Callable[[], None]] = []
        self._on_lock: list[Callable[[], None]] = []

    # -- lifecycle -------------------------------------------------------
    def on_unlock(self, callback: Callable[[], None]) -> None:
        self._on_unlock.append(callback)

    def on_lock(self, callback: Callable[[], None]) -> None:
        self._on_lock.append(callback)

    @property
    def initialized(self) -> bool:
        return self.keystore.exists()

    @property
    def unlocked(self) -> bool:
        return self._dek is not None

    def setup(self, password: str, slot: str | None = None) -> str:
        check_password_strength(password)
        dek, recovery = self.keystore.create(password, slot)
        self._open(dek)
        return recovery

    def unlock(self, password: str, slot: str | None = None) -> None:
        self._open(self.keystore.unlock(password, slot))

    def unlock_with_recovery(self, code: str) -> None:
        self._open(self.keystore.unlock_with_recovery(code))

    def _open(self, dek: bytes) -> None:
        with self._lock:
            self._dek = bytearray(dek)
            self.touch()
            try:
                for callback in self._on_unlock:
                    callback()
            except Exception:
                self.lock()
                raise

    def lock(self) -> None:
        with self._lock:
            for callback in self._on_lock:
                try:
                    callback()
                except Exception:
                    pass
            if self._dek is not None:
                for i in range(len(self._dek)):
                    self._dek[i] = 0
            self._dek = None

    def touch(self) -> None:
        self._last_activity = time.monotonic()

    def idle_seconds(self) -> float:
        return time.monotonic() - self._last_activity

    # -- keys ------------------------------------------------------------
    def _master(self) -> bytes:
        dek = self._dek
        if dek is None:
            raise VaultLocked()
        return bytes(dek)

    def subkey(self, purpose: str) -> bytes:
        return crypto.hkdf(self._master(), f"mymoney/{purpose}")

    def change_password(self, current: str, new: str, slot: str | None = None) -> None:
        check_password_strength(new)
        dek = self.keystore.unlock(current, slot)
        if dek != self._master():
            raise VaultLocked()
        self.keystore.set_password(dek, new, slot)

    def add_user_slot(self, slot: str, password: str) -> None:
        """Give a new user their own password to the same vault (the vault must be open)."""
        check_password_strength(password)
        self.keystore.add_slot(self._master(), slot, password)

    def remove_user_slot(self, slot: str) -> None:
        self.keystore.remove_slot(slot)

    def regenerate_recovery(self) -> str:
        return self.keystore.regenerate_recovery(self._master())

    # -- helpers for application-level encryption ------------------------
    def encrypt_json(self, value: Any, aad: str) -> bytes:
        return crypto.encrypt(self.subkey("secrets"), json.dumps(value).encode(), aad.encode())

    def decrypt_json(self, blob: bytes | None, aad: str) -> Any:
        if not blob:
            return {}
        return json.loads(crypto.decrypt(self.subkey("secrets"), blob, aad.encode()))

    def encrypt_file(self, data: bytes, aad: str) -> bytes:
        return crypto.encrypt(self.subkey("files"), data, aad.encode())

    def decrypt_file(self, blob: bytes, aad: str) -> bytes:
        return crypto.decrypt(self.subkey("files"), blob, aad.encode())


def check_password_strength(password: str) -> None:
    if len(password) < MIN_PASSWORD_LENGTH:
        raise ValueError(f"Le mot de passe maître doit faire au moins {MIN_PASSWORD_LENGTH} caractères")
    classes = sum(
        [
            any(c.islower() for c in password),
            any(c.isupper() for c in password),
            any(c.isdigit() for c in password),
            any(not c.isalnum() for c in password),
        ]
    )
    if classes < 3 and len(password) < 20:
        raise ValueError(
            "Mot de passe trop simple : mélange majuscules, minuscules, chiffres et symboles, "
            "ou utilise une phrase de passe de 20 caractères ou plus"
        )
