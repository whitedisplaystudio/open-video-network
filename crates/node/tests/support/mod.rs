//! Shared scaffolding for the acceptance tests.
//!
//! Each `tests/*.rs` file is its own binary, so not every helper is used by
//! every one of them.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use ovn_node::{start, NodeConfig, RunningNode};

/// A node running on ephemeral ports in a throwaway directory.
pub struct TestNode {
    pub dir: tempfile::TempDir,
    pub running: RunningNode,
}

impl TestNode {
    pub fn node(&self) -> &ovn_node::Node {
        self.running.node()
    }

    pub async fn shutdown(self) {
        self.running.shutdown().await;
    }
}

/// Start a node with everything ephemeral: port 0 for both listeners, mDNS
/// off, no bootstrap peers. Nothing outside this process is involved, which
/// is the point of Principle 1.
pub async fn spawn_node(name: &str) -> TestNode {
    spawn_node_with(name, |_| {}).await
}

/// A node with the usual test defaults, then whatever `adjust` changes.
pub async fn spawn_node_with(name: &str, adjust: impl FnOnce(&mut NodeConfig)) -> TestNode {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut config = NodeConfig::new(dir.path())
        .with_p2p_port(0)
        .with_api_port(0);
    config.node_name = name.to_string();
    config.network.enable_mdns = false;
    // Loopback only: a test must not touch the machine's real interfaces.
    config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
    adjust(&mut config);
    let running = start(config).await.expect("node starts");
    // Wait for the swarm to report an address before anyone tries to share it.
    wait_until(Duration::from_secs(10), || {
        !running.node().advertised_addresses().is_empty()
    })
    .await
    .expect("the node reports a listen address");
    TestNode { dir, running }
}

/// Poll `condition` until it holds or `timeout` expires.
///
/// Peer-to-peer state is eventually consistent by nature — a mesh forms, a
/// message propagates — so the tests wait for outcomes rather than sleeping
/// for a fixed time and hoping.
pub async fn wait_until<F: FnMut() -> bool>(
    timeout: Duration,
    mut condition: F,
) -> Result<(), &'static str> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if condition() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Like [`wait_until`] but for a condition that needs to await.
pub async fn wait_until_async<F, Fut>(
    timeout: Duration,
    mut condition: F,
) -> Result<(), &'static str>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if condition().await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Write a deterministic, incompressible-ish file of `size` bytes.
pub fn write_sample_file(dir: &std::path::Path, name: &str, size: usize) -> PathBuf {
    write_seeded_file(dir, name, size, 0)
}

/// The same, but with content that differs per `seed`.
///
/// Two files of the same length and the same contents are one video as far
/// as the block store is concerned — content addressing deduplicates them.
/// That is the right behaviour and a trap for a test that means to fill a
/// cache.
pub fn write_seeded_file(dir: &std::path::Path, name: &str, size: usize, seed: u64) -> PathBuf {
    let path = dir.join(name);
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let bytes: Vec<u8> = (0..size)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
        })
        .collect();
    std::fs::write(&path, bytes).expect("write sample file");
    path
}

/// Publish `path`, retrying until the announcement actually reached the
/// gossip mesh.
///
/// A publish a moment after two nodes connect has nobody subscribed to the
/// topic yet. Re-publishing is harmless — the content id is the same — and it
/// is what a test wants rather than a fixed sleep.
pub async fn publish_until_announced(
    node: &ovn_node::Node,
    path: &std::path::Path,
    title: &str,
    tags: &[&str],
) -> ovn_protocol::ContentId {
    publish_until_announced_from(node, path, title, tags, "https://videos.example/clip.mp4").await
}

