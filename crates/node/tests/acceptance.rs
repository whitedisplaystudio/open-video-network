//! The V1 acceptance tests from section 37, run as real nodes in one process.
//!
//! Every node here is a full node: its own identity, its own SQLite file, its
//! own libp2p swarm on an ephemeral loopback port. Nothing outside the test
//! process is contacted, and no node is special.

mod support;

use std::time::Duration;

use support::*;

use ovn_database::WatchEvent;
use ovn_protocol::{to_cbor_vec, ContentId};

// ---------------------------------------------------------------- Test A

#[tokio::test(flavor = "multi_thread")]
async fn test_a_five_peers_form_a_network_with_no_central_server() {
    let a = spawn_node("peer-a").await;
    let b = spawn_node("peer-b").await;
    let c = spawn_node("peer-c").await;
    let d = spawn_node("peer-d").await;
    let e = spawn_node("peer-e").await;

    // No API server, no database, no directory service: every peer joins by
    // being handed one link from another peer.
    for peer in [&b, &c, &d, &e] {
        join_via_share_link(peer, &a).await;
    }

    let a_node = a.node().clone();
    wait_until_async(PROPAGATION_TIMEOUT, || {
        let node = a_node.clone();
        async move { node.status().await.map(|s| s.connected_peers).unwrap_or(0) >= 4 }
    })
    .await
    .expect("peer A should see four peers");

    for peer in [&b, &c, &d, &e] {
        let status = peer.node().status().await.unwrap();
        assert!(
            status.connected_peers >= 1,
            "{} has no connections",
            status.node_name
        );
    }

    for peer in [a, b, c, d, e] {
        peer.shutdown().await;
    }
}

// ---------------------------------------------------------------- Test B

#[tokio::test(flavor = "multi_thread")]
async fn test_b_a_node_starts_from_nothing_with_no_configuration() {
    let dir = tempfile::tempdir().unwrap();
    // Exactly what `ourvideo start` does, apart from the ports, which the
    // test picks so it can run alongside anything else.
    let mut config = ovn_node::NodeConfig::new(dir.path())
        .with_p2p_port(0)
        .with_api_port(0);
    config.network.enable_mdns = false;
    config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];

    let running = ovn_node::start(config).await.expect("a first run works");

    // Everything the node needs was created for the user.
    assert!(dir.path().join("identity.key").is_file());
    assert!(dir.path().join("node.db").is_file());
    assert!(dir.path().join("blocks").is_dir());
    assert!(dir.path().join("runtime.json").is_file());
    assert!(running.api_addr().is_some());

    let status = running.node().status().await.unwrap();
    assert!(!status.peer_id.is_empty());
    assert_eq!(status.known_videos, 0);

    let first_identity = status.public_key.clone();
    running.shutdown().await;

    // Restarting keeps the same identity: the user is the same person.
    let mut config = ovn_node::NodeConfig::new(dir.path())
        .with_p2p_port(0)
        .with_api_port(0);
    config.network.enable_mdns = false;
    config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
    let running = ovn_node::start(config).await.expect("a second run works");
    assert_eq!(
        running.node().status().await.unwrap().public_key,
        first_identity
    );
    running.shutdown().await;
}

// ---------------------------------------------------------------- Test C

