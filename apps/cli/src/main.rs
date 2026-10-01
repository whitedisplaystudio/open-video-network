//! `ourvideo` — the reference client.
//!
//! The CLI is a client of the core, not part of it (section 11). `start` runs
//! a node in this process; every other command talks to a running node over
//! its local HTTP API. A GUI would be a second client of the same API.

mod client;
mod render;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use client::{resolve_data_dir, Client};
use ovn_node::{NodeConfig, DEFAULT_API_PORT, DEFAULT_P2P_PORT};

#[derive(Parser, Debug)]
#[command(
    name = "ourvideo",
    version,
    about = "A video network with no centre",
    long_about = "A peer-to-peer video network.\n\n\
                  The network holds the videos. Your device holds what it knows \
                  about you: watch history, preferences and recommendations are \
                  computed here and never sent anywhere."
)]
struct Cli {
    /// Where this node keeps its data.
    #[arg(long, global = true, env = "OURVIDEO_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Print the raw JSON the node returned.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start a node. This is the only command you need to run first.
    Start(StartArgs),
    /// Show what this node is doing.
    Status,
    /// Check this installation and say what to do about anything wrong.
    Doctor(DoctorArgs),
    /// Stop the running node.
    Stop,
    /// Peers this node knows about.
    #[command(subcommand)]
    Peer(PeerCommand),
    /// A link others can use to reach this node.
    ShareLink,
    /// Channels: your own link, and the creators you subscribe to.
    #[command(subcommand)]
    Channel(ChannelCommand),
    /// Open the web interface in a browser.
    Ui(UiArgs),
    /// Publish, list and fetch videos.
    #[command(subcommand)]
    Video(VideoCommand),
    /// Search the videos this node has discovered. Never leaves the device.
    Search {
        /// What to look for.
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Your feed, computed on this device.
    #[command(subcommand, name = "recommendation", alias = "rec")]
    Recommendation(RecommendationCommand),
    /// Record that you watched something. Stored locally, never sent.
    Watch(WatchArgs),
    /// Inspect and erase the data this device keeps about you.
    #[command(subcommand)]
    Privacy(PrivacyCommand),
    /// Hide content or creators on this node.
    #[command(subcommand)]
    Block(BlockCommand),
    /// Publish a display name for your identity.
    Profile(ProfileArgs),
    /// Follow or unfollow a creator.
    Follow {
        /// Creator public key, as shown by `ourvideo video info`.
        public_key: String,
        /// Stop following instead.
        #[arg(long)]
        undo: bool,
    },
}

#[derive(Args, Debug)]
struct UiArgs {
    /// Open the node administration page instead of the viewer.
    #[arg(long)]
    admin: bool,
    /// Print the links instead of opening a browser.
    #[arg(long)]
    print: bool,
}

#[derive(Subcommand, Debug)]
enum ChannelCommand {
    /// Print a link others can use to subscribe to you.
    Link,
    /// Subscribe to a creator using their channel link.
    Subscribe {
        /// An `ourvideo://c/…` link.
        link: String,
    },
    /// Stop subscribing. Videos already discovered are kept.
    Unsubscribe {
        /// Creator public key, as `ourvideo channel list` shows it.
        public_key: String,
    },
    /// Channels you subscribe to.
    List,
    /// Everything this device knows one creator has published.
    Show {
        /// Creator public key.
        public_key: String,
    },
    /// Go and ask whether your channels have published anything new.
    Refresh {
        /// Only this creator, rather than all of them.
        #[arg(long)]
        public_key: Option<String>,
    },
}

#[derive(Args, Debug)]
struct DoctorArgs {
    /// Peer-to-peer port to test. Only used when no node is running.
    #[arg(long, default_value_t = DEFAULT_P2P_PORT)]
    port: u16,
    /// Local API port to test. Only used when no node is running.
    #[arg(long, default_value_t = DEFAULT_API_PORT)]
    api_port: u16,
    /// How many stored blocks to rehash. 0 skips the cache.
    #[arg(long, default_value_t = 128)]
    verify_blocks: usize,
}

#[derive(Args, Debug)]
struct StartArgs {
    /// Port for peer-to-peer traffic (QUIC and TCP).
    #[arg(long, default_value_t = DEFAULT_P2P_PORT)]
    port: u16,
    /// Port for the local API.
    #[arg(long, default_value_t = DEFAULT_API_PORT)]
    api_port: u16,
    /// Fetch videos whose source URL points inside your network rather than
    /// out at the internet.
    ///
    /// Off by default. A node fetches whatever address an announcement gives
    /// it, so without this a stranger could publish `http://192.168.0.1/` and
    /// have every viewer's node knock on doors inside their own house. Turn it
    /// on when somebody you trust is serving from a machine on your network.
    #[arg(long)]
    allow_private_sources: bool,
    /// Also answer on this machine's network address, so a phone or another
    /// computer on the same network can open the web interface.
    ///
    /// Off by default: the interface can read your viewing history and
    /// control this node. With this on, a token is required — `--ui-auth
    /// none` is refused — and the node still refuses any request that names
    /// it by a hostname rather than an address.
    #[arg(long)]
    lan: bool,
    /// Name to publish in this node's descriptor.
    #[arg(long)]
    name: Option<String>,
    /// Do not look for peers on the local network.
    #[arg(long)]
    no_mdns: bool,
    /// Do not start the local API. The CLI cannot talk to the node without it.
    #[arg(long)]
    no_api: bool,
    /// Optional entry points. The network works without any.
    #[arg(long = "bootstrap", value_name = "MULTIADDR")]
    bootstrap: Vec<String>,
    /// Publicly reachable address to advertise, for a node behind a NAT.
    #[arg(long = "external-addr", value_name = "MULTIADDR")]
    external_addrs: Vec<String>,
    /// Cache ceiling for fetched content, in gibibytes.
    #[arg(long, default_value_t = 10.0)]
    cache_limit_gib: f64,
    /// Interface language, e.g. `ja` or `pt-BR`. Left unset, the web
    /// interface works it out from the browser and this machine's settings.
    #[arg(long, env = "OURVIDEO_LOCALE")]
    locale: Option<String>,
    /// How the web interface authenticates: `token`, or `none` to let a
    /// bookmarked URL work with no setup.
    ///
    /// `none` is for a machine you do not share. It still refuses anything
    /// that is not loopback, but any other account on this machine could
    /// then control the node and read your viewing history.
    #[arg(
        long,
        value_name = "MODE",
        default_value = "token",
        env = "OURVIDEO_UI_AUTH"
    )]
    ui_auth: String,
}

