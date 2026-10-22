import io
import json

import pytest
from fastapi.testclient import TestClient
from PIL import Image

from mymoney.web.app import create_app
from mymoney.web.pwa import ICONS


@pytest.fixture
def client(settings):
    with TestClient(create_app(settings)) as c:
        yield c


def test_pwa_files_are_public_and_installable(client):
    # Fetched by the browser before any login: no redirect to /setup or /login.
    r = client.get("/manifest.webmanifest", follow_redirects=False)
    assert r.status_code == 200 and r.headers["content-type"].startswith("application/manifest+json")
    manifest = json.loads(r.text)
    assert manifest["display"] == "standalone" and manifest["start_url"] == "/" and manifest["scope"] == "/"
    assert manifest["name"] and manifest["short_name"]
    purposes = {(i["sizes"], i["purpose"]) for i in manifest["icons"]}
    assert {("192x192", "any"), ("512x512", "any"), ("512x512", "maskable")} <= purposes
    for icon in manifest["icons"]:
        assert client.get(icon["src"], follow_redirects=False).status_code == 200


def test_icons_are_pngs_of_the_declared_size(client):
    for name, (size, maskable) in ICONS.items():
        r = client.get(f"/icons/{name}", follow_redirects=False)
        assert r.status_code == 200 and r.headers["content-type"] == "image/png"
        image = Image.open(io.BytesIO(r.content))
        assert image.format == "PNG" and image.size == (size, size)
        corner = image.getpixel((0, 0))
        assert (corner[3] == 255) == maskable  # maskable icons are full-bleed, the others have rounded corners
    assert client.get("/icons/unknown.png").status_code == 404


def test_service_worker_is_root_scoped_and_caches_nothing(client):
    r = client.get("/sw.js", follow_redirects=False)
    assert r.status_code == 200 and r.headers["content-type"].startswith("text/javascript")
    assert r.headers["service-worker-allowed"] == "/"
    # Financial data must never end up in the browser's storage.
    assert "caches" not in r.text and "cache.put" not in r.text and "addAll" not in r.text


def test_pages_declare_the_manifest_and_csp_allows_it(client):
    r = client.get("/setup")
    assert 'rel="manifest" href="/manifest.webmanifest"' in r.text and 'name="theme-color"' in r.text
    csp = r.headers["content-security-policy"]
    assert "manifest-src 'self'" in csp and "worker-src 'self'" in csp and "default-src 'none'" in csp
