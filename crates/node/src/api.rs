//! The local HTTP API.
//!
//! This is the seam the design asks for in section 34: the CLI is a client of
//! the core, and a future GUI will be another one. It binds to loopback and
//! requires a bearer token stored in the data directory with owner-only
//! permissions, because it can read watch history — loopback keeps the
//! network out, the token keeps other accounts on the machine out.
//!
//! The one exception is `/.well-known/ovn/node.json`, which serves the
//! node's public descriptor unauthenticated so an operator can put a reverse
//! proxy in front of it and hand out a URL (section 14).

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tokio::sync::oneshot;

use crate::progress::NodeEvent;
use crate::range::{parse_range, ByteRange, RangeError};

use ovn_database::WatchEvent;
use ovn_identity::PublicKey;
use ovn_protocol::ContentId;

use crate::dto::*;
use crate::node::Node;
use crate::{NodeError, Result};

pub(crate) struct ApiServer {
    pub addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// How long a graceful shutdown waits for connections to drain.
///
/// A server-sent-events stream and a video download both stay open for as
/// long as the client wants them, and `axum`'s graceful shutdown waits for
/// every in-flight connection. Without a deadline, one open browser tab
/// would stop the node from ever exiting.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

impl ApiServer {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let mut task = self.task;
        if tokio::time::timeout(SHUTDOWN_GRACE, &mut task)
            .await
            .is_err()
        {
            tracing::debug!("local API still had open connections; closing them");
            task.abort();
            let _ = task.await;
        }
    }
}

pub(crate) async fn serve(node: Node, addr: SocketAddr) -> Result<ApiServer> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| NodeError::ApiBind { addr, source })?;
    let bound = listener
        .local_addr()
        .map_err(|source| NodeError::ApiBind { addr, source })?;

    let app = router(node);
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = rx.await;
        });
        if let Err(e) = server.await {
            tracing::error!(error = %e, "local API stopped");
        }
    });

    tracing::info!(%bound, "local API listening");
    Ok(ApiServer {
        addr: bound,
        shutdown: tx,
        task,
    })
}

fn router(node: Node) -> Router {
    let protected = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/peers", get(list_peers).post(add_peer))
        .route("/v1/peers/{peer_id}", delete(forget_peer))
        .route("/v1/share-link", get(share_link))
        .route("/v1/videos", get(list_videos).post(publish_video))
        .route("/v1/videos/local", get(list_local_videos))
        .route("/v1/videos/{cid}", get(video_info))
        .route("/v1/videos/{cid}/fetch", post(fetch_video))
        .route("/v1/videos/{cid}/export", post(export_video))
        .route(
            "/v1/videos/{cid}/stream",
            get(stream_video).head(stream_video),
        )
        .route("/v1/videos/{cid}/thumbnail", get(video_thumbnail))
        .route("/v1/upload", post(upload_video))
        .route("/v1/events", get(event_stream))
        .route("/v1/search", get(search))
        .route("/v1/recommendations", get(recommendations))
        .route("/v1/recommendations/{cid}", get(explain))
        .route("/v1/watch", post(record_watch).get(watch_history))
        .route("/v1/watch", delete(clear_history))
        .route("/v1/preferences", get(preferences))
        .route("/v1/profile", post(publish_profile))
        .route("/v1/follow/{public_key}", post(follow).delete(unfollow))
        .route("/v1/blocked/cids", get(blocked_cids))
        .route(
            "/v1/blocked/cids/{cid}",
            post(block_cid).delete(unblock_cid),
        )
        .route("/v1/blocked/creators", get(blocked_creators))
        .route(
            "/v1/blocked/creators/{public_key}",
            post(block_creator).delete(unblock_creator),
        )
        .route("/v1/shutdown", post(shutdown))
        .layer(axum::middleware::from_fn_with_state(
            node.clone(),
            require_token,
        ));

    Router::new()
        // Public: this is what a URL hands to a newcomer.
        .route(ovn_protocol::WELL_KNOWN_DESCRIPTOR_PATH, get(descriptor))
        .route("/health", get(health))
        // Where this build's source is. Section 13 of the AGPL requires that a
        // user who reaches the program over a network be offered it, so it sits
        // outside the token like the pages do.
        .route("/v1/about", get(about))
        // The web UI. The pages themselves hold no data — everything they
        // show comes from /v1, which is authenticated — so they are served
        // without a token. `/auth` is what turns a token into a cookie.
        .route("/", get(|| async { Redirect::temporary("/ui") }))
        .route("/ui", get(viewer_page))
        .route("/admin", get(admin_page))
        .route("/assets/app.css", get(asset_css))
        .route("/assets/common.js", get(asset_common_js))
        .route("/assets/viewer.js", get(asset_viewer_js))
        .route("/assets/admin.js", get(asset_admin_js))
        .route("/assets/zones.js", get(asset_zones_js))
        // Language packs, like the pages, carry nothing private: the UI needs
        // them before it can render even its "not authorised" message.
        .route("/v1/locales", get(list_locales))
        .route("/v1/locales/{code}", get(get_locale))
        .route("/auth", get(authenticate))
        .merge(protected)
        // Applied to everything, including the public routes: a page on
        // another origin must not be able to drive this node.
        .layer(axum::middleware::from_fn(require_local_host))
        .with_state(node)
}

