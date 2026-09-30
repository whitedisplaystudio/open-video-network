//! `ourvideo doctor` — what is wrong with this installation, and what to do.
//!
//! The binary is unsigned and there is nobody to ask for help, so a node has
//! to be able to explain its own problems. Every check here answers two
//! questions: what did we find, and what should the person do about it. A
//! check that cannot suggest anything useful is not worth printing.
//!
//! Nothing in here changes anything. A diagnostic that repairs as it goes
//! leaves the user unable to tell what was actually broken.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use ovn_content::BlockStore;
use ovn_database::Database;
use ovn_protocol::PROTOCOL_VERSION;

use crate::config::{NodeConfig, RuntimeInfo};

/// How many stored blocks to rehash. Verifying every block would read the
/// whole cache, which on a full one is tens of gibibytes.
const BLOCKS_TO_VERIFY: usize = 128;

/// How long to give the local API before deciding it is not answering.
const API_TIMEOUT: Duration = Duration::from_secs(5);

/// Free space below which a node will struggle regardless of its cache limit.
const LOW_DISK_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Nothing to do.
    Ok,
    /// Works now, will bite later.
    Warning,
    /// Broken, or will not start.
    Problem,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// A few words naming what was examined.
    pub name: String,
    pub severity: Severity,
    /// What we found, in one line.
    pub detail: String,
    /// What to do about it. `None` when there is nothing to do.
    pub remedy: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            severity: Severity::Ok,
            detail: detail.into(),
            remedy: None,
        }
    }

    fn warn(name: &str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            severity: Severity::Warning,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }

    fn problem(name: &str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            severity: Severity::Problem,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub data_dir: PathBuf,
    /// True while a node was answering on the local API during the checks.
    pub node_running: bool,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn problems(&self) -> usize {
        self.count(Severity::Problem)
    }

    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    fn count(&self, severity: Severity) -> usize {
        self.checks
            .iter()
            .filter(|c| c.severity == severity)
            .count()
    }

    /// Nothing is broken. Warnings do not count against this.
    pub fn is_healthy(&self) -> bool {
        self.problems() == 0
    }
}

/// What to examine. The ports matter because, with no node running, there is
/// nothing to ask and the defaults are all we have to go on.
#[derive(Clone, Debug)]
pub struct DoctorOptions {
    pub data_dir: PathBuf,
    pub p2p_port: u16,
    pub api_port: u16,
    /// How many blocks to rehash. Zero skips the cache entirely.
    pub blocks_to_verify: usize,
}

impl DoctorOptions {
    pub fn new(data_dir: impl Into<PathBuf>, p2p_port: u16, api_port: u16) -> Self {
        Self {
            data_dir: data_dir.into(),
            p2p_port,
            api_port,
            blocks_to_verify: BLOCKS_TO_VERIFY,
        }
    }
}

/// Run every check and return what they found.
///
/// This never fails: a check that cannot run is itself a finding.
pub async fn run(options: DoctorOptions) -> Report {
    let config = NodeConfig::new(&options.data_dir);
    let runtime = RuntimeInfo::read(config.runtime_path());
    let live = match &runtime {
        Some(info) => probe_api(info).await,
        None => None,
    };

    let mut checks = vec![check_version()];
    checks.push(check_data_dir(&options.data_dir));
    checks.extend(check_secrets(&config));
    checks.extend(check_database(&config));
    checks.extend(check_blocks(&config, options.blocks_to_verify));
    checks.push(check_disk_space(&options.data_dir));
    checks.extend(check_media_tools());
    checks.extend(check_ports(&options, runtime.is_some(), live.is_some()));
    checks.extend(check_node(runtime.as_ref(), live.as_ref()));

    Report {
        data_dir: options.data_dir,
        node_running: live.is_some(),
        checks,
    }
}

fn check_version() -> Check {
    Check::ok(
        "version",
        format!(
            "ourvideo {}, wire protocol {PROTOCOL_VERSION}",
            env!("CARGO_PKG_VERSION")
        ),
    )
}

fn check_data_dir(dir: &Path) -> Check {
    if !dir.exists() {
        return Check::ok(
            "data directory",
            format!(
                "{} does not exist yet; starting a node creates it",
                dir.display()
            ),
        );
    }
    if !dir.is_dir() {
        return Check::problem(
            "data directory",
            format!("{} is a file, not a directory", dir.display()),
            "Move it aside, or point elsewhere with --data-dir.",
        );
    }
    // Permissions are not worth reasoning about from the metadata: the only
    // honest test of whether we can write here is to write here.
    let probe = dir.join(".ourvideo-doctor-write-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check::ok("data directory", "writable")
        }
        Err(e) => Check::problem(
            "data directory",
            format!("cannot write to {}: {e}", dir.display()),
            "Check who owns the directory, or point elsewhere with --data-dir.",
        ),
    }
}

