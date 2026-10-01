//! The UI is plain files rather than a compiled bundle, so nothing checks it
//! at build time. These tests stand in for that: every element the scripts
//! reach for must exist in the page, and every API path they call must be a
//! route the node actually serves.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use support::*;

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ui")
}

fn read(name: &str) -> String {
    let path = ui_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    // `.gitattributes` asks for LF everywhere, but a checkout configured
    // otherwise should make a test fail for a real reason or not at all — not
    // because an assertion spanning a newline met `\r\n`. This is how the
    // Windows runner failed while Linux and macOS passed.
    text.replace("\r\n", "\n")
}

/// Element ids a script looks up, from `$('x')` and `getElementById('x')`.
fn referenced_ids(script: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for (marker, open) in [("$(", '\''), ("getElementById(", '\'')] {
        let mut rest = script;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let quote = match rest.chars().next() {
                Some(q) if q == open || q == '"' => q,
                _ => continue,
            };
            let after = &rest[quote.len_utf8()..];
            if let Some(end) = after.find(quote) {
                let id = &after[..end];
                // `$(...)` is also used for other things; an id has no spaces.
                if !id.is_empty() && !id.contains(char::is_whitespace) {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids
}

fn declared_ids(html: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut rest = html;
    while let Some(at) = rest.find("id=\"") {
        rest = &rest[at + 4..];
        if let Some(end) = rest.find('"') {
            ids.insert(rest[..end].to_string());
        }
    }
    ids
}

#[test]
fn every_element_the_viewer_reaches_for_exists_in_its_page() {
    let missing: Vec<String> = referenced_ids(&read("viewer.js"))
        .difference(&declared_ids(&read("viewer.html")))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "viewer.js looks up missing ids: {missing:?}"
    );
}

#[test]
fn every_element_the_admin_page_reaches_for_exists_in_its_page() {
    let missing: Vec<String> = referenced_ids(&read("admin.js"))
        .difference(&declared_ids(&read("admin.html")))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "admin.js looks up missing ids: {missing:?}"
    );
}

#[test]
fn both_pages_load_the_scripts_and_stylesheet_the_node_serves() {
    for (page, script) in [
        ("viewer.html", "/assets/viewer.js"),
        ("admin.html", "/assets/admin.js"),
    ] {
        let html = read(page);
        assert!(html.contains("/assets/app.css"), "{page} has no stylesheet");
        assert!(html.contains(script), "{page} does not load {script}");
        assert!(
            html.contains(r#"type="module""#),
            "{page} must load its script as a module"
        );
        // A strict CSP forbids inline scripts and styles, so there must be
        // none to break.
        assert!(!html.contains("<script>"), "{page} has an inline script");
        assert!(!html.contains("<style"), "{page} has an inline stylesheet");
        assert!(
            !html.contains(" style=\""),
            "{page} has an inline style attribute"
        );
        assert!(!html.contains(" onclick="), "{page} has an inline handler");
    }
}

/// API paths the scripts call, with `${...}` placeholders left in.
fn referenced_api_paths(script: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for quote in ['\'', '`'] {
        let mut rest = script;
        while let Some(at) = rest.find(quote) {
            rest = &rest[at + 1..];
            let Some(end) = rest.find(quote) else { break };
            let literal = &rest[..end];
            rest = &rest[end + 1..];
            if literal.starts_with("/v1/") {
                paths.insert(literal.to_string());
            }
        }
    }
    paths
}

#[tokio::test(flavor = "multi_thread")]
async fn every_api_path_the_ui_calls_is_a_real_route() {
    let node = spawn_node("wiring").await;
    let source = write_sample_file(node.dir.path(), "clip.mp4", 4096);
    let report = node
        .node()
        .publish_video(
            &source,
            Some("Wiring".into()),
            String::new(),
            vec![],
            "https://videos.example/clip.mp4".to_string(),
        )
        .await
        .unwrap();
    let cid = report.video.cid.clone();
    let key = node.node().public_key().to_hex();

    let base = node.running.api_url().unwrap();
    let token = node.node().api_token().to_string();
    let http = reqwest::Client::new();

    let mut paths = referenced_api_paths(&read("viewer.js"));
    paths.extend(referenced_api_paths(&read("admin.js")));
    paths.extend(referenced_api_paths(&read("common.js")));
    assert!(
        paths.len() > 10,
        "expected to find the UI's API calls, got {paths:?}"
    );

    for path in &paths {
        // Substitute the placeholders with real values, and drop the query.
        let concrete = substitute(path, &cid, &key);
        let concrete = concrete.split('?').next().unwrap().to_string();

        let response = http
            .get(format!("{base}{concrete}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("requesting {concrete}: {e}"));

        let status = response.status();
        assert_ne!(
            status,
            reqwest::StatusCode::UNAUTHORIZED,
            "{concrete} rejected a valid token"
        );
        // 405 means the route exists but wants another method, which is fine:
        // this is about the path, not the verb. A 404 is ambiguous, so tell
        // the two apart by the body — a handler answers with a JSON error,
        // while an unmatched route produces nothing at all.
        if status == reqwest::StatusCode::NOT_FOUND {
            let body = response.text().await.unwrap_or_default();
            assert!(
                body.contains("\"error\""),
                "the UI calls {path}, which resolves to {concrete}, \
                 and the node has no such route"
            );
        }
    }

    node.shutdown().await;
}

/// Replace `${…}` with something the route will accept.
fn substitute(path: &str, cid: &str, key: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find('}') else {
            break;
        };
        let expression = &rest[at + 2..at + end];
        // The only two kinds of identifier in a path.
        out.push_str(
            if expression.contains("creator") || expression.contains("key") {
                key
            } else {
                cid
            },
        );
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Names a module exports: `export function x`, `export const x`, `export class x`.
fn exported_names(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in source.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("export ") else {
            continue;
        };
        let rest = rest.strip_prefix("async ").unwrap_or(rest);
        for keyword in ["function ", "const ", "let ", "class "] {
            if let Some(after) = rest.strip_prefix(keyword) {
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                if !name.is_empty() {
                    names.insert(name);
                }
                break;
            }
        }
    }
    names
}

/// Named imports in a module, as `(module path, name)` pairs.
fn imported_names(source: &str) -> Vec<(String, String)> {
    let mut imports = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("import {") {
        rest = &rest[at + "import {".len()..];
        let Some(close) = rest.find('}') else { break };
        let names = &rest[..close];
        let after = &rest[close + 1..];
        let Some(from) = after.find("from ") else {
            break;
        };
        let tail = &after[from + 5..];
        let quote = match tail.chars().next() {
            Some(q @ ('\'' | '"')) => q,
            _ => continue,
        };
        let Some(end) = tail[1..].find(quote) else {
            break;
        };
        let module = tail[1..1 + end].to_string();
        for name in names.split(',') {
            // `a as b` imports `a`.
            let name = name.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                imports.push((module.clone(), name));
            }
        }
        rest = &tail[end..];
    }
    imports
}

#[test]
fn every_name_the_ui_imports_is_actually_exported() {
    // JavaScript has no compiler to catch this, and the failure is total:
    // one missing export and the module never links, so the page renders its
    // frame and nothing else. That has happened once; this is why it cannot
    // happen twice.
    let modules: BTreeMap<&str, BTreeSet<String>> = ["common.js", "zones.js"]
        .into_iter()
        .map(|file| (file, exported_names(&read(file))))
        .collect();

    let mut missing = Vec::new();
    for file in ["viewer.js", "admin.js", "common.js"] {
        for (module, name) in imported_names(&read(file)) {
            let target = module.rsplit('/').next().unwrap_or(&module).to_string();
            let Some(exports) = modules.get(target.as_str()) else {
                panic!("{file} imports from {module}, which is not a UI module");
            };
            if !exports.contains(&name) {
                missing.push(format!(
                    "{file} imports {name} from {module}, which does not export it"
                ));
            }
        }
    }
    assert!(missing.is_empty(), "{missing:#?}");
}

#[test]
fn the_shared_module_exports_what_the_pages_need() {
    // A blunt guard on the handful of helpers both pages rely on, so a
    // rewrite of common.js cannot quietly drop one.
    let exports = exported_names(&read("common.js"));
    for name in [
        "t",
        "languagePicker",
        "whenLocaleChanges",
        "setLocale",
        "translate",
        "el",
        "mount",
        "clear",
        "empty",
        "toast",
        "reportError",
        "get",
        "post",
        "del",
        "api",
        "liveEvents",
        "poll",
        "router",
        "go",
        "bytes",
        "duration",
        "ago",
        "shortId",
        "date",
        "decimal",
        "percent",
        "number",
    ] {
        assert!(exports.contains(name), "common.js no longer exports {name}");
    }
}

// ---------------------------------------------------------------- language

fn english_keys() -> BTreeSet<String> {
    let body = read("locales/en.json");
    let pack: serde_json::Value = serde_json::from_str(&body).expect("en.json must parse");
    pack["strings"]
        .as_object()
        .expect("en.json has a strings object")
        .keys()
        .cloned()
        .collect()
}

/// Translation keys a file asks for: `data-i18n="…"` in HTML, `t('…')` in JS.
fn referenced_keys(source: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for marker in [
        "data-i18n=\"",
        "data-i18n-placeholder=\"",
        "data-i18n-title=\"",
        "data-i18n-label=\"",
    ] {
        let mut rest = source;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            if let Some(end) = rest.find('"') {
                keys.insert(rest[..end].to_string());
            }
        }
    }
    let mut rest = source;
    while let Some(at) = rest.find("t('") {
        // Skip `…t('` inside a longer identifier, such as `format('`.
        let preceded_by_word = source[..source.len() - rest.len() + at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
        rest = &rest[at + 3..];
        if preceded_by_word {
            continue;
        }
        if let Some(end) = rest.find('\'') {
            let key = &rest[..end];
            if key.contains('.') {
                keys.insert(key.to_string());
            }
        }
    }
    keys
}

#[test]
fn every_translation_key_the_ui_uses_exists_in_english() {
    // English is the canonical key set; the loader tests check that every
    // other pack matches it. This checks the other side: that the interface
    // never asks for a key nobody wrote.
    let english = english_keys();
    let mut missing: Vec<(String, String)> = Vec::new();

    for file in [
        "viewer.html",
        "admin.html",
        "viewer.js",
        "admin.js",
        "common.js",
    ] {
        for key in referenced_keys(&read(file)) {
            // A key used with `{ count }` is looked up as `key_one`,
            // `key_other` and so on, so accept either spelling.
            let pluralised = english.contains(&format!("{key}_other"));
            if !english.contains(&key) && !pluralised {
                missing.push((file.to_string(), key));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the UI asks for keys English does not define: {missing:?}"
    );
}

#[test]
fn the_interface_has_no_english_left_hard_coded_in_its_markup() {
    // Anything a person reads must come from a pack. A bare sentence in the
    // HTML would show in English whatever language was chosen.
    for page in ["viewer.html", "admin.html"] {
        let html = read(page);
        for (index, line) in html.lines().enumerate() {
            let trimmed = line.trim();
            // Text between tags, on a line that does not carry a key.
            let Some(start) = trimmed.find('>') else {
                continue;
            };
            let after = &trimmed[start + 1..];
            let Some(end) = after.find('<') else { continue };
            let text = after[..end].trim();
            if text.len() < 4 || !text.chars().any(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            assert!(
                line.contains("data-i18n"),
                "{page}:{} has untranslated text {text:?}",
                index + 1
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_serves_every_shipped_language() {
    let node = spawn_node("locales").await;
    let base = node.running.api_url().unwrap();
    let http = reqwest::Client::new();

    // Unauthenticated on purpose: the UI needs its strings before it can
    // render even an error about not being authorised.
    let catalogue: serde_json::Value = http
        .get(format!("{base}/v1/locales"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listing = catalogue["locales"].as_array().unwrap().clone();

    let codes: Vec<&str> = listing
        .iter()
        .map(|row| row["locale"].as_str().unwrap())
        .collect();
    for expected in ["en", "ja", "es", "pt", "ar"] {
        assert!(
            codes.contains(&expected),
            "{expected} should be served, got {codes:?}"
        );
    }
    for row in &listing {
        assert_eq!(row["coverage"].as_f64(), Some(1.0), "{row}");
        assert_eq!(row["source"], "built-in");
        assert!(!row["name"].as_str().unwrap().is_empty());
        // Every shipped pack says which countries it serves, which is what
        // makes the time-zone guess possible.
        assert!(
            !row["regions"].as_array().unwrap().is_empty(),
            "{} claims no regions",
            row["locale"]
        );
    }
    let japanese = listing.iter().find(|r| r["locale"] == "ja").unwrap();
    assert_eq!(japanese["regions"], serde_json::json!(["JP"]));

    // Each pack comes back complete, and Arabic is flagged right to left.
    let english_count = english_keys().len();
    for code in ["en", "ja", "ar"] {
        let pack: serde_json::Value = http
            .get(format!("{base}/v1/locales/{code}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(pack["locale"], code);
        let strings = pack["strings"].as_object().unwrap();
        assert!(strings.len() >= english_count, "{code} is missing keys");
        assert_eq!(
            pack["direction"],
            if code == "ar" { "rtl" } else { "ltr" },
            "{code}"
        );
    }

    let unknown = http
        .get(format!("{base}/v1/locales/xx"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_language_pack_dropped_into_the_data_directory_is_served() {
    // The whole point of the plugin layout: add a language without touching
    // the build.
    let node = spawn_node("locales").await;
    let dir = node.node().config().locales_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("de.json"),
        serde_json::json!({
            "locale": "de",
            "name": "Deutsch",
            "englishName": "German",
            "direction": "ltr",
            "formatVersion": 1,
            "strings": { "nav.browse": "Durchsuchen" }
        })
        .to_string(),
    )
    .unwrap();

    let base = node.running.api_url().unwrap();
    let http = reqwest::Client::new();

    let catalogue: serde_json::Value = http
        .get(format!("{base}/v1/locales"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listing = catalogue["locales"].as_array().unwrap().clone();
    let german = listing
        .iter()
        .find(|row| row["locale"] == "de")
        .expect("the dropped pack should be listed");
    assert_eq!(german["source"], "installed");
    assert_eq!(german["name"], "Deutsch");
    // One key of two hundred: reported honestly rather than rounded up.
    let coverage = german["coverage"].as_f64().unwrap();
    assert!(coverage > 0.0 && coverage < 0.1, "coverage was {coverage}");

    let pack: serde_json::Value = http
        .get(format!("{base}/v1/locales/de"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pack["strings"]["nav.browse"], "Durchsuchen");
    // Everything it does not cover falls back to English, so the page is
    // never left with a bare key on screen.
    assert_eq!(pack["strings"]["nav.library"], "Library");

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_offers_a_default_language_without_looking_anything_up() {
    // Nothing here contacts anything: `configured` is what the operator set,
    // `suggested` is what this machine's own settings say. A GeoIP lookup
    // would mean telling a third party where the user is and that they are
    // running this, which Principle 1 rules out.
    let dir = tempfile::tempdir().unwrap();
    let mut config = ovn_node::NodeConfig::new(dir.path())
        .with_p2p_port(0)
        .with_api_port(0);
    config.network.enable_mdns = false;
    config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
    config.default_locale = Some("pt-BR".into());
    let running = ovn_node::start(config).await.unwrap();

    let base = running.api_url().unwrap();
    let catalogue: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/v1/locales"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // `pt-BR` has no pack of its own, so it resolves to the Portuguese one.
    assert_eq!(catalogue["configured"], "pt");
    // The suggestion comes from the machine and may be anything, or nothing.
    assert!(catalogue["suggested"].is_string() || catalogue["suggested"].is_null());

    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_with_no_language_configured_says_so() {
    let node = spawn_node("locales").await;
    let base = node.running.api_url().unwrap();
    let catalogue: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/v1/locales"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(catalogue["configured"].is_null());
    node.shutdown().await;
}

#[test]
fn the_time_zone_table_covers_the_regions_the_packs_claim() {
    // A pack claiming a country nothing maps to would never be chosen by
    // region, which is a silent failure rather than a loud one.
    let zones = read("zones.js");
    let mut mapped: BTreeSet<String> = BTreeSet::new();
    for line in zones.lines() {
        if let Some((_, country)) = line.rsplit_once(": '") {
            mapped.insert(country.trim_end_matches("',").to_string());
        }
    }

    for code in ["en", "ja", "es", "pt", "ar"] {
        let pack: serde_json::Value =
            serde_json::from_str(&read(&format!("locales/{code}.json"))).unwrap();
        for region in pack["regions"].as_array().unwrap() {
            let region = region.as_str().unwrap();
            assert!(
                mapped.contains(region),
                "{code} claims {region}, which no time zone maps to"
            );
        }
    }
}

#[test]
fn placeholders_are_substituted_by_kind() {
    assert_eq!(
        substitute("/v1/videos/${cid}/stream", "CID", "KEY"),
        "/v1/videos/CID/stream"
    );
    assert_eq!(
        substitute("/v1/follow/${video.creator}", "CID", "KEY"),
        "/v1/follow/KEY"
    );
    assert_eq!(substitute("/v1/status", "CID", "KEY"), "/v1/status");
}

// ------------------------------------------------------------ accessibility

/// Custom properties declared in a `:root`-like block, so the palette can be
/// checked rather than eyeballed.
///
/// The stylesheet declares the light theme on `:root` and overrides it inside
/// a `prefers-color-scheme: dark` block; both have to stand on their own.
fn palette(css: &str, dark: bool) -> BTreeMap<String, String> {
    let region = if dark {
        let at = css
            .find("@media (prefers-color-scheme: dark)")
            .expect("a dark theme block");
        &css[at..]
    } else {
        let end = css
            .find("@media (prefers-color-scheme: dark)")
            .unwrap_or(css.len());
        &css[..end]
    };
    let mut found = BTreeMap::new();
    for line in region.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("--") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim();
        if value.starts_with('#') && (value.len() == 7 || value.len() == 4) {
            found.insert(name.trim().to_string(), value.to_string());
        }
    }
    found
}

/// WCAG relative luminance of an `#rrggbb` colour.
fn luminance(hex: &str) -> f64 {
    let channel = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let bytes = hex.trim_start_matches('#');
    let part = |i: usize| u8::from_str_radix(&bytes[i..i + 2], 16).expect("two hex digits");
    0.2126 * channel(part(0)) + 0.7152 * channel(part(2)) + 0.0722 * channel(part(4))
}

/// WCAG contrast ratio between two colours, 1.0 to 21.0.
fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[test]
fn the_palette_is_readable_in_both_themes() {
    // Contrast is the one accessibility property that can be checked exactly,
    // so there is no excuse for deciding it by eye. Text needs 4.5:1 (WCAG
    // 1.4.3 AA) against every surface it is placed on; the border of anything
    // you can operate needs 3:1 (1.4.11).
    let css = read("app.css");
    let text_colours = [
        "text",
        "text-dim",
        "text-faint",
        "accent",
        "danger",
        "ok",
        "warn",
    ];
    let surfaces = ["bg", "surface", "surface-2"];

    for dark in [false, true] {
        let theme = if dark { "dark" } else { "light" };
        let colours = palette(&css, dark);
        for name in text_colours {
            let fg = colours
                .get(name)
                .unwrap_or_else(|| panic!("the {theme} theme declares no --{name}"));
            for surface in surfaces {
                let bg = colours
                    .get(surface)
                    .unwrap_or_else(|| panic!("the {theme} theme declares no --{surface}"));
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= 4.5,
                    "{theme}: --{name} ({fg}) on --{surface} ({bg}) is {ratio:.2}:1, \
                     under the 4.5:1 that text needs"
                );
            }
        }
        let border = colours
            .get("border-strong")
            .unwrap_or_else(|| panic!("the {theme} theme declares no --border-strong"));
        for surface in surfaces {
            let bg = &colours[surface];
            let ratio = contrast(border, bg);
            assert!(
                ratio >= 3.0,
                "{theme}: --border-strong ({border}) on --{surface} ({bg}) is {ratio:.2}:1, \
                 under the 3:1 that the edge of a control needs"
            );
        }
        // And the label on the accent button against the accent itself.
        let ratio = contrast(&colours["accent-text"], &colours["accent"]);
        assert!(
            ratio >= 4.5,
            "{theme}: --accent-text on --accent is {ratio:.2}:1"
        );
    }
}

#[test]
fn a_keyboard_can_get_past_the_header() {
    // Both pages put a dozen controls in the header. Without a skip link,
    // reaching the content means tabbing through all of them on every page.
    for page in ["viewer.html", "admin.html"] {
        let html = read(page);
        assert!(
            html.contains("class=\"skip-link\"") && html.contains("href=\"#main\""),
            "{page} has no skip link"
        );
        assert!(
            html.contains("<main id=\"main\" tabindex=\"-1\">"),
            "{page} has no focusable main landmark for a skip link to reach"
        );
        // The link must come before the header, or it is not first in the tab
        // order and does not help.
        let skip = html.find("skip-link").expect("a skip link");
        let header = html.find("<header").expect("a header");
        assert!(
            skip < header,
            "{page} puts its skip link after the header it exists to skip"
        );
    }
}

#[test]
fn everything_that_changes_by_itself_announces_that_it_changed() {
    // Progress, errors and the connection state all change without anybody
    // touching the page. Unannounced, they are invisible to a screen reader.
    for page in ["viewer.html", "admin.html"] {
        let html = read(page);
        for (id, what) in [
            ("toasts", "progress and errors"),
            ("connection", "the connection state"),
        ] {
            let at = html
                .find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("{page} has no #{id}"));
            // Look at the element's own tag, not the rest of the document.
            let tag_end = html[at..].find('>').expect("a closing angle bracket") + at;
            let tag = &html[at..tag_end];
            assert!(
                tag.contains("aria-live"),
                "{page}: #{id} carries {what} but is not a live region: {tag}"
            );
        }
    }
}

#[test]
fn a_landmark_that_appears_twice_has_a_name() {
    // A screen reader lists the landmarks. Two pages each with a <nav> and no
    // name gives the reader two entries called "navigation".
    for page in ["viewer.html", "admin.html"] {
        let html = read(page);
        let at = html
            .find("<nav")
            .unwrap_or_else(|| panic!("{page} has no nav"));
        let tag_end = html[at..].find('>').expect("a closing angle bracket") + at;
        let tag = &html[at..tag_end];
        assert!(
            tag.contains("data-i18n-label") || tag.contains("aria-label"),
            "{page}: the nav has no accessible name: {tag}"
        );
    }
    // The player is a control with no visible label of its own.
    let viewer = read("viewer.html");
    let at = viewer.find("<video").expect("a video element");
    let tag_end = viewer[at..].find('>').expect("a closing angle bracket") + at;
    assert!(
        viewer[at..tag_end].contains("data-i18n-label"),
        "the player has no accessible name"
    );
}

#[test]
fn focus_is_always_visible_somewhere() {
    let css = read("app.css");
    assert!(
        css.contains(":focus-visible {\n  outline: 2px solid var(--accent);"),
        "there is no shared focus ring for operable elements"
    );
    // The search input suppresses its own outline, so the ring has to be on
    // the wrapper the viewer actually sees.
    let at = css
        .find(".searchbar:focus-within {")
        .expect("a focus style for the search bar");
    let block_end = css[at..].find('}').expect("a closing brace") + at;
    assert!(
        css[at..block_end].contains("outline:"),
        "the search bar hides the input's outline and puts nothing in its place"
    );
}

#[test]
fn motion_can_be_turned_down() {
    let css = read("app.css");
    assert!(
        css.contains("@media (prefers-reduced-motion: reduce)"),
        "the stylesheet animates and transitions but never asks whether that is wanted"
    );
}

#[test]
fn the_accessibility_strings_are_translated_everywhere() {
    // A skip link that says "Skip to content" in the middle of an Arabic
    // interface is worse than none.
    let keys = [
        "a11y.skipToContent",
        "a11y.player",
        "nav.viewer.label",
        "nav.admin.label",
    ];
    for code in ["en", "ja", "es", "pt", "ar"] {
        let pack: serde_json::Value =
            serde_json::from_str(&read(&format!("locales/{code}.json"))).expect("valid JSON");
        let strings = pack["strings"].as_object().expect("a strings object");
        for key in keys {
            let value = strings
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("{code}.json is missing {key}"));
            assert!(!value.trim().is_empty(), "{code}.json has {key} empty");
        }
    }
}

#[test]
fn tab_labels_cannot_be_squeezed_into_vertical_text() {
    // A flex item may shrink to its min-content width. In Japanese, Chinese
    // and Korean a line may break between any two characters, so min-content
    // is one character and the label renders one character per line. Latin
    // text hides this, because its min-content is the longest word.
    let css = read("app.css");
    let at = css
        .find(".nav button {")
        .expect("the nav buttons are styled");
    let block_end = css[at..].find('}').expect("a closing brace") + at;
    let block = &css[at..block_end];
    assert!(
        block.contains("white-space: nowrap"),
        "a tab label must not be allowed to wrap: {block}"
    );
    assert!(
        block.contains("flex: 0 0 auto"),
        "a tab must not shrink below its text: {block}"
    );

    // And when they genuinely do not fit, the row scrolls rather than
    // crushing them.
    let at = css.find(".nav {").expect("the nav is styled");
    let block_end = css[at..].find('}').expect("a closing brace") + at;
    assert!(
        css[at..block_end].contains("overflow-x: auto"),
        "the tab row should scroll when it overflows"
    );
}