/// Refuse a request whose `Host` is not a loopback name.
///
/// The socket is already bound to 127.0.0.1, but that alone does not stop DNS
/// rebinding: an attacker's domain can be made to resolve here, and then the
/// browser treats their page as same-origin with this server. Checking the
/// host they asked for closes that.
async fn require_local_host(
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Strip the port, and the brackets around a literal IPv6 address.
    let name = host
        .rsplit_once(':')
        .filter(|(before, _)| !before.is_empty() && !before.ends_with(']') || before.contains(']'))
        .map(|(before, _)| before)
        .unwrap_or(host)
        .trim_start_matches('[')
        .trim_end_matches(']');

    if host.is_empty() || matches!(name, "127.0.0.1" | "localhost" | "::1") {
        Ok(next.run(request).await)
    } else {
        tracing::warn!(%host, "refused a request for a non-loopback host name");
        Err(StatusCode::MISDIRECTED_REQUEST)
    }
}

/// Constant-time-ish bearer check. The token is 32 random bytes, so an
/// attacker guessing it is not the threat model; the check is here to stop a
/// different local user simply calling the API.
async fn require_token(
    State(node): State<Node>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    // An operator can trade the token away for a URL that needs no setup.
    // Loopback binding and the host check below still apply either way.
    if node.config().api_auth == crate::config::ApiAuth::None {
        return Ok(next.run(request).await);
    }
    // A command line client sends a bearer header; a browser sends the
    // cookie `/auth` gave it, because a page cannot add headers to a
    // `<video src>` or an `EventSource`.
    let headers = request.headers();
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| cookie_token(headers))
        .unwrap_or_default();
    if token_matches(presented, node.api_token()) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

// --------------------------------------------------------------- handlers

async fn health() -> &'static str {
    "ok"
}

/// What this build is and where its source lives.
///
/// Read from the crate metadata rather than written out here, so that a fork
/// which changes `repository` in `Cargo.toml` — as the AGPL requires it to,
/// once it has modified anything — gets a correct notice for free instead of
/// shipping ours.
async fn about() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": ovn_protocol::PROTOCOL_VERSION,
        "licence": env!("CARGO_PKG_LICENSE"),
        "sourceUrl": env!("CARGO_PKG_REPOSITORY"),
    }))
}

async fn descriptor(State(node): State<Node>) -> ApiResult<Json<ovn_protocol::NodeDescriptor>> {
    Ok(Json(node.descriptor()?))
}

async fn status(State(node): State<Node>) -> ApiResult<Json<StatusDto>> {
    Ok(Json(node.status().await?.into()))
}

async fn share_link(State(node): State<Node>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({ "shareLink": node.share_link()? })))
}

async fn list_peers(State(node): State<Node>) -> ApiResult<Json<Vec<ovn_database::PeerRecord>>> {
    Ok(Json(node.peers()?))
}

async fn add_peer(
    State(node): State<Node>,
    Json(request): Json<AddPeerRequest>,
) -> ApiResult<Json<AddPeerDto>> {
    Ok(Json(node.add_peer(&request.target).await?.into()))
}

async fn forget_peer(
    State(node): State<Node>,
    Path(peer_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(
        serde_json::json!({ "removed": node.forget_peer(&peer_id)? }),
    ))
}

