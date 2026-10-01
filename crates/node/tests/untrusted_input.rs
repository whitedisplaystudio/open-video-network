//! Section 31: everything arriving from the network is untrusted input.
//!
//! Every decoder that sits between a stranger's bytes and this node's state
//! is fed malformed, truncated, spliced and outright random input here. The
//! contract is narrow and absolute: **never panic, never hang, never
//! allocate without bound.** Returning an error is always an acceptable
//! answer; crashing is not, because a peer that can crash a node can crash
//! every node.
//!
//! The iteration count is small by default so this stays cheap in CI. Turn
//! it up when hunting:
//!
//! ```text
//! OVN_FUZZ_ITERATIONS=5000000 cargo test -p ovn-node --test untrusted_input --release
//! ```

use std::time::{Duration, Instant};

use ovn_content::VideoManifest;
use ovn_identity::Identity;
use ovn_protocol::{
    from_cbor_slice, to_cbor_vec, ContentId, Envelope, MessageType, NewVideo, NodeDescriptor,
    ProfileUpdate, VideoAnnouncement, VideoProvider, VideoQuery,
};

/// Deterministic, so a failure can be reproduced from the seed alone.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*, plenty for shuffling bytes and no dependency.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
}

/// Valid encodings to mutate from. Starting from something well-formed
/// reaches far deeper into a parser than random noise ever does.
fn seeds() -> Vec<Vec<u8>> {
    let identity = Identity::generate();
    let manifest = VideoManifest::new(
        "video/mp4",
        "clip.mp4",
        3000,
        1000,
        (0..3)
            .map(|i| ContentId::from_raw(format!("chunk {i}").as_bytes()))
            .collect(),
    );
    let announcement = VideoAnnouncement::sign(
        NewVideo {
            video_cid: Some(manifest.content_id().unwrap()),
            title: "A title".into(),
            description: "A description".into(),
            tags: vec!["gaming".into(), "indie".into()],
            duration_secs: 600,
            thumbnail_cid: Some(ContentId::from_raw(b"thumb")),
            source_url: "https://videos.example/clip.mp4".to_string(),
        },
        &identity,
    )
    .unwrap();

    vec![
        to_cbor_vec(&announcement).unwrap(),
        to_cbor_vec(&ProfileUpdate::sign("Creator".into(), "A bio".into(), &identity).unwrap())
            .unwrap(),
        to_cbor_vec(
            &NodeDescriptor::sign(
                "a node".into(),
                vec!["/ip4/192.0.2.1/udp/4800/quic-v1".into()],
                vec![ovn_protocol::Capability::bootstrap()],
                &identity,
            )
            .unwrap(),
        )
        .unwrap(),
        to_cbor_vec(
            &Envelope::seal(
                MessageType::VideoQuery,
                &VideoQuery {
                    query: "rust".into(),
                    limit: 10,
                },
                &identity,
            )
            .unwrap(),
        )
        .unwrap(),
        manifest.to_bytes().unwrap(),
        to_cbor_vec(&VideoProvider {
            video_cid: manifest.content_id().unwrap(),
            providers: vec!["12D3KooWaaa".into()],
        })
        .unwrap(),
    ]
}

/// The mutations a hostile peer is cheapest to perform.
fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    match rng.below(8) {
        // Flip a bit. Finds off-by-one reads of tags and lengths.
        0 => {
            if !out.is_empty() {
                let at = rng.below(out.len());
                out[at] ^= 1 << rng.below(8);
            }
        }
        // Replace a byte outright.
        1 => {
            if !out.is_empty() {
                let at = rng.below(out.len());
                out[at] = rng.byte();
            }
        }
        // Truncate. Finds decoders that trust a declared length.
        2 => {
            let keep = rng.below(out.len() + 1);
            out.truncate(keep);
        }
        // Append rubbish.
        3 => {
            for _ in 0..rng.below(64) {
                out.push(rng.byte());
            }
        }
        // Delete a run from the middle.
        4 => {
            if out.len() > 2 {
                let at = rng.below(out.len() - 1);
                let len = rng.below(out.len() - at);
                out.drain(at..at + len);
            }
        }
        // Splice in another message's bytes.
        5 => {
            let others = seeds();
            let other = &others[rng.below(others.len())];
            let at = rng.below(out.len() + 1);
            let take = rng.below(other.len() + 1);
            out.splice(at..at, other[..take].iter().copied());
        }
        // Claim an enormous length. The classic allocation bomb.
        6 => {
            let at = rng.below(out.len().max(1));
            for (i, b) in [0x5Bu8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
                .iter()
                .enumerate()
            {
                if at + i < out.len() {
                    out[at + i] = *b;
                }
            }
        }
        // Pure noise, occasionally.
        _ => {
            out.clear();
            for _ in 0..rng.below(256) {
                out.push(rng.byte());
            }
        }
    }
    out
}

