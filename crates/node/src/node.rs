//! The node itself: everything wired together.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use libp2p::Multiaddr;

use ovn_content::{
    extract_thumbnail, looks_like_jpeg, probe_duration_secs, BlockStore, VideoManifest,
};
use ovn_database::{
    CacheSummary, Database, PeerRecord, PeerSource, VideoRecord, VideoUpsert, WatchEvent,
};
use ovn_discovery::{dial_addresses, DescriptorFetcher, Target};
use ovn_identity::{Identity, PublicKey};
use ovn_network::{Network, NetworkStatus, PeerId};
use ovn_protocol::{
    to_cbor_vec, Capability, ContentId, NewVideo, NodeDescriptor, ProfileUpdate, VideoAnnouncement,
};
use ovn_recommendation::{Engine, PreferenceModel, Recommendation};
use ovn_storage::{EvictionReport, Storage};

use crate::config::{NodeConfig, RuntimeInfo};
use crate::progress::{EventBus, NodeEvent};
use crate::range::ByteRange;
use crate::{NodeError, Result};

/// How many chunk transfers run at once. Enough to hide the round trip
/// without turning one downloading node into a burst of load on a provider.
const CONCURRENT_CHUNK_FETCHES: usize = 4;

/// How many previously known peers to dial on start.
const STARTUP_DIAL_LIMIT: usize = 16;

pub(crate) struct Inner {
    pub config: NodeConfig,
    pub identity: Identity,
    pub db: Database,
    pub storage: Storage,
    pub network: Network,
    pub engine: Engine,
    pub started_at: u64,
    pub api_token: String,
    /// Addresses the swarm is actually listening on, as they are reported.
    pub listen_addrs: Mutex<Vec<Multiaddr>>,
    /// Live progress for anything watching the local event stream.
    pub events: EventBus,
}

/// A handle to a running node. Cheap to clone.
#[derive(Clone)]
pub struct Node {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("peer_id", &self.peer_id().to_base58())
            .finish_non_exhaustive()
    }
}

/// What a publish produced.
#[derive(Clone, Debug)]
pub struct PublishReport {
    pub video: VideoRecord,
    pub chunks: usize,
    pub total_bytes: u64,
    /// False when there was nobody to gossip to yet. The video is still
    /// published locally and discoverable through the DHT.
    pub announced_to_network: bool,
}

/// What a fetch did.
#[derive(Clone, Debug)]
pub struct FetchReport {
    pub cid: ContentId,
    pub chunks_fetched: usize,
    pub chunks_already_held: usize,
    pub bytes_fetched: u64,
    pub providers_tried: usize,
    pub eviction: EvictionReport,
}

/// Everything a streaming response needs, resolved before the first byte is
/// written.
#[derive(Clone, Debug)]
pub struct StreamPlan {
    pub cid: ContentId,
    pub manifest: VideoManifest,
    pub media_type: String,
    pub total_size: u64,
    /// Peers to ask for chunks we do not hold. Empty when we hold them all.
    pub providers: Vec<PeerId>,
}

/// The result of `peer add`.
#[derive(Clone, Debug)]
pub struct AddPeerReport {
    pub peer_id: String,
    pub node_name: String,
    pub addresses: Vec<String>,
    pub connected: bool,
}

/// Everything `ourvideo status` shows.
#[derive(Clone, Debug)]
pub struct NodeStatus {
    pub peer_id: String,
    pub public_key: String,
    pub node_name: String,
    pub started_at: u64,
    pub uptime_secs: u64,
    pub data_dir: PathBuf,
    pub listen_addrs: Vec<String>,
    pub connected_peers: usize,
    pub known_peers: i64,
    pub routing_table_peers: usize,
    pub known_videos: i64,
    pub local_videos: usize,
    pub providing: usize,
    pub cache: CacheSummary,
    pub cache_limit_bytes: u64,
}

impl Node {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    // ----------------------------------------------------------- identity

    pub fn peer_id(&self) -> PeerId {
        self.inner.network.local_peer_id()
    }

