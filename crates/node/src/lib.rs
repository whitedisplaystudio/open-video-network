//! The node: one command to start, everything else wired underneath.
//!
//! ```text
//! ourvideo start
//!     ├── identity      generate or load an Ed25519 key
//!     ├── data dir      create it if this is a first run
//!     ├── SQLite        open and migrate
//!     ├── block store   open and reconcile with the database
//!     ├── libp2p        listen, subscribe, join the DHT
//!     ├── peers         redial everyone we knew before
//!     └── local API     bind to loopback for the CLI and any future GUI
//! ```
//!
//! No step asks the user for anything.

mod api;
mod config;
mod dto;
mod events;
mod node;

use std::net::SocketAddr;

use tokio::task::JoinHandle;

use ovn_database::Database;
use ovn_identity::Identity;

pub use config::{NodeConfig, RuntimeInfo, DEFAULT_API_PORT};
pub use node::{AddPeerReport, FetchReport, Node, NodeStatus, PublishReport};
pub use ovn_network::DEFAULT_P2P_PORT;

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("could not prepare the data directory {path}: {source}")]
    DataDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Identity(#[from] ovn_identity::IdentityError),
    #[error(transparent)]
    Database(#[from] ovn_database::DatabaseError),
    #[error(transparent)]
    Content(#[from] ovn_content::ContentError),
    #[error(transparent)]
    Storage(#[from] ovn_storage::StorageError),
    #[error(transparent)]
    Network(#[from] ovn_network::NetworkError),
    #[error(transparent)]
    Protocol(#[from] ovn_protocol::ProtocolError),
    #[error(transparent)]
    Discovery(#[from] ovn_discovery::DiscoveryError),
    #[error(transparent)]
    Recommendation(#[from] ovn_recommendation::RecommendationError),
    #[error("`{0}` is not a valid peer id")]
    InvalidPeerId(String),
    #[error("could not reach {peer_id}: {reason}")]
    PeerUnreachable { peer_id: String, reason: String },
    #[error("no such file: {0}")]
    NoSuchFile(String),
    #[error("not found")]
    NotFound,
    #[error("nobody on the network is offering {0}")]
    NoProviders(String),
    #[error("could not fetch {cid}: {reason}")]
    BlockUnavailable { cid: String, reason: String },
    #[error("{0} has not been fetched yet")]
    NotFetched(String),
    #[error("{0} is blocked on this node")]
    Blocked(String),
    #[error("the local API could not start on {addr}: {source}")]
    ApiBind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime state: {0}")]
    Runtime(String),
}

pub type Result<T> = std::result::Result<T, NodeError>;

/// A started node and the tasks keeping it alive.
pub struct RunningNode {
    node: Node,
    network_task: JoinHandle<()>,
    /// Whether `network_task` has already been awaited to completion. A
    /// `JoinHandle` panics if it is polled again afterwards, and both
    /// `stopped` and `shutdown` want to wait on it.
    network_finished: bool,
    event_task: JoinHandle<()>,
    api: Option<api::ApiServer>,
}

impl RunningNode {
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// Where the local API is listening, if it was started.
    pub fn api_addr(&self) -> Option<SocketAddr> {
        self.api.as_ref().map(|a| a.addr)
    }

    pub fn api_url(&self) -> Option<String> {
        self.api_addr().map(|addr| format!("http://{addr}"))
    }

    /// Resolves when the node has stopped for any reason.
    ///
    /// A stop can come from outside this process — `ourvideo stop` posts to
    /// the local API — so a foreground `start` has to wait on this as well as
    /// on Ctrl-C, or it would sit there holding a node that is already gone.
    pub async fn stopped(&mut self) {
        if self.network_finished {
            return;
        }
        // Awaiting through a mutable borrow keeps the handle usable if this
        // future is dropped, which is what happens when it loses a `select!`.
        let _ = (&mut self.network_task).await;
        self.network_finished = true;
    }

    /// Stop everything and wait for the tasks to finish.
    pub async fn shutdown(self) {
        if let Some(api) = self.api {
            api.shutdown().await;
        }
        let _ = self.node.shutdown().await;
        let _ = self.event_task.await;
        if !self.network_finished {
            let _ = self.network_task.await;
        }
    }
}

/// Start a node: the whole of `ourvideo start`.
pub async fn start(config: NodeConfig) -> Result<RunningNode> {
    std::fs::create_dir_all(&config.data_dir).map_err(|source| NodeError::DataDir {
        path: config.data_dir.display().to_string(),
        source,
    })?;
    std::fs::create_dir_all(config.downloads_dir()).map_err(|source| NodeError::DataDir {
        path: config.downloads_dir().display().to_string(),
        source,
    })?;

    let identity = Identity::load_or_create(config.identity_path())?;
    let db = Database::open(config.database_path())?;
    let api_token = generate_api_token();

    let (network, network_events, network_task) =
        ovn_network::spawn(&identity, config.network.clone())?;

    let inner = node::build_inner(config, identity, db, network, api_token)?;
    let node = Node::new(inner);
    let event_task = tokio::spawn(events::run(node.clone(), network_events));

    let api = if node.config().enable_api {
        Some(api::serve(node.clone(), node.config().api_addr).await?)
    } else {
        None
    };

    let api_url = api
        .as_ref()
        .map(|a| format!("http://{}", a.addr))
        .unwrap_or_default();
    node::write_runtime_info(
        node.config(),
        &api_url,
        node.api_token(),
        &node.peer_id().to_base58(),
        ovn_protocol::now_secs(),
    )?;

    // Rejoin in the background so `start` returns as soon as the node is
    // usable; a slow or unreachable peer must not hold up the command.
    let joining = node.clone();
    tokio::spawn(async move {
        joining.dial_known_peers().await;
        let _ = joining.network().bootstrap().await;
        match joining.reprovide().await {
            Ok(n) if n > 0 => tracing::info!(count = n, "re-announced held content"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not re-announce held content"),
        }
    });

    tracing::info!(
        peer_id = %node.peer_id(),
        data_dir = %node.config().data_dir.display(),
        "node started"
    );

    Ok(RunningNode {
        node,
        network_task,
        network_finished: false,
        event_task,
        api,
    })
}

/// A 32-byte random bearer token for the local API, hex encoded.
fn generate_api_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system provides randomness");
    data_encoding::HEXLOWER.encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_tokens_are_long_and_unique() {
        let a = generate_api_token();
        let b = generate_api_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