/// The identity seed and the API token are the two files that must not be
/// readable by anyone else on the machine.
fn check_secrets(config: &NodeConfig) -> Vec<Check> {
    let root = config.data_dir.clone();
    [
        ("identity key", config.identity_path(), Some(32u64)),
        ("API token", config.api_token_path(), None),
    ]
    .into_iter()
    .map(|(name, path, expected_len)| check_secret_file(name, &path, &root, expected_len))
    .collect()
}

/// A path as the reader wants to see it: just the part below the data
/// directory, which the report has already named at the top.
fn near(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn check_secret_file(name: &str, path: &Path, root: &Path, expected_len: Option<u64>) -> Check {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => {
            return Check::ok(
                name,
                format!("{} will be created on first start", near(path, root)),
            )
        }
    };
    if let Some(expected) = expected_len {
        if metadata.len() != expected {
            return Check::problem(
                name,
                format!(
                    "{} is {} bytes, not {expected}",
                    near(path, root),
                    metadata.len()
                ),
                "This node cannot prove who it is. Move the file aside to start over \
                 with a new identity; anything you published under the old one stays \
                 published but you can no longer add to it.",
            );
        }
    }
    permissions(name, path, root, &metadata)
}

/// Whether a file that must stay private actually is.
#[cfg(unix)]
fn permissions(name: &str, path: &Path, root: &Path, metadata: &std::fs::Metadata) -> Check {
    use std::os::unix::fs::MetadataExt;

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Check::problem(
            name,
            format!(
                "{} is readable by other accounts (mode {mode:o})",
                near(path, root)
            ),
            format!("chmod 600 {}", path.display()),
        );
    }
    Check::ok(
        name,
        format!("{} is private (mode {mode:o})", near(path, root)),
    )
}

/// Windows has no mode bits to read; the file living under the user's profile
/// is the protection, and saying more than we know would be worse than
/// saying nothing.
#[cfg(not(unix))]
fn permissions(name: &str, path: &Path, root: &Path, metadata: &std::fs::Metadata) -> Check {
    Check::ok(
        name,
        format!("{} present ({} bytes)", near(path, root), metadata.len()),
    )
}

fn check_database(config: &NodeConfig) -> Vec<Check> {
    let path = config.database_path();
    let shown = near(&path, &config.data_dir);
    if !path.exists() {
        return vec![Check::ok(
            "database",
            format!("{shown} will be created on first start"),
        )];
    }
    // Opening a second connection is safe while the node runs: the database
    // is in WAL mode, so this reader does not block its writer.
    let db = match Database::open(&path) {
        Ok(db) => db,
        Err(e) => {
            return vec![Check::problem(
                "database",
                format!("cannot open {shown}: {e}"),
                corrupt_database_remedy(&path),
            )]
        }
    };
    let mut checks = Vec::new();
    match db.integrity_check() {
        Ok(damage) if damage.is_empty() => {
            checks.push(Check::ok("database", format!("{shown} is intact")))
        }
        Ok(damage) => checks.push(Check::problem(
            "database",
            format!("{shown} is damaged: {}", damage.join("; ")),
            corrupt_database_remedy(&path),
        )),
        Err(e) => checks.push(Check::problem(
            "database",
            format!("cannot check {shown}: {e}"),
            corrupt_database_remedy(&path),
        )),
    }
    if db.full_text_search_works() {
        checks.push(Check::ok("search index", "answers queries"));
    } else {
        checks.push(Check::problem(
            "search index",
            "the full-text index does not answer queries",
            "Search will not work. Stop the node and move the database aside; \
             it will be rebuilt from what the network announces.",
        ));
    }
    checks
}

fn corrupt_database_remedy(path: &Path) -> String {
    format!(
        "Stop the node and move {} aside. Discovered videos come back from the \
         network on their own. Watch history and recommendations do not: they \
         exist only on this device, and there is no copy anywhere.",
        path.display()
    )
}