    pub fn public_key(&self) -> PublicKey {
        self.inner.identity.public_key()
    }

    pub fn config(&self) -> &NodeConfig {
        &self.inner.config
    }

    pub fn database(&self) -> &Database {
        &self.inner.db
    }

    pub fn storage(&self) -> &Storage {
        &self.inner.storage
    }

    pub fn network(&self) -> &Network {
        &self.inner.network
    }

    pub fn api_token(&self) -> &str {
        &self.inner.api_token
    }

    /// Live events: peers coming and going, videos discovered, fetch
    /// progress. Nothing here describes viewing behaviour.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<NodeEvent> {
        self.inner.events.subscribe()
    }

    pub(crate) fn emit(&self, event: NodeEvent) {
        self.inner.events.emit(event);
    }

    /// Tell every event-stream listener that this node is going away.
    pub fn notify_shutdown(&self) {
        self.inner.events.emit(NodeEvent::ShuttingDown);
    }

    /// A signed description of how to reach this node.
    pub fn descriptor(&self) -> Result<NodeDescriptor> {
        let addresses = self
            .advertised_addresses()
            .iter()
            .map(|a| a.to_string())
            .collect();
        Ok(NodeDescriptor::sign(
            self.inner.config.node_name.clone(),
            addresses,
            vec![
                Capability::video_store(),
                Capability::dht_server(),
                Capability::gossip_relay(),
            ],
            &self.inner.identity,
        )?)
    }

    /// A link that carries the descriptor, so it works with no web server.
    pub fn share_link(&self) -> Result<String> {
        Ok(ovn_discovery::share_link(&self.descriptor()?)?)
    }

    /// Addresses worth telling other people about: configured external ones
    /// first, then routable local ones, with loopback last because it is only
    /// useful to something on this machine.
    pub fn advertised_addresses(&self) -> Vec<Multiaddr> {
        let listening = self
            .inner
            .listen_addrs
            .lock()
            .map(|a| a.clone())
            .unwrap_or_default();
        let mut out: Vec<Multiaddr> = self.inner.config.external_addrs.clone();
        let mut loopback = Vec::new();
        for addr in listening {
            if is_loopback(&addr) {
                loopback.push(addr);
            } else if !out.contains(&addr) {
                out.push(addr);
            }
        }
        for addr in loopback {
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
        out.truncate(ovn_protocol::MAX_ADDRESSES);
        out
    }

    pub(crate) fn record_listen_addr(&self, addr: Multiaddr) {
        if let Ok(mut addrs) = self.inner.listen_addrs.lock() {
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
    }

    // ------------------------------------------------------------ peering

    /// `ourvideo peer add <URL-or-link-or-address>`.
    ///
    /// The user supplies one string and nothing else. Whatever it is, it ends
    /// up as a verified identity and a dialled connection, or as an error
    /// that says what went wrong.
    pub async fn add_peer(&self, input: &str) -> Result<AddPeerReport> {
        let target = Target::parse(input)?;
        let (peer_id, node_name, addresses) = match target {
            Target::Address(addr) => {
                let peer = self.inner.network.dial(addr.clone()).await?;
                (peer.to_base58(), String::new(), vec![addr])
            }
            Target::ShareLink(descriptor) => self.add_peer_from_descriptor(&descriptor).await?,
            Target::Url(url) => {
                let descriptor = DescriptorFetcher::new()?.fetch(&url).await?;
                self.add_peer_from_descriptor(&descriptor).await?
            }
        };

        let address_strings: Vec<String> = addresses.iter().map(|a| a.to_string()).collect();
        self.inner.db.upsert_peer(
            &peer_id,
            &address_strings,
            PeerSource::Url,
            Some(&node_name),
            None,
        )?;
        self.inner.db.mark_peer_connected(&peer_id)?;

        // One peer is enough to find everyone else.
        let _ = self.inner.network.bootstrap().await;

        Ok(AddPeerReport {
            peer_id,
            node_name,
            addresses: address_strings,
            connected: true,
        })
    }

    async fn add_peer_from_descriptor(
        &self,
        descriptor: &NodeDescriptor,
    ) -> Result<(String, String, Vec<Multiaddr>)> {
        descriptor.verify()?;
        let addresses = dial_addresses(descriptor)?;
        let peer: PeerId = descriptor
            .peer_id
            .parse()
            .map_err(|_| NodeError::InvalidPeerId(descriptor.peer_id.clone()))?;

        for addr in &addresses {
            self.inner
                .network
                .add_peer_address(peer, addr.clone())
                .await?;
        }

        let mut last_error = None;
        for addr in &addresses {
            match self.inner.network.dial(addr.clone()).await {
                Ok(_) => {
                    return Ok((
                        descriptor.peer_id.clone(),
                        descriptor.node_name.clone(),
                        addresses,
                    ))
                }
                Err(e) => last_error = Some(e),
            }
        }
        Err(NodeError::PeerUnreachable {
            peer_id: descriptor.peer_id.clone(),
            reason: last_error.map(|e| e.to_string()).unwrap_or_default(),
        })
    }

    pub fn peers(&self) -> Result<Vec<PeerRecord>> {
        Ok(self.inner.db.peers()?)
    }

    pub fn forget_peer(&self, peer_id: &str) -> Result<bool> {
        Ok(self.inner.db.forget_peer(peer_id)?)
    }

    /// Dial peers we have met before. This is what makes a node survive the
    /// disappearance of every bootstrap node and domain (Test H).
    pub async fn dial_known_peers(&self) -> usize {
        let candidates = match self.inner.db.dial_candidates(STARTUP_DIAL_LIMIT) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "could not read known peers");
                return 0;
            }
        };
        let mut connected = 0;
        for peer in candidates {
            for address in &peer.addresses {
                let Ok(addr) = address.parse::<Multiaddr>() else {
                    continue;
                };
                match self.inner.network.dial(addr).await {
                    Ok(_) => {
                        let _ = self.inner.db.mark_peer_connected(&peer.peer_id);
                        connected += 1;
                        break;
                    }
                    Err(_) => {
                        let _ = self.inner.db.mark_peer_failed(&peer.peer_id);
                    }
                }
            }
        }
        if connected > 0 {
            tracing::info!(connected, "reconnected to previously known peers");
        }
        connected
    }

    // ----------------------------------------------------------- publishing

    /// `ourvideo video publish <FILE>`.
    pub async fn publish_video(
        &self,
        path: impl AsRef<Path>,
        title: Option<String>,
        description: String,
        tags: Vec<String>,
    ) -> Result<PublishReport> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(NodeError::NoSuchFile(path.display().to_string()));
        }
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.emit(NodeEvent::PublishStarted {
            file_name: file_name.clone(),
        });

        let imported = self.inner.storage.import_and_pin(path)?;
        let duration_secs = probe_duration_secs(path).unwrap_or(0);
        let thumbnail_cid = self.make_thumbnail(path);
        let title = title.unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".to_string())
        });

        let announcement = VideoAnnouncement::sign(
            NewVideo {
                video_cid: Some(imported.content_id),
                title,
                description,
                tags,
                duration_secs,
                thumbnail_cid,
            },
            &self.inner.identity,
        )?;
        let bytes = to_cbor_vec(&announcement)?;

        self.inner.db.upsert_video(VideoUpsert {
            announcement: &announcement,
            announcement_bytes: &bytes,
            is_local: true,
        })?;
        self.inner
            .db
            .set_have_manifest(&imported.content_id, true)?;
        self.inner.db.set_have_content(&imported.content_id, true)?;

        // Tell the DHT we hold it, then gossip the metadata.
        if let Err(e) = self
            .inner
            .network
            .start_providing(imported.content_id)
            .await
        {
            tracing::warn!(error = %e, "could not announce as a provider");
        }
        let announced = match self.inner.network.publish_announcement(bytes).await {
            Ok(()) => true,
            Err(ovn_network::NetworkError::NoPeers) => {
                tracing::info!("no peers subscribed yet; the video is published locally");
                false
            }
            Err(e) => return Err(e.into()),
        };

        let video = self
            .inner
            .db
            .video(&imported.content_id)?
            .ok_or(NodeError::NotFound)?;
        self.emit(NodeEvent::PublishCompleted {
            cid: video.cid.clone(),
            title: video.title.clone(),
        });
        Ok(PublishReport {
            video,
            chunks: imported.manifest.chunks.len(),
            total_bytes: imported.manifest.total_size,
            announced_to_network: announced,
        })
    }

    /// Publish a signed profile so other nodes can show a creator name.
    pub async fn publish_profile(&self, display_name: String, bio: String) -> Result<()> {
        let profile = ProfileUpdate::sign(display_name, bio, &self.inner.identity)?;
        self.inner.db.upsert_creator(&profile)?;
        let bytes = to_cbor_vec(&profile)?;
        match self.inner.network.publish_profile(bytes).await {
            Ok(()) | Err(ovn_network::NetworkError::NoPeers) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // ------------------------------------------------------------ fetching

    /// Fetch a video's manifest and every chunk it needs.
    pub async fn fetch_video(&self, cid: ContentId) -> Result<FetchReport> {
        if self.inner.db.is_cid_blocked(&cid)? {
            return Err(NodeError::Blocked(cid.to_string()));
        }
        let providers = self.providers_for(cid).await?;
        if providers.is_empty() {
            return Err(NodeError::NoProviders(cid.to_string()));
        }

        let manifest = self.fetch_manifest(cid, &providers).await?;
        self.inner.db.set_have_manifest(&cid, true)?;

        let missing = self.inner.storage.store().missing_chunks(&manifest);
        let already_held = manifest.chunks.len() - missing.len();
        self.emit(NodeEvent::FetchStarted {
            cid: cid.to_string(),
            total_chunks: manifest.chunks.len(),
            already_held,
        });

        let fetched = match self
            .fetch_chunks(cid, &missing, &providers, already_held)
            .await
        {
            Ok(fetched) => fetched,
            Err(e) => {
                self.emit(NodeEvent::FetchFailed {
                    cid: cid.to_string(),
                    error: e.to_string(),
                });
                return Err(e);
            }
        };
        let bytes_fetched = fetched.iter().sum::<u64>();
        self.emit(NodeEvent::FetchCompleted {
            cid: cid.to_string(),
            bytes_fetched,
        });

        if self.inner.storage.store().has_all_chunks(&manifest) {
            self.inner.db.set_have_content(&cid, true)?;
            // Now that we hold it, offer it to others.
            let _ = self.inner.network.start_providing(cid).await;
        }

        let eviction = self.inner.storage.enforce_limit()?;
        Ok(FetchReport {
            cid,
            chunks_fetched: fetched.len(),
            chunks_already_held: already_held,
            bytes_fetched,
            providers_tried: providers.len(),
            eviction,
        })
    }

    async fn providers_for(&self, cid: ContentId) -> Result<Vec<PeerId>> {
        let mut providers: Vec<PeerId> = self.inner.network.get_providers(cid).await?;
        // Peers we are already connected to are worth asking even if the DHT
        // has not indexed them yet — which is the normal case on a small or
        // brand new network.
        let status = self.inner.network.status().await?;
        for peer in status.connected_peers {
            if !providers.contains(&peer) {
                providers.push(peer);
            }
        }
        Ok(providers)
    }

    async fn fetch_manifest(&self, cid: ContentId, providers: &[PeerId]) -> Result<VideoManifest> {
        if let Some(bytes) = self.inner.storage.try_get(&cid)? {
            return Ok(VideoManifest::from_bytes(&bytes)?);
        }
        let bytes = self.fetch_block(cid, providers).await?;
        let manifest = VideoManifest::from_bytes(&bytes)?;
        Ok(manifest)
    }

    /// Ask each provider in turn for one block, verifying every answer.
    ///
    /// A peer that sends bytes that do not hash to what we asked for is
    /// skipped: section 20 in practice.
    async fn fetch_block(&self, cid: ContentId, providers: &[PeerId]) -> Result<Vec<u8>> {
        let mut last_error = None;
        for peer in providers {
            match self.inner.network.request_block(*peer, cid).await {
                Ok(data) => match self.inner.storage.put_from_peer(&cid, &data) {
                    Ok(()) => return Ok(data),
                    Err(e) => {
                        tracing::warn!(%peer, %cid, error = %e, "peer served bytes that failed verification");
                        last_error = Some(e.to_string());
                    }
                },
                Err(e) => last_error = Some(e.to_string()),
            }
        }
        Err(NodeError::BlockUnavailable {
            cid: cid.to_string(),
            reason: last_error.unwrap_or_else(|| "no provider answered".to_string()),
        })
    }

    async fn fetch_chunks(
        &self,
        video: ContentId,
        missing: &[ContentId],
        providers: &[PeerId],
        already_held: usize,
    ) -> Result<Vec<u64>> {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

        let total_chunks = already_held + missing.len();
        let done = AtomicUsize::new(already_held);
        let bytes = AtomicU64::new(0);
        let (done, bytes) = (&done, &bytes);

        let results: Vec<Result<u64>> = futures::stream::iter(missing.iter().copied())
            .map(|cid| async move {
                let data = self.fetch_block(cid, providers).await?;
                let len = data.len() as u64;
                // Chunks finish out of order, so report a count rather than
                // an index: a progress bar only needs "how many of how many".
                let completed = done.fetch_add(1, Ordering::Relaxed) + 1;
                let so_far = bytes.fetch_add(len, Ordering::Relaxed) + len;
                self.emit(NodeEvent::FetchProgress {
                    cid: video.to_string(),
                    completed_chunks: completed,
                    total_chunks,
                    bytes_fetched: so_far,
                });
                Ok(len)
            })
            .buffer_unordered(CONCURRENT_CHUNK_FETCHES)
            .collect()
            .await;
        results.into_iter().collect()
    }

    /// Write a fetched video back out as a playable file.
    pub fn export_video(&self, cid: ContentId, out_path: Option<PathBuf>) -> Result<PathBuf> {
        let bytes = self
            .inner
            .storage
            .try_get(&cid)?
            .ok_or(NodeError::NotFetched(cid.to_string()))?;
        let manifest = VideoManifest::from_bytes(&bytes)?;
        let out_path = out_path.unwrap_or_else(|| {
            self.inner
                .config
                .downloads_dir()
                .join(sanitise_file_name(&manifest.file_name))
        });
        self.inner.storage.store().assemble(&manifest, &out_path)?;
        Ok(out_path)
    }

    pub fn manifest(&self, cid: &ContentId) -> Result<Option<VideoManifest>> {
        match self.inner.storage.try_get(cid)? {
            Some(bytes) => Ok(Some(VideoManifest::from_bytes(&bytes)?)),
            None => Ok(None),
        }
    }

    // ---------------------------------------------------------- thumbnails

    /// Generate a thumbnail for a file being published and store it as a
    /// pinned block. Returns `None` when FFmpeg is unavailable or the file
    /// has no decodable frame — a publish never fails over a thumbnail.
    fn make_thumbnail(&self, path: &Path) -> Option<ContentId> {
        let jpeg = extract_thumbnail(path)?;
        let cid = self.inner.storage.store().put_raw(&jpeg).ok()?;
        let _ = self.inner.db.record_cached(&cid, jpeg.len() as u64, true);
        tracing::debug!(%cid, bytes = jpeg.len(), "generated a thumbnail");
        Some(cid)
    }

    /// The thumbnail for a video, fetching it from a peer if we do not hold
    /// it. Thumbnails are ordinary blocks, so this is the normal transfer
    /// path with the normal integrity check.
    ///
    /// The bytes are checked to actually be a JPEG before being returned: a
    /// peer can serve whatever hashes correctly, and this is served to a
    /// browser.
    pub async fn thumbnail(&self, video: &ContentId) -> Result<Option<Vec<u8>>> {
        let Some(record) = self.inner.db.video(video)? else {
            return Ok(None);
        };
        let Some(thumbnail_cid) = record.thumbnail_cid else {
            return Ok(None);
        };
        let cid = ContentId::parse(&thumbnail_cid)?;

        let data = match self.inner.storage.try_get(&cid)? {
            Some(data) => data,
            None => {
                let providers = self.providers_for(*video).await?;
                if providers.is_empty() {
                    return Ok(None);
                }
                match self.fetch_block(cid, &providers).await {
                    Ok(data) => data,
                    Err(e) => {
                        tracing::debug!(%cid, error = %e, "could not fetch a thumbnail");
                        return Ok(None);
                    }
                }
            }
        };

        if !looks_like_jpeg(&data) {
            tracing::warn!(%cid, "a thumbnail block is not a JPEG; refusing to serve it");
            return Ok(None);
        }
        Ok(Some(data))
    }

    // ----------------------------------------------------------- streaming

    /// Work out what a streaming request needs before any bytes are sent.
    ///
    /// The manifest is fetched if we do not have it, so a video can be played
    /// without being downloaded first.
    pub async fn prepare_stream(&self, cid: ContentId) -> Result<StreamPlan> {
        if self.inner.db.is_cid_blocked(&cid)? {
            return Err(NodeError::Blocked(cid.to_string()));
        }
        let manifest = match self.manifest(&cid)? {
            Some(manifest) => manifest,
            None => {
                let providers = self.providers_for(cid).await?;
                if providers.is_empty() {
                    return Err(NodeError::NoProviders(cid.to_string()));
                }
                let manifest = self.fetch_manifest(cid, &providers).await?;
                self.inner.db.set_have_manifest(&cid, true)?;
                manifest
            }
        };
        // Resolved once, then reused for every chunk in the response.
        let providers = if self.inner.storage.store().has_all_chunks(&manifest) {
            Vec::new()
        } else {
            self.providers_for(cid).await?
        };
        Ok(StreamPlan {
            cid,
            media_type: manifest.media_type.clone(),
            total_size: manifest.total_size,
            manifest,
            providers,
        })
    }

    /// Bytes for one byte range, as a stream.
    ///
    /// Chunks are produced in order and fetched on demand, so playback can
    /// start on the first chunk rather than the last. A chunk that cannot be
    /// fetched ends the stream with an error, which the player sees as a
    /// truncated response.
    pub fn stream_range(
        &self,
        plan: StreamPlan,
        range: ByteRange,
    ) -> impl futures::Stream<Item = std::result::Result<Vec<u8>, std::io::Error>> + Send {
        let node = self.clone();
        let chunk_size = plan.manifest.chunk_size as u64;
        let indices = range.chunk_indices(chunk_size);
        let state = (node, plan, range, *indices.start(), *indices.end());

        futures::stream::unfold(state, move |(node, plan, range, index, last)| async move {
            if index > last {
                return None;
            }
            let chunk_cid = plan.manifest.chunks.get(index).copied()?;
            let data = match node.block_for_stream(chunk_cid, &plan.providers).await {
                Ok(data) => data,
                Err(e) => {
                    return Some((
                        Err(std::io::Error::other(e.to_string())),
                        (node, plan, range, last + 1, last),
                    ))
                }
            };

            // Trim the first and last chunks to the requested range.
            let chunk_start = index as u64 * chunk_size;
            let from = range.start.saturating_sub(chunk_start) as usize;
            let to = ((range.end - chunk_start + 1) as usize).min(data.len());
            let slice = data.get(from..to).unwrap_or_default().to_vec();

            Some((Ok(slice), (node, plan, range, index + 1, last)))
        })
    }

    async fn block_for_stream(&self, cid: ContentId, providers: &[PeerId]) -> Result<Vec<u8>> {
        if let Some(data) = self.inner.storage.try_get(&cid)? {
            return Ok(data);
        }
        if providers.is_empty() {
            return Err(NodeError::NoProviders(cid.to_string()));
        }
        self.fetch_block(cid, providers).await
    }

    // -------------------------------------------------------------- browse

    pub fn videos(&self, limit: usize, offset: usize) -> Result<Vec<VideoRecord>> {
        Ok(self.inner.db.videos(limit, offset)?)
    }

    pub fn local_videos(&self) -> Result<Vec<VideoRecord>> {
        Ok(self.inner.db.local_videos()?)
    }

    pub fn video(&self, cid: &ContentId) -> Result<Option<VideoRecord>> {
        Ok(self.inner.db.video(cid)?)
    }

    /// Local full-text search. The query does not leave this process.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<VideoRecord>> {
        Ok(self.inner.db.search_videos(query, limit)?)
    }

    // ------------------------------------------------- local-only viewing

    /// Record that the user watched something, and refresh the model.
    ///
    /// Nothing here is sent anywhere. There is no code path from this call to
    /// the network.
    pub fn record_watch(&self, event: &WatchEvent) -> Result<()> {
        self.inner.db.record_watch(event)?;
        self.inner.engine.refresh(&self.inner.db)?;
        Ok(())
    }

    pub fn recommendations(&self, limit: usize) -> Result<Vec<Recommendation>> {
        Ok(self.inner.engine.recommend(&self.inner.db, limit)?)
    }

    pub fn explain(&self, cid: &ContentId) -> Result<Option<Recommendation>> {
        Ok(self.inner.engine.explain(&self.inner.db, cid)?)
    }

    pub fn preference_model(&self) -> Result<PreferenceModel> {
        Ok(self.inner.engine.stored_model(&self.inner.db)?)
    }

    pub fn clear_local_history(&self) -> Result<()> {
        self.inner.db.clear_watch_history()?;
        self.inner.db.clear_preferences()?;
        Ok(())
    }

    // ---------------------------------------------------------- moderation

    pub fn block_cid(&self, cid: &ContentId, reason: &str) -> Result<()> {
        Ok(self.inner.db.block_cid(cid, reason)?)
    }

    pub fn unblock_cid(&self, cid: &ContentId) -> Result<bool> {
        Ok(self.inner.db.unblock_cid(cid)?)
    }

    pub fn block_creator(&self, key: &PublicKey, reason: &str) -> Result<()> {
        Ok(self.inner.db.block_creator(key, reason)?)
    }

    pub fn unblock_creator(&self, key: &PublicKey) -> Result<bool> {
        Ok(self.inner.db.unblock_creator(key)?)
    }

    pub fn follow(&self, key: &PublicKey, follow: bool) -> Result<()> {
        Ok(self.inner.db.set_following(key, follow)?)
    }

    // -------------------------------------------------------------- status

    pub async fn status(&self) -> Result<NodeStatus> {
        let network: NetworkStatus = self.inner.network.status().await?;
        let cache = self.inner.storage.usage()?;
        Ok(NodeStatus {
            peer_id: network.local_peer_id.to_base58(),
            public_key: self.public_key().to_hex(),
            node_name: self.inner.config.node_name.clone(),
            started_at: self.inner.started_at,
            uptime_secs: ovn_protocol::now_secs().saturating_sub(self.inner.started_at),
            data_dir: self.inner.config.data_dir.clone(),
            listen_addrs: self
                .advertised_addresses()
                .iter()
                .map(|a| a.to_string())
                .collect(),
            connected_peers: network.connected_peers.len(),
            known_peers: self.inner.db.peer_count()?,
            routing_table_peers: network.routing_table_peers,
            known_videos: self.inner.db.video_count()?,
            local_videos: self.inner.db.local_videos()?.len(),
            providing: network.providing,
            cache,
            cache_limit_bytes: self.inner.storage.config().cache_limit_bytes,
        })
    }

    /// Re-announce everything we hold, so a restarted node is findable again.
    pub async fn reprovide(&self) -> Result<usize> {
        let mut count = 0;
        let mut seen = HashSet::new();
        for video in self.inner.db.local_videos()? {
            let Ok(cid) = ContentId::parse(&video.cid) else {
                continue;
            };
            if seen.insert(cid) && self.inner.network.start_providing(cid).await.is_ok() {
                count += 1;
            }
        }
        for video in self.inner.db.videos(512, 0)? {
            if !video.have_content {
                continue;
            }
            let Ok(cid) = ContentId::parse(&video.cid) else {
                continue;
            };
            if seen.insert(cid) && self.inner.network.start_providing(cid).await.is_ok() {
                count += 1;
            }
        }
        Ok(count)
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _ = std::fs::remove_file(self.inner.config.runtime_path());
        self.inner.network.shutdown().await?;
        Ok(())
    }
}

