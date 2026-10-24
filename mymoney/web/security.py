"""Web sessions, CSRF protection and security headers."""

from __future__ import annotations

import hmac
import secrets
import threading
import time
from dataclasses import dataclass

from fastapi import HTTPException, Request

from ..services.access import Identity

COOKIE = "mm_session"

CSP = (
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; "
    "connect-src 'self'; manifest-src 'self'; worker-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
)
SECURITY_HEADERS = {
    "Content-Security-Policy": CSP,
    "X-Content-Type-Options": "nosniff",
    "X-Frame-Options": "DENY",
    "Referrer-Policy": "no-referrer",
    "Permissions-Policy": "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Cache-Control": "no-store",
}


@dataclass
class WebSession:
    csrf: str
    last_seen: float
    identity: Identity | None = None  # who is signed in (None only in the short step where a pre-users vault creates its first user)


class Sessions:
    def __init__(self, idle_minutes: int):
        self.idle = idle_minutes * 60
        self._items: dict[str, WebSession] = {}
        self._lock = threading.Lock()

    def create(self, identity: Identity | None = None) -> str:
        token = secrets.token_urlsafe(32)
        with self._lock:
            self._items[token] = WebSession(csrf=secrets.token_urlsafe(32), last_seen=time.monotonic(), identity=identity)
        return token

    def drop_user(self, user_id: int) -> None:
        """Sign a user out everywhere (their account was removed or changed)."""
        with self._lock:
            for token in [t for t, s in self._items.items() if s.identity and s.identity.id == user_id]:
                del self._items[token]

    def update_identity(self, identity: Identity) -> None:
        with self._lock:
            for session in self._items.values():
                if session.identity and session.identity.id == identity.id:
                    session.identity = identity

    def get(self, token: str | None) -> WebSession | None:
        if not token:
            return None
        with self._lock:
            session = self._items.get(token)
            if session is None:
                return None
            if time.monotonic() - session.last_seen > self.idle:
                del self._items[token]
                return None
            session.last_seen = time.monotonic()
            return session

    def drop(self, token: str | None) -> None:
        with self._lock:
            self._items.pop(token or "", None)

    def clear(self) -> None:
        with self._lock:
            self._items.clear()

    def active(self) -> bool:
        now = time.monotonic()
        with self._lock:
            for token in [t for t, s in self._items.items() if now - s.last_seen > self.idle]:
                del self._items[token]
            return bool(self._items)


async def verified_form(request: Request) -> dict[str, str]:
    """Parse a POSTed form and check its CSRF token."""
    form = await request.form()
    session: WebSession | None = getattr(request.state, "session", None)
    token = form.get("csrf")
    if session is None or not isinstance(token, str) or not hmac.compare_digest(token, session.csrf):
        raise HTTPException(status_code=403, detail="Jeton CSRF invalide : recharge la page")
    return {k: v for k, v in form.items() if isinstance(v, str)}
