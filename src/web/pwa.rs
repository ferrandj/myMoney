//! Installable web app (PWA): manifest, icons and a service worker that never caches anything.

pub const THEME_COLOR: &str = "#4f46e5";
pub const BACKGROUND_COLOR: &str = "#f6f7f9";

/// `json.dumps(MANIFEST, ensure_ascii=False)`.
pub fn manifest() -> String {
    let icons = [("/icons/icon-192.png", "192x192", "any"), ("/icons/icon-512.png", "512x512", "any"), ("/icons/maskable-512.png", "512x512", "maskable")]
        .iter()
        .map(|(src, sizes, purpose)| format!(r#"{{"src": "{src}", "sizes": "{sizes}", "type": "image/png", "purpose": "{purpose}"}}"#))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{"id": "/", "name": "myMoney", "short_name": "myMoney", "description": "Gestionnaire de patrimoine personnel auto-hébergé, chiffré au repos", "lang": "fr", "start_url": "/", "scope": "/", "display": "standalone", "theme_color": "{THEME_COLOR}", "background_color": "{BACKGROUND_COLOR}", "icons": [{icons}]}}"#
    )
}

/// Only there to make the app installable and to explain a down server. Nothing is cached on purpose: balances and
/// documents must never be readable from the browser's storage while the vault is locked.
pub const SERVICE_WORKER: &str = r#"const OFFLINE = '<!doctype html><html lang="fr"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">' +
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
"#;

/// The icons, drawn once by the Python version (same artwork as static/icon.svg).
pub fn icon(name: &str) -> Option<&'static [u8]> {
    match name {
        "icon-192.png" => Some(include_bytes!("../../web/icons/icon-192.png")),
        "icon-512.png" => Some(include_bytes!("../../web/icons/icon-512.png")),
        "maskable-512.png" => Some(include_bytes!("../../web/icons/maskable-512.png")),
        _ => None,
    }
}