async fn list_videos(
    State(node): State<Node>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<ovn_database::VideoRecord>>> {
    Ok(Json(node.videos(query.limit.min(200), query.offset)?))
}

async fn list_local_videos(
    State(node): State<Node>,
) -> ApiResult<Json<Vec<ovn_database::VideoRecord>>> {
    Ok(Json(node.local_videos()?))
}

async fn video_info(
    State(node): State<Node>,
    Path(cid): Path<String>,
) -> ApiResult<Json<ovn_database::VideoRecord>> {
    let cid = parse_cid(&cid)?;
    node.video(&cid)?
        .map(Json)
        .ok_or(ApiError(NodeError::NotFound))
}

async fn publish_video(
    State(node): State<Node>,
    Json(request): Json<PublishRequest>,
) -> ApiResult<Json<PublishDto>> {
    Ok(Json(
        node.publish_video(
            &request.path,
            request.title,
            request.description,
            request.tags,
        )
        .await?
        .into(),
    ))
}

async fn fetch_video(
    State(node): State<Node>,
    Path(cid): Path<String>,
) -> ApiResult<Json<FetchDto>> {
    let cid = parse_cid(&cid)?;
    Ok(Json(node.fetch_video(cid).await?.into()))
}

async fn export_video(
    State(node): State<Node>,
    Path(cid): Path<String>,
    Json(request): Json<ExportRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let cid = parse_cid(&cid)?;
    let path = node.export_video(cid, request.path)?;
    Ok(Json(serde_json::json!({ "path": path })))
}

async fn search(
    State(node): State<Node>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Vec<ovn_database::VideoRecord>>> {
    Ok(Json(node.search(&query.q, query.limit.min(200))?))
}

async fn recommendations(
    State(node): State<Node>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<RecommendationDto>>> {
    let recommendations = node.recommendations(query.limit.min(200))?;
    Ok(Json(
        recommendations
            .iter()
            .map(RecommendationDto::from_recommendation)
            .collect(),
    ))
}

async fn explain(
    State(node): State<Node>,
    Path(cid): Path<String>,
) -> ApiResult<Json<RecommendationDto>> {
    let cid = parse_cid(&cid)?;
    node.explain(&cid)?
        .as_ref()
        .map(RecommendationDto::from_recommendation)
        .map(Json)
        .ok_or(ApiError(NodeError::NotFound))
}

async fn record_watch(
    State(node): State<Node>,
    Json(request): Json<WatchRequest>,
) -> ApiResult<StatusCode> {
    let cid = parse_cid(&request.cid)?;
    node.record_watch(&WatchEvent {
        cid,
        watched_secs: request.watched_secs,
        duration_secs: request.duration_secs,
        completed: request.completed,
        skipped: request.skipped,
        liked: request.liked,
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn watch_history(
    State(node): State<Node>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let history = node.database().watch_history(query.limit.min(500))?;
    let summary: WatchSummaryDto = node.database().watch_summary()?.into();
    let entries: Vec<WatchRecordDto> = history.iter().map(WatchRecordDto::from_record).collect();
    Ok(Json(serde_json::json!({
        "summary": summary,
        "entries": entries,
    })))
}

async fn clear_history(State(node): State<Node>) -> ApiResult<StatusCode> {
    node.clear_local_history()?;
    Ok(StatusCode::NO_CONTENT)
}

async fn preferences(State(node): State<Node>) -> ApiResult<Json<Vec<PreferenceDto>>> {
    Ok(Json(PreferenceDto::from_model(&node.preference_model()?)))
}

async fn publish_profile(
    State(node): State<Node>,
    Json(request): Json<ProfileRequest>,
) -> ApiResult<StatusCode> {
    node.publish_profile(request.display_name, request.bio)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn follow(State(node): State<Node>, Path(key): Path<String>) -> ApiResult<StatusCode> {
    node.follow(&parse_key(&key)?, true)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unfollow(State(node): State<Node>, Path(key): Path<String>) -> ApiResult<StatusCode> {
    node.follow(&parse_key(&key)?, false)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn blocked_cids(State(node): State<Node>) -> ApiResult<Json<Vec<ovn_database::BlockEntry>>> {
    Ok(Json(node.database().blocked_cids()?))
}

async fn block_cid(
    State(node): State<Node>,
    Path(cid): Path<String>,
    body: Option<Json<BlockRequest>>,
) -> ApiResult<StatusCode> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    node.block_cid(&parse_cid(&cid)?, &reason)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unblock_cid(State(node): State<Node>, Path(cid): Path<String>) -> ApiResult<StatusCode> {
    node.unblock_cid(&parse_cid(&cid)?)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn blocked_creators(
    State(node): State<Node>,
) -> ApiResult<Json<Vec<ovn_database::BlockEntry>>> {
    Ok(Json(node.database().blocked_creators()?))
}

async fn block_creator(
    State(node): State<Node>,
    Path(key): Path<String>,
    body: Option<Json<BlockRequest>>,
) -> ApiResult<StatusCode> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    node.block_creator(&parse_key(&key)?, &reason)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unblock_creator(
    State(node): State<Node>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    node.unblock_creator(&parse_key(&key)?)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn shutdown(State(node): State<Node>) -> ApiResult<StatusCode> {
    // Reply first, then stop: the caller should not see a dropped connection.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = node.shutdown().await;
    });
    Ok(StatusCode::ACCEPTED)
}

// ------------------------------------------------------------ the web UI

const VIEWER_HTML: &str = include_str!("ui/viewer.html");
const ADMIN_HTML: &str = include_str!("ui/admin.html");
const APP_CSS: &str = include_str!("ui/app.css");
const COMMON_JS: &str = include_str!("ui/common.js");
const VIEWER_JS: &str = include_str!("ui/viewer.js");
const ADMIN_JS: &str = include_str!("ui/admin.js");
const ZONES_JS: &str = include_str!("ui/zones.js");

fn page(html: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            // The UI loads nothing from anywhere else, so say so.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; media-src 'self' blob:; img-src 'self' data:; \
                 script-src 'self'; style-src 'self'; connect-src 'self'; \
                 base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ),
        ],
        html,
    )
        .into_response()
}

fn script(body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

async fn viewer_page() -> Response {
    page(VIEWER_HTML)
}

async fn admin_page() -> Response {
    page(ADMIN_HTML)
}

async fn asset_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        APP_CSS,
    )
        .into_response()
}

async fn asset_common_js() -> Response {
    script(COMMON_JS)
}

async fn asset_viewer_js() -> Response {
    script(VIEWER_JS)
}

async fn asset_admin_js() -> Response {
    script(ADMIN_JS)
}

async fn asset_zones_js() -> Response {
    script(ZONES_JS)
}

#[derive(Debug, serde::Deserialize)]
struct AuthQuery {
    token: String,
    #[serde(default)]
    next: Option<String>,
}

/// Exchange the API token for a cookie, once, so the browser can make
/// authenticated requests afterwards.
///
/// A page cannot attach an `Authorization` header to `<video src>`, `<img
/// src>` or an `EventSource`, so the UI needs a cookie. `SameSite=Strict`
/// means another site cannot make the browser send it.
async fn authenticate(State(node): State<Node>, Query(query): Query<AuthQuery>) -> Response {
    if !token_matches(&query.token, node.api_token()) {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    // Only a same-origin path, never an absolute or protocol-relative URL.
    let next = query
        .next
        .filter(|n| n.starts_with('/') && !n.starts_with("//"))
        .unwrap_or_else(|| "/ui".to_string());

    let cookie = format!(
        "{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
        node.api_token()
    );
    let mut response = Redirect::to(&next).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

const COOKIE_NAME: &str = "ovn_token";

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name.trim() == COOKIE_NAME).then(|| value.trim())
        })
}

fn token_matches(presented: &str, expected: &str) -> bool {
    presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

// ------------------------------------------------------------- languages

/// The languages this node can render its interface in, and what it would
/// choose if the browser expressed no preference.
///
/// `configured` is an operator's deliberate choice and outranks the
/// browser's language; `suggested` is only derived from the machine's own
/// settings, so it sits below. Neither is a lookup: nothing leaves the
/// machine to work either of them out.
async fn list_locales(State(node): State<Node>) -> Json<serde_json::Value> {
    let packs = crate::i18n::load(&node.config().locales_dir());
    let configured = node
        .config()
        .default_locale
        .as_deref()
        .and_then(|tag| crate::i18n::best_match(&packs, tag))
        .map(|pack| pack.locale.clone());
    let suggested = crate::i18n::system_locale();

    Json(serde_json::json!({
        "locales": crate::i18n::summarise(&packs),
        "configured": configured,
        "suggested": suggested,
    }))
}

/// One language pack, merged over English so the UI always has every key.
async fn get_locale(
    State(node): State<Node>,
    Path(code): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let packs = crate::i18n::load(&node.config().locales_dir());
    let (pack, _) = packs.get(&code).ok_or(ApiError(NodeError::NotFound))?;
    Ok(Json(serde_json::json!({
        "locale": pack.locale,
        "name": pack.name,
        "englishName": pack.english_name,
        "direction": pack.direction.as_str(),
        "strings": crate::i18n::merged_strings(pack),
    })))
}

// ----------------------------------------------------------- streaming

/// Media types we will echo back as a `Content-Type`.
///
/// The type comes out of a manifest a stranger wrote, and this response is
/// same-origin with the UI. Handing a browser `text/html` because a peer
/// asked us to would be a scripting hole, so anything unrecognised is served
/// as opaque bytes.
fn safe_media_type(declared: &str) -> &'static str {
    match declared.trim().to_ascii_lowercase().as_str() {
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "video/ogg" => "video/ogg",
        "video/x-matroska" => "video/x-matroska",
        "video/quicktime" => "video/quicktime",
        "audio/mpeg" => "audio/mpeg",
        "audio/mp4" => "audio/mp4",
        "audio/ogg" => "audio/ogg",
        "audio/opus" => "audio/opus",
        "audio/flac" => "audio/flac",
        "audio/wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// Serve a video, honouring `Range` so a player can seek, and fetching
/// chunks from peers as they are needed rather than up front.
async fn stream_video(
    State(node): State<Node>,
    Path(cid): Path<String>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let cid = parse_cid(&cid)?;
    let plan = node.prepare_stream(cid).await?;
    let total = plan.total_size;
    let media_type = safe_media_type(&plan.media_type);

    let Some(whole) = ByteRange::whole(total) else {
        return Ok((StatusCode::NO_CONTENT, "").into_response());
    };

    let requested = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, range) = match requested {
        None => (StatusCode::OK, whole),
        Some(raw) => match parse_range(raw, total) {
            Ok(range) => (StatusCode::PARTIAL_CONTENT, range),
            // Malformed headers are ignored and the whole thing is sent,
            // which is what RFC 9110 asks for.
            Err(RangeError::Malformed) => (StatusCode::OK, whole),
            Err(RangeError::Unsatisfiable) => {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{total}"))],
                    "",
                )
                    .into_response())
            }
        },
    };

    let mut response_headers = vec![
        (header::CONTENT_TYPE, media_type.to_string()),
        (header::ACCEPT_RANGES, "bytes".to_string()),
        (header::CONTENT_LENGTH, range.byte_count().to_string()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        (header::CACHE_CONTROL, "no-store".to_string()),
    ];
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.push((
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{total}", range.start, range.end),
        ));
    }

    // A HEAD is how a player asks for the length before it asks for bytes.
    let body = if method == axum::http::Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(node.stream_range(plan, range))
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;
    for (name, value) in response_headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    }
    Ok(response)
}

/// A video's thumbnail, fetched from a peer if we do not hold it.
async fn video_thumbnail(State(node): State<Node>, Path(cid): Path<String>) -> ApiResult<Response> {
    let cid = parse_cid(&cid)?;
    match node.thumbnail(&cid).await? {
        Some(jpeg) => Ok((
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                // Thumbnails are immutable: they are named by their hash.
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            jpeg,
        )
            .into_response()),
        // A JSON body, like every other error here: it tells a client the
        // difference between "no thumbnail for this video" and "no such
        // route".
        None => Err(ApiError(NodeError::NotFound)),
    }
}

// -------------------------------------------------------------- uploading

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// Comma separated, because this arrives in a query string.
    #[serde(default)]
    tags: Option<String>,
}

