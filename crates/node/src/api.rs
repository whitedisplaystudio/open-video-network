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

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tokio::sync::oneshot;

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

impl ApiServer {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
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
        .merge(protected)
        .with_state(node)
}

/// Constant-time-ish bearer check. The token is 32 random bytes, so an
/// attacker guessing it is not the threat model; the check is here to stop a
/// different local user simply calling the API.
async fn require_token(
    State(node): State<Node>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let expected = node.api_token();
    if presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

// --------------------------------------------------------------- handlers

async fn health() -> &'static str {
    "ok"
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