#[derive(Subcommand, Debug)]
enum PeerCommand {
    /// List known peers.
    List,
    /// Join the network through a URL or a share link.
    Add {
        /// `https://video.example.jp`, `ourvideo://…`, or a multiaddr.
        target: String,
    },
    /// Forget a peer.
    Remove { peer_id: String },
}

#[derive(Subcommand, Debug)]
enum VideoCommand {
    /// Publish a file to the network.
    Publish {
        file: PathBuf,
        /// Where viewers will fetch the file from, e.g.
        /// `https://videos.example/clip.mp4`.
        ///
        /// The network carries the title, tags, thumbnail and the hashes the
        /// file must match. It does not carry the file: that is served from
        /// here, and every viewer checks what arrives against the hashes.
        #[arg(long = "source-url", value_name = "URL")]
        source_url: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
        /// Repeatable. Tags drive the local recommendation model.
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// List discovered videos.
    List {
        /// Only videos published by this node.
        #[arg(long)]
        local: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show everything known about one video.
    Info { cid: String },
    /// Fetch a video's data from the network.
    Get { cid: String },
}

#[derive(Subcommand, Debug)]
enum RecommendationCommand {
    /// Your feed.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Why a video is where it is in your feed.
    Explain { cid: String },
}

#[derive(Args, Debug)]
struct WatchArgs {
    cid: String,
    /// Seconds actually watched.
    #[arg(long, value_name = "N")]
    seconds: u32,
    /// Length of the video, if the player knows it.
    #[arg(long, value_name = "N", default_value_t = 0)]
    duration: u32,
    /// Watched to the end.
    #[arg(long)]
    completed: bool,
    /// Moved on early.
    #[arg(long)]
    skipped: bool,
    /// Liked it.
    #[arg(long)]
    liked: bool,
}

#[derive(Subcommand, Debug)]
enum PrivacyCommand {
    /// What this device has recorded about your viewing.
    Show {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// The preference model derived from it.
    Preferences,
    /// Erase viewing history and the preference model.
    Clear,
}

#[derive(Subcommand, Debug)]
enum BlockCommand {
    /// Hide one video.
    Cid {
        cid: String,
        #[arg(long, default_value = "")]
        reason: String,
        /// Unblock instead.
        #[arg(long)]
        undo: bool,
    },
    /// Hide everything from a creator.
    Creator {
        public_key: String,
        #[arg(long, default_value = "")]
        reason: String,
        #[arg(long)]
        undo: bool,
    },
    /// Show what is blocked.
    List,
}

#[derive(Args, Debug)]
struct ProfileArgs {
    #[arg(long)]
    name: String,
    #[arg(long, default_value = "")]
    bio: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = resolve_data_dir(cli.data_dir.clone());

    match cli.command {
        Command::Start(args) => start(args, data_dir).await,
        // `doctor` is the one command that has to work when nothing else
        // does, so it never goes through the API client.
        Command::Doctor(args) => doctor(args, data_dir, cli.json).await,
        other => run_client_command(other, data_dir, cli.json).await,
    }
}

/// Diagnose the installation, and exit non-zero if something is broken so
/// that a script can tell.
async fn doctor(args: DoctorArgs, data_dir: PathBuf, as_json: bool) -> Result<()> {
    let mut options = ovn_node::doctor::DoctorOptions::new(&data_dir, args.port, args.api_port);
    options.blocks_to_verify = args.verify_blocks;
    let report = ovn_node::doctor::run(options).await;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render::doctor(&report));
    }
    if report.is_healthy() {
        Ok(())
    } else {
        // The report has already said what is wrong and what to do; anyhow
        // would only print it again.
        std::process::exit(1);
    }
}

/// This machine's addresses on the networks it is attached to.
///
/// Read from the interfaces rather than guessed, and loopback is left out
/// because the banner has already printed that one.
fn lan_urls(port: u16) -> Vec<String> {
    let Ok(output) = std::process::Command::new("ifconfig").output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut urls = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("inet ") else {
            continue;
        };
        let Some(address) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(ip) = address.parse::<std::net::IpAddr>() else {
            continue;
        };
        if ip.is_loopback() {
            continue;
        }
        urls.push(format!("http://{ip}:{port}"));
    }
    urls
}