#[tokio::test(flavor = "multi_thread")]
async fn test_c_a_newcomer_joins_with_one_share_link() {
    let a = spawn_node("host").await;
    let b = spawn_node("newcomer").await;

    // The whole of what the user does: paste one string.
    let link = a.node().share_link().unwrap();
    assert!(link.starts_with("ourvideo://"));

    let report = b.node().add_peer(&link).await.expect("joining");
    assert!(report.connected);
    assert_eq!(report.peer_id, a.node().peer_id().to_base58());
    assert_eq!(report.node_name, "host");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_c_a_newcomer_joins_from_a_url() {
    let a = spawn_node("host").await;
    let b = spawn_node("newcomer").await;

    // A node publishes its descriptor at the well-known path, so putting a
    // web server in front of it is all an operator has to do. Here the node's
    // own local API plays that part.
    let url = format!(
        "{}{}",
        a.running.api_url().expect("the API is running"),
        ovn_protocol::WELL_KNOWN_DESCRIPTOR_PATH
    );

    let report = b.node().add_peer(&url).await.expect("joining from a URL");
    assert!(report.connected);
    assert_eq!(report.peer_id, a.node().peer_id().to_base58());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_c_a_hostile_link_cannot_impersonate_a_node() {
    let a = spawn_node("host").await;
    let b = spawn_node("newcomer").await;

    let mut descriptor = a.node().descriptor().unwrap();
    descriptor.node_name = "Totally The Real Node".to_string();
    let forged = ovn_discovery::share_link(&descriptor).unwrap();

    let err = b.node().add_peer(&forged).await.unwrap_err();
    assert!(
        err.to_string().contains("verification") || err.to_string().contains("signature"),
        "{err}"
    );

    a.shutdown().await;
    b.shutdown().await;
}

// ---------------------------------------------------------------- Test D

#[tokio::test(flavor = "multi_thread")]
async fn test_d_a_published_video_is_discovered_over_p2p_and_fetched_from_its_source() {
    let a = spawn_node("publisher").await;
    let b = spawn_node_fetching_locally("viewer").await;
    join_via_share_link(&b, &a).await;

    // Two chunks and a bit, so more than one round trip to the server is
    // needed and the order they come back in matters.
    let source = write_sample_file(a.dir.path(), "clip.mp4", 2 * 1024 * 1024 + 4242);
    let original = std::fs::read(&source).unwrap();
    let origin = OriginServer::serving(original.clone()).await;

    let cid = publish_until_announced_from(
        a.node(),
        &source,
        "Kingdom speedrun",
        &["gaming"],
        &origin.base_url,
    )
    .await;

    // B learns that the video exists through GossipSub, with no server
    // involved in the finding.
    let b_node = b.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        b_node.video(&cid).map(|v| v.is_some()).unwrap_or(false)
    })
    .await
    .expect("the announcement should reach B");

    let discovered = b.node().video(&cid).unwrap().unwrap();
    assert_eq!(discovered.title, "Kingdom speedrun");
    assert_eq!(discovered.tags, vec!["gaming"]);
    assert_eq!(discovered.creator, a.node().public_key().to_hex());
    assert_eq!(discovered.source_url, origin.base_url);
    assert!(!discovered.have_content);

    // And fetches the bytes from where the creator put them, checking every
    // chunk against the manifest it heard about over the network.
    let report = b.node().fetch_video(cid).await.expect("fetching");
    assert_eq!(report.chunks_fetched, 3);
    assert_eq!(report.bytes_fetched, original.len() as u64);
    assert_eq!(report.source_url, origin.base_url);

    let written = b.node().config().downloads_dir().join("clip.mp4");
    assert_eq!(
        std::fs::read(&written).unwrap(),
        original,
        "the file written out must be the file that was announced"
    );

    // B holds the file it asked for, and is not holding the video on behalf of
    // the network: what it passes on is the metadata, not the bytes.
    assert!(!b.node().video(&cid).unwrap().unwrap().have_content);

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_d_local_search_finds_a_video_discovered_over_the_network() {
    let a = spawn_node("publisher").await;
    let b = spawn_node("viewer").await;
    join_via_share_link(&b, &a).await;

    let source = write_sample_file(a.dir.path(), "ambient.mp4", 64 * 1024);
    let cid = publish_until_announced(a.node(), &source, "Ambient guitar set", &["music"]).await;

    let b_node = b.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        b_node.video(&cid).map(|v| v.is_some()).unwrap_or(false)
    })
    .await
    .expect("the announcement should reach B");

    assert_eq!(b.node().search("ambient", 10).unwrap().len(), 1);
    assert_eq!(b.node().search("guitar", 10).unwrap().len(), 1);
    assert_eq!(b.node().search("music", 10).unwrap().len(), 1);
    assert_eq!(b.node().search("kingdom", 10).unwrap().len(), 0);

    a.shutdown().await;
    b.shutdown().await;
}

