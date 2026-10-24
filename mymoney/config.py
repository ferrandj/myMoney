"""Runtime configuration, read from environment variables (see .env.example)."""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path


def _bool(value: str | None, default: bool) -> bool:
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "on"}


@dataclass
class Settings:
    data_dir: Path = field(default_factory=lambda: Path(os.environ.get("MYMONEY_DATA_DIR", "./data")).resolve())
    # Mark the session cookie "Secure". Enable it as soon as the app is served over HTTPS.
    cookie_secure: bool = field(default_factory=lambda: _bool(os.environ.get("MYMONEY_COOKIE_SECURE"), False))
    # Minutes of inactivity after which the vault is locked and the key wiped from memory.
    auto_lock_minutes: int = field(default_factory=lambda: int(os.environ.get("MYMONEY_AUTO_LOCK_MINUTES", "15")))
    # Opt-in unattended unlock from a Docker secret file. Lowers security: see SECURITY.md.
    unlock_password_file: str | None = field(default_factory=lambda: os.environ.get("MYMONEY_UNLOCK_PASSWORD_FILE"))
    # E-mail of the user whose password is in that file (needed once there are several users).
    unlock_email: str | None = field(default_factory=lambda: os.environ.get("MYMONEY_UNLOCK_EMAIL"))
    # Cloudflare Access: with both set, a request carrying a valid signed Access token signs in the user of its e-mail.
    cf_team_domain: str | None = field(default_factory=lambda: os.environ.get("MYMONEY_CF_TEAM_DOMAIN"))
    cf_audience: str | None = field(default_factory=lambda: os.environ.get("MYMONEY_CF_AUD"))
    # Extra origins accepted for form posts (the public address behind a tunnel), separated by commas.
    trusted_origins: str = field(default_factory=lambda: os.environ.get("MYMONEY_TRUSTED_ORIGINS", ""))
    max_upload_mb: int = field(default_factory=lambda: int(os.environ.get("MYMONEY_MAX_UPLOAD_MB", "25")))
    # Argon2id cost. Defaults fit a modest NAS; raise memory if the NAS has RAM to spare.
    argon2_memory_kib: int = field(default_factory=lambda: int(os.environ.get("MYMONEY_ARGON2_MEMORY_KIB", str(128 * 1024))))
    argon2_time_cost: int = field(default_factory=lambda: int(os.environ.get("MYMONEY_ARGON2_TIME_COST", "3")))

    @property
    def keystore_path(self) -> Path:
        return self.data_dir / "keystore.json"

    @property
    def db_path(self) -> Path:
        return self.data_dir / "mymoney.db"

    @property
    def documents_dir(self) -> Path:
        return self.data_dir / "documents"

    def ensure_dirs(self) -> None:
        # Files created by the app (database, WAL, documents) are readable by its user only.
        os.umask(0o077)
        for path in (self.data_dir, self.documents_dir):
            path.mkdir(parents=True, exist_ok=True)
            os.chmod(path, 0o700)


settings = Settings()