async fn start(args: StartArgs, data_dir: PathBuf) -> Result<()> {
    init_logging();

    let mut config = NodeConfig::new(&data_dir)
        .with_p2p_port(args.port)
        .with_api_port(args.api_port);
    config.enable_api = !args.no_api;
    config.network.enable_mdns = !args.no_mdns;
    config.storage.cache_limit_bytes =
        (args.cache_limit_gib.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64;
    if let Some(name) = args.name {
        config.node_name = name;
    }
    if let Some(locale) = args.locale {
        config.default_locale = Some(locale);
    }
    config.allow_private_sources = args.allow_private_sources;
    config.api_auth = args
        .ui_auth
        .parse()
        .map_err(|e| anyhow::anyhow!("--ui-auth: {e}"))?;
    if args.lan {
        if config.api_auth == ovn_node::ApiAuth::None {
            return Err(anyhow::anyhow!(
                "--lan and --ui-auth none together would let anyone on your network \n\
                 read your viewing history and control this node. Drop one of them."
            ));
        }
        // Every interface, so it works whichever one the phone is on.
        config.api_addr = std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            args.api_port,
        );
    }
    for addr in &args.bootstrap {
        config.network.bootstrap_addrs.push(
            addr.parse()
                .with_context(|| format!("bootstrap address {addr}"))?,
        );
    }
    for addr in &args.external_addrs {
        config.external_addrs.push(
            addr.parse()
                .with_context(|| format!("external address {addr}"))?,
        );
    }

    let mut running = ovn_node::start(config).await.map_err(|e| match e {
        ovn_node::NodeError::ApiBind { addr, source } => anyhow::anyhow!(
            "the local API could not bind to {addr}: {source}\n\n\
             Another node is probably already running. Use --api-port and --port \
             to run a second one."
        ),
        other => anyhow::anyhow!(other),
    })?;

    let node = running.node();
    println!("ourvideo is running.");
    println!();
    println!("  Peer ID    {}", node.peer_id());
    println!("  Data dir   {}", data_dir.display());
    if let Some(url) = running.api_url() {
        println!("  Local API  {url}");
    }
    println!();
    println!("Share this link so others can reach you:");
    match node.share_link() {
        Ok(link) => println!("  {link}"),
        Err(e) => println!("  (not available yet: {e})"),
    }
    if let Some(url) = running.api_url() {
        println!();
        println!("Web interface:");
        println!("  {url}/ui      watch");
        println!("  {url}/admin   manage this node");
        println!();
        if running.node().config().api_auth == ovn_node::ApiAuth::None {
            println!("Open either one and bookmark it. No sign-in step: this node was");
            println!("started with --ui-auth none, so anything on this machine may use it.");
        } else {
            println!("The first time you open these in a browser, run `ourvideo ui` to");
            println!("sign that browser in. After that you can bookmark them.");
        }
        if !running.node().config().api_is_loopback() {
            println!();
            println!("Reachable from your network (--lan). On a phone on the same");
            println!("Wi-Fi, open this once — it signs that browser in, then bookmark it:");
            println!();
            for address in lan_urls(running.node().config().api_addr.port()) {
                println!(
                    "  {address}/auth?token={}&next=/ui",
                    running.node().api_token()
                );
            }
            println!();
            println!("Anyone on this network who has that link has this node. Stop");
            println!("sharing it by deleting api.token and restarting.");
        }
    }
    println!();
    println!("Leave this running. In another terminal:");
    println!("  ourvideo status");
    println!("  ourvideo video publish my-video.mp4");
    println!();
    println!("Press Ctrl-C to stop, or run `ourvideo stop` elsewhere.");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!();
            println!("Stopping…");
        }
        _ = running.stopped() => {
            println!();
            println!("Stopped on request.");
        }
    }
    running.shutdown().await;
    Ok(())
}