/// Assemble the pieces. Called by [`crate::start`].
pub(crate) fn build_inner(
    config: NodeConfig,
    identity: Identity,
    db: Database,
    network: Network,
    api_token: String,
) -> Result<Arc<Inner>> {
    let store = BlockStore::open(config.blocks_dir())?;
    let storage = Storage::new(store, db.clone(), config.storage);
    storage.reconcile()?;
    Ok(Arc::new(Inner {
        identity,
        db,
        storage,
        network,
        engine: Engine::default(),
        events: EventBus::new(),
        started_at: ovn_protocol::now_secs(),
        api_token,
        listen_addrs: Mutex::new(Vec::new()),
        config,
    }))
}

pub(crate) fn write_runtime_info(
    config: &NodeConfig,
    api_url: &str,
    api_token: &str,
    peer_id: &str,
    started_at: u64,
) -> Result<()> {
    let info = RuntimeInfo {
        pid: std::process::id(),
        api_url: api_url.to_string(),
        api_token: api_token.to_string(),
        peer_id: peer_id.to_string(),
        started_at,
    };
    let path = config.runtime_path();
    let json = serde_json::to_vec_pretty(&info)
        .map_err(|e| NodeError::Runtime(format!("encoding runtime info: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| NodeError::Runtime(format!("writing {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Contains the API token, so it is readable only by its owner.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn is_loopback(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| match p {
        libp2p::multiaddr::Protocol::Ip4(ip) => ip.is_loopback(),
        libp2p::multiaddr::Protocol::Ip6(ip) => ip.is_loopback(),
        _ => false,
    })
}

/// Keep a downloaded file name from escaping the downloads directory.
pub(crate) fn sanitise_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        "video".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_file_names_survive_sanitising() {
        assert_eq!(sanitise_file_name("clip.mp4"), "clip.mp4");
        assert_eq!(sanitise_file_name("ゲーム実況.mp4"), "ゲーム実況.mp4");
    }

    #[test]
    fn file_names_cannot_escape_the_downloads_directory() {
        // A manifest comes from a stranger, so its file name is hostile input.
        // Whatever comes out must be a single, ordinary path component.
        for hostile in [
            "../../etc/passwd",
            "/absolute",
            "..",
            "....",
            "a\0b",
            "C:\\Windows\\system32",
            "",
            "   ",
            "..\\..\\secrets",
        ] {
            let safe = sanitise_file_name(hostile);
            assert!(!safe.is_empty(), "{hostile:?} produced an empty name");
            assert!(
                !safe.contains('/') && !safe.contains('\\') && !safe.contains('\0'),
                "{hostile:?} produced {safe:?}"
            );
            assert_ne!(safe, ".");
            assert_ne!(safe, "..");
            let joined = std::path::Path::new("/downloads").join(&safe);
            assert_eq!(
                joined.components().count(),
                3,
                "{hostile:?} produced {safe:?}, which is not one component"
            );
            assert!(joined.starts_with("/downloads"));
        }
    }

    #[test]
    fn loopback_addresses_are_recognised() {
        assert!(is_loopback(&"/ip4/127.0.0.1/tcp/1".parse().unwrap()));
        assert!(is_loopback(&"/ip6/::1/tcp/1".parse().unwrap()));
        assert!(!is_loopback(&"/ip4/192.0.2.1/tcp/1".parse().unwrap()));
    }
}