// ---------------------------------------------------------------- Test E

#[tokio::test(flavor = "multi_thread")]
async fn test_e_a_tampered_announcement_is_rejected() {
    let a = spawn_node("liar").await;
    let b = spawn_node("careful").await;
    join_via_share_link(&b, &a).await;

    let source = write_sample_file(a.dir.path(), "real.mp4", 32 * 1024);
    let honest_cid = publish_until_announced(a.node(), &source, "Honest title", &["gaming"]).await;

    let b_node = b.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        b_node
            .video(&honest_cid)
            .map(|v| v.is_some())
            .unwrap_or(false)
    })
    .await
    .expect("the honest announcement should arrive");

    // Now A rewrites the announcement it already signed and gossips it again.
    let raw = a
        .node()
        .database()
        .announcement_bytes(&honest_cid)
        .unwrap()
        .unwrap();
    let mut forged: ovn_protocol::VideoAnnouncement = ovn_protocol::from_cbor_slice(&raw).unwrap();
    forged.title = "Free Money Click Here".to_string();
    forged.created_at += 60; // newer, so it would win if it were believed
    assert!(forged.verify().is_err(), "the forgery must not verify");

    a.node()
        .network()
        .publish_announcement(to_cbor_vec(&forged).unwrap())
        .await
        .expect("publishing the forged bytes");

    // Give it every chance to be accepted, then confirm it was not.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        b.node().video(&honest_cid).unwrap().unwrap().title,
        "Honest title",
        "a rewritten announcement must not overwrite a verified one"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e_a_completely_forged_announcement_is_never_stored() {
    let a = spawn_node("liar").await;
    let b = spawn_node("careful").await;
    join_via_share_link(&b, &a).await;

    // An announcement claiming to come from a creator A cannot sign for.
    let victim = ovn_identity::Identity::generate();
    let mut forged = ovn_protocol::VideoAnnouncement::sign(
        ovn_protocol::NewVideo {
            video_cid: Some(ContentId::from_dag_cbor(b"imaginary")),
            title: "Posted by someone else".into(),
            description: String::new(),
            tags: vec!["gaming".into()],
            duration_secs: 60,
            thumbnail_cid: None,
            source_url: "https://videos.example/clip.mp4".to_string(),
        },
        &victim,
    )
    .unwrap();
    // Keep the victim's signature but claim a different author.
    forged.creator_public_key = a.node().public_key().to_vec();

    a.node()
        .network()
        .publish_announcement(to_cbor_vec(&forged).unwrap())
        .await
        .ok();

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(b.node().database().video_count().unwrap(), 0);

    a.shutdown().await;
    b.shutdown().await;
}

// ---------------------------------------------------------------- Test F

