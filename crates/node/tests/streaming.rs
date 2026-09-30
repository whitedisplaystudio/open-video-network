//! Range requests, and the read-ahead behind them.
//!
//! A player asks for byte ranges, not chunks, and it asks for a new one every
//! time somebody drags the scrubber. Two things have to hold: the bytes handed
//! back are exactly the bytes at those offsets, and the fetching of chunk N+1
//! does not wait for chunk N to reach the player.

mod support;

use std::path::Path;

use futures::StreamExt;
use ovn_node::ByteRange;
use ovn_protocol::ContentId;
use support::*;

/// 1 MiB, matching the chunk size the content layer uses.
const CHUNK: u64 = 1024 * 1024;

/// Collect a whole range into one buffer.
async fn read_range(node: &ovn_node::Node, cid: ContentId, range: ByteRange) -> Vec<u8> {
    let plan = node.prepare_stream(cid).await.expect("a stream plan");
    let mut out = Vec::new();
    let mut stream = Box::pin(node.stream_range(plan, range));
    while let Some(piece) = stream.next().await {
        out.extend_from_slice(&piece.expect("a chunk of the response"));
    }
    out
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
        // Entirely inside the first chunk.
        ByteRange {
            start: 10,
            end: 1000,
        },
        // Ending exactly on a boundary, and starting exactly on one.
        ByteRange {
            start: 0,
            end: CHUNK - 1,
        },
        ByteRange {
            start: CHUNK,
            end: CHUNK + 5,
        },
        // Straddling one boundary, then two.
        ByteRange {
            start: CHUNK - 3,
            end: CHUNK + 3,
        },
        ByteRange {
            start: CHUNK - 1,
            end: 2 * CHUNK + 1,
        },
        // The tail, which is a partial chunk.
        ByteRange {
            start: 3 * CHUNK,
            end: last,
        },
    ];
    ranges.retain(|r| r.end <= last && r.start <= r.end);
    ranges
}

/// Every file under the block store, so a test can count what has arrived.
fn stored_blocks(blocks_dir: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else if !path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with(".tmp-"))
                .unwrap_or(false)
            {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(blocks_dir, &mut count);
    count
}

#[tokio::test(flavor = "multi_thread")]
async fn ranges_are_byte_exact_including_across_chunk_boundaries() {
    let node = spawn_node("reader").await;
    // Three and a half chunks, so the last one is partial.
    let size = (3.5 * CHUNK as f64) as usize;
    let file = write_sample_file(node.dir.path(), "clip.bin", size);
    let source = std::fs::read(&file).unwrap();

    let report = node
        .node()
        .publish_video(
            &file,
            Some("Clip".into()),
            String::new(),
            vec!["test".into()],
        )
        .await
        .expect("publish");
    let cid: ContentId = report.video.cid.parse().unwrap();

    for range in interesting_ranges(source.len() as u64) {
        let got = read_range(node.node(), cid, range).await;
        let expected = &source[range.start as usize..=range.end as usize];
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
async fn ranges_are_byte_exact_when_the_bytes_come_from_a_peer() {
    // The same offsets, but every chunk has to be fetched, so the read-ahead
    // window rather than the local store decides what arrives when.
    let server = spawn_node("server").await;
    let client = spawn_node("client").await;
    join_via_share_link(&client, &server).await;

    let size = (2.5 * CHUNK as f64) as usize;
    let file = write_sample_file(server.dir.path(), "clip.bin", size);
    let source = std::fs::read(&file).unwrap();
    let cid = publish_until_announced(server.node(), &file, "Clip", &["test"]).await;

    wait_until(PROPAGATION_TIMEOUT, || {
        client.node().video(&cid).ok().flatten().is_some()
    })
    .await
    .expect("the announcement reaches the client");

    for range in interesting_ranges(source.len() as u64) {
        let got = read_range(client.node(), cid, range).await;
        let expected = &source[range.start as usize..=range.end as usize];
        assert_eq!(
            got, expected,
            "wrong bytes for {}-{} fetched from a peer",
            range.start, range.end
        );
    }

    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chunks_the_player_has_not_reached_are_already_on_their_way() {
    // This is the whole point of the window. Ask for a range covering several
    // chunks, take only the first, and the rest should already be arriving
    // rather than waiting to be asked for.
    let server = spawn_node("server").await;
    let client = spawn_node("client").await;
    join_via_share_link(&client, &server).await;

    let size = (4.0 * CHUNK as f64) as usize;
    let file = write_sample_file(server.dir.path(), "clip.bin", size);
    let cid = publish_until_announced(server.node(), &file, "Clip", &["test"]).await;

    wait_until(PROPAGATION_TIMEOUT, || {
        client.node().video(&cid).ok().flatten().is_some()
    })
    .await
    .expect("the announcement reaches the client");

    let blocks = client.dir.path().join("blocks");
    let plan = client
        .node()
        .prepare_stream(cid)
        .await
        .expect("a stream plan");
    // Fetching the manifest already stored one block; count from there.
    let before = stored_blocks(&blocks);

    let mut stream = Box::pin(client.node().stream_range(
        plan,
        ByteRange {
            start: 0,
            end: (4 * CHUNK) - 1,
        },
    ));
    let first = stream.next().await.expect("a first chunk");
    assert!(first.is_ok());

    // Chunk 0 has been handed over. With a window of four, chunks 1..=3 were
    // requested at the same time and should land without anybody reading on.
    wait_until(std::time::Duration::from_secs(30), || {
        stored_blocks(&blocks) >= before + 3
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "after one chunk was read, only {} blocks had arrived (started at {before}); \
             chunks are being fetched one at a time",
            stored_blocks(&blocks)
        )
    });

    drop(stream);
    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abandoning_a_stream_partway_leaves_the_node_working() {
    // A seek drops the response mid-flight, which aborts whatever was being
    // fetched for it. The next request must not inherit any of that.
    let server = spawn_node("server").await;
    let client = spawn_node("client").await;
    join_via_share_link(&client, &server).await;

    let size = (3.0 * CHUNK as f64) as usize;
    let file = write_sample_file(server.dir.path(), "clip.bin", size);
    let source = std::fs::read(&file).unwrap();
    let cid = publish_until_announced(server.node(), &file, "Clip", &["test"]).await;

    wait_until(PROPAGATION_TIMEOUT, || {
        client.node().video(&cid).ok().flatten().is_some()
    })
    .await
    .expect("the announcement reaches the client");

    for _ in 0..3 {
        let plan = client.node().prepare_stream(cid).await.expect("a plan");
        let mut stream = Box::pin(client.node().stream_range(
            plan,
            ByteRange {
                start: 0,
                end: source.len() as u64 - 1,
            },
        ));
        // One chunk, then walk away — as a player does when the viewer seeks.
        let _ = stream.next().await;
        drop(stream);
    }

    // And the whole file still reads back correctly afterwards.
    let whole = read_range(
        client.node(),
        cid,
        ByteRange {
            start: 0,
            end: source.len() as u64 - 1,
        },
    )
    .await;
    assert_eq!(
        whole, source,
        "the file did not read back after three seeks"
    );

    client.shutdown().await;
    server.shutdown().await;
}