/// Largest upload accepted through the browser.
const MAX_UPLOAD_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Publish a file the browser sent, rather than one already on disk.
///
/// The body is streamed straight to a file so that a large video never has
/// to fit in memory, and the staged copy is removed once it has been chunked
/// into the block store.
async fn upload_video(
    State(node): State<Node>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> ApiResult<Json<PublishDto>> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let file_name = crate::node::sanitise_file_name(query.file_name.as_deref().unwrap_or("upload"));
    let staging = node.config().uploads_dir();
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| NodeError::Runtime(format!("creating {}: {e}", staging.display())))?;
    let staged = staging.join(format!("{}-{}", std::process::id(), file_name));

    let mut written: u64 = 0;
    {
        let mut file = tokio::fs::File::create(&staged)
            .await
            .map_err(|e| NodeError::Runtime(format!("creating {}: {e}", staged.display())))?;
        let mut stream = body.into_data_stream();
        while let Some(part) = stream.next().await {
            let part = part.map_err(|e| NodeError::Runtime(format!("upload failed: {e}")))?;
            written += part.len() as u64;
            if written > MAX_UPLOAD_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&staged).await;
                return Err(ApiError(NodeError::Runtime(format!(
                    "upload exceeds the {MAX_UPLOAD_BYTES} byte limit"
                ))));
            }
            file.write_all(&part)
                .await
                .map_err(|e| NodeError::Runtime(format!("writing the upload: {e}")))?;
        }
        file.flush()
            .await
            .map_err(|e| NodeError::Runtime(format!("writing the upload: {e}")))?;
    }
    if written == 0 {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(ApiError(NodeError::Content(
            ovn_content::ContentError::EmptyFile,
        )));
    }

    let tags = query
        .tags
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();

    let result = node
        .publish_video(
            &staged,
            query.title.filter(|t| !t.trim().is_empty()),
            query.description.unwrap_or_default(),
            tags,
        )
        .await;
    // The bytes now live in the block store, so the staged copy is dead
    // weight either way.
    let _ = tokio::fs::remove_file(&staged).await;
    Ok(Json(result?.into()))
}

