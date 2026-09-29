//! The handle the rest of the node uses to talk to the swarm.
//!
//! The swarm itself is not `Sync` and lives in one task. Everything else
//! interacts with it through this handle, which sends commands down a channel
//! and waits for a reply. That keeps all libp2p state in one place and means
//! the node crate never has to reason about swarm internals.

use libp2p::request_response::ResponseChannel;
use libp2p::{Multiaddr, PeerId};
use tokio::sync::{mpsc, oneshot};

use ovn_protocol::ContentId;

use crate::behaviour::BlockResponse;
use crate::{NetworkError, Result};

pub(crate) enum Command {
    Dial {
        addr: Multiaddr,
        reply: oneshot::Sender<Result<PeerId>>,
    },
    AddPeerAddress {
        peer: PeerId,
        addr: Multiaddr,
    },
    AddExternalAddress {
        addr: Multiaddr,
    },
    Publish {
        topic: &'static str,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    StartProviding {
        cid: ContentId,
        reply: oneshot::Sender<Result<()>>,
    },
    GetProviders {
        cid: ContentId,
        reply: oneshot::Sender<Vec<PeerId>>,
    },
    RequestBlock {
        peer: PeerId,
        cid: ContentId,
        reply: oneshot::Sender<Result<Vec<u8>>>,
    },
    RespondBlock {
        channel: Box<ResponseChannel<BlockResponse>>,
        response: BlockResponse,
    },
    Status {
        reply: oneshot::Sender<NetworkStatus>,
    },
    Bootstrap,
    Shutdown,
}

/// Whether other peers can dial us directly.
///
/// The thing that decides whether this node can serve content or only
/// consume it, and the reason the relay and hole-punching machinery exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Reachability {
    /// Nobody has told us yet.
    #[default]
    Unknown,
    /// Other peers can dial us. We can serve content, and relay for others.
    Public,
    /// A router stands in the way. We reach out fine, but need a relay
    /// before anyone can reach in.
    Private,
}

impl Reachability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

/// A snapshot of what the swarm is doing, for `ourvideo status`.
#[derive(Clone, Debug)]
pub struct NetworkStatus {
    pub local_peer_id: PeerId,
    pub listen_addrs: Vec<Multiaddr>,
    pub external_addrs: Vec<Multiaddr>,
    pub connected_peers: Vec<PeerId>,
    pub routing_table_peers: usize,
    pub providing: usize,
    pub reachability: Reachability,
    /// Peers relaying for us, so that others have an address to dial.
    pub relays: Vec<PeerId>,
}

/// How we came to hear about a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoverySource {
    Mdns,
    Identify,
    Dht,
}

/// Something that happened on the network and that the node should react to.
#[derive(Debug)]
pub enum NetworkEvent {
    Listening(Multiaddr),
    PeerDiscovered {
        peer: PeerId,
        addresses: Vec<Multiaddr>,
        source: DiscoverySource,
    },
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
    /// Raw CBOR from the announcement topic. Still unverified.
    GossipAnnouncement {
        from: PeerId,
        data: Vec<u8>,
    },
    /// Raw CBOR from the profile topic. Still unverified.
    GossipProfile {
        from: PeerId,
        data: Vec<u8>,
    },
    /// A peer wants a block. Answer through the responder.
    BlockRequested {
        peer: PeerId,
        cid: ContentId,
        responder: BlockResponder,
    },
    /// We learned whether other peers can dial us.
    ReachabilityChanged {
        reachability: Reachability,
    },
    /// A peer agreed to relay for us, so we now have an address others can
    /// dial even though our router will not accept connections.
    RelayReserved {
        relay: PeerId,
        address: Multiaddr,
    },
    /// A relayed connection was upgraded to a direct one, through both
    /// routers at once.
    HolePunched {
        peer: PeerId,
    },
}

/// Lets the node answer an inbound block request without touching the swarm.
#[derive(Debug)]
pub struct BlockResponder {
    channel: Box<ResponseChannel<BlockResponse>>,
    commands: mpsc::Sender<Command>,
}

impl BlockResponder {
    pub(crate) fn new(
        channel: ResponseChannel<BlockResponse>,
        commands: mpsc::Sender<Command>,
    ) -> Self {
        Self {
            channel: Box::new(channel),
            commands,
        }
    }

    pub async fn respond(self, response: BlockResponse) {
        let _ = self
            .commands
            .send(Command::RespondBlock {
                channel: self.channel,
                response,
            })
            .await;
    }
}

/// Cheap to clone; every clone talks to the same swarm task.
#[derive(Clone, Debug)]
pub struct Network {
    commands: mpsc::Sender<Command>,
    local_peer_id: PeerId,
}

impl Network {
    pub(crate) fn new(commands: mpsc::Sender<Command>, local_peer_id: PeerId) -> Self {
        Self {
            commands,
            local_peer_id,
        }
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    async fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| NetworkError::Stopped)
    }

    async fn request<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Command) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.send(make(tx)).await?;
        rx.await.map_err(|_| NetworkError::Stopped)
    }

    /// Dial a multiaddr and wait for the connection to be established.
    ///
    /// Gives up rather than waiting indefinitely: a relayed address can fail
    /// in ways the swarm never reports against the peer we asked for.
    pub async fn dial(&self, addr: Multiaddr) -> Result<PeerId> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Dial { addr, reply: tx }).await?;
        match rx.await {
            Ok(result) => result,
            // The event loop dropped the sender, which is how it reports a
            // dial that never resolved.
            Err(_) if !self.commands.is_closed() => {
                Err(NetworkError::Dial("timed out".to_string()))
            }
            Err(_) => Err(NetworkError::Stopped),
        }
    }

    /// Teach the routing table where a peer lives.
    pub async fn add_peer_address(&self, peer: PeerId, addr: Multiaddr) -> Result<()> {
        self.send(Command::AddPeerAddress { peer, addr }).await
    }

    /// Declare an address we are reachable on, for an operator who forwarded
    /// a port themselves rather than waiting to be told by other peers.
    pub async fn add_external_address(&self, addr: Multiaddr) -> Result<()> {
        self.send(Command::AddExternalAddress { addr }).await
    }

    pub async fn publish_announcement(&self, data: Vec<u8>) -> Result<()> {
        self.request(|reply| Command::Publish {
            topic: ovn_protocol::TOPIC_VIDEO_ANNOUNCE,
            data,
            reply,
        })
        .await?
    }

    pub async fn publish_profile(&self, data: Vec<u8>) -> Result<()> {
        self.request(|reply| Command::Publish {
            topic: ovn_protocol::TOPIC_PROFILE_UPDATE,
            data,
            reply,
        })
        .await?
    }

    /// Announce to the DHT that we hold this content.
    pub async fn start_providing(&self, cid: ContentId) -> Result<()> {
        self.request(|reply| Command::StartProviding { cid, reply })
            .await?
    }

    /// Ask the DHT who holds this content.
    pub async fn get_providers(&self, cid: ContentId) -> Result<Vec<PeerId>> {
        self.request(|reply| Command::GetProviders { cid, reply })
            .await
    }

    /// Fetch one block from one peer. The bytes are not verified here — the
    /// caller checks them against the content id.
    pub async fn request_block(&self, peer: PeerId, cid: ContentId) -> Result<Vec<u8>> {
        self.request(|reply| Command::RequestBlock { peer, cid, reply })
            .await?
    }

    pub async fn status(&self) -> Result<NetworkStatus> {
        self.request(|reply| Command::Status { reply }).await
    }

    pub async fn bootstrap(&self) -> Result<()> {
        self.send(Command::Bootstrap).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.send(Command::Shutdown).await
    }
}
