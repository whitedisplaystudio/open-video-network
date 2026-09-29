//! A network left running, doing the same things over and over.
//!
//! Every other test here finishes in seconds. Some failures only appear
//! after a node has been up for a while: memory that grows without bound, a
//! map that is added to and never pruned, a periodic timer that eventually
//! races something. This test exists to find those, and is ignored by
//! default because it is measured in minutes.
//!
//! ```text
//! OVN_SOAK_SECONDS=1800 cargo test -p ovn-node --test soak --release -- --ignored --nocapture
//! ```

mod support;

use std::time::{Duration, Instant};

use support::*;

use ovn_database::WatchEvent;
use ovn_protocol::ContentId;

fn soak_seconds() -> u64 {
    std::env::var("OVN_SOAK_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
}

/// Resident memory of this process, in bytes.
///
/// Crude, and the allocator does not return everything promptly, but a leak
/// large enough to matter shows up as a trend regardless.
fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs for minutes; see the module comment"]
async fn a_network_left_running_stays_healthy() {
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node("viewer").await;
    let bystander = spawn_node("bystander").await;
    join_via_share_link(&viewer, &publisher).await;
    join_via_share_link(&bystander, &publisher).await;

    let deadline = Instant::now() + Duration::from_secs(soak_seconds());
    let baseline = resident_bytes();
    let mut round = 0u32;
    let mut published = 0u32;
    let mut arrived = 0u32;
    let mut fetched = 0u32;

    while Instant::now() < deadline {
        round += 1;

        // A new video every round, so the database, the block store and the
        // gossip mesh all keep growing.
        // Unique content every round, or the block store deduplicates it all
        // and the cache never grows.
        let source = write_seeded_file(
            publisher.dir.path(),
            &format!("clip-{round}.mp4"),
            256 * 1024 + (round as usize % 7) * 1024,
            round as u64,
        );
        let report = publisher
            .node()
            .publish_video(
                &source,
                Some(format!("Round {round}")),
                "A soak test clip.".into(),
                vec![if round % 2 == 0 { "gaming" } else { "music" }.into()],
            )
            .await
            .expect("publishing should keep working");
        published += 1;
        let cid = ContentId::parse(&report.video.cid).unwrap();
        // The file has served its purpose; the bytes live in the block store.
        let _ = std::fs::remove_file(&source);

        // The viewer fetches and watches; the bystander only listens, which
        // is the commoner case and exercises a different path.
        let viewer_node = viewer.node().clone();
        let reached = wait_until(Duration::from_secs(30), || {
            viewer_node
                .video(&cid)
                .map(|v| v.is_some())
                .unwrap_or(false)
        })
        .await;

        if reached.is_ok() {
            arrived += 1;
            if viewer.node().fetch_video(cid).await.is_ok() {
                fetched += 1;
            }
            viewer
                .node()
                .record_watch(&WatchEvent {
                    cid,
                    watched_secs: 60,
                    duration_secs: 120,
                    completed: round % 3 == 0,
                    skipped: round % 5 == 0,
                    liked: round % 4 == 0,
                })
                .expect("recording a view should keep working");
            // Recomputing the model every round is the expensive local path.
            assert!(!viewer.node().recommendations(20).unwrap().is_empty());
        }

        // Searching and listing are what a UI does constantly.
        let _ = viewer.node().search("soak", 20).unwrap();
        let _ = bystander.node().videos(50, 0).unwrap();

        if round % 10 == 0 {
            let status = viewer.node().status().await.unwrap();
            eprintln!(
                "round {round}: published {published}, fetched {fetched}, \
                 videos {}, peers {}, cache {} bytes, rss {:?} MB",
                status.known_videos,
                status.connected_peers,
                status.cache.total_bytes,
                resident_bytes().map(|b| b / 1024 / 1024),
            );
        }
    }

    // Everything must still work at the end, not merely have survived.
    let status = viewer.node().status().await.unwrap();
    assert!(status.connected_peers >= 1, "the viewer lost its peers");
    assert!(
        status.known_videos as u32 >= arrived,
        "the viewer forgot videos it had already been told about"
    );
    // The first publish or two happen before the gossip mesh has formed, so
    // a perfect score is not the bar. A drop below this would mean
    // propagation itself had degraded over the run, which is what this is
    // watching for.
    let propagated = arrived as f64 / published as f64;
    assert!(
        propagated > 0.95,
        "only {:.1}% of {published} announcements arrived",
        propagated * 100.0
    );
    assert!(fetched > 0, "nothing was ever fetched");
    assert!(!viewer.node().recommendations(10).unwrap().is_empty());

    // Memory should not have run away. The bound is generous on purpose:
    // this is looking for a leak, not measuring an allocator.
    if let (Some(before), Some(after)) = (baseline, resident_bytes()) {
        let growth = after.saturating_sub(before);
        eprintln!(
            "resident memory: {} MB -> {} MB over {round} rounds",
            before / 1024 / 1024,
            after / 1024 / 1024
        );
        assert!(
            growth < 512 * 1024 * 1024,
            "resident memory grew by {} MB over {round} rounds",
            growth / 1024 / 1024
        );
    }

    eprintln!(
        "soak finished: {round} rounds, {published} published, {arrived} arrived, {fetched} fetched, \
         cache {} MB",
        status.cache.total_bytes / 1024 / 1024
    );

    publisher.shutdown().await;
    viewer.shutdown().await;
    bystander.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs for minutes; see the module comment"]
async fn a_node_survives_peers_arriving_and_leaving_repeatedly() {
    // Churn is the normal condition of a peer-to-peer network, and the
    // bookkeeping that tracks peers is exactly where an unbounded map hides.
    let anchor = spawn_node("anchor").await;
    let deadline = Instant::now() + Duration::from_secs(soak_seconds());
    let baseline = resident_bytes();
    let mut cycles = 0u32;

    while Instant::now() < deadline {
        cycles += 1;
        let visitor = spawn_node(&format!("visitor-{cycles}")).await;
        join_via_share_link(&visitor, &anchor).await;

        let anchor_node = anchor.node().clone();
        let _ = wait_until_async(Duration::from_secs(15), || {
            let node = anchor_node.clone();
            async move {
                node.status()
                    .await
                    .map(|s| s.connected_peers >= 1)
                    .unwrap_or(false)
            }
        })
        .await;

        visitor.shutdown().await;

        if cycles % 10 == 0 {
            eprintln!(
                "cycle {cycles}: known peers {}, rss {:?} MB",
                anchor.node().peers().unwrap().len(),
                resident_bytes().map(|b| b / 1024 / 1024),
            );
        }
    }

    // The anchor remembers everyone it met, which is intended — that is how
    // a node rejoins after everything else has gone away. What matters is
    // that it is still working.
    let status = anchor.node().status().await.unwrap();
    assert_eq!(
        status.known_peers as u32, cycles,
        "peers were not all recorded"
    );

    if let (Some(before), Some(after)) = (baseline, resident_bytes()) {
        let growth = after.saturating_sub(before);
        eprintln!(
            "resident memory: {} MB -> {} MB over {cycles} cycles",
            before / 1024 / 1024,
            after / 1024 / 1024
        );
        assert!(
            growth < 512 * 1024 * 1024,
            "resident memory grew by {} MB over {cycles} cycles",
            growth / 1024 / 1024
        );
    }

    eprintln!("churn finished: {cycles} peers came and went");
    anchor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs for minutes; see the module comment"]
async fn a_small_cache_evicts_forever_without_losing_what_it_should_keep() {
    // The other soak runs never come close to filling a 10 GiB cache, so
    // eviction only ever gets exercised by unit tests. Here the ceiling is
    // small enough that every round pushes something out, for as long as the
    // run lasts.
    const LIMIT: u64 = 8 * 1024 * 1024;

    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node_with("viewer", |config| {
        config.storage.cache_limit_bytes = LIMIT;
    })
    .await;
    join_via_share_link(&viewer, &publisher).await;

    // Something the viewer published itself. It is pinned, and must survive
    // however much pressure the cache comes under.
    let precious_source = write_seeded_file(viewer.dir.path(), "mine.mp4", 512 * 1024, 0xABCD);
    let precious = viewer
        .node()
        .publish_video(&precious_source, Some("Mine".into()), String::new(), vec![])
        .await
        .unwrap();
    let precious_cid = ContentId::parse(&precious.video.cid).unwrap();
    let precious_manifest = viewer.node().manifest(&precious_cid).unwrap().unwrap();

    let deadline = Instant::now() + Duration::from_secs(soak_seconds());
    let mut round = 0u32;
    let mut evicted_total = 0u64;

    while Instant::now() < deadline {
        round += 1;
        let source = write_seeded_file(
            publisher.dir.path(),
            &format!("clip-{round}.mp4"),
            1024 * 1024 + 7,
            round as u64,
        );
        let report = publisher
            .node()
            .publish_video(
                &source,
                Some(format!("Round {round}")),
                String::new(),
                vec![],
            )
            .await
            .expect("publishing should keep working");
        let _ = std::fs::remove_file(&source);
        let cid = ContentId::parse(&report.video.cid).unwrap();

        let viewer_node = viewer.node().clone();
        if wait_until(Duration::from_secs(30), || {
            viewer_node
                .video(&cid)
                .map(|v| v.is_some())
                .unwrap_or(false)
        })
        .await
        .is_err()
        {
            continue;
        }

        if let Ok(fetched) = viewer.node().fetch_video(cid).await {
            evicted_total += fetched.eviction.bytes_freed;
        }

        // The ceiling is a ceiling, not a target to drift past.
        let usage = viewer.node().storage().usage().unwrap();
        let unpinned = (usage.total_bytes - usage.pinned_bytes).max(0) as u64;
        assert!(
            unpinned <= LIMIT,
            "round {round}: {unpinned} unpinned bytes over a {LIMIT} byte ceiling"
        );

        if round % 10 == 0 {
            eprintln!(
                "round {round}: cache {} KB ({} KB pinned), {} MB evicted so far, rss {:?} MB",
                usage.total_bytes / 1024,
                usage.pinned_bytes / 1024,
                evicted_total / 1024 / 1024,
                resident_bytes().map(|b| b / 1024 / 1024),
            );
        }
    }

    assert!(
        evicted_total > 0,
        "nothing was ever evicted, so nothing was actually tested"
    );

    // What this node published came through all of it intact.
    for chunk in &precious_manifest.chunks {
        assert!(
            viewer.node().storage().has(chunk),
            "a pinned chunk was evicted under pressure"
        );
    }
    assert!(viewer.node().export_video(precious_cid, None).is_ok());

    eprintln!(
        "eviction soak finished: {round} rounds, {} MB evicted",
        evicted_total / 1024 / 1024
    );

    publisher.shutdown().await;
    viewer.shutdown().await;
}