/// A response together with the function that prints it.
type Rendered = (Value, Box<dyn Fn(&Value)>);

async fn run_client_command(command: Command, data_dir: PathBuf, as_json: bool) -> Result<()> {
    let client = Client::connect(&data_dir)?;

    // Every arm produces the value the node returned and a closure that
    // renders it, so `--json` works uniformly without each command repeating
    // the check.
    let (value, render): Rendered = match command {
        Command::Start(_) | Command::Doctor(_) => {
            unreachable!("handled before connecting")
        }

        Command::Status => (client.get("/v1/status").await?, Box::new(render::status)),

        Command::Channel(ChannelCommand::Link) => (
            client.get("/v1/channel/link").await?,
            Box::new(render::channel_link),
        ),
        Command::Channel(ChannelCommand::Subscribe { link }) => (
            client
                .post("/v1/subscriptions", json!({ "link": link }))
                .await?,
            Box::new(render::subscribed),
        ),
        Command::Channel(ChannelCommand::Unsubscribe { public_key }) => {
            client
                .delete(&format!("/v1/subscriptions/{public_key}"))
                .await?;
            println!("Unsubscribed. The videos you already have are still there.");
            return Ok(());
        }
        Command::Channel(ChannelCommand::List) => (
            client.get("/v1/subscriptions").await?,
            Box::new(render::subscriptions),
        ),
        Command::Channel(ChannelCommand::Show { public_key }) => (
            client
                .get(&format!("/v1/channels/{public_key}/videos"))
                .await?,
            Box::new(|v| render::videos(v, "This creator has published nothing you know about.")),
        ),
        Command::Channel(ChannelCommand::Refresh { public_key }) => {
            let path = match &public_key {
                Some(key) => format!("/v1/channels/{key}/refresh"),
                None => "/v1/subscriptions/refresh".to_string(),
            };
            (
                client.post(&path, json!({})).await?,
                Box::new(render::refreshed),
            )
        }

        Command::Stop => {
            client.post("/v1/shutdown", json!({})).await?;
            println!("Stopped.");
            return Ok(());
        }

        Command::Ui(args) => {
            let page = if args.admin { "/admin" } else { "/ui" };
            let url = client.ui_url(page);
            if args.print {
                println!("Viewer  {}", client.ui_url("/ui"));
                println!("Admin   {}", client.ui_url("/admin"));
                println!();
                println!("These links carry this node's API token. Keep them to yourself.");
                println!();
                println!("Once a browser has followed one, bookmark this instead:");
                println!("  {}/ui", client.base_url());
                println!("It keeps working, including after this node restarts.");
            } else {
                println!("Opening {}{page}", client.base_url());
                if let Err(e) = open_in_browser(&url) {
                    println!();
                    println!("Could not open a browser ({e}). Open this yourself:");
                    println!("  {url}");
                }
            }
            return Ok(());
        }

        Command::ShareLink => {
            let value = client.get("/v1/share-link").await?;
            let link = value
                .get("shareLink")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (
                value,
                Box::new(move |_| {
                    println!("{link}");
                }),
            )
        }

        Command::Peer(PeerCommand::List) => {
            (client.get("/v1/peers").await?, Box::new(render::peers))
        }

        Command::Peer(PeerCommand::Add { target }) => {
            let value = client
                .post("/v1/peers", json!({ "target": target }))
                .await?;
            (
                value,
                Box::new(|v| {
                    println!(
                        "Connected to {}",
                        v.get("nodeName")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| v
                                .get("peerId")
                                .and_then(Value::as_str)
                                .unwrap_or("the peer"))
                    );
                    println!(
                        "  {}",
                        v.get("peerId").and_then(Value::as_str).unwrap_or("")
                    );
                    println!();
                    println!("You are on the network. This link is no longer needed:");
                    println!("other peers will be found through the network itself.");
                }),
            )
        }

        Command::Peer(PeerCommand::Remove { peer_id }) => {
            client.delete(&format!("/v1/peers/{peer_id}")).await?;
            println!("Forgotten.");
            return Ok(());
        }

        Command::Video(VideoCommand::Publish {
            file,
            source_url,
            title,
            description,
            tags,
        }) => {
            let path = std::fs::canonicalize(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            println!("Publishing {}…", path.display());
            let value = client
                .post(
                    "/v1/videos",
                    json!({
                        "path": path,
                        "sourceUrl": source_url,
                        "title": title,
                        "description": description,
                        "tags": tags,
                    }),
                )
                .await?;
            (
                value,
                Box::new(|v| {
                    println!();
                    println!("Published.");
                    println!("  {}", v.get("cid").and_then(Value::as_str).unwrap_or(""));
                    println!(
                        "  {} in {} chunks",
                        render::bytes(v.get("totalBytes").and_then(Value::as_u64).unwrap_or(0)),
                        v.get("chunks").and_then(Value::as_u64).unwrap_or(0)
                    );
                    if v.get("announcedToNetwork").and_then(Value::as_bool) == Some(false) {
                        println!();
                        println!("No peers are listening yet, so nobody has been told.");
                        println!("It will be announced as soon as this node meets one.");
                    }
                }),
            )
        }

        Command::Video(VideoCommand::List { local, limit }) => {
            let path = if local {
                "/v1/videos/local".to_string()
            } else {
                format!("/v1/videos?limit={limit}")
            };
            let hint = if local {
                "You have not published anything yet:\n    ourvideo video publish my-video.mp4"
            } else {
                "No videos discovered yet. Connect to a peer:\n    ourvideo peer add <link>"
            };
            (
                client.get(&path).await?,
                Box::new(move |v| render::videos(v, hint)),
            )
        }

        Command::Video(VideoCommand::Info { cid }) => (
            client.get(&format!("/v1/videos/{cid}")).await?,
            Box::new(render::video_info),
        ),

        Command::Video(VideoCommand::Get { cid }) => {
            println!("Fetching from the creator's server…");
            (
                client
                    .post(&format!("/v1/videos/{cid}/fetch"), json!({}))
                    .await?,
                Box::new(print_fetch),
            )
        }

        Command::Search { query, limit } => {
            let query = query.join(" ");
            let encoded = url_encode(&query);
            (
                client
                    .get(&format!("/v1/search?q={encoded}&limit={limit}"))
                    .await?,
                Box::new(|v| render::videos(v, "Nothing matched.")),
            )
        }

        Command::Recommendation(RecommendationCommand::List { limit }) => (
            client
                .get(&format!("/v1/recommendations?limit={limit}"))
                .await?,
            Box::new(render::recommendations),
        ),

        Command::Recommendation(RecommendationCommand::Explain { cid }) => (
            client.get(&format!("/v1/recommendations/{cid}")).await?,
            Box::new(render::explanation),
        ),

        Command::Watch(args) => {
            client
                .post(
                    "/v1/watch",
                    json!({
                        "cid": args.cid,
                        "watchedSecs": args.seconds,
                        "durationSecs": args.duration,
                        "completed": args.completed,
                        "skipped": args.skipped,
                        "liked": args.liked,
                    }),
                )
                .await?;
            println!("Recorded locally. Nothing was sent anywhere.");
            return Ok(());
        }

        Command::Privacy(PrivacyCommand::Show { limit }) => (
            client.get(&format!("/v1/watch?limit={limit}")).await?,
            Box::new(render::watch_history),
        ),

        Command::Privacy(PrivacyCommand::Preferences) => (
            client.get("/v1/preferences").await?,
            Box::new(render::preferences),
        ),

        Command::Privacy(PrivacyCommand::Clear) => {
            client.delete("/v1/watch").await?;
            println!("Viewing history and preference model erased.");
            return Ok(());
        }

        Command::Block(BlockCommand::Cid { cid, reason, undo }) => {
            let path = format!("/v1/blocked/cids/{cid}");
            if undo {
                client.delete(&path).await?;
                println!("Unblocked.");
            } else {
                client.post(&path, json!({ "reason": reason })).await?;
                println!("Blocked on this node. Other nodes are unaffected.");
            }
            return Ok(());
        }

        Command::Block(BlockCommand::Creator {
            public_key,
            reason,
            undo,
        }) => {
            let path = format!("/v1/blocked/creators/{public_key}");
            if undo {
                client.delete(&path).await?;
                println!("Unblocked.");
            } else {
                client.post(&path, json!({ "reason": reason })).await?;
                println!("Blocked on this node. Other nodes are unaffected.");
            }
            return Ok(());
        }

        Command::Block(BlockCommand::List) => {
            let cids = client.get("/v1/blocked/cids").await?;
            let creators = client.get("/v1/blocked/creators").await?;
            let value = json!({ "cids": cids, "creators": creators });
            (
                value,
                Box::new(|v| {
                    print_block_list("Blocked videos", v.get("cids"));
                    print_block_list("Blocked creators", v.get("creators"));
                }),
            )
        }

        Command::Profile(args) => {
            client
                .post(
                    "/v1/profile",
                    json!({ "displayName": args.name, "bio": args.bio }),
                )
                .await?;
            println!("Profile published.");
            return Ok(());
        }

        Command::Follow { public_key, undo } => {
            let path = format!("/v1/follow/{public_key}");
            if undo {
                client.delete(&path).await?;
                println!("Unfollowed.");
            } else {
                client.post(&path, json!({})).await?;
                println!("Following.");
            }
            return Ok(());
        }
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        render(&value);
    }
    Ok(())
}