fn check_blocks(config: &NodeConfig, to_verify: usize) -> Vec<Check> {
    let dir = config.blocks_dir();
    if !dir.exists() {
        return vec![Check::ok("stored blocks", "nothing stored yet".to_string())];
    }
    let store = match BlockStore::open(&dir) {
        Ok(store) => store,
        Err(e) => {
            return vec![Check::problem(
                "stored blocks",
                format!("cannot open {}: {e}", near(&dir, &config.data_dir)),
                "Check who owns the directory. Deleting it costs only fetched \
                 content, which comes back from the network — but it also \
                 deletes videos published here, which does not.",
            )]
        }
    };
    let on_disk = match store.list() {
        Ok(list) => list,
        Err(e) => {
            return vec![Check::problem(
                "stored blocks",
                format!("cannot read {}: {e}", near(&dir, &config.data_dir)),
                "Check who owns the directory.",
            )]
        }
    };
    let bytes: u64 = on_disk.iter().map(|(_, size)| *size).sum();
    let mut checks = vec![Check::ok(
        "stored blocks",
        format!("{} blocks, {}", on_disk.len(), human_bytes(bytes)),
    )];

    if to_verify > 0 && !on_disk.is_empty() {
        // Spread the sample across the store rather than taking the first
        // blocks: corruption from a bad sector or a truncated write is not
        // evenly distributed, and the oldest blocks are the least
        // interesting.
        let step = (on_disk.len() / to_verify).max(1);
        let mut corrupt = Vec::new();
        let mut checked = 0usize;
        for (cid, _) in on_disk.iter().step_by(step).take(to_verify) {
            checked += 1;
            match store.verify(cid) {
                Ok(Some(true)) | Ok(None) => {}
                Ok(Some(false)) => corrupt.push(cid.to_string()),
                Err(e) => {
                    tracing::debug!(%cid, error = %e, "cannot read a block while checking");
                    corrupt.push(cid.to_string());
                }
            }
        }
        if corrupt.is_empty() {
            checks.push(Check::ok(
                "block integrity",
                format!(
                    "{checked} of {} blocks rehashed, all correct",
                    on_disk.len()
                ),
            ));
        } else {
            checks.push(Check::problem(
                "block integrity",
                format!(
                    "{} of {checked} blocks rehashed do not match their id (e.g. {})",
                    corrupt.len(),
                    corrupt[0]
                ),
                "The node drops a block that fails its hash rather than serving \
                 bad bytes, so nothing wrong reaches a peer — but content \
                 published here cannot be refetched. If this keeps happening, \
                 the disk is failing.",
            ));
        }
    }

    // The database's idea of the cache and what is on disk can drift after a
    // crash. The node reconciles at startup, so this is only worth saying
    // when a restart would fix it.
    if let Ok(db) = Database::open(config.database_path()) {
        if let Ok(entries) = db.all_cache_entries() {
            let recorded: std::collections::HashSet<&str> =
                entries.iter().map(|e| e.cid.as_str()).collect();
            let present: std::collections::HashSet<String> =
                on_disk.iter().map(|(cid, _)| cid.to_string()).collect();
            let unrecorded = present
                .iter()
                .filter(|c| !recorded.contains(c.as_str()))
                .count();
            let missing = recorded.iter().filter(|c| !present.contains(**c)).count();
            if unrecorded + missing > 0 {
                checks.push(Check::warn(
                    "cache accounting",
                    format!("{unrecorded} blocks on disk are unrecorded, {missing} recorded blocks are gone"),
                    "Restart the node. It reconciles the two at startup.",
                ));
            }
        }
    }
    checks
}

fn check_disk_space(dir: &Path) -> Check {
    // The directory may not exist yet; ask about the nearest ancestor that
    // does, which is the filesystem the node will end up on.
    let mut probe = dir.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(parent) => probe = parent.to_path_buf(),
            None => break,
        }
    }
    match available_bytes(&probe) {
        Some(free) if free < LOW_DISK_BYTES => Check::warn(
            "disk space",
            format!("only {} free", human_bytes(free)),
            "Lower the cache ceiling with --cache-limit-gib, or free some space. \
             A node with nowhere to put a chunk cannot play or serve anything.",
        ),
        Some(free) => Check::ok("disk space", format!("{} free", human_bytes(free))),
        None => Check::ok("disk space", "could not be determined on this platform"),
    }
}

