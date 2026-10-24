"""Low-level primitives: AES-256-GCM authenticated encryption, Argon2id and HKDF."""

from __future__ import annotations

import os

from argon2.low_level import Type, hash_secret_raw
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

MAGIC = b"MM1"
NONCE_SIZE = 12
KEY_SIZE = 32


class DecryptionError(Exception):
    """Wrong key or tampered ciphertext."""


def random_key() -> bytes:
    return os.urandom(KEY_SIZE)


def encrypt(key: bytes, plaintext: bytes, aad: bytes = b"") -> bytes:
    nonce = os.urandom(NONCE_SIZE)
    return MAGIC + nonce + AESGCM(key).encrypt(nonce, plaintext, aad)


def decrypt(key: bytes, blob: bytes, aad: bytes = b"") -> bytes:
    if not blob.startswith(MAGIC):
        raise DecryptionError("Format de données chiffrées inconnu")
    nonce = blob[len(MAGIC) : len(MAGIC) + NONCE_SIZE]
    try:
        return AESGCM(key).decrypt(nonce, blob[len(MAGIC) + NONCE_SIZE :], aad)
    except Exception as exc:  # InvalidTag
        raise DecryptionError("Clé invalide ou données altérées") from exc


def argon2id(secret: bytes, salt: bytes, time_cost: int, memory_kib: int, parallelism: int = 2) -> bytes:
    return hash_secret_raw(
        secret=secret,
        salt=salt,
        time_cost=time_cost,
        memory_cost=memory_kib,
        parallelism=parallelism,
        hash_len=KEY_SIZE,
        type=Type.ID,
    )


def hkdf(master: bytes, info: str) -> bytes:
    return HKDF(algorithm=hashes.SHA256(), length=KEY_SIZE, salt=None, info=info.encode()).derive(master)
