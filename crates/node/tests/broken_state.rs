//! What happens when the machine underneath lets us down.
//!
//! A node runs on someone's desktop. The power goes out mid-write, a disk
//! develops a bad sector, a backup tool copies half a file back, somebody
//! deletes a directory to free space. None of that should produce a crash, a
//! silent wrong answer, or bytes served to a peer that do not match what was
//! asked for.
//!
//! Each test breaks something on purpose and then asks what the node says
//! about it.

mod support;

use std::path::Path;

use ovn_node::doctor::{self, DoctorOptions, Severity};
use ovn_node::{start, NodeConfig};
use ovn_protocol::ContentId;
use support::*;

/// Find the check named `name`, or fail saying which checks did exist.
fn check<'a>(report: &'a doctor::Report, name: &str) -> &'a doctor::Check {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| {
            let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
            panic!("no check called {name:?}; there were {names:?}")
        })
}

/// A doctor run that does not test any ports, so a test never depends on
/// whether some unrelated program holds 4800.
async fn diagnose(dir: &Path) -> doctor::Report {
    // Port 0 is always bindable, so the port checks pass without saying
    // anything about the machine this runs on.
    doctor::run(DoctorOptions::new(dir, 0, 0)).await
}

/// Overwrite the file holding `cid` with something that does not hash to it.
fn corrupt_block(blocks_dir: &Path, cid: &ContentId) {
    let path = block_path(blocks_dir, cid);
    let original = std::fs::read(&path).expect("read the block we are about to break");
    let mut broken = original.clone();
    // Flip one bit in the middle. A length change would also be caught, but
    // by the size check rather than by the hash, which is the weaker test.
    let middle = broken.len() / 2;
    broken[middle] ^= 0b1000_0000;
    std::fs::write(&path, &broken).expect("write the corrupted block");
    assert_ne!(original, broken);
}

/// Where the block store files a block. Mirrors `BlockStore::path_for`, which
/// shards on the first two characters of the id.
fn block_path(blocks_dir: &Path, cid: &ContentId) -> std::path::PathBuf {
    let id = cid.to_string();
    let found = walk(blocks_dir)
        .into_iter()
        .find(|p| p.file_name().map(|n| n == id.as_str()).unwrap_or(false));
    found.unwrap_or_else(|| panic!("no file named {id} under {}", blocks_dir.display()))
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test]
async fn a_corrupt_block_is_dropped_rather_than_read_back() {
    // The blocks a node holds are manifests and thumbnails — it does not hold
    // video. A manifest is what makes a creator's file checkable, so a damaged
    // one must not be read back and must not be served on.
    let node = spawn_node("keeper").await;
    let file = write_sample_file(node.dir.path(), "clip.bin", 300_000);
    let cid = publish_until_announced(node.node(), &file, "Clip", &["test"]).await;

    let blocks = node.dir.path().join("blocks");
    assert!(
        block_path_exists(&blocks, &cid),
        "the manifest should be held"
    );
    corrupt_block(&blocks, &cid);

    // Reading it must not hand back the bytes that are there.
    let err = node.node().manifest(&cid).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains(&cid.to_string()) || message.contains("integrity"),
        "the error should name the block that failed: {message}"
    );
    // And the file must be gone, so the node does not try to serve it.
    assert!(
        !block_path_exists(&blocks, &cid),
        "a block that failed its hash should have been deleted"
    );

    node.shutdown().await;
}

fn block_path_exists(blocks_dir: &Path, cid: &ContentId) -> bool {
    let id = cid.to_string();
    walk(blocks_dir)
        .iter()
        .any(|p| p.file_name().map(|n| n == id.as_str()).unwrap_or(false))
}