#[tokio::test(flavor = "multi_thread")]
async fn test_f_two_viewers_with_different_habits_get_different_feeds() {
    let publisher = spawn_node("publisher").await;
    let bob = spawn_node("bob").await;
    let carol = spawn_node("carol").await;
    join_via_share_link(&bob, &publisher).await;
    join_via_share_link(&carol, &publisher).await;

    let mut published = Vec::new();
    for (name, tag) in [
        ("game-one", "gaming"),
        ("game-two", "gaming"),
        ("song-one", "music"),
        ("song-two", "music"),
    ] {
        let source = write_sample_file(publisher.dir.path(), &format!("{name}.mp4"), 16 * 1024);
        let cid = publish_until_announced(publisher.node(), &source, name, &[tag]).await;
        published.push((name, cid));
    }

    // Discovery is this test's setup, not its subject — what it checks is that
    // two viewers end up with different feeds. So the videos are pulled rather
    // than waited for: asking a peer what a creator published either works or
    // says why, where waiting on gossip is a timing assumption that a slow
    // machine turns into a flake.
    pull_until_discovered(
        &[bob.node(), carol.node()],
        &publisher.node().public_key(),
        4,
    )
    .await
    .expect("all four videos should reach both viewers");

    // Bob watches games; Carol watches music. Neither tells anyone.
    let game_one = published.iter().find(|(n, _)| *n == "game-one").unwrap().1;
    let song_one = published.iter().find(|(n, _)| *n == "song-one").unwrap().1;
    bob.node()
        .record_watch(&WatchEvent {
            cid: game_one,
            watched_secs: 600,
            duration_secs: 600,
            completed: true,
            skipped: false,
            liked: true,
        })
        .unwrap();
    carol
        .node()
        .record_watch(&WatchEvent {
            cid: song_one,
            watched_secs: 600,
            duration_secs: 600,
            completed: true,
            skipped: false,
            liked: true,
        })
        .unwrap();

    let bob_top = &bob.node().recommendations(10).unwrap()[0];
    let carol_top = &carol.node().recommendations(10).unwrap()[0];
    assert_eq!(
        bob_top.title, "game-two",
        "Bob should be offered the other game"
    );
    assert_eq!(
        carol_top.title, "song-two",
        "Carol should be offered the other song"
    );

    // The same network, the same videos, two different feeds.
    assert_ne!(bob_top.cid, carol_top.cid);

    // And each can say why.
    let why = bob
        .node()
        .explain(&ContentId::parse(&bob_top.cid).unwrap())
        .unwrap()
        .unwrap();
    assert!(why
        .contributions
        .iter()
        .any(|c| c.factor == "tag" && c.detail.as_deref() == Some("gaming")));

    publisher.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

// ---------------------------------------------------------------- Test H

#[tokio::test(flavor = "multi_thread")]
async fn test_h_the_network_survives_losing_the_node_everyone_joined_through() {
    // `hub` stands in for everything the developers might run: the bootstrap
    // node, the domain, the website. It is used to join, and then it dies.
    let hub = spawn_node("official-bootstrap").await;
    let a = spawn_node("peer-a").await;
    let b = spawn_node_fetching_locally("peer-b").await;

    join_via_share_link(&a, &hub).await;
    join_via_share_link(&b, &hub).await;

    // Through the DHT, A and B learn about each other without ever having
    // spoken. This is what makes the hub disposable.
    for peer in [&a, &b] {
        peer.node().network().bootstrap().await.unwrap();
    }
    let a_node = a.node().clone();
    let b_peer_id = b.node().peer_id().to_base58();
    wait_until(PROPAGATION_TIMEOUT, || {
        a_node
            .peers()
            .map(|peers| {
                peers
                    .iter()
                    .any(|p| p.peer_id == b_peer_id && !p.addresses.is_empty())
            })
            .unwrap_or(false)
    })
    .await
    .expect("A should learn about B through the DHT");

    // Switch off everything the developers run.
    let hub_peer_id = hub.node().peer_id().to_base58();
    hub.shutdown().await;

    // A and B reconnect to each other directly, using what they already knew.
    a.node().forget_peer(&hub_peer_id).unwrap();
    b.node().forget_peer(&hub_peer_id).unwrap();
    let b_node = b.node().clone();
    wait_until_async(PROPAGATION_TIMEOUT, || {
        let node = b_node.clone();
        async move {
            node.dial_known_peers().await;
            node.status().await.map(|s| s.connected_peers).unwrap_or(0) >= 1
        }
    })
    .await
    .expect("B should reconnect without the hub");

    // Announcement, discovery and transfer all still work.
    let source = write_sample_file(a.dir.path(), "after.mp4", 128 * 1024);
    let origin = OriginServer::serving(std::fs::read(&source).unwrap()).await;
    let cid = publish_until_announced_from(
        a.node(),
        &source,
        "Life after the bootstrap",
        &["indie"],
        &origin.base_url,
    )
    .await;

    let b_node = b.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        b_node.video(&cid).map(|v| v.is_some()).unwrap_or(false)
    })
    .await
    .expect("announcements should still propagate");

    let report = b
        .node()
        .fetch_video(cid)
        .await
        .expect("fetching still works");
    assert!(report.chunks_fetched > 0);

    // And recommendations, which never needed the network anyway.
    b.node()
        .record_watch(&WatchEvent {
            cid,
            watched_secs: 60,
            duration_secs: 60,
            completed: true,
            skipped: false,
            liked: false,
        })
        .unwrap();
    assert!(!b.node().recommendations(10).unwrap().is_empty());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_requested_through_the_api_stops_the_node() {
    // `ourvideo stop` posts to the local API from a different process. The
    // foreground `start` has to notice and exit, rather than sitting on a
    // node that is no longer running.
    let dir = tempfile::tempdir().unwrap();
    let mut config = ovn_node::NodeConfig::new(dir.path())
        .with_p2p_port(0)
        .with_api_port(0);
    config.network.enable_mdns = false;
    config.network.listen_addrs = vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()];
    let mut running = ovn_node::start(config).await.unwrap();

    let url = format!("{}/v1/shutdown", running.api_url().unwrap());
    let token = running.node().api_token().to_string();
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(url)
            .bearer_auth(token)
            .send()
            .await;
    });

    tokio::time::timeout(Duration::from_secs(10), running.stopped())
        .await
        .expect("the node should notice it was asked to stop");

    running.shutdown().await;
    // The runtime file is removed, so the CLI reports no node rather than a
    // stale one.
    assert!(!dir.path().join("runtime.json").exists());
}