// ------------------------------------------------------------ live events

/// Server-sent events: peers, discoveries and download progress.
async fn event_stream(
    State(node): State<Node>,
) -> Sse<impl futures::Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    use futures::StreamExt;

    let stream = tokio_stream::wrappers::BroadcastStream::new(node.subscribe())
        .filter_map(|result| async move {
            // A lagging consumer just misses events; it is never an error
            // worth tearing the stream down for.
            result.ok()
        })
        // Deliver the shutdown notice, then end. Otherwise this connection
        // would outlive the node it is reporting on.
        .scan(false, |ended, event: NodeEvent| {
            let finished = *ended;
            *ended = matches!(event, NodeEvent::ShuttingDown);
            async move { (!finished).then_some(event) }
        })
        .filter_map(|event| async move { Event::default().json_data(event).ok().map(Ok) });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ----------------------------------------------------------------- plumbing

fn parse_cid(text: &str) -> std::result::Result<ContentId, ApiError> {
    ContentId::parse(text).map_err(|e| ApiError(NodeError::Protocol(e)))
}

fn parse_key(text: &str) -> std::result::Result<PublicKey, ApiError> {
    PublicKey::from_hex(text).map_err(|e| ApiError(NodeError::Identity(e)))
}

type ApiResult<T> = std::result::Result<T, ApiError>;

