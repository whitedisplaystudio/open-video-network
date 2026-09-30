//! Channel subscription: following a person rather than a machine.
//!
//! The promise being tested is the one that makes a subscription worth
//! having — subscribe once, and afterwards you can *check*. Gossip already
//! delivers an announcement to whoever happens to be connected when it is
//! made; that is luck, not a subscription.

mod support;

use std::time::Duration;

use ovn_identity::PublicKey;
use ovn_protocol::ChannelLink;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_link_carries_the_creator_and_not_the_machine() {
    let creator = spawn_node("creator").await;
    let link = creator.node().channel_link().expect("a channel link");
    assert!(link.starts_with("ourvideo://c/"), "{link}");

    // What it names is the identity, which is what outlives the machine.
    let parsed = ovn_discovery::parse_channel_link(&link).expect("a valid link");
    assert_eq!(
        parsed.public_key,
        creator.node().public_key().to_vec(),
        "a channel link should name the creator's key"
    );

    creator.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribing_finds_what_was_published_before_you_subscribed() {
    // The case gossip cannot serve: the video was announced while the
    // subscriber was not even running.
    let creator = spawn_node("creator").await;
    let file = write_sample_file(creator.dir.path(), "clip.bin", 120_000);
    let cid = publish_until_announced(creator.node(), &file, "Earlier", &["news"]).await;

    let viewer = spawn_node("viewer").await;
    assert!(
        viewer.node().video(&cid).unwrap().is_none(),
        "the viewer must start out not knowing about it"
    );

    let link = creator.node().channel_link().unwrap();
    let report = viewer
        .node()
        .subscribe_channel(&link)
        .await
        .expect("subscribing");

    assert_eq!(report.display_name, "creator");
    assert!(
        report.new_videos >= 1,
        "subscribing should have found the earlier video, found {}",
        report.new_videos
    );
    assert!(
        viewer.node().video(&cid).unwrap().is_some(),
        "the video should now be known to the viewer"
    );

    viewer.shutdown().await;
    creator.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_still_works_when_the_creator_is_offline() {
    // The property that makes this a network rather than a link to one
    // machine. Announcements are signed by the creator, so a third node that
    // kept them can answer for a creator that is switched off — and cannot
    // forge anything while doing it.
    let creator = spawn_node("creator").await;
    let keeper = spawn_node("keeper").await;
    join_via_share_link(&keeper, &creator).await;

    let file = write_sample_file(creator.dir.path(), "clip.bin", 120_000);
    let cid = publish_until_announced(creator.node(), &file, "Broadcast", &["news"]).await;
    wait_until(PROPAGATION_TIMEOUT, || {
        keeper.node().video(&cid).ok().flatten().is_some()
    })
    .await
    .expect("the keeper hears the announcement");

    let link = creator.node().channel_link().unwrap();
    let creator_key = creator.node().public_key();

    // The creator goes away entirely.
    creator.shutdown().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let viewer = spawn_node("viewer").await;
    join_via_share_link(&viewer, &keeper).await;
    let report = viewer
        .node()
        .subscribe_channel(&link)
        .await
        .expect("subscribing to an offline creator");

    assert!(
        report.new_videos >= 1,
        "somebody who kept the announcement should have been able to answer"
    );
    let known = viewer.node().channel_videos(&creator_key).unwrap();
    assert_eq!(known.len(), 1, "{known:?}");
    assert_eq!(known[0].title, "Broadcast");

    viewer.shutdown().await;
    keeper.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checking_again_picks_up_what_was_published_since() {
    // "Subscribe once, then you can check."
    let creator = spawn_node("creator").await;
    let viewer = spawn_node("viewer").await;

    let first = write_sample_file(creator.dir.path(), "one.bin", 120_000);
    publish_until_announced(creator.node(), &first, "First", &["news"]).await;

    let link = creator.node().channel_link().unwrap();
    viewer
        .node()
        .subscribe_channel(&link)
        .await
        .expect("subscribe");
    let creator_key = creator.node().public_key();
    assert_eq!(viewer.node().channel_videos(&creator_key).unwrap().len(), 1);

    // Published after the subscription, with nobody watching for it.
    let second = write_seeded_file(creator.dir.path(), "two.bin", 120_000, 7);
    publish_until_announced(creator.node(), &second, "Second", &["news"]).await;

    wait_until_async(PROPAGATION_TIMEOUT, || async {
        viewer.node().refresh_channel(&creator_key).await.ok();
        viewer
            .node()
            .channel_videos(&creator_key)
            .map(|v| v.len() >= 2)
            .unwrap_or(false)
    })
    .await
    .expect("checking again should find the newer video");

    viewer.shutdown().await;
    creator.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_cannot_answer_a_channel_request_with_somebody_elses_work() {
    // A channel request names a creator. An answering node chooses what to
    // send, but every announcement carries that creator's signature, so it
    // cannot pass off its own videos as theirs.
    let creator = spawn_node("creator").await;
    let liar = spawn_node("liar").await;
    let viewer = spawn_node("viewer").await;
    join_via_share_link(&liar, &creator).await;
    join_via_share_link(&viewer, &liar).await;

    let theirs = write_sample_file(liar.dir.path(), "mine.bin", 120_000);
    let forged = publish_until_announced(liar.node(), &theirs, "Not theirs", &["fake"]).await;

    let link = creator.node().channel_link().unwrap();
    viewer
        .node()
        .subscribe_channel(&link)
        .await
        .expect("subscribe");

    let creator_key = creator.node().public_key();
    let attributed = viewer.node().channel_videos(&creator_key).unwrap();
    assert!(
        attributed.iter().all(|v| v.cid != forged.to_string()),
        "the liar's own video was attributed to the creator: {attributed:?}"
    );

    viewer.shutdown().await;
    liar.shutdown().await;
    creator.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forged_channel_link_is_refused() {
    let viewer = spawn_node("viewer").await;
    let identity = ovn_identity::Identity::generate();
    let mut link = ChannelLink::sign("Real Person".into(), vec![], &identity).unwrap();
    link.display_name = "Someone Else".into();
    let encoded = ovn_discovery::channel_link(&link).unwrap();

    assert!(
        viewer.node().subscribe_channel(&encoded).await.is_err(),
        "a link whose name was changed after signing must not be accepted"
    );
    assert!(viewer.node().subscriptions().unwrap().is_empty());

    viewer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_link_pasted_where_a_channel_belongs_says_which_is_which() {
    // Both are `ourvideo://` links and people will mix them up.
    let node = spawn_node("node").await;
    let share = node.node().share_link().unwrap();
    let error = node
        .node()
        .subscribe_channel(&share)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("channel link"), "{error}");

    let channel = node.node().channel_link().unwrap();
    let error = node
        .node()
        .add_peer(&channel)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("channel link"), "{error}");

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unsubscribing_leaves_the_videos_but_drops_the_subscription() {
    let creator = spawn_node("creator").await;
    let viewer = spawn_node("viewer").await;
    let file = write_sample_file(creator.dir.path(), "clip.bin", 120_000);
    publish_until_announced(creator.node(), &file, "Clip", &["news"]).await;

    let link = creator.node().channel_link().unwrap();
    viewer
        .node()
        .subscribe_channel(&link)
        .await
        .expect("subscribe");
    let key = creator.node().public_key();
    assert_eq!(viewer.node().subscriptions().unwrap().len(), 1);

    viewer.node().unsubscribe(&key).expect("unsubscribe");
    assert!(viewer.node().subscriptions().unwrap().is_empty());
    // Unsubscribing is not deleting: what was already discovered stays.
    assert!(!viewer.node().channel_videos(&key).unwrap().is_empty());

    viewer.shutdown().await;
    creator.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn who_you_subscribe_to_is_not_announced() {
    // Subscribing is a local decision about where this device goes asking.
    // It is not a signal, and nobody is told.
    let creator = spawn_node("creator").await;
    let viewer = spawn_node("viewer").await;
    join_via_share_link(&viewer, &creator).await;

    let link = creator.node().channel_link().unwrap();
    viewer
        .node()
        .subscribe_channel(&link)
        .await
        .expect("subscribe");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The creator learns nothing about having gained a subscriber.
    let key = PublicKey::from_hex(&viewer.node().public_key().to_hex()).unwrap();
    assert!(
        creator.node().channel_videos(&key).unwrap().is_empty(),
        "the creator should hold nothing about the viewer"
    );
    assert!(
        creator.node().subscriptions().unwrap().is_empty(),
        "subscribing must not create a subscription on the other side"
    );

    viewer.shutdown().await;
    creator.shutdown().await;
}
