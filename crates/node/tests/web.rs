//! The local web layer: streaming, thumbnails, live events, and the UI.

mod support;

use std::time::Duration;

use support::*;

use ovn_protocol::ContentId;

/// Build a short real video with FFmpeg, or `None` if it is not installed.
///
/// FFmpeg is optional for the project, so tests that need a decodable file
/// skip rather than fail when it is missing.
fn make_video(dir: &std::path::Path, name: &str, seconds: u32) -> Option<std::path::PathBuf> {
    let path = dir.join(name);
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=duration={seconds}:size=160x120:rate=10"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-movflags",
            "+faststart",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .status()
        .ok()?;
    (status.success() && path.is_file()).then_some(path)
}

struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    fn new(node: &TestNode) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            base: node.running.api_url().expect("the API is running"),
            token: node.node().api_token().to_string(),
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{path}", self.base))
    }

    fn authed(&self, path: &str) -> reqwest::RequestBuilder {
        self.get(path).bearer_auth(&self.token)
    }
}

async fn publish(node: &ovn_node::Node, path: &std::path::Path, title: &str) -> ContentId {
    let report = node
        .publish_video(
            path,
            Some(title.to_string()),
            String::new(),
            vec!["test".into()],
        )
        .await
        .expect("publishing");
    ContentId::parse(&report.video.cid).unwrap()
}

// ------------------------------------------------------------- streaming

#[tokio::test(flavor = "multi_thread")]
async fn a_whole_video_streams_with_range_support_advertised() {
    let node = spawn_node("streamer").await;
    let source = write_sample_file(node.dir.path(), "clip.mp4", 2 * 1024 * 1024 + 500);
    let original = std::fs::read(&source).unwrap();
    let cid = publish(node.node(), &source, "Clip").await;
    let client = Client::new(&node);

    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert_eq!(response.headers()["content-type"], "video/mp4");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.headers()["content-length"],
        original.len().to_string().as_str()
    );
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        original.as_slice()
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_request_returns_exactly_that_slice() {
    let node = spawn_node("streamer").await;
    // Three chunks, so a range can span a chunk boundary.
    let source = write_sample_file(node.dir.path(), "clip.mp4", 2 * 1024 * 1024 + 1000);
    let original = std::fs::read(&source).unwrap();
    let cid = publish(node.node(), &source, "Clip").await;
    let client = Client::new(&node);

    // A slice inside one chunk.
    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .header("Range", "bytes=10-19")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.headers()["content-range"],
        format!("bytes 10-19/{}", original.len()).as_str()
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), &original[10..=19]);

    // A slice crossing a chunk boundary.
    let start = 1024 * 1024 - 5;
    let end = 1024 * 1024 + 5;
    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .header("Range", format!("bytes={start}-{end}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        &original[start..=end]
    );

    // The tail, which is what a player asks for to find an mp4's index.
    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .header("Range", "bytes=-100")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        &original[original.len() - 100..]
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_head_request_reports_the_length_without_the_body() {
    let node = spawn_node("streamer").await;
    let source = write_sample_file(node.dir.path(), "clip.mp4", 50_000);
    let cid = publish(node.node(), &source, "Clip").await;
    let client = Client::new(&node);

    let response = client
        .http
        .head(format!("{}/v1/videos/{cid}/stream", client.base))
        .bearer_auth(&client.token)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "50000");
    assert!(response.bytes().await.unwrap().is_empty());

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsatisfiable_range_is_refused_with_the_real_length() {
    let node = spawn_node("streamer").await;
    let source = write_sample_file(node.dir.path(), "clip.mp4", 1000);
    let cid = publish(node.node(), &source, "Clip").await;
    let client = Client::new(&node);

    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .header("Range", "bytes=5000-6000")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 416);
    assert_eq!(response.headers()["content-range"], "bytes */1000");

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_media_type_is_served_as_opaque_bytes() {
    // The media type comes out of a manifest a stranger wrote, and the
    // response is same-origin with the UI. Anything unrecognised must not be
    // echoed back as a content type a browser would execute.
    let node = spawn_node("streamer").await;
    let source = write_sample_file(node.dir.path(), "payload.html", 500);
    let cid = publish(node.node(), &source, "Suspicious").await;
    let client = Client::new(&node);

    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_video_streams_from_a_peer_without_being_downloaded_first() {
    // The point of streaming: playback starts without a prior `video get`.
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    let source = write_sample_file(publisher.dir.path(), "clip.mp4", 2 * 1024 * 1024 + 77);
    let original = std::fs::read(&source).unwrap();

    let cid = loop {
        let report = publisher
            .node()
            .publish_video(&source, Some("Remote".into()), String::new(), vec![])
            .await
            .unwrap();
        if report.announced_to_network {
            break ContentId::parse(&report.video.cid).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let viewer_node = viewer.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        viewer_node
            .video(&cid)
            .map(|v| v.is_some())
            .unwrap_or(false)
    })
    .await
    .expect("the announcement should arrive");
    assert!(
        !viewer.node().video(&cid).unwrap().unwrap().have_content,
        "the viewer must not hold the content yet"
    );

    let client = Client::new(&viewer);
    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .header("Range", "bytes=0-1023")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        &original[0..=1023]
    );

    // And the whole thing, still without an explicit fetch.
    let response = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        original.as_slice()
    );

    publisher.shutdown().await;
    viewer.shutdown().await;
}