pub(crate) struct ApiError(NodeError);

impl From<NodeError> for ApiError {
    fn from(e: NodeError) -> Self {
        Self(e)
    }
}

impl From<ovn_database::DatabaseError> for ApiError {
    fn from(e: ovn_database::DatabaseError) -> Self {
        Self(NodeError::Database(e))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            NodeError::NotFound | NodeError::NotFetched(_) => StatusCode::NOT_FOUND,
            NodeError::NoSuchFile(_) | NodeError::Protocol(_) | NodeError::Discovery(_) => {
                StatusCode::BAD_REQUEST
            }
            NodeError::Content(ovn_content::ContentError::EmptyFile)
            | NodeError::Content(ovn_content::ContentError::ChunkTooLarge { .. })
            | NodeError::Content(ovn_content::ContentError::MalformedManifest(_)) => {
                StatusCode::BAD_REQUEST
            }
            NodeError::Content(ovn_content::ContentError::Missing { .. }) => StatusCode::NOT_FOUND,
            NodeError::Content(ovn_content::ContentError::IntegrityFailure { .. }) => {
                StatusCode::BAD_GATEWAY
            }
            NodeError::InvalidPeerId(_) => StatusCode::BAD_REQUEST,
            NodeError::Blocked(_) => StatusCode::FORBIDDEN,
            NodeError::NoProviders(_) | NodeError::PeerUnreachable { .. } => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            NodeError::BlockUnavailable { .. } => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(serde_json::json!({ "error": self.0.to_string() }));
        (status, body).into_response()
    }
}
