//! Test G: nothing about what you watch leaves the device.
//!
//! Section 32 asks for something stronger than a setting: no send mechanism
//! at all. These tests check that claim three ways.
//!
//! 1. **Type level** — the local-only types do not implement `Serialize`, so
//!    no encoder can accept them.
//! 2. **Dependency level** — the crates that can talk to the network do not
//!    depend on the crates that hold viewing data, so they could not name
//!    those types even if they wanted to.
//! 3. **Behavioural** — with two real nodes connected, one watches a lot and
//!    the other learns nothing.

mod support;

use std::marker::PhantomData;
use std::path::Path;
use std::time::Duration;

use support::*;

use ovn_database::{TagWeight, WatchEvent, WatchRecord, WatchSummary};
use ovn_protocol::{ContentId, NodeDescriptor, VideoAnnouncement};
use ovn_recommendation::{Contribution, PreferenceModel, Recommendation};

// ------------------------------------------------------------ 1. type level

/// Detects whether `T` implements `Serialize`, without a negative bound.
///
/// The inherent method wins whenever its `T: Serialize` bound is satisfied;
/// otherwise the blanket trait method applies. So `is_serialize()` reports
/// what is actually implemented, at compile time, as a value we can assert on.
struct Probe<T>(PhantomData<T>);

trait MaybeSerialize {
    fn is_serialize(&self) -> bool {
        false
    }
}
impl<T> MaybeSerialize for Probe<T> {}
impl<T: serde::Serialize> Probe<T> {
    fn is_serialize(&self) -> bool {
        true
    }
}

/// A macro rather than a generic function: method resolution has to happen
/// where the concrete type is known, so that the inherent `Serialize` impl is
/// a candidate at all.
macro_rules! is_serializable {
    ($t:ty) => {{
        #[allow(unused_imports)]
        use crate::MaybeSerialize as _;
        Probe::<$t>(PhantomData).is_serialize()
    }};
}

#[test]
fn viewing_data_has_no_serialisation_at_all() {
    // If any of these ever gain a `Serialize` derive, this test fails and the
    // guarantee in section 32 has quietly been given up.
    assert!(!is_serializable!(WatchEvent), "WatchEvent");
    assert!(!is_serializable!(WatchRecord), "WatchRecord");
    assert!(!is_serializable!(WatchSummary), "WatchSummary");
    assert!(!is_serializable!(TagWeight), "TagWeight");
    assert!(!is_serializable!(PreferenceModel), "PreferenceModel");
    assert!(!is_serializable!(Recommendation), "Recommendation");
    assert!(!is_serializable!(Contribution), "Contribution");
}

#[test]
fn the_probe_itself_works() {
    // A control: things that are meant to be serialisable still are, so a
    // passing test above means something.
    assert!(is_serializable!(VideoAnnouncement));
    assert!(is_serializable!(NodeDescriptor));
    assert!(is_serializable!(ContentId));
    assert!(is_serializable!(String));
}

// ------------------------------------------------------ 2. dependency level