// ------------------------------------------------------------ thumbnails

#[tokio::test(flavor = "multi_thread")]
async fn publishing_a_real_video_produces_a_thumbnail_a_peer_can_fetch() {
    let publisher = spawn_node("publisher").await;
    let Some(source) = make_video(publisher.dir.path(), "clip.mp4", 3) else {
        eprintln!("skipping: ffmpeg is not installed");
        publisher.shutdown().await;
        return;
    };
    let viewer = spawn_node("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    let cid = loop {
        let report = publisher
            .node()
            .publish_video(&source, Some("Thumbed".into()), String::new(), vec![])
            .await
            .unwrap();
        if report.announced_to_network {
            break ContentId::parse(&report.video.cid).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let record = publisher.node().video(&cid).unwrap().unwrap();
    let thumbnail_cid = record
        .thumbnail_cid
        .expect("a decodable video should get a thumbnail");
    assert!(ContentId::parse(&thumbnail_cid).is_ok());

    // The publisher serves it.
    let response = Client::new(&publisher)
        .authed(&format!("/v1/videos/{cid}/thumbnail"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/jpeg");
    let jpeg = response.bytes().await.unwrap();
    assert!(ovn_content::looks_like_jpeg(&jpeg));

    // And a peer fetches it over the block protocol, having only heard the
    // announcement.
    let viewer_node = viewer.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        viewer_node
            .video(&cid)
            .map(|v| v.is_some())
            .unwrap_or(false)
    })
    .await
    .expect("the announcement should arrive");

    let response = Client::new(&viewer)
        .authed(&format!("/v1/videos/{cid}/thumbnail"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap(), jpeg);

    publisher.shutdown().await;
    viewer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_video_with_no_thumbnail_reports_not_found_rather_than_failing() {
    let node = spawn_node("plain").await;
    // Random bytes named .mp4: nothing to extract a frame from.
    let source = write_sample_file(node.dir.path(), "clip.mp4", 4096);
    let cid = publish(node.node(), &source, "No thumbnail").await;
    assert!(node
        .node()
        .video(&cid)
        .unwrap()
        .unwrap()
        .thumbnail_cid
        .is_none());

    let response = Client::new(&node)
        .authed(&format!("/v1/videos/{cid}/thumbnail"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);

    node.shutdown().await;
}

// ----------------------------------------------------------- live events

#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_reports_its_progress_from_start_to_finish() {
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    let mut events = viewer.node().subscribe();

    let source = write_sample_file(publisher.dir.path(), "clip.mp4", 3 * 1024 * 1024);
    let cid = loop {
        let report = publisher
            .node()
            .publish_video(&source, Some("Progress".into()), String::new(), vec![])
            .await
            .unwrap();
        if report.announced_to_network {
            break ContentId::parse(&report.video.cid).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let viewer_node = viewer.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        viewer_node
            .video(&cid)
            .map(|v| v.is_some())
            .unwrap_or(false)
    })
    .await
    .expect("the announcement should arrive");

    viewer.node().fetch_video(cid).await.unwrap();

    let mut started = false;
    let mut progress = 0;
    let mut completed = false;
    let mut discovered = false;
    while let Ok(event) = events.try_recv() {
        match event {
            ovn_node::NodeEvent::VideoDiscovered { .. } => discovered = true,
            ovn_node::NodeEvent::FetchStarted { total_chunks, .. } => {
                assert_eq!(total_chunks, 3);
                started = true;
            }
            ovn_node::NodeEvent::FetchProgress {
                completed_chunks,
                total_chunks,
                ..
            } => {
                assert!(completed_chunks <= total_chunks);
                progress += 1;
            }
            ovn_node::NodeEvent::FetchCompleted { bytes_fetched, .. } => {
                assert_eq!(bytes_fetched, 3 * 1024 * 1024);
                completed = true;
            }
            _ => {}
        }
    }
    assert!(discovered, "a discovery should have been reported");
    assert!(started, "a fetch start should have been reported");
    assert_eq!(progress, 3, "one progress event per chunk");
    assert!(completed, "a fetch completion should have been reported");

    publisher.shutdown().await;
    viewer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_event_stream_is_server_sent_events_and_needs_the_token() {
    let node = spawn_node("events").await;
    let client = Client::new(&node);

    let unauthorised = client.get("/v1/events").send().await.unwrap();
    assert_eq!(unauthorised.status(), 401);

    let response = client.authed("/v1/events").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    drop(response);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutting_down_does_not_wait_for_an_open_event_stream() {
    // An event stream never ends on its own, and a graceful shutdown waits
    // for in-flight connections. Leaving a browser tab open on the UI must
    // not stop `ourvideo stop` from returning.
    let node = spawn_node("events").await;
    let client = Client::new(&node);

    let open = client.authed("/v1/events").send().await.unwrap();
    assert_eq!(open.status(), 200);

    tokio::time::timeout(Duration::from_secs(20), node.shutdown())
        .await
        .expect("shutdown must not block on a streaming connection");
    drop(open);
}

// -------------------------------------------------------------- the UI

#[tokio::test(flavor = "multi_thread")]
async fn the_ui_is_served_and_its_data_still_needs_the_token() {
    let node = spawn_node("ui").await;
    let client = Client::new(&node);

    // Pages and assets carry no data, so they need no token.
    for (path, content_type) in [
        ("/ui", "text/html"),
        ("/admin", "text/html"),
        ("/assets/app.css", "text/css"),
        ("/assets/common.js", "text/javascript"),
        ("/assets/viewer.js", "text/javascript"),
        ("/assets/admin.js", "text/javascript"),
    ] {
        let response = client.get(path).send().await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with(content_type),
            "{path}"
        );
    }

    // Everything that reads anything does.
    let response = client.get("/v1/status").send().await.unwrap();
    assert_eq!(response.status(), 401);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_auth_route_exchanges_the_token_for_a_usable_cookie() {
    let node = spawn_node("ui").await;
    let client = Client::new(&node);

    let wrong = client
        .get(&format!("/auth?token={}", "0".repeat(64)))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    let response = client
        .get(&format!("/auth?token={}&next=/admin", client.token))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/admin");

    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");

    // A browser would now send that cookie, and the API accepts it — which is
    // what makes `<video src>` and `EventSource` work.
    let jar = cookie.split(';').next().unwrap().to_string();
    let response = client
        .get("/v1/status")
        .header("Cookie", &jar)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_session_survives_restarting_the_node() {
    // Someone should be able to bookmark the interface. That only works if
    // the token behind the session cookie outlives the process.
    let dir = tempfile::tempdir().unwrap();
    let config = || {
        let mut config = ovn_node::NodeConfig::new(dir.path())
            .with_p2p_port(0)
            .with_api_port(0);
        config.network.enable_mdns = false;
        config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
        config
    };

    let running = ovn_node::start(config()).await.unwrap();
    let first_token = running.node().api_token().to_string();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // Authenticate the way a browser would, and keep the cookie.
    let response = http
        .get(format!(
            "{}/auth?token={first_token}&next=/ui",
            running.api_url().unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    running.shutdown().await;

    // Start again from the same data directory, as a person would.
    let running = ovn_node::start(config()).await.unwrap();
    assert_eq!(
        running.node().api_token(),
        first_token,
        "the token must not change across a restart"
    );

    // The cookie the browser still holds works, with no trip to a terminal.
    let response = http
        .get(format!("{}/v1/status", running.api_url().unwrap()))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_token_file_ends_every_session() {
    // The documented way to revoke access.
    let dir = tempfile::tempdir().unwrap();
    let config = || {
        let mut config = ovn_node::NodeConfig::new(dir.path())
            .with_p2p_port(0)
            .with_api_port(0);
        config.network.enable_mdns = false;
        config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
        config
    };

    let running = ovn_node::start(config()).await.unwrap();
    let old_token = running.node().api_token().to_string();
    running.shutdown().await;

    std::fs::remove_file(dir.path().join("api.token")).unwrap();

    let running = ovn_node::start(config()).await.unwrap();
    assert_ne!(running.node().api_token(), old_token);

    let stale = reqwest::Client::new()
        .get(format!("{}/v1/status", running.api_url().unwrap()))
        .header("Cookie", format!("ovn_token={old_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 401);

    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_auth_redirect_cannot_be_pointed_at_another_site() {
    let node = spawn_node("ui").await;
    let client = Client::new(&node);

    for hostile in ["//evil.example", "https://evil.example/x"] {
        let response = client
            .get(&format!(
                "/auth?token={}&next={}",
                client.token,
                urlencoding(hostile)
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 303);
        assert_eq!(response.headers()["location"], "/ui", "{hostile}");
    }

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_started_without_token_auth_serves_a_plain_bookmarked_url() {
    // The point of the setting: open the URL, it works, nothing to set up.
    let dir = tempfile::tempdir().unwrap();
    let mut config = ovn_node::NodeConfig::new(dir.path())
        .with_p2p_port(0)
        .with_api_port(0);
    config.network.enable_mdns = false;
    config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
    config.api_auth = ovn_node::ApiAuth::None;
    let running = ovn_node::start(config).await.unwrap();
    let base = running.api_url().unwrap();
    let http = reqwest::Client::new();

    for path in [
        "/ui",
        "/admin",
        "/v1/status",
        "/v1/watch",
        "/v1/preferences",
    ] {
        let response = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
    }

    // Everything else that keeps the outside out still applies: this is a
    // trade against other accounts on this machine, not against the network.
    let rebound = http
        .get(format!("{base}/v1/status"))
        .header("Host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 421);
    assert!(running.api_addr().unwrap().ip().is_loopback());

    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_still_refuses_an_unauthenticated_request() {
    // A guard on the default, so the convenience setting cannot creep into
    // being the normal one.
    let node = spawn_node("guarded").await;
    assert_eq!(node.node().config().api_auth, ovn_node::ApiAuth::Token);

    let client = Client::new(&node);
    assert_eq!(client.get("/v1/watch").send().await.unwrap().status(), 401);
    assert_eq!(client.get("/v1/status").send().await.unwrap().status(), 401);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_for_a_non_loopback_host_is_refused() {
    // DNS rebinding: an attacker's domain resolving to 127.0.0.1 would make
    // their page same-origin with this server. Checking the requested host
    // closes that, regardless of cookies.
    let node = spawn_node("ui").await;
    let client = Client::new(&node);

    let response = client
        .authed("/v1/status")
        .header("Host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 421);

    // The real thing still works.
    assert_eq!(
        client.authed("/v1/status").send().await.unwrap().status(),
        200
    );

    node.shutdown().await;
}

fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

// -------------------------------------------------------------- uploading

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_upload_publishes_the_file() {
    let node = spawn_node("uploader").await;
    let client = Client::new(&node);
    let payload: Vec<u8> = (0..(1024 * 1024 + 321)).map(|i| (i % 253) as u8).collect();

    let response = client
        .http
        .post(format!(
            "{}/v1/upload?fileName=holiday.mp4&title=Holiday&tags=travel,%20family",
            client.base
        ))
        .bearer_auth(&client.token)
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let published: serde_json::Value = response.json().await.unwrap();
    assert_eq!(published["title"], "Holiday");
    assert_eq!(published["chunks"], 2);
    assert_eq!(published["totalBytes"], payload.len());

    let cid = ContentId::parse(published["cid"].as_str().unwrap()).unwrap();
    let record = node.node().video(&cid).unwrap().unwrap();
    assert_eq!(record.tags, vec!["travel", "family"]);
    assert!(record.is_local && record.have_content);

    // The staged copy is gone; the bytes live in the block store now.
    let staged = std::fs::read_dir(node.node().config().uploads_dir())
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(staged, 0);

    // And it streams straight back out.
    let streamed = client
        .authed(&format!("/v1/videos/{cid}/stream"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(streamed.as_ref(), payload.as_slice());

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_upload_is_refused() {
    let node = spawn_node("uploader").await;
    let client = Client::new(&node);

    let response = client
        .http
        .post(format!("{}/v1/upload?fileName=nothing.mp4", client.base))
        .bearer_auth(&client.token)
        .body(Vec::<u8>::new())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error(), "{}", response.status());

    node.shutdown().await;
}

/// Section 13 of the AGPL: a user who interacts with the program over a
/// network must be offered its Corresponding Source. For this node the
/// interaction is the web interface, so the offer has to be reachable by
/// anyone who can reach the pages — which means outside the token.
#[tokio::test(flavor = "multi_thread")]
async fn the_source_offer_is_reachable_without_signing_in() {
    let node = spawn_node_with("licensed", |config| {
        config.api_auth = ovn_node::ApiAuth::Token;
    })
    .await;
    let base = node
        .running
        .api_url()
        .expect("the node serves its local API");

    let response = reqwest::get(format!("{base}/v1/about"))
        .await
        .expect("asking the node about itself");
    assert!(
        response.status().is_success(),
        "the source offer must not need a token: {}",
        response.status()
    );
    let about: serde_json::Value = response.json().await.expect("JSON");

    let source = about["sourceUrl"].as_str().unwrap_or_default();
    assert!(
        source.starts_with("https://"),
        "the offer has to be somewhere a person can actually go: {source:?}"
    );
    let licence = about["licence"].as_str().unwrap_or_default();
    assert!(
        licence.contains("AGPL"),
        "the node should name the licence it is under, not a different one: {licence:?}"
    );
    assert!(!about["version"].as_str().unwrap_or_default().is_empty());

    // And the pages have somewhere to put it.
    for page in ["/ui", "/admin"] {
        let html = reqwest::get(format!("{base}{page}"))
            .await
            .expect("fetching the page")
            .text()
            .await
            .expect("the page body");
        assert!(
            html.contains("id=\"source-link\""),
            "{page} has nowhere to show where its source is"
        );
    }

    node.shutdown().await;
}