/// Feed one blob to every decoder that a peer's bytes can reach.
///
/// Results are ignored on purpose: an error is a fine answer. What is being
/// asserted is that control returns at all.
fn feed(bytes: &[u8]) {
    let _ = from_cbor_slice::<VideoAnnouncement>(bytes).map(|a| a.verify());
    let _ = from_cbor_slice::<ProfileUpdate>(bytes).map(|p| p.verify());
    let _ = from_cbor_slice::<NodeDescriptor>(bytes).map(|d| d.verify());
    let _ = from_cbor_slice::<Envelope>(bytes).map(|e| {
        let _ = e.verify();
        e.open::<VideoQuery>()
    });
    let _ = from_cbor_slice::<VideoProvider>(bytes).map(|p| p.validate());
    let _ = from_cbor_slice::<VideoQuery>(bytes).map(|q| q.validate());
    let _ = VideoManifest::from_bytes(bytes);
    let _ = ContentId::from_bytes(bytes);

    // The same bytes read as text, which is how ids and links arrive.
    if let Ok(text) = std::str::from_utf8(bytes) {
        let _ = ContentId::parse(text);
        let _ = ovn_discovery::parse_share_link(text);
        let _ = ovn_discovery::Target::parse(text);
        let _ = ovn_node::parse_range(text, 4096);
    }
}

fn iterations() -> usize {
    std::env::var("OVN_FUZZ_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

#[test]
fn no_malformed_message_can_crash_a_decoder() {
    let seeds = seeds();
    let total = iterations();
    let mut rng = Rng::new(0x0FFE_D00D);
    let started = Instant::now();

    for i in 0..total {
        let seed = &seeds[rng.below(seeds.len())];
        let input = mutate(&mut rng, seed);

        // A single input that takes this long is a denial of service even if
        // it eventually returns.
        let before = Instant::now();
        feed(&input);
        let took = before.elapsed();
        assert!(
            took < Duration::from_millis(500),
            "iteration {i} took {took:?} on {} bytes",
            input.len()
        );
    }

    eprintln!(
        "{total} mutated messages survived in {:?}",
        started.elapsed()
    );
}

#[test]
fn no_random_noise_can_crash_a_decoder() {
    let mut rng = Rng::new(0x5EED_1234);
    for _ in 0..iterations() {
        let len = rng.below(2048);
        let mut input = Vec::with_capacity(len);
        for _ in 0..len {
            input.push(rng.byte());
        }
        feed(&input);
    }
}

#[test]
fn a_declared_length_never_becomes_an_allocation() {
    // CBOR can say "an array of 2^64 items" in nine bytes. A decoder that
    // believes it is a memory bomb reachable by anyone who can send us one
    // gossip message.
    let bombs: Vec<Vec<u8>> = vec![
        vec![0x9B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        vec![0xBB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        vec![0x5B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        vec![0x7B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        vec![0x9F; 4096],
    ];
    for bomb in bombs {
        let before = Instant::now();
        feed(&bomb);
        assert!(before.elapsed() < Duration::from_secs(1), "{bomb:?}");
    }
}

#[test]
fn oversized_input_is_refused_before_it_is_parsed() {
    let huge = vec![0u8; ovn_protocol::MAX_MESSAGE_SIZE + 1];
    let before = Instant::now();
    assert!(from_cbor_slice::<VideoAnnouncement>(&huge).is_err());
    assert!(
        before.elapsed() < Duration::from_millis(100),
        "the size check should come before the parse"
    );
}