#[tokio::test]
async fn dropping_a_corrupt_block_also_drops_its_place_in_the_cache() {
    // The bookkeeping has to follow the file. Otherwise the cache keeps
    // charging for bytes that are gone, and the node keeps counting itself a
    // provider of a manifest it can no longer hand over.
    let node = spawn_node("accountant").await;
    let file = write_sample_file(node.dir.path(), "clip.bin", 300_000);
    let cid = publish_until_announced(node.node(), &file, "Clip", &["test"]).await;

    let before = node.node().status().await.unwrap().cache;
    corrupt_block(&node.dir.path().join("blocks"), &cid);
    let _ = node.node().manifest(&cid);

    let after = node.node().status().await.unwrap().cache;
    assert_eq!(
        after.block_count,
        before.block_count - 1,
        "the corrupt block should no longer be counted"
    );
    assert!(
        after.total_bytes < before.total_bytes,
        "cache usage should have gone down: {} then {}",
        before.total_bytes,
        after.total_bytes
    );

    node.shutdown().await;
}

#[tokio::test]
async fn a_peer_never_receives_a_block_that_does_not_match_its_id() {
    // The strongest promise the block layer makes: whatever happens to the
    // disk on the serving side, nothing wrong arrives on the other. The blocks
    // in question are manifests now, which makes it matter more rather than
    // less — a manifest is what tells a viewer whether the creator's file is
    // the file that was announced.
    let server = spawn_node("server").await;
    let client = spawn_node("client").await;
    join_via_share_link(&client, &server).await;

    let file = write_sample_file(server.dir.path(), "clip.bin", 300_000);
    let cid = publish_until_announced(server.node(), &file, "Clip", &["test"]).await;

    wait_until(PROPAGATION_TIMEOUT, || {
        client.node().video(&cid).ok().flatten().is_some()
    })
    .await
    .expect("the announcement reaches the client");

    corrupt_block(&server.dir.path().join("blocks"), &cid);

    // Asking for it must fail rather than produce a manifest that was not
    // what the creator signed.
    let outcome = client.node().prepare_stream(cid).await;
    assert!(
        outcome.is_err(),
        "a corrupt manifest must not be handed over as a good one"
    );
    assert!(
        !block_path_exists(&client.dir.path().join("blocks"), &cid),
        "the client stored a block it should have rejected"
    );

    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test]
async fn deleting_the_block_directory_by_hand_does_not_stop_the_node() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = || {
        let mut c = NodeConfig::new(dir.path())
            .with_p2p_port(0)
            .with_api_port(0);
        c.network.enable_mdns = false;
        c.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
        c
    };

    let running = start(config()).await.expect("first start");
    let file = write_sample_file(dir.path(), "clip.bin", 300_000);
    running
        .node()
        .publish_video(
            &file,
            Some("Clip".into()),
            String::new(),
            vec!["test".into()],
            "https://videos.example/clip.mp4".to_string(),
        )
        .await
        .expect("publish");
    let counted = running.node().status().await.unwrap().cache.block_count;
    assert!(counted > 0);
    running.shutdown().await;

    // Somebody frees up space the crude way.
    std::fs::remove_dir_all(dir.path().join("blocks")).expect("remove the block store");

    let running = start(config())
        .await
        .expect("the node starts with no blocks");
    let after = running.node().status().await.unwrap().cache;
    assert_eq!(
        after.block_count, 0,
        "startup should have reconciled the cache down to what is on disk"
    );
    assert_eq!(after.total_bytes, 0);
    running.shutdown().await;
}

#[tokio::test]
async fn a_damaged_database_is_reported_rather_than_crashing_the_diagnosis() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = NodeConfig::new(dir.path());
    std::fs::create_dir_all(dir.path()).unwrap();
    // A file that is the right name and the wrong thing entirely. This is
    // what a truncated restore or a half-finished copy looks like.
    std::fs::write(config.database_path(), b"this is not a database").unwrap();

    let report = diagnose(dir.path()).await;
    let db = check(&report, "database");
    assert_eq!(db.severity, Severity::Problem, "{db:?}");
    assert!(
        db.remedy.as_deref().unwrap_or_default().contains("aside"),
        "the remedy should say what to do with the file: {db:?}"
    );
    assert!(!report.is_healthy());
}