#[cfg(unix)]
fn available_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` fills the struct it is handed and reads the path only
    // for the duration of the call.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    // `f_bavail` is what an unprivileged process may actually use, which is
    // the number that matters here.
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
fn available_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free: u64 = 0;
    // SAFETY: the path is NUL-terminated and the out-parameter is a local we
    // own; the unused parameters are documented as optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

/// FFmpeg is optional: without it a publish still works, but with no
/// thumbnail and no duration.
fn check_media_tools() -> Vec<Check> {
    ["ffmpeg", "ffprobe"]
        .into_iter()
        .map(|tool| {
            match std::process::Command::new(tool)
                .arg("-version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Ok(status) if status.success() => Check::ok(tool, "found on PATH"),
                _ => Check::warn(
                    tool,
                    "not found on PATH",
                    match tool {
                        "ffmpeg" => {
                            "Videos you publish will have no thumbnail. Install FFmpeg \
                                 to get one."
                        }
                        _ => {
                            "Videos you publish will show no duration. Install FFmpeg to get \
                          one."
                        }
                    },
                ),
            }
        })
        .collect()
}

fn check_ports(options: &DoctorOptions, runtime_exists: bool, live: bool) -> Vec<Check> {
    if live {
        return vec![Check::ok(
            "ports",
            format!(
                "{} and {} are held by this node",
                options.p2p_port, options.api_port
            ),
        )];
    }
    let mut checks = Vec::new();
    for (name, port, loopback_only) in [
        ("local API port", options.api_port, true),
        ("peer-to-peer port", options.p2p_port, false),
    ] {
        let host = if loopback_only {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        };
        match std::net::TcpListener::bind((host, port)) {
            Ok(listener) => {
                drop(listener);
                checks.push(Check::ok(name, format!("{port} is free")));
            }
            Err(e) if runtime_exists => checks.push(Check::warn(
                name,
                format!("{port} is in use ({e})"),
                "A node may already be running. `ourvideo status` says which.",
            )),
            Err(e) => checks.push(Check::problem(
                name,
                format!("{port} is in use ({e})"),
                format!(
                    "Another program holds it. Find out which with `lsof -i :{port}`, \
                     or start on a different one: `ourvideo start --{} <PORT>`",
                    if loopback_only { "api-port" } else { "port" }
                ),
            )),
        }
    }
    checks
}

fn check_node(runtime: Option<&RuntimeInfo>, live: Option<&LiveStatus>) -> Vec<Check> {
    let Some(runtime) = runtime else {
        return vec![Check::ok(
            "running node",
            "none; start one with `ourvideo start`",
        )];
    };
    let Some(live) = live else {
        return vec![Check::warn(
            "running node",
            format!(
                "a node with pid {} was recorded but is not answering",
                runtime.pid
            ),
            "It was probably killed. `ourvideo start` replaces the record.",
        )];
    };

    let mut checks = vec![Check::ok(
        "running node",
        format!("{} is answering on the local API", live.peer_id),
    )];

    if live.connected_peers == 0 {
        checks.push(Check::warn(
            "peers",
            "none connected",
            "Nothing to discover from. Ask someone for a share link and run \
             `ourvideo peer add <link>`, or leave mDNS on to find nodes on this \
             network.",
        ));
    } else {
        checks.push(Check::ok(
            "peers",
            format!(
                "{} connected, {} in the routing table",
                live.connected_peers, live.routing_table_peers
            ),
        ));
    }

    match live.reachability.as_str() {
        "public" => checks.push(Check::ok(
            "reachability",
            "other peers can reach this node directly",
        )),
        "private" if live.relays == 0 => checks.push(Check::warn(
            "reachability",
            "behind a router, with no peer relaying yet",
            "This node can watch but others cannot reach it to fetch what it \
             publishes. It will keep looking for a relay. Forwarding the \
             peer-to-peer port on your router removes the need for one.",
        )),
        "private" => checks.push(Check::ok(
            "reachability",
            format!("behind a router; {} peer(s) relaying", live.relays),
        )),
        other => checks.push(Check::ok(
            "reachability",
            format!("{other}; still being worked out"),
        )),
    }
    checks
}

/// The parts of a running node's status that a diagnosis needs.
struct LiveStatus {
    peer_id: String,
    connected_peers: usize,
    routing_table_peers: usize,
    reachability: String,
    relays: usize,
}

async fn probe_api(info: &RuntimeInfo) -> Option<LiveStatus> {
    if info.api_url.is_empty() {
        return None;
    }
    let http = reqwest::Client::builder()
        .timeout(API_TIMEOUT)
        .build()
        .ok()?;
    let body: serde_json::Value = http
        .get(format!("{}/v1/status", info.api_url))
        .bearer_auth(&info.api_token)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    Some(LiveStatus {
        peer_id: info.peer_id.clone(),
        connected_peers: number(&body, "connectedPeers"),
        routing_table_peers: number(&body, "routingTablePeers"),
        reachability: body
            .get("reachability")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        relays: number(&body, "relays"),
    })
}

fn number(value: &serde_json::Value, key: &str) -> usize {
    value.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
