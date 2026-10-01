//! Range requests, and what the node does with the bytes a server hands it.
//!
//! A player asks for byte ranges, not chunks, and it asks for a new one every
//! time somebody drags the scrubber. Three things have to hold: the bytes
//! handed back are exactly the bytes at those offsets, fetching chunk N+1 does
//! not wait for chunk N to reach the player, and bytes that do not match what
//! the creator signed never reach the player at all.

mod support;

use futures::StreamExt;
use ovn_node::ByteRange;
use ovn_protocol::ContentId;
use support::*;

/// 1 MiB, matching the chunk size the content layer uses.
const CHUNK: u64 = 1024 * 1024;

/// Collect a whole range into one buffer.
async fn read_range(
    node: &ovn_node::Node,
    cid: ContentId,
    range: ByteRange,
) -> std::result::Result<Vec<u8>, String> {
    let plan = node
        .prepare_stream(cid)
        .await
        .map_err(|e| format!("preparing: {e}"))?;
    let mut out = Vec::new();
    let mut stream = Box::pin(node.stream_range(plan, range));
    while let Some(piece) = stream.next().await {
        out.extend_from_slice(&piece.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Offsets worth checking for a file of `total` bytes: the ends, the chunk
/// boundaries, and ranges that span one, two and three chunks.
fn interesting_ranges(total: u64) -> Vec<ByteRange> {
    let last = total - 1;
    let mut ranges = vec![
        ByteRange { start: 0, end: 0 },
        ByteRange {
            start: last,
            end: last,
        },
        ByteRange {
            start: 0,
            end: last,
        },
        ByteRange {
            start: 10,
            end: 1000,
        },
        ByteRange {
            start: 0,
            end: CHUNK - 1,
        },
        ByteRange {
            start: CHUNK,
            end: CHUNK + 5,
        },
        ByteRange {
            start: CHUNK - 3,
            end: CHUNK + 3,
        },
        ByteRange {
            start: CHUNK - 1,
            end: 2 * CHUNK + 1,
        },
        ByteRange {
            start: 3 * CHUNK,
            end: last,
        },
    ];
    ranges.retain(|r| r.end <= last && r.start <= r.end);
    ranges
}

/// A node, a creator's server, and a published video served from it.
async fn published(name: &str, size: usize) -> (TestNode, OriginServer, ContentId, Vec<u8>) {
    published_with(name, size, OriginBehaviour::default()).await
}

async fn published_with(
    name: &str,
    size: usize,
    behaviour: OriginBehaviour,
) -> (TestNode, OriginServer, ContentId, Vec<u8>) {
    let node = spawn_node_fetching_locally(name).await;
    let file = write_sample_file(node.dir.path(), "clip.mp4", size);
    let body = std::fs::read(&file).unwrap();
    // Published before the server is told to misbehave, so the manifest
    // describes the real file and the misbehaviour is a departure from it.
    let origin = OriginServer::serving_with(body.clone(), behaviour).await;
    let cid = publish_from(node.node(), &file, "Clip", &["test"], &origin).await;
    (node, origin, cid, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn ranges_are_byte_exact_including_across_chunk_boundaries() {
    // Three and a half chunks, so the last one is partial.
    let size = (3.5 * CHUNK as f64) as usize;
    let (node, _origin, cid, body) = published("reader", size).await;

    for range in interesting_ranges(body.len() as u64) {
        let got = read_range(node.node(), cid, range)
            .await
            .unwrap_or_else(|e| panic!("bytes {}-{}: {e}", range.start, range.end));
        let expected = &body[range.start as usize..=range.end as usize];
        assert_eq!(
            got.len(),
            expected.len(),
            "wrong length for bytes {}-{}",
            range.start,
            range.end
        );
        assert_eq!(
            got, expected,
            "wrong bytes for {}-{}",
            range.start, range.end
        );
    }

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_held_locally_just_because_it_was_watched() {
    // The point of the whole arrangement: a node carries what is needed to
    // find and check a video, and does not carry the video.
    let size = (2.0 * CHUNK as f64) as usize;
    let (node, _origin, cid, body) = published("viewer", size).await;

    let whole = read_range(
        node.node(),
        cid,
        ByteRange {
            start: 0,
            end: body.len() as u64 - 1,
        },
    )
    .await
    .expect("the whole file");
    assert_eq!(whole, body);

    // The manifest and the thumbnail are held. Two megabytes of video are not.
    let cache = node.node().status().await.unwrap().cache;
    assert!(
        (cache.total_bytes as u64) < CHUNK,
        "watching a {size}-byte video left {} bytes in the cache",
        cache.total_bytes
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chunks_the_player_has_not_reached_are_already_on_their_way() {
    // The read-ahead window, which is what stops every chunk costing a fresh
    // round trip to the creator's server.
    let size = (4.0 * CHUNK as f64) as usize;
    let (node, _origin, cid, _body) = published("reader", size).await;

    let plan = node.node().prepare_stream(cid).await.expect("a plan");
    let started = std::time::Instant::now();
    let mut stream = Box::pin(node.node().stream_range(
        plan,
        ByteRange {
            start: 0,
            end: (4 * CHUNK) - 1,
        },
    ));
    let first = stream.next().await.expect("a first chunk");
    assert!(first.is_ok(), "{:?}", first.err());

    // Draining the rest must not take four more round trips' worth of work;
    // with a window of four they were all requested at once.
    let mut total = first.unwrap().len();
    while let Some(piece) = stream.next().await {
        total += piece.expect("a chunk").len();
    }
    assert_eq!(total as u64, 4 * CHUNK);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "streaming four chunks took {:?}",
        started.elapsed()
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abandoning_a_stream_partway_leaves_the_node_working() {
    let size = (3.0 * CHUNK as f64) as usize;
    let (node, _origin, cid, body) = published("seeker", size).await;

    for _ in 0..3 {
        let plan = node.node().prepare_stream(cid).await.expect("a plan");
        let mut stream = Box::pin(node.node().stream_range(
            plan,
            ByteRange {
                start: 0,
                end: body.len() as u64 - 1,
            },
        ));
        // One chunk, then walk away — as a player does when the viewer seeks.
        let _ = stream.next().await;
        drop(stream);
    }

    let whole = read_range(
        node.node(),
        cid,
        ByteRange {
            start: 0,
            end: body.len() as u64 - 1,
        },
    )
    .await
    .expect("the file still reads back");
    assert_eq!(whole, body, "the file did not read back after three seeks");

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_serves_something_else_is_caught() {
    // The reason the manifest still exists. A chunk hash is no longer an
    // address — nobody asks a peer for a chunk — but it is still a promise,
    // signed by the creator, about what the bytes at an offset must be. A
    // server that is swapped, compromised, or told to serve one viewer
    // something different cannot do it unnoticed.
    let size = (2.0 * CHUNK as f64) as usize;
    let (node, _origin, cid, body) = published_with(
        "suspicious",
        size,
        OriginBehaviour {
            corrupt: true,
            ..Default::default()
        },
    )
    .await;

    let error = read_range(
        node.node(),
        cid,
        ByteRange {
            start: 0,
            end: body.len() as u64 - 1,
        },
    )
    .await
    .expect_err("altered bytes must not be streamed to the player");
    assert!(
        error.contains("not the file that was announced") || error.contains("do not match"),
        "the error should say the file is not what was announced: {error}"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_ignores_range_requests_is_refused() {
    // Reading a whole video to find one chunk of it would mean holding the
    // whole video in memory, which is exactly what this design exists to
    // avoid.
    let size = (2.0 * CHUNK as f64) as usize;
    let (node, _origin, cid, _body) = published_with(
        "stubborn",
        size,
        OriginBehaviour {
            ignore_ranges: true,
            ..Default::default()
        },
    )
    .await;

    let error = read_range(
        node.node(),
        cid,
        ByteRange {
            start: 0,
            end: 1023,
        },
    )
    .await
    .expect_err("a server that cannot do ranges cannot be streamed from");
    assert!(
        error.contains("range") || error.contains("offered"),
        "the error should name the problem: {error}"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_has_gone_away_is_reported_rather_than_hung_on_to() {
    let size = (1.5 * CHUNK as f64) as usize;
    let (node, mut origin, cid, _body) = published("abandoned", size).await;
    origin.stop();
    // Give the listener time to actually close.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let error = read_range(
        node.node(),
        cid,
        ByteRange {
            start: 0,
            end: 1023,
        },
    )
    .await
    .expect_err("there is nowhere to fetch from");
    assert!(!error.is_empty());

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_inside_a_network_is_refused_unless_that_was_asked_for() {
    // A node fetches whatever address an announcement gives it. Without this
    // a stranger could publish `http://192.168.0.1/` and have every viewer's
    // node knock on doors inside their own house.
    let node = spawn_node("cautious").await;
    let file = write_sample_file(node.dir.path(), "clip.mp4", 200_000);
    let body = std::fs::read(&file).unwrap();
    let origin = OriginServer::serving(body.clone()).await;
    let cid = publish_from(node.node(), &file, "Clip", &["test"], &origin).await;

    let error = read_range(
        node.node(),
        cid,
        ByteRange {
            start: 0,
            end: 1023,
        },
    )
    .await
    .expect_err("a loopback source must be refused by default");
    assert!(
        error.contains("not an address on the internet"),
        "the error should say why: {error}"
    );

    node.shutdown().await;
}
