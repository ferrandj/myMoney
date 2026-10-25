//! python/tests/test_pwa.py

use serde_json::Value;

use crate::testkit::decode_png;
use crate::testkit::env::client;

#[test]
fn test_pwa_files_are_public_and_installable() {
    let mut client = client();
    // Fetched by the browser before any login: no redirect to /setup or /login.
    let r = client.get_raw("/manifest.webmanifest");
    assert!(r.status == 200 && r.header("content-type").unwrap().starts_with("application/manifest+json"));
    let manifest: Value = serde_json::from_str(&r.text()).unwrap();
    assert!(manifest["display"] == "standalone" && manifest["start_url"] == "/" && manifest["scope"] == "/");
    assert!(manifest["name"].as_str().map(|s| !s.is_empty()).unwrap_or(false) && manifest["short_name"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
    let purposes: Vec<(String, String)> = manifest["icons"].as_array().unwrap().iter().map(|i| (i["sizes"].as_str().unwrap().to_string(), i["purpose"].as_str().unwrap().to_string())).collect();
    for expected in [("192x192", "any"), ("512x512", "any"), ("512x512", "maskable")] {
        assert!(purposes.contains(&(expected.0.to_string(), expected.1.to_string())), "{expected:?}");
    }
    for icon in manifest["icons"].as_array().unwrap() {
        assert_eq!(client.get_raw(icon["src"].as_str().unwrap()).status, 200);
    }
}

#[test]
fn test_icons_are_pngs_of_the_declared_size() {
    let mut client = client();
    for (name, size, maskable) in [("icon-192.png", 192usize, false), ("icon-512.png", 512, false), ("maskable-512.png", 512, true)] {
        let r = client.get_raw(&format!("/icons/{name}"));
        assert!(r.status == 200 && r.header("content-type") == Some("image/png"));
        let image = decode_png(&r.body);
        assert_eq!((image.width, image.height), (size, size));
        let corner = image.pixel(0, 0);
        assert_eq!(corner[3] == 255, maskable, "{name}"); // maskable icons are full-bleed, the others have rounded corners
    }
    assert_eq!(client.get("/icons/unknown.png").status, 404);
}

#[test]
fn test_service_worker_is_root_scoped_and_caches_nothing() {
    let mut client = client();
    let r = client.get_raw("/sw.js");
    assert!(r.status == 200 && r.header("content-type").unwrap().starts_with("text/javascript"));
    assert_eq!(r.header("service-worker-allowed"), Some("/"));
    // Financial data must never end up in the browser's storage.
    let text = r.text();
    assert!(!text.contains("caches") && !text.contains("cache.put") && !text.contains("addAll"));
}

#[test]
fn test_pages_declare_the_manifest_and_csp_allows_it() {
    let mut client = client();
    let r = client.get("/setup");
    let text = r.text();
    assert!(text.contains(r#"rel="manifest" href="/manifest.webmanifest""#) && text.contains(r#"name="theme-color""#));
    let csp = r.header("content-security-policy").unwrap();
    assert!(csp.contains("manifest-src 'self'") && csp.contains("worker-src 'self'") && csp.contains("default-src 'none'"));
}