#[tokio::test]
async fn the_diagnosis_finds_a_corrupt_block() {
    let node = spawn_node("patient").await;
    let file = write_sample_file(node.dir.path(), "clip.bin", 300_000);
    let report = node
        .node()
        .publish_video(
            &file,
            Some("Clip".into()),
            String::new(),
            vec!["test".into()],
            "https://videos.example/clip.mp4".to_string(),
        )
        .await
        .expect("publish");
    let cid: ContentId = report.video.cid.parse().unwrap();

    let healthy = diagnose(node.dir.path()).await;
    assert_eq!(
        check(&healthy, "block integrity").severity,
        Severity::Ok,
        "a healthy store should pass: {:?}",
        check(&healthy, "block integrity")
    );

    corrupt_block(&node.dir.path().join("blocks"), &cid);

    let sick = diagnose(node.dir.path()).await;
    let integrity = check(&sick, "block integrity");
    assert_eq!(integrity.severity, Severity::Problem, "{integrity:?}");
    assert!(
        integrity.detail.contains(&cid.to_string()),
        "the finding should name a block: {integrity:?}"
    );
    assert!(!sick.is_healthy());

    node.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_diagnosis_notices_an_identity_key_other_accounts_can_read() {
    use std::os::unix::fs::PermissionsExt;

    let node = spawn_node("exposed").await;
    let key = NodeConfig::new(node.dir.path()).identity_path();
    assert_eq!(
        check(&diagnose(node.dir.path()).await, "identity key").severity,
        Severity::Ok
    );

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

    let report = diagnose(node.dir.path()).await;
    let finding = check(&report, "identity key");
    assert_eq!(finding.severity, Severity::Problem, "{finding:?}");
    assert!(
        finding
            .remedy
            .as_deref()
            .unwrap_or_default()
            .contains("chmod 600"),
        "the remedy should be the command to run: {finding:?}"
    );

    node.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_diagnosis_says_so_when_it_cannot_write_to_the_data_directory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("temp dir");
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

    let report = diagnose(&locked).await;
    let finding = check(&report, "data directory");
    assert_eq!(finding.severity, Severity::Problem, "{finding:?}");
    assert!(!report.is_healthy());

    // Leave it removable.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn the_diagnosis_does_not_trust_a_record_of_a_node_that_is_gone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = NodeConfig::new(dir.path());
    std::fs::create_dir_all(dir.path()).unwrap();
    // What is left behind when a node is killed rather than stopped.
    std::fs::write(
        config.runtime_path(),
        serde_json::to_vec(&serde_json::json!({
            "pid": 999_999,
            "apiUrl": "http://127.0.0.1:1",
            "apiToken": "stale",
            "peerId": "12D3KooWnobody",
            "startedAt": 0,
        }))
        .unwrap(),
    )
    .unwrap();

    let report = diagnose(dir.path()).await;
    assert!(!report.node_running);
    let finding = check(&report, "running node");
    assert_eq!(finding.severity, Severity::Warning, "{finding:?}");
    assert!(finding.detail.contains("not answering"), "{finding:?}");
    // A stale record is not a reason to call the installation broken.
    assert!(report.is_healthy());
}

#[tokio::test]
async fn a_healthy_node_passes_its_own_diagnosis() {
    let node = spawn_node("healthy").await;
    let file = write_sample_file(node.dir.path(), "clip.bin", 300_000);
    node.node()
        .publish_video(
            &file,
            Some("Clip".into()),
            String::new(),
            vec!["test".into()],
            "https://videos.example/clip.mp4".to_string(),
        )
        .await
        .expect("publish");

    let report = diagnose(node.dir.path()).await;
    let failures: Vec<&doctor::Check> = report
        .checks
        .iter()
        .filter(|c| c.severity == Severity::Problem)
        .collect();
    assert!(
        failures.is_empty(),
        "a node that just started and published should have nothing wrong: {failures:?}"
    );

    node.shutdown().await;
}
