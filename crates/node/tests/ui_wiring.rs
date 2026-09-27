//! The UI is plain files rather than a compiled bundle, so nothing checks it
//! at build time. These tests stand in for that: every element the scripts
//! reach for must exist in the page, and every API path they call must be a
//! route the node actually serves.

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;

use support::*;

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ui")
}

fn read(name: &str) -> String {
    let path = ui_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
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
        .publish_video(&source, Some("Wiring".into()), String::new(), vec![])
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
