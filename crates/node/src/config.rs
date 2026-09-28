//! Where the node keeps its things, and what it is allowed to use.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use libp2p::Multiaddr;

use ovn_network::{default_listen_addrs, NetworkConfig};
use ovn_storage::{StorageConfig, DEFAULT_CACHE_LIMIT_BYTES};

/// Default port for the local HTTP API. Bound to loopback only.
pub const DEFAULT_API_PORT: u16 = 4801;

/// How the local API decides whether a request is allowed.
///
/// Both settings bind to loopback and refuse a request whose `Host` is not a
/// loopback name; the difference is only whether a bearer token or session
/// cookie is required on top of that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ApiAuth {
    /// Require the token. Protects your viewing history from other accounts
    /// on this machine.
    #[default]
    Token,
    /// Trust anything that reaches the loopback interface.
    ///
    /// On a machine you do not share, this makes the interface work from a
    /// plain bookmarked URL with no setup at all. On a shared machine it
    /// lets any other account read your viewing history and control the
    /// node, which is why it is not the default.
    None,
}

impl ApiAuth {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::None => "none",
        }
    }
}

impl std::str::FromStr for ApiAuth {
    type Err = String;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "token" => Ok(Self::Token),
            "none" => Ok(Self::None),
            other => Err(format!("expected `token` or `none`, found `{other}`")),
        }
    }
}

/// Everything `ourvideo start` needs, all of it optional.
///
/// Principle 3: a first run must work with none of this set. The defaults
/// below are the whole configuration for a normal user.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// Where identity, database and blocks live.
    pub data_dir: PathBuf,
    /// Human-readable name published in the node descriptor.
    pub node_name: String,
    /// Address the local API binds to. Loopback by default: this API can read
    /// watch history, so it must not be reachable from the network.
    pub api_addr: SocketAddr,
    /// Start the local HTTP API at all.
    pub enable_api: bool,
    /// Whether the local API requires its token.
    pub api_auth: ApiAuth,
    pub network: NetworkConfig,
    pub storage: StorageConfig,
    /// Publicly reachable addresses to advertise in the descriptor, for a
    /// node behind a NAT or a reverse proxy.
    pub external_addrs: Vec<Multiaddr>,
    /// Interface language for this node, if the operator set one deliberately.
    /// Left unset, the interface works it out from the browser and the
    /// machine's own settings.
    pub default_locale: Option<String>,
}

impl NodeConfig {
    /// Config for a data directory, with defaults for everything else.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            node_name: default_node_name(),
            api_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_API_PORT),
            enable_api: true,
            api_auth: ApiAuth::default(),
            network: NetworkConfig::default(),
            storage: StorageConfig {
                cache_limit_bytes: DEFAULT_CACHE_LIMIT_BYTES,
            },
            external_addrs: Vec::new(),
            default_locale: std::env::var("OURVIDEO_LOCALE")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }
    }

    /// The standard per-user data directory for this platform.
    pub fn default_data_dir() -> PathBuf {
        directories::ProjectDirs::from("network", "OpenVideoNetwork", "ourvideo")
            .map(|dirs| dirs.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".ourvideo"))
    }

    /// Config rooted at the standard data directory.
    pub fn with_default_data_dir() -> Self {
        Self::new(Self::default_data_dir())
    }

    pub fn with_p2p_port(mut self, port: u16) -> Self {
        self.network.listen_addrs = default_listen_addrs(port);
        self
    }

    pub fn with_api_port(mut self, port: u16) -> Self {
        self.api_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        self
    }

    pub fn identity_path(&self) -> PathBuf {
        self.data_dir.join("identity.key")
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("node.db")
    }

    pub fn blocks_dir(&self) -> PathBuf {
        self.data_dir.join("blocks")
    }

    /// Written on start so the CLI can find a running node.
    pub fn runtime_path(&self) -> PathBuf {
        self.data_dir.join("runtime.json")
    }

    /// Bearer token for the local API.
    ///
    /// Kept across restarts so that a browser can bookmark the interface:
    /// a token regenerated on every start would invalidate the session
    /// cookie every time, and send the user back to the terminal. Delete
    /// this file and restart to rotate it.
    pub fn api_token_path(&self) -> PathBuf {
        self.data_dir.join("api.token")
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.data_dir.join("downloads")
    }

    /// Where a browser upload is staged before it is chunked into the block
    /// store. Emptied as soon as the publish finishes.
    pub fn uploads_dir(&self) -> PathBuf {
        self.data_dir.join("uploads")
    }

    /// Language packs added by the operator. A JSON file dropped in here
    /// shows up in the interface on the next page load — no rebuild, no
    /// restart, nothing to register.
    pub fn locales_dir(&self) -> PathBuf {
        self.data_dir.join("locales")
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::with_default_data_dir()
    }
}

/// The machine's host name, or a neutral fallback. Deliberately not anything
/// derived from the user's identity.
fn default_node_name() -> String {
    std::env::var("OURVIDEO_NODE_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| hostname_from_env().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "ourvideo node".to_string())
}

fn hostname_from_env() -> Option<String> {
    // No extra dependency for something this small; every platform we target
    // exposes one of these.
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key) {
            return Some(value);
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What a running node writes to `runtime.json` so the CLI can reach it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub pid: u32,
    pub api_url: String,
    /// Bearer token for the local API. Loopback binding stops the network
    /// from reaching it; this stops another user on the same machine.
    pub api_token: String,
    pub peer_id: String,
    pub started_at: u64,
}

impl RuntimeInfo {
    pub fn read(path: impl AsRef<Path>) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_network::DEFAULT_P2P_PORT;

    #[test]
    fn paths_all_sit_under_the_data_directory() {
        let config = NodeConfig::new("/tmp/ovn-test");
        for path in [
            config.identity_path(),
            config.database_path(),
            config.blocks_dir(),
            config.runtime_path(),
            config.api_token_path(),
            config.downloads_dir(),
            config.locales_dir(),
        ] {
            assert!(path.starts_with("/tmp/ovn-test"), "{}", path.display());
        }
    }

    #[test]
    fn the_api_requires_its_token_by_default() {
        // Viewing history is exactly the data this project promises to keep
        // local; leaving it readable by any account on the machine would
        // undercut that.
        assert_eq!(NodeConfig::new("/tmp/x").api_auth, ApiAuth::Token);
    }

    #[test]
    fn api_auth_parses_from_what_a_person_would_type() {
        use std::str::FromStr;
        assert_eq!(ApiAuth::from_str("token"), Ok(ApiAuth::Token));
        assert_eq!(ApiAuth::from_str("none"), Ok(ApiAuth::None));
        assert_eq!(ApiAuth::from_str("  None  "), Ok(ApiAuth::None));
        assert!(ApiAuth::from_str("off").is_err());
        assert!(ApiAuth::from_str("").is_err());
    }

    #[test]
    fn the_api_is_loopback_only_by_default() {
        // The local API can read watch history; it must never be exposed.
        assert!(NodeConfig::new("/tmp/x").api_addr.ip().is_loopback());
    }

    #[test]
    fn ports_can_be_overridden_without_touching_anything_else() {
        let config = NodeConfig::new("/tmp/x")
            .with_p2p_port(5900)
            .with_api_port(5901);
        assert_eq!(config.api_addr.port(), 5901);
        assert!(config
            .network
            .listen_addrs
            .iter()
            .all(|a| a.to_string().contains("5900")));
    }

    #[test]
    fn default_ports_do_not_collide() {
        assert_ne!(DEFAULT_API_PORT, DEFAULT_P2P_PORT);
    }
}
