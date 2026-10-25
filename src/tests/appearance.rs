//! python/tests/test_appearance.py: copy buttons, "hide sensitive data", colour themes and the logo.

use regex::Regex;
use rust_decimal::Decimal;

use crate::py::PyDec;
use crate::testkit::env::{client, csrf_of, Client, ADMIN_EMAIL, PASSWORD};
use crate::testkit::fixtures::*;
use crate::testkit::{decode_png, Png};
use crate::web::jinja::Value;
use crate::web::themes::{self, Theme};
use crate::web::views::{f_money, f_money_text, f_qty, iban, identity_text};

fn d(s: &str) -> Decimal {
    dec(s)
}

fn login(client: &mut Client) -> String {
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("password", PASSWORD), ("confirm", PASSWORD)]);
    csrf_of(&client.get("/documents").text())
}

fn account_page(client: &mut Client) -> String {
    let csrf = login(client);
    let data = boursobank_statement_pdf(&Spec::new(sample_entries()));
    let review = client.post_files("/documents", &[("csrf", &csrf)], &[crate::testkit::env::Upload { field: "files", filename: "releve.pdf", data: &data, content_type: "application/pdf" }]);
    let doc = Regex::new(r"/documents/(\d+)").unwrap().captures(&review.url).unwrap()[1].to_string();
    let done = client.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte courant"), ("import_transactions", "on")]);
    Regex::new(r"/accounts/(\d+)").unwrap().captures(&done.url).unwrap()[1].to_string()
}

// ------------------------------------------------------------------------------------------------- themes
fn luminance(color: &str) -> f64 {
    let c = color.trim_start_matches('#');
    let channel = |i: usize| i64::from_str_radix(&c[i..i + 2], 16).unwrap() as f64 / 255.0;
    let f = |c: f64| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * f(channel(0)) + 0.7152 * f(channel(2)) + 0.0722 * f(channel(4))
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x >= y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

#[test]
fn test_there_are_at_least_ten_light_and_ten_dark_themes() {
    let light = themes::THEMES.iter().filter(|t| t.kind == "light").count();
    let dark = themes::THEMES.iter().filter(|t| t.kind == "dark").count();
    assert!(light >= 30 && dark >= 30);
    let ids: Vec<&str> = themes::THEMES.iter().map(|t| t.id).collect();
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), ids.len());
    let safe = Regex::new(r"^[a-z-]+$").unwrap();
    assert!(ids.iter().all(|i| safe.is_match(i))); // ids are safe in an attribute and in localStorage
    assert_eq!(themes::groups().iter().map(|(title, _)| *title).collect::<Vec<_>>(), vec!["Clairs", "Sombres"]);
    for (i, t) in themes::THEMES.iter().enumerate() {
        for u in &themes::THEMES[i + 1..] {
            assert!(t.colors[0] != u.colors[0] || t.colors[1] != u.colors[1], "{} and {} are identical", t.id, u.id); // no two identical
        }
    }
}

fn check_theme(id: &str) {
    let theme: &Theme = themes::THEMES.iter().find(|t| t.id == id).unwrap();
    let [bg, surface, text, muted, _border, primary, primary_text, pos, neg, warn] = theme.colors;
    assert!(!text.is_empty() && theme.series.len() == 8);
    for background in [bg, surface] {
        assert!(contrast(text, background) >= 7.0, "text");
        assert!(contrast(muted, background) >= 4.5, "muted text");
        for (name, color) in [("primary", primary), ("pos", pos), ("neg", neg), ("warn", warn)] {
            // used as text colours (links, amounts, warnings)
            assert!(contrast(color, background) >= 4.5, "{name}");
        }
    }
    assert!(contrast(primary_text, primary) >= 4.5); // text of the main buttons
    assert!(theme.series.iter().all(|color| contrast(color, surface) >= 3.0)); // chart colours on cards
    assert_eq!(luminance(bg) < 0.2, theme.kind == "dark");
}

macro_rules! theme_tests {
    ($($name:ident: $id:expr;)*) => {
        $(
            #[test]
            fn $name() {
                check_theme($id);
            }
        )*
    };
}

