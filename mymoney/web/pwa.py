"""Installable web app (PWA): manifest, icons and a service worker that never caches anything."""

from __future__ import annotations

import io
import json
from functools import lru_cache

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response

THEME_COLOR = "#3b5bdb"
BACKGROUND_COLOR = "#f6f7f9"

# name -> (pixel size, maskable). Android needs a 192 and a 512 icon, plus a full-bleed one for adaptive launchers.
ICONS = {
    "icon-192.png": (192, False),
    "icon-512.png": (512, False),
    "maskable-512.png": (512, True),
}

MANIFEST = {
    "id": "/",
    "name": "myMoney",
    "short_name": "myMoney",
    "description": "Gestionnaire de patrimoine personnel auto-hébergé, chiffré au repos",
    "lang": "fr",
    "start_url": "/",
    "scope": "/",
    "display": "standalone",
    "theme_color": THEME_COLOR,
    "background_color": BACKGROUND_COLOR,
    "icons": [
        {"src": "/icons/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any"},
        {"src": "/icons/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any"},
        {"src": "/icons/maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"},
    ],
}

# Only there to make the app installable and to explain a down server. Nothing is cached on purpose:
# balances and documents must never be readable from the browser's storage while the vault is locked.
SERVICE_WORKER = """\
const OFFLINE = '<!doctype html><html lang="fr"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">' +
  '<title>myMoney</title><body style="font:16px system-ui;margin:3rem 1.5rem;text-align:center">' +
  '<h1>myMoney est injoignable</h1><p>Le NAS ne répond pas. Vérifie le VPN ou le réseau, puis recharge la page.</p></body></html>';

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));
self.addEventListener("fetch", (event) => {
  if (event.request.mode !== "navigate") return;
  event.respondWith(
    fetch(event.request).catch(() => new Response(OFFLINE, { status: 503, headers: { "Content-Type": "text/html; charset=utf-8" } })),
  );
});
"""

# Glyph of static/icon.svg, in its 32x32 coordinate system.
_GLYPH = [(8, 22), (8, 10), (16, 17), (24, 10), (24, 22)]
_GLYPH_WIDTH = 3


@lru_cache(maxsize=None)
def render_icon(size: int, maskable: bool) -> bytes:
    """Draw the app icon as a PNG (same artwork as icon.svg, no binary file to ship)."""
    from PIL import Image, ImageDraw

    oversample = 4  # drawn large then scaled down: smooth edges without anti-aliasing support in ImageDraw
    px = size * oversample
    image = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    blue = tuple(int(THEME_COLOR[i : i + 2], 16) for i in (1, 3, 5)) + (255,)
    white = (255, 255, 255, 255)
    if maskable:
        draw.rectangle((0, 0, px, px), fill=blue)  # the launcher applies its own mask
    else:
        draw.rounded_rectangle((0, 0, px - 1, px - 1), radius=px * 8 // 32, fill=blue)
    k = px / 32 * (0.6 if maskable else 1)  # maskable icons keep the glyph inside the central safe zone
    points = [(px / 2 + (x - 16) * k, px / 2 + (y - 16) * k) for x, y in _GLYPH]
    width = round(_GLYPH_WIDTH * k)
    draw.line(points, fill=white, width=width, joint="curve")
    for x, y in (points[0], points[-1]):  # round caps
        draw.ellipse((x - width / 2, y - width / 2, x + width / 2, y + width / 2), fill=white)
    out = io.BytesIO()
    image.resize((size, size), Image.LANCZOS).save(out, "PNG", optimize=True)
    return out.getvalue()


def install(app: FastAPI) -> None:
    """Register the PWA routes. They are public: the browser fetches them before any login."""

    @app.get("/manifest.webmanifest")
    async def manifest():
        return Response(json.dumps(MANIFEST, ensure_ascii=False), media_type="application/manifest+json")

    @app.get("/sw.js")
    async def service_worker():
        # Served from the root so its scope covers the whole app.
        return Response(SERVICE_WORKER, media_type="text/javascript", headers={"Service-Worker-Allowed": "/"})

    @app.get("/icons/{name}")
    async def icon(name: str):
        if name not in ICONS:
            raise HTTPException(404)
        size, maskable = ICONS[name]
        return Response(render_icon(size, maskable), media_type="image/png", headers={"Cache-Control": "public, max-age=86400"})