/// Hand a URL to the desktop's browser.
fn open_in_browser(url: &str) -> Result<()> {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let status = std::process::Command::new(program)
        .args(args)
        .arg(url)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}

fn print_fetch(v: &Value) {
    let fetched = v.get("chunksFetched").and_then(Value::as_u64).unwrap_or(0);
    let held = v
        .get("chunksAlreadyHeld")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!(
        "Fetched {} chunks ({}), {held} already held, from {} providers.",
        fetched,
        render::bytes(v.get("bytesFetched").and_then(Value::as_u64).unwrap_or(0)),
        v.get("providersTried").and_then(Value::as_u64).unwrap_or(0)
    );
}

fn print_block_list(heading: &str, value: Option<&Value>) {
    let rows = value.and_then(Value::as_array).cloned().unwrap_or_default();
    println!("{heading}:");
    if rows.is_empty() {
        println!("  (none)");
    }
    for row in rows {
        let subject = row.get("subject").and_then(Value::as_str).unwrap_or("");
        let reason = row.get("reason").and_then(Value::as_str).unwrap_or("");
        if reason.is_empty() {
            println!("  {subject}");
        } else {
            println!("  {subject}  — {reason}");
        }
    }
    println!();
}

/// Percent-encode a query string. Small enough not to warrant a dependency,
/// and it has to handle the CJK case correctly.
fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("OURVIDEO_LOG")
        .unwrap_or_else(|_| EnvFilter::new("ourvideo=info,ovn_=info,info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_commands_the_design_specifies_all_parse() {
        // Section 11's interface, verbatim.
        let invocations: Vec<Vec<&str>> = vec![
            vec!["ourvideo", "start"],
            vec!["ourvideo", "status"],
            vec!["ourvideo", "stop"],
            vec!["ourvideo", "peer", "list"],
            vec!["ourvideo", "peer", "add", "https://video.example.jp"],
            vec![
                "ourvideo",
                "video",
                "publish",
                "clip.mp4",
                "--source-url",
                "https://videos.example/clip.mp4",
            ],
            vec!["ourvideo", "video", "list"],
            vec!["ourvideo", "video", "info", "bafy..."],
            vec!["ourvideo", "video", "get", "bafy..."],
            vec!["ourvideo", "search", "rust"],
            vec!["ourvideo", "recommendation", "list"],
        ];
        for argv in invocations {
            assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
    }

    #[test]
    fn publishing_without_saying_where_the_file_lives_is_refused() {
        // The network does not carry the file, so an announcement with nowhere
        // to fetch it from describes a video nobody can watch. Better to say
        // so at the command line than to publish one.
        assert!(Cli::try_parse_from(["ourvideo", "video", "publish", "clip.mp4"]).is_err());
    }

    #[test]
    fn starting_needs_no_arguments_at_all() {
        // Test B: one command, no configuration.
        let cli = Cli::try_parse_from(["ourvideo", "start"]).unwrap();
        let Command::Start(args) = cli.command else {
            panic!("expected start");
        };
        assert_eq!(args.port, DEFAULT_P2P_PORT);
        assert_eq!(args.api_port, DEFAULT_API_PORT);
        assert!(!args.no_mdns);
        assert!(args.bootstrap.is_empty());
    }

    #[test]
    fn peer_add_takes_one_string_and_nothing_else() {
        // Test C: no peer id, public key or multiaddr typed by hand.
        for target in [
            "https://video.example.jp",
            "ourvideo://AAAA",
            "/ip4/192.0.2.1/udp/4800/quic-v1/p2p/12D3KooW",
        ] {
            let cli = Cli::try_parse_from(["ourvideo", "peer", "add", target]).unwrap();
            let Command::Peer(PeerCommand::Add { target: parsed }) = cli.command else {
                panic!("expected peer add");
            };
            assert_eq!(parsed, target);
        }
    }

    #[test]
    fn a_multi_word_search_is_joined_not_rejected() {
        let cli = Cli::try_parse_from(["ourvideo", "search", "rust", "async", "talk"]).unwrap();
        let Command::Search { query, .. } = cli.command else {
            panic!("expected search");
        };
        assert_eq!(query.join(" "), "rust async talk");
    }

    #[test]
    fn query_encoding_handles_spaces_and_non_ascii() {
        assert_eq!(url_encode("rust async"), "rust%20async");
        assert_eq!(url_encode("ゲーム"), "%E3%82%B2%E3%83%BC%E3%83%A0");
        assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn json_output_is_available_on_every_command() {
        let cli = Cli::try_parse_from(["ourvideo", "--json", "status"]).unwrap();
        assert!(cli.json);
        let cli = Cli::try_parse_from(["ourvideo", "status", "--json"]).unwrap();
        assert!(cli.json);
    }
}