theme_tests! {
    test_every_theme_is_readable_clair: "clair";
    test_every_theme_is_readable_neige: "neige";
    test_every_theme_is_readable_sable: "sable";
    test_every_theme_is_readable_menthe: "menthe";
    test_every_theme_is_readable_lavande: "lavande";
    test_every_theme_is_readable_ciel: "ciel";
    test_every_theme_is_readable_rose: "rose";
    test_every_theme_is_readable_papier: "papier";
    test_every_theme_is_readable_solarized_clair: "solarized-clair";
    test_every_theme_is_readable_agrume: "agrume";
    test_every_theme_is_readable_sombre: "sombre";
    test_every_theme_is_readable_minuit: "minuit";
    test_every_theme_is_readable_ardoise: "ardoise";
    test_every_theme_is_readable_charbon: "charbon";
    test_every_theme_is_readable_foret: "foret";
    test_every_theme_is_readable_aubergine: "aubergine";
    test_every_theme_is_readable_nord: "nord";
    test_every_theme_is_readable_dracula: "dracula";
    test_every_theme_is_readable_solarized_sombre: "solarized-sombre";
    test_every_theme_is_readable_oled: "oled";
    test_every_theme_is_readable_aube: "aube";
    test_every_theme_is_readable_brume: "brume";
    test_every_theme_is_readable_coton: "coton";
    test_every_theme_is_readable_perle: "perle";
    test_every_theme_is_readable_lin: "lin";
    test_every_theme_is_readable_amande: "amande";
    test_every_theme_is_readable_pistache: "pistache";
    test_every_theme_is_readable_citron: "citron";
    test_every_theme_is_readable_abricot: "abricot";
    test_every_theme_is_readable_corail: "corail";
    test_every_theme_is_readable_peche: "peche";
    test_every_theme_is_readable_framboise: "framboise";
    test_every_theme_is_readable_lilas: "lilas";
    test_every_theme_is_readable_glycine: "glycine";
    test_every_theme_is_readable_ocean: "ocean";
    test_every_theme_is_readable_lagune: "lagune";
    test_every_theme_is_readable_givre: "givre";
    test_every_theme_is_readable_argile: "argile";
    test_every_theme_is_readable_nuage: "nuage";
    test_every_theme_is_readable_pivoine: "pivoine";
    test_every_theme_is_readable_cosmos: "cosmos";
    test_every_theme_is_readable_orage: "orage";
    test_every_theme_is_readable_encre: "encre";
    test_every_theme_is_readable_obsidienne: "obsidienne";
    test_every_theme_is_readable_sapin: "sapin";
    test_every_theme_is_readable_bordeaux: "bordeaux";
    test_every_theme_is_readable_prune: "prune";
    test_every_theme_is_readable_cobalt: "cobalt";
    test_every_theme_is_readable_abysse: "abysse";
    test_every_theme_is_readable_braise: "braise";
    test_every_theme_is_readable_cuivre: "cuivre";
    test_every_theme_is_readable_mousse: "mousse";
    test_every_theme_is_readable_crepuscule: "crepuscule";
    test_every_theme_is_readable_aurore: "aurore";
    test_every_theme_is_readable_fusain: "fusain";
    test_every_theme_is_readable_ebene: "ebene";
    test_every_theme_is_readable_petrole: "petrole";
    test_every_theme_is_readable_neon: "neon";
    test_every_theme_is_readable_cerise: "cerise";
    test_every_theme_is_readable_brouillard: "brouillard";
}