/// The same, naming where the file is actually served from.
pub async fn publish_until_announced_from(
    node: &ovn_node::Node,
    path: &std::path::Path,
    title: &str,
    tags: &[&str],
    source_url: &str,
) -> ovn_protocol::ContentId {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let report = node
            .publish_video(
                path,
                Some(title.to_string()),
                String::new(),
                tags.iter().map(|t| t.to_string()).collect(),
                source_url.to_string(),
            )
            .await
            .expect("publishing");
        if report.announced_to_network || tokio::time::Instant::now() >= deadline {
            return ovn_protocol::ContentId::parse(&report.video.cid).expect("a valid content id");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Connect `from` to `to` using nothing but a share link, the way a person
/// would.
pub async fn join_via_share_link(from: &TestNode, to: &TestNode) {
    let link = to.node().share_link().expect("share link");
    from.node()
        .add_peer(&link)
        .await
        .expect("joining through a share link");
}

/// Wait until every viewer knows `expected` videos, re-announcing while
/// waiting.
///
/// `publish_video` reports success when gossipsub accepted the message, which
/// is not the same as it having been delivered: a mesh that is still forming
/// can accept a publish and pass it to nobody. Waiting longer does not fix
/// that, because the message is already gone. Saying it again does, and it is
/// what the node itself does on reconnect.
pub async fn wait_until_all_discovered(
    publisher: &ovn_node::Node,
    viewers: &[&ovn_node::Node],
    expected: i64,
) -> Result<(), &'static str> {
    let deadline = tokio::time::Instant::now() + PROPAGATION_TIMEOUT;
    loop {
        let everyone_has_them = viewers
            .iter()
            .all(|node| node.database().video_count().unwrap_or(0) >= expected);
        if everyone_has_them {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out");
        }
        let _ = publisher.reannounce().await;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

pub const PROPAGATION_TIMEOUT: Duration = Duration::from_secs(20);

// ------------------------------------------------------- a creator's server

/// A static file server standing in for wherever a creator put their video.
///
/// Small on purpose: it speaks exactly the part of HTTP the node relies on —
/// `Range` with a `206` — so that a test which passes here is a test that
/// passes against a real server, and a test exercising a *badly behaved*
/// server can be written by changing one of these flags rather than by mocking
/// out the fetch.
pub struct OriginServer {
    pub base_url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

/// How the server should misbehave, for the tests that need it to.
#[derive(Clone, Copy, Debug, Default)]
pub struct OriginBehaviour {
    /// Answer `200` with the whole file however narrow the range asked for.
    pub ignore_ranges: bool,
    /// Flip one bit of every body, as a server serving something other than
    /// what was announced.
    pub corrupt: bool,
    /// Answer `404` to everything.
    pub missing: bool,
}

impl OriginServer {
    pub async fn serving(body: Vec<u8>) -> Self {
        Self::serving_with(body, OriginBehaviour::default()).await
    }

    pub async fn serving_with(body: Vec<u8>, behaviour: OriginBehaviour) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding a test origin");
        let port = listener.local_addr().expect("local addr").port();
        let (shutdown, mut stop) = tokio::sync::oneshot::channel();
        let body = std::sync::Arc::new(body);

        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = &mut stop => return,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { continue };
                let body = std::sync::Arc::clone(&body);
                tokio::spawn(async move {
                    let _ = serve_one(stream, &body, behaviour).await;
                });
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}/clip.mp4"),
            shutdown: Some(shutdown),
        }
    }

    /// Stop answering, as a creator's server going down.
    pub fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl Drop for OriginServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn serve_one(
    mut stream: tokio::net::TcpStream,
    body: &[u8],
    behaviour: OriginBehaviour,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Read until the end of the headers. Enough for a request with no body,
    // which is all this ever receives.
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..n]);
        if request.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&request);

    if behaviour.missing {
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    let range = text
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))
        .and_then(|line| line.split('=').nth(1))
        .and_then(|spec| {
            let (start, end) = spec.trim().split_once('-')?;
            Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
        });

    let (status, slice) = match range {
        Some((start, end)) if !behaviour.ignore_ranges && start < body.len() => {
            let end = end.min(body.len() - 1);
            ("206 Partial Content", &body[start..=end])
        }
        _ => ("200 OK", body),
    };

    let mut payload = slice.to_vec();
    if behaviour.corrupt && !payload.is_empty() {
        let middle = payload.len() / 2;
        payload[middle] ^= 0b1000_0000;
    }

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n\
         Content-Type: video/mp4\r\n\r\n",
        payload.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

/// A node that will fetch from a loopback origin, which the guard against
/// probing a viewer's own network otherwise refuses.
pub async fn spawn_node_fetching_locally(name: &str) -> TestNode {
    spawn_node_with(name, |config| config.allow_private_sources = true).await
}

/// Publish `path`, served from `origin`, retrying until announced.
pub async fn publish_from(
    node: &ovn_node::Node,
    path: &std::path::Path,
    title: &str,
    tags: &[&str],
    origin: &OriginServer,
) -> ovn_protocol::ContentId {
    publish_until_announced_from(node, path, title, tags, &origin.base_url).await
}