fn manifest(crate_name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let path = root.join("crates").join(crate_name).join("Cargo.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn the_network_facing_crates_cannot_even_name_viewing_data() {
    // `ovn-network` and `ovn-protocol` are everything that touches the wire.
    // Neither depends on the crates that define viewing data, so no amount of
    // refactoring inside them can reach it.
    for crate_name in ["network", "protocol"] {
        let manifest = manifest(crate_name);
        for forbidden in ["ovn-database", "ovn-recommendation", "ovn-storage"] {
            assert!(
                !manifest.contains(forbidden),
                "ovn-{crate_name} must not depend on {forbidden}"
            );
        }
    }
}

#[test]
fn the_recommendation_crate_does_not_depend_on_the_network() {
    let manifest = manifest("recommendation");
    for forbidden in ["ovn-network", "ovn-discovery", "reqwest", "libp2p"] {
        assert!(
            !manifest.contains(forbidden),
            "ovn-recommendation must not depend on {forbidden}"
        );
    }
}

// --------------------------------------------------------- 3. behavioural

#[tokio::test(flavor = "multi_thread")]
async fn watching_intensively_tells_the_other_peer_nothing() {
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    // Publish a few videos so there is something to watch.
    let mut cids = Vec::new();
    for (name, tag) in [("game", "gaming"), ("song", "music"), ("talk", "lecture")] {
        let source = write_sample_file(publisher.dir.path(), &format!("{name}.mp4"), 8 * 1024);
        let report = loop {
            let report = publisher
                .node()
                .publish_video(
                    &source,
                    Some(name.to_string()),
                    String::new(),
                    vec![tag.to_string()],
                )
                .await
                .unwrap();
            if report.announced_to_network {
                break report;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        cids.push(ContentId::parse(&report.video.cid).unwrap());
    }

    let viewer_node = viewer.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        viewer_node.database().video_count().unwrap_or(0) == 3
    })
    .await
    .expect("announcements should arrive");

    // The viewer watches, likes and skips — the full range of signals.
    for (index, cid) in cids.iter().enumerate() {
        viewer
            .node()
            .record_watch(&WatchEvent {
                cid: *cid,
                watched_secs: 300 + index as u32,
                duration_secs: 600,
                completed: index == 0,
                skipped: index == 2,
                liked: index == 0,
            })
            .unwrap();
    }

    // The model exists, locally.
    let model = viewer.node().preference_model().unwrap();
    assert!(
        !model.is_empty(),
        "the viewer should have learned something"
    );
    assert!(!viewer.node().recommendations(10).unwrap().is_empty());
    assert_eq!(
        viewer
            .node()
            .database()
            .watch_summary()
            .unwrap()
            .event_count,
        3
    );

    // Give anything that might be sent plenty of time to arrive.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The publisher knows nothing about any of it.
    let publisher_db = publisher.node().database();
    assert_eq!(
        publisher_db.watch_summary().unwrap().event_count,
        0,
        "the publisher must have no viewing data"
    );
    assert!(
        publisher_db.tag_weights().unwrap().is_empty(),
        "the publisher must have no preference vector"
    );

    // Nor is there any field about it in what the publisher stores. Checking
    // field names rather than raw text keeps the test meaningful: a base32
    // content id can contain almost any letter sequence by chance.
    let mut fields = std::collections::BTreeSet::new();
    collect_keys(
        &serde_json::to_value(publisher_db.peers().unwrap()).unwrap(),
        &mut fields,
    );
    collect_keys(
        &serde_json::to_value(publisher_db.videos(100, 0).unwrap()).unwrap(),
        &mut fields,
    );
    assert!(!fields.is_empty(), "the publisher should know something");

    // An allowlist rather than a denylist: anything new that appears in what
    // a peer stores has to be justified here, deliberately, by a person.
    // (A substring search would be useless anyway — "durationSecs" contains
    // "ratio", and a base32 content id can contain any letters at all.)
    let permitted: std::collections::BTreeSet<&str> = [
        // PeerRecord
        "peerId",
        "publicKey",
        "nodeName",
        "addresses",
        "source",
        "firstSeen",
        "lastSeen",
        "lastConnected",
        "failedAttempts",
        // VideoRecord
        "cid",
        "creator",
        "title",
        "description",
        "tags",
        "durationSecs",
        "thumbnailCid",
        "createdAt",
        "discoveredAt",
        "isLocal",
        "haveManifest",
        "haveContent",
    ]
    .into_iter()
    .collect();

    let unexpected: Vec<&String> = fields
        .iter()
        .filter(|f| !permitted.contains(f.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "a peer is storing fields nobody signed off on: {unexpected:?}"
    );

    publisher.shutdown().await;
    viewer.shutdown().await;
}

/// Every object key in a JSON value, however deeply nested.
fn collect_keys(value: &serde_json::Value, into: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                into.insert(key.clone());
                collect_keys(nested, into);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_keys(item, into);
            }
        }
        _ => {}
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_api_that_can_read_history_is_not_reachable_without_the_token() {
    let node = spawn_node("private").await;
    let base = node.running.api_url().expect("the API is running");

    // Loopback only.
    assert!(node.running.api_addr().expect("bound").ip().is_loopback());

    let http = reqwest::Client::new();

    // Viewing history needs the token.
    let unauthorised = http.get(format!("{base}/v1/watch")).send().await.unwrap();
    assert_eq!(unauthorised.status(), reqwest::StatusCode::UNAUTHORIZED);

    let with_wrong_token = http
        .get(format!("{base}/v1/watch"))
        .bearer_auth("0".repeat(64))
        .send()
        .await
        .unwrap();
    assert_eq!(with_wrong_token.status(), reqwest::StatusCode::UNAUTHORIZED);

    let authorised = http
        .get(format!("{base}/v1/watch"))
        .bearer_auth(node.node().api_token())
        .send()
        .await
        .unwrap();
    assert!(authorised.status().is_success());

    // The public descriptor is deliberately open: it is how people join.
    let descriptor = http
        .get(format!(
            "{base}{}",
            ovn_protocol::WELL_KNOWN_DESCRIPTOR_PATH
        ))
        .send()
        .await
        .unwrap();
    assert!(descriptor.status().is_success());
    let body: serde_json::Value = descriptor.json().await.unwrap();
    assert!(body.get("peerId").is_some());
    // …and carries nothing personal.
    let text = body.to_string().to_lowercase();
    for leak in ["watch", "preference", "history", "recommend"] {
        assert!(!text.contains(leak), "{leak:?} in the public descriptor");
    }

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn erasing_local_history_actually_erases_it() {
    let node = spawn_node("forgetful").await;
    let source = write_sample_file(node.dir.path(), "v.mp4", 4096);
    let report = node
        .node()
        .publish_video(&source, Some("v".into()), String::new(), vec!["tag".into()])
        .await
        .unwrap();
    let cid = ContentId::parse(&report.video.cid).unwrap();

    node.node()
        .record_watch(&WatchEvent {
            cid,
            watched_secs: 60,
            duration_secs: 60,
            completed: true,
            skipped: false,
            liked: true,
        })
        .unwrap();
    assert_eq!(
        node.node().database().watch_summary().unwrap().event_count,
        1
    );
    assert!(!node.node().preference_model().unwrap().is_empty());

    node.node().clear_local_history().unwrap();

    assert_eq!(
        node.node().database().watch_summary().unwrap().event_count,
        0
    );
    assert!(node.node().preference_model().unwrap().is_empty());
    // The video itself is untouched: erasing history is not erasing content.
    assert!(node.node().video(&cid).unwrap().is_some());

    node.shutdown().await;
}
