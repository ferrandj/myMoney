"""Cloudflare Access: who is the person behind this request?

With an Access application in front of the app, Cloudflare authenticates the visitor (the e-mail list, a one-time code, a
Google account...) and adds to every request that reaches the origin a signed token in the `Cf-Access-Jwt-Assertion`
header. The token says who the visitor is (`email`), which Access application it is for (`aud`), who issued it (`iss`)
and until when it is valid (`exp`).

Reading the e-mail from a plain header would be a hole: the NAS also answers on the local network, where anyone can send
any header. Only a token whose signature checks out against Cloudflare's published keys, for this very application, counts.
The keys are fetched from `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs` and kept for an hour.
"""

from __future__ import annotations

import logging
import threading
import time
from typing import Callable

import jwt

log = logging.getLogger(__name__)

HEADER = "cf-access-jwt-assertion"
KEY_LIFETIME = 3600
ISSUER_SUFFIX = ".cloudflareaccess.com"


def team_host(team: str) -> str:
    """"acme", "acme.cloudflareaccess.com" or "https://acme.cloudflareaccess.com/" all name the same team."""
    value = team.strip().lower().removeprefix("https://").removeprefix("http://").strip("/")
    return value if value.endswith(ISSUER_SUFFIX) else value + ISSUER_SUFFIX


def fetch_keys(url: str) -> list[dict]:
    import httpx

    response = httpx.get(url, timeout=10, follow_redirects=False)
    response.raise_for_status()
    return response.json().get("keys", [])


class CloudflareAccess:
    def __init__(self, team_domain: str | None, audience: str | None, fetcher: Callable[[str], list[dict]] = fetch_keys):
        self.audience = (audience or "").strip()
        self.host = team_host(team_domain) if team_domain and team_domain.strip() else ""
        self._fetcher = fetcher
        self._keys: dict[str, object] = {}
        self._fetched_at = 0.0
        self._lock = threading.Lock()

    @property
    def enabled(self) -> bool:
        return bool(self.host and self.audience)

    @property
    def issuer(self) -> str:
        return f"https://{self.host}"

    @property
    def logout_url(self) -> str:
        return "/cdn-cgi/access/logout"

    def _key_for(self, kid: str | None):
        with self._lock:
            stale = time.monotonic() - self._fetched_at > KEY_LIFETIME
            if stale or kid not in self._keys:  # a key unknown to the cache may be a new one: look again (rate-limited)
                if stale or time.monotonic() - self._fetched_at > 30:
                    self._keys = {k["kid"]: jwt.PyJWK(k).key for k in self._fetcher(f"{self.issuer}/cdn-cgi/access/certs") if "kid" in k}
                    self._fetched_at = time.monotonic()
            return self._keys.get(kid)

    def verify(self, token: str | None) -> str | None:
        """The verified e-mail of the visitor (lower case), or None for anything that is not a valid Access token
        for this application (no token, bad signature, other application, expired, a service token without e-mail)."""
        if not self.enabled or not token:
            return None
        try:
            kid = jwt.get_unverified_header(token).get("kid")
            key = self._key_for(kid)
            if key is None:
                return None
            claims = jwt.decode(
                token, key=key, algorithms=["RS256"], audience=self.audience, issuer=self.issuer, leeway=30,
                options={"require": ["exp", "iss", "aud"]},
            )
        except jwt.PyJWTError as exc:
            log.info("Jeton Cloudflare Access refusé : %s", exc.__class__.__name__)
            return None
        except Exception:  # keys unreachable: nobody is signed in by Cloudflare, the password still works
            log.warning("Clés Cloudflare Access injoignables", exc_info=True)
            return None
        email = claims.get("email")
        return email.strip().lower() if isinstance(email, str) and "@" in email else None

    def email_of(self, headers) -> str | None:
        return self.verify(headers.get(HEADER))
