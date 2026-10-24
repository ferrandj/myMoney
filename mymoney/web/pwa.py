"""Installable web app (PWA): manifest, icons and a service worker that never caches anything."""

from __future__ import annotations

import io
import json
from functools import lru_cache

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response

THEME_COLOR = "#4f46e5"
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

# The artwork of static/icon.svg (32x32 coordinates): an "m" made of two arches, and a coin above it.
_ARCHES = [((8.5, 23), (8.5, 15.5)), ((8.5, 15.5), (8.5, 10.5), (16, 10.5), (16, 15.5)), ((16, 15.5), (16, 23)),
           ((16, 15.5), (16, 10.5), (23.5, 10.5), (23.5, 15.5)), ((23.5, 15.5), (23.5, 23))]
_STROKE = 2.8
_FROM, _TO = (0x4F, 0x46, 0xE5), (0x08, 0x91, 0xB2)


def _bezier(points, steps=24):
    if len(points) == 2:
        return list(points)
    out = []
    for i in range(steps + 1):
        t = i / steps
        a, b, c, d = points
        out.append(tuple((1 - t) ** 3 * a[k] + 3 * (1 - t) ** 2 * t * b[k] + 3 * (1 - t) * t**2 * c[k] + t**3 * d[k] for k in (0, 1)))
    return out


@lru_cache(maxsize=None)
def render_icon(size: int, maskable: bool) -> bytes:
    """Draw the app icon as a PNG (same artwork as icon.svg, no binary file to ship)."""
    from PIL import Image, ImageDraw

    oversample = 4  # drawn large then scaled down: smooth edges without anti-aliasing support in ImageDraw
    px = size * oversample
    # Diagonal gradient, one line per diagonal.
    gradient = Image.new("RGBA", (px, px))
    gdraw = ImageDraw.Draw(gradient)
    for d in range(2 * px):
        t = d / (2 * px - 1)
        color = tuple(round(_FROM[i] + (_TO[i] - _FROM[i]) * t) for i in range(3)) + (255,)
        gdraw.line([(d, 0), (0, d)], fill=color, width=2)
    mask = Image.new("L", (px, px), 0)
    if maskable:
        ImageDraw.Draw(mask).rectangle((0, 0, px, px), fill=255)  # the launcher applies its own mask
    else:
        ImageDraw.Draw(mask).rounded_rectangle((0, 0, px - 1, px - 1), radius=px * 8 // 32, fill=255)
    image = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    image.paste(gradient, (0, 0), mask)
    draw = ImageDraw.Draw(image)
    k = px / 32 * (0.6 if maskable else 1)  # maskable icons keep the glyph inside the central safe zone
    to_px = lambda p: (px / 2 + (p[0] - 16) * k, px / 2 + (p[1] - 16) * k)
    width = max(1, round(_STROKE * k))
    for segment in _ARCHES:
        points = [to_px(p) for p in _bezier(segment)]
        draw.line(points, fill=(255, 255, 255, 255), width=width, joint="curve")
        for x, y in (points[0], points[-1]):  # round caps
            draw.ellipse((x - width / 2, y - width / 2, x + width / 2, y + width / 2), fill=(255, 255, 255, 255))
    cx, cy = to_px((24.2, 7.6))
    for radius, fill in ((2.6, (252, 211, 77, 255)), (1.1, (245, 158, 11, 255))):
        r = radius * k
        if radius < 2:
            draw.ellipse((cx - r, cy - r, cx + r, cy + r), outline=fill, width=max(1, round(0.8 * k)))
        else:
            draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=fill)
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