// ------------------------------------------------------- moderation (§33)

#[tokio::test(flavor = "multi_thread")]
async fn blocking_a_creator_stops_their_announcements_being_stored() {
    let a = spawn_node("publisher").await;
    let b = spawn_node("viewer").await;
    join_via_share_link(&b, &a).await;

    b.node()
        .block_creator(&a.node().public_key(), "not for me")
        .unwrap();

    let source = write_sample_file(a.dir.path(), "blocked.mp4", 16 * 1024);
    let cid = publish_until_announced(a.node(), &source, "Should not appear", &[]).await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(b.node().video(&cid).unwrap().is_none());
    assert_eq!(b.node().database().video_count().unwrap(), 0);

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_content_stops_this_node_passing_it_on() {
    // Section 33: blocking is not only about what you see, it is about not
    // helping to distribute something. What this node can pass on is the
    // metadata and the manifest — the manifest being what lets anyone else
    // check the creator's file — so that is what goes.
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node_fetching_locally("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    let source = write_sample_file(publisher.dir.path(), "unwanted.mp4", 2 * 1024 * 1024 + 11);
    let origin = OriginServer::serving(std::fs::read(&source).unwrap()).await;
    let cid = publish_until_announced_from(
        publisher.node(),
        &source,
        "Unwanted",
        &["gaming"],
        &origin.base_url,
    )
    .await;

    let viewer_node = viewer.node().clone();
    wait_until(PROPAGATION_TIMEOUT, || {
        viewer_node
            .video(&cid)
            .map(|v| v.is_some())
            .unwrap_or(false)
    })
    .await
    .expect("the announcement should arrive");

    // Watching it pulls the manifest in, which is the thing this node could
    // then hand to somebody else.
    viewer.node().prepare_stream(cid).await.expect("a plan");
    assert!(
        viewer.node().storage().has(&cid),
        "the manifest should be held after watching"
    );

    viewer.node().block_cid(&cid, "not for me").unwrap();

    assert!(
        !viewer.node().storage().has(&cid),
        "a blocked video's manifest should not still be here to serve"
    );
    assert!(!viewer.node().video(&cid).unwrap().unwrap().have_manifest);
    // And it will not play here either.
    assert!(
        viewer.node().prepare_stream(cid).await.is_err(),
        "a blocked video must not stream"
    );

    publisher.shutdown().await;
    viewer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_a_creator_discards_everything_of_theirs() {
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node_fetching_locally("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    let mut cids = Vec::new();
    let mut origins = Vec::new();
    for name in ["first", "second"] {
        let source = write_sample_file(publisher.dir.path(), &format!("{name}.mp4"), 512 * 1024);
        let origin = OriginServer::serving(std::fs::read(&source).unwrap()).await;
        cids.push(
            publish_until_announced_from(publisher.node(), &source, name, &[], &origin.base_url)
                .await,
        );
        origins.push(origin);
    }

    // Setup, not subject: ask rather than wait, so a slow machine cannot turn
    // this into a flake. What this test is about is what blocking does.
    pull_until_discovered(&[viewer.node()], &publisher.node().public_key(), 2)
        .await
        .expect("both videos should reach the viewer");

    for cid in &cids {
        viewer.node().prepare_stream(*cid).await.expect("a plan");
        assert!(viewer.node().storage().has(cid));
    }

    viewer
        .node()
        .block_creator(&publisher.node().public_key(), "spam")
        .unwrap();

    for cid in &cids {
        assert!(!viewer.node().video(cid).unwrap().unwrap().have_manifest);
        assert!(
            !viewer.node().storage().has(cid),
            "nothing of a blocked creator's should still be here to serve"
        );
    }

    publisher.shutdown().await;
    viewer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_one_video_leaves_another_alone() {
    // Blocking is per video. Discarding what one video needs must not take
    // away what another the user still wants needs too.
    let publisher = spawn_node("publisher").await;
    let viewer = spawn_node_fetching_locally("viewer").await;
    join_via_share_link(&viewer, &publisher).await;

    let unwanted_file = write_sample_file(publisher.dir.path(), "unwanted.mp4", 300 * 1024);
    let wanted_file = write_seeded_file(publisher.dir.path(), "wanted.mp4", 300 * 1024, 9);
    let unwanted_origin = OriginServer::serving(std::fs::read(&unwanted_file).unwrap()).await;
    let wanted_origin = OriginServer::serving(std::fs::read(&wanted_file).unwrap()).await;

    let unwanted = publish_until_announced_from(
        publisher.node(),
        &unwanted_file,
        "Unwanted",
        &[],
        &unwanted_origin.base_url,
    )
    .await;
    let wanted = publish_until_announced_from(
        publisher.node(),
        &wanted_file,
        "Wanted",
        &[],
        &wanted_origin.base_url,
    )
    .await;

    // Setup, not subject: ask rather than wait, so a slow machine cannot turn
    // this into a flake. What this test is about is what blocking does.
    pull_until_discovered(&[viewer.node()], &publisher.node().public_key(), 2)
        .await
        .expect("both videos should reach the viewer");

    for cid in [unwanted, wanted] {
        viewer.node().prepare_stream(cid).await.expect("a plan");
    }

    viewer.node().block_cid(&unwanted, "not for me").unwrap();

    assert!(!viewer.node().storage().has(&unwanted));
    assert!(
        viewer.node().storage().has(&wanted),
        "blocking one video took away another one's manifest"
    );
    assert!(
        viewer.node().prepare_stream(wanted).await.is_ok(),
        "the video the user kept should still play"
    );

    publisher.shutdown().await;
    viewer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_our_own_video_hides_it_without_destroying_it() {
    // A node that published something may be the only record of it. Hiding it
    // locally should not be a way to lose it by accident.
    let node = spawn_node("solo").await;
    let file = write_sample_file(node.dir.path(), "mine.mp4", 200 * 1024);
    let cid = publish_until_announced(node.node(), &file, "Mine", &[]).await;

    node.node().block_cid(&cid, "second thoughts").unwrap();

    assert!(
        node.node().storage().has(&cid),
        "our own manifest should be kept"
    );
    assert!(
        node.node().prepare_stream(cid).await.is_err(),
        "it should still be hidden here"
    );

    node.shutdown().await;
}