#[test]
fn test_the_stylesheet_has_one_rule_per_theme_and_is_public() {
    let mut client = client();
    let css = client.get("/themes.css"); // served before login: the login page uses the saved style too
    assert!(css.status == 200 && css.header("content-type").unwrap().starts_with("text/css"));
    let text = css.text();
    for theme in themes::THEMES {
        let rule = Regex::new(&format!(r#"\[data-theme="{}"\] \{{([^}}]*)\}}"#, regex::escape(theme.id))).unwrap().captures(&text).unwrap()[1].to_string();
        for variable in ["bg", "surface", "text", "muted", "border", "primary", "primary-text", "pos", "neg", "warn", "s0", "s7", "cat-dettes"] {
            assert!(rule.contains(&format!("--{variable}:")), "{} {variable}", theme.id);
        }
        assert!(rule.contains(&format!("color-scheme: {}", theme.kind)) && rule.contains(theme.colors[0]));
    }
    assert!(!text.contains('<') && !text.contains("url(") && !text.contains("@import"));
}

#[test]
fn test_pages_load_the_themes_and_the_scripts_without_anything_inline() {
    let mut client = client();
    for page in [client.get("/login"), client.get("/setup")] {
        let text = page.text();
        assert!(text.contains(r#"<link rel="stylesheet" href="/themes.css">"#) && text.contains(r#"<script src="/static/theme.js"></script>"#));
    }
    assert_eq!(client.get("/static/theme.js").status, 200);
    let script = client.get("/static/theme.js").text();
    assert!(script.contains("mymoney-theme") && script.contains("mymoney-privacy"));
    let csrf = login(&mut client);
    let home = client.get("/");
    let text = home.text();
    assert!(!text.contains(" style=") && !text.contains("<script>")); // strict Content-Security-Policy
    assert!(home.header("content-security-policy").unwrap().contains("style-src 'self'"));
    for theme in themes::THEMES {
        assert!(text.contains(&format!("data-set-theme=\"{}\"", theme.id)) && text.contains(theme.label), "{}", theme.id);
    }
    assert!(text.contains("data-set-theme=\"auto\"") && text.contains("id=\"privacy-toggle\"") && !csrf.is_empty());
    assert!(!text.contains("class=\"dots\"") && !text.contains("<i></i>")); // swatches are plain: no decorative dots
}

#[test]
fn test_app_js_keeps_the_choices_on_the_device() {
    let mut client = client();
    let js = client.get("/static/app.js").text();
    for expected in ["mymoney-theme", "mymoney-privacy", "data-set-theme", "classList.toggle(\"privacy\")", "navigator.clipboard", "execCommand(\"copy\")"] {
        assert!(js.contains(expected), "{expected}");
    }
}

// ----------------------------------------------------------------------------------- sensitive data
#[test]
fn test_amounts_are_marked_as_sensitive_in_the_page_but_not_in_attributes() {
    let dec_value = |s: &str| Value::pydec(PyDec::from(&d(s)));
    assert_eq!(f_money(&dec_value("1234.5"), &[], &[]).unwrap().to_str(), r#"<span class="sensitive">1 234,50 €</span>"#);
    assert_eq!(f_money_text(&dec_value("-1234.5"), &[], &[]).unwrap().to_str(), "-1 234,50 €");
    assert_eq!(f_money(&Value::None, &[], &[]).unwrap().to_str(), r#"<span class="sensitive">—</span>"#);
    assert_eq!(f_qty(&dec_value("0.5"), &[], &[]).unwrap().to_str(), r#"<span class="sensitive">0,5</span>"#);
    assert!(!f_money_text(&dec_value("1"), &[], &[]).unwrap().to_str().contains('<'));
}

#[test]
fn test_the_privacy_mode_blurs_what_is_sensitive() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/static/style.css")).unwrap();
    let rule = Regex::new(r"\.privacy \.sensitive, ([^{]*)\{([^}]*)\}").unwrap().captures(&css).unwrap();
    assert!(rule[2].contains("filter: blur") && rule[2].contains("user-select: none"));
    for target in [".privacy code", ".privacy .chart-svg", ".privacy .donut-svg", ".privacy .spark"] {
        assert!(rule[0].contains(target), "{target}");
    }
    assert!(css.contains(".privacy .chart-svg .pop { display: none !important; }")); // the tooltips of the charts show amounts too
}

#[test]
fn test_identifiers_names_and_labels_carry_the_sensitive_class() {
    let mut client = client();
    let account_id = account_page(&mut client);
    let page = client.get(&format!("/accounts/{account_id}")).text();
    assert!(page.contains(r#"<span class="sensitive">M DUPONT JEAN</span>"#) && page.contains(r#"<span class="sensitive">12 RUE DES LILAS"#));
    assert!(page.matches("class=\"sensitive\"").count() >= 10); // amounts, holder, address, operation labels
    assert!(page.contains("<code>FR76")); // IBAN and RIB are in <code>, blurred by the privacy rule
    assert!(Regex::new(r#"<td class="sensitive-cell"><span class="sensitive">"#).unwrap().is_match(&page));
    assert!(client.get("/documents").text().contains("class=\"sensitive\">BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf"));
}

// ----------------------------------------------------------------------------------------- copy buttons
#[test]
fn test_iban_and_identity_text_helpers() {
    assert_eq!(iban(&Value::str("FR7612345678900001234567858")), "FR76 1234 5678 9000 0123 4567 858");
    assert_eq!(iban(&Value::None), "");
    let account = Value::sdict(vec![("holder_name", "M DUPONT JEAN".into()), ("iban", iban_plain().into()), ("bic", "BOUSFRPPXXX".into()), ("rib", "12345 67890 00012345678 58".into())]);
    assert_eq!(
        identity_text(&account).unwrap().to_str(),
        format!("Titulaire : M DUPONT JEAN\nIBAN : {}\nBIC : BOUSFRPPXXX\nRIB : 12345 67890 00012345678 58", iban(&Value::str(iban_plain())))
    );
}

fn iban_plain() -> String {
    crate::testkit::fixtures::iban()
}

#[test]
fn test_the_account_page_has_a_copy_button_for_each_piece_of_identity() {
    let mut client = client();
    let account_id = account_page(&mut client);
    let page = client.get(&format!("/accounts/{account_id}")).text();
    let copies: Vec<String> = Regex::new(r#"data-copy="([^"]*)""#).unwrap().captures_iter(&page).map(|c| c[1].to_string()).collect();
    assert!(copies.contains(&iban_plain())); // the IBAN is copied without spaces: safe in any form
    for expected in ["BOUSFRPPXXX", "12345 67890 00012345678 58", "M DUPONT JEAN"] {
        assert!(copies.iter().any(|c| c == expected), "{expected}");
    }
    assert!(copies.iter().any(|c| c.contains("12 RUE DES LILAS") && c.contains("75011 PARIS")));
    let everything = copies.iter().find(|c| c.starts_with("Titulaire")).unwrap();
    assert!(everything.contains(&format!("IBAN : {}", iban(&Value::str(iban_plain())))) && everything.contains("BIC : BOUSFRPPXXX") && everything.contains("RIB : 12345"));
    assert!(page.contains("Tout copier") && page.contains("FR76 1234 5678 9000")); // displayed in groups of four
    assert!(page.matches("class=\"btn small copy\"").count() >= 6);
}

#[test]
fn test_contacts_have_copy_buttons_for_their_iban() {
    let mut client = client();
    let csrf = login(&mut client);
    let done = client.post("/contacts/new", &[("csrf", &csrf), ("name", "Pauline"), ("kind", "person"), ("iban", &iban_plain())]);
    assert_eq!(done.status, 200);
    let needle = format!("data-copy=\"{}\"", iban_plain());
    assert!(done.text().contains(&needle) || client.get("/contacts").text().contains(&needle));
}

// ------------------------------------------------------------------------------------------------ logo
#[test]
fn test_the_logo_is_served_and_shown() {
    let mut client = client();
    let logo = client.get("/static/logo.svg");
    assert!(logo.status == 200 && logo.header("content-type").unwrap().starts_with("image/svg") && logo.text().contains("linearGradient"));
    assert_eq!(client.get("/static/icon.svg").text(), logo.text());
    assert!(client.get("/login").text().replace('\n', "").contains(r#"src="/static/logo.svg""#) || client.get("/setup").text().contains(r#"src="/static/logo.svg""#));
    login(&mut client);
    let home = client.get("/").text();
    assert!(home.contains("class=\"logo\"") && home.contains(r#"<span class="wordmark">my<b>Money</b></span>"#));
}

fn icon(client: &mut Client, name: &str) -> Png {
    decode_png(&client.get(&format!("/icons/{name}")).body)
}

#[test]
fn test_the_app_icons_are_drawn_from_the_same_artwork() {
    let mut client = client();
    for (name, size, maskable) in [("icon-192.png", 192usize, false), ("icon-512.png", 512, false), ("maskable-512.png", 512, true)] {
        let image = icon(&mut client, name);
        assert_eq!((image.width, image.height), (size, size));
        let at = |fx: f64, fy: f64| image.pixel((size as f64 * fx) as usize, (size as f64 * fy) as usize);
        let top_left = at(0.2, 0.2);
        let bottom_right = if maskable { at(0.9, 0.9) } else { at(0.85, 0.85) };
        assert!(top_left[2] > top_left[0] && bottom_right[1] > top_left[1], "{name}"); // indigo at the top left, teal at the bottom right
        let centre = at(0.5, if maskable { 0.62 } else { 0.7 });
        assert!(centre[..3].iter().min().unwrap() > &200, "{name}"); // the white stroke of the "m"
    }
    assert_eq!(&client.get("/icons/icon-192.png").body[..8], b"\x89PNG\r\n\x1a\n");
}
