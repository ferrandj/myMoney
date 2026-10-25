"""Master-key storage.

A random 256-bit data key (DEK) encrypts everything. It is never written to disk in
clear: the keystore only holds it *wrapped* (AES-256-GCM) by keys derived with
Argon2id from
  * one password per user (a "key slot", like the slots of an encrypted disk), and
  * a one-time recovery code shown at setup.
Changing a password re-wraps the DEK for that slot only; the database does not need re-encrypting.

Version 1 keystores (a single "password" entry, before there were users) are still read: that entry is the
legacy slot, which the first user adopts (`adopt_legacy_slot`) without needing the password again.
"""

from __future__ import annotations

import base64
import json
import os
import secrets
from dataclasses import dataclass
from pathlib import Path

from . import crypto

VERSION = 2
_RECOVERY_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"  # no 0/O/1/I ambiguity


class InvalidPassword(Exception):
    pass


def _b64(data: bytes) -> str:
    return base64.b64encode(data).decode()


def _unb64(data: str) -> bytes:
    return base64.b64decode(data)


def generate_recovery_code() -> str:
    raw = "".join(secrets.choice(_RECOVERY_ALPHABET) for _ in range(32))
    return "-".join(raw[i : i + 4] for i in range(0, 32, 4))


def normalize_recovery_code(code: str) -> str:
    return "".join(c for c in code.upper() if c.isalnum())


@dataclass
class Keystore:
    path: Path
    time_cost: int
    memory_kib: int

    def exists(self) -> bool:
        return self.path.exists()

    def _wrap(self, secret: bytes, dek: bytes, purpose: str) -> dict:
        salt = os.urandom(16)
        kek = crypto.argon2id(secret, salt, self.time_cost, self.memory_kib)
        return {
            "salt": _b64(salt),
            "time_cost": self.time_cost,
            "memory_kib": self.memory_kib,
            "wrapped": _b64(crypto.encrypt(kek, dek, aad=f"mymoney-keystore-v{VERSION}|{purpose}".encode())),
        }

    @staticmethod
    def _unwrap(secret: bytes, entry: dict, purpose: str) -> bytes:
        kek = crypto.argon2id(secret, _unb64(entry["salt"]), entry["time_cost"], entry["memory_kib"])
        try:
            return crypto.decrypt(kek, _unb64(entry["wrapped"]), aad=f"mymoney-keystore-v{VERSION}|{purpose}".encode())
        except crypto.DecryptionError as exc:
            raise InvalidPassword() from exc

    def _write(self, data: dict) -> None:
        tmp = self.path.with_suffix(".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as fh:
            json.dump(data, fh, indent=2)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, self.path)

    def _read(self) -> dict:
        return json.loads(self.path.read_text())

    def create(self, password: str, slot: str | None = None) -> tuple[bytes, str]:
        """Create a new keystore, with one key slot (the first user's, or the legacy one). Returns (DEK, recovery code)."""
        if self.exists():
            raise FileExistsError("Le coffre est déjà initialisé")
        dek = crypto.random_key()
        recovery = generate_recovery_code()
        data = {"version": VERSION, "slots": {}, "recovery": self._wrap(normalize_recovery_code(recovery).encode(), dek, "recovery")}
        if slot:
            data["slots"][slot] = self._wrap(password.encode(), dek, "password")
        else:
            data["password"] = self._wrap(password.encode(), dek, "password")
        self._write(data)
        return dek, recovery

    # -- key slots (one per user) --------------------------------------------------------------------------
    def has_legacy_slot(self) -> bool:
        return self.exists() and "password" in self._read()

    def has_slot(self, slot: str) -> bool:
        return slot in self._read().get("slots", {})

    def slots(self) -> list[str]:
        return list(self._read().get("slots", {}))

    def unlock(self, password: str, slot: str | None = None) -> bytes:
        """The DEK, from a user's password (`slot`) or, before users exist, from the legacy single password."""
        data = self._read()
        entry = data.get("slots", {}).get(slot) if slot else data.get("password")
        if entry is None:
            raise InvalidPassword()  # unknown user: same answer as a wrong password
        return self._unwrap(password.encode(), entry, "password")

    def unlock_with_recovery(self, code: str) -> bytes:
        return self._unwrap(normalize_recovery_code(code).encode(), self._read()["recovery"], "recovery")

    def add_slot(self, dek: bytes, slot: str, password: str) -> None:
        data = self._read()
        data.setdefault("slots", {})[slot] = self._wrap(password.encode(), dek, "password")
        data["version"] = VERSION
        self._write(data)

    def set_password(self, dek: bytes, new_password: str, slot: str | None = None) -> None:
        data = self._read()
        if slot:
            data.setdefault("slots", {})[slot] = self._wrap(new_password.encode(), dek, "password")
        else:
            data["password"] = self._wrap(new_password.encode(), dek, "password")
        self._write(data)

    def remove_slot(self, slot: str) -> None:
        data = self._read()
        data.get("slots", {}).pop(slot, None)
        self._write(data)

    def adopt_legacy_slot(self, slot: str) -> None:
        """The single password of a version 1 keystore becomes the slot of the first user (same password)."""
        data = self._read()
        if "password" in data:
            data.setdefault("slots", {})[slot] = data.pop("password")
            data["version"] = VERSION
            self._write(data)

    def regenerate_recovery(self, dek: bytes) -> str:
        data = self._read()
        recovery = generate_recovery_code()
        data["recovery"] = self._wrap(normalize_recovery_code(recovery).encode(), dek, "recovery")
        self._write(data)
        return recovery
