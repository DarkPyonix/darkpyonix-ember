//! HTTP API for the hub integration (FR-N2), in two routers:
//!
//! - [`router`] — reading the status and the account's devices, and adding one as a computer.
//!   Served like `/api/v1/computers` (TCP and, to allowed devices, the transport).
//! - [`admin_router`] — everything else. **Local only**: `main.rs` merges it into the TCP
//!   listener's app, never into the app served over the transport (like device management: a
//!   device must not be able to (de)register the server, let devices into the account or
//!   remove them).
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/v1/hub` | → `HubStatus` (503 when the hub is off) |
//! | GET    | `/api/v1/hub/devices` | → `[HubDeviceView]` (the account's devices) |
//! | POST   | `/api/v1/hub/devices/{endpoint_id}/computer` | `{name?, token}` → 201 `Computer` |
//! | *admin* | | |
//! | POST   | `/api/v1/hub/link` | `{name?}` → 202 `PendingView` (user code + verification URL); 409 if registered. A removed server may link again once the owner re-admitted it on darkpyonix.dev (else 409 with that advice) |
//! | DELETE | `/api/v1/hub/link` | → 204 (stop waiting) |
//! | POST   | `/api/v1/hub/check` | → `{state}`: `active`, `revoked`, `rejected`, `unreachable`, `unregistered` |
//! | DELETE | `/api/v1/hub/registration[?local=1]` | → 204: leave the account (removed on the hub with the server's own token, then forgotten); `local=1` only forgets here |
//! | DELETE | `/api/v1/hub/devices/{endpoint_id}` | → 204 (removed on the hub) |
//! | GET    | `/api/v1/hub/link-codes/{user_code}` | → `LinkCodeInfo` |
//! | POST   | `/api/v1/hub/link-codes/{user_code}` | `{approve}` → 204; 403 when the hub needs a browser session (a `main_server` link, a re-admitted device) |
//! | POST   | `/api/v1/hub/sync-devices` | → `SyncReport` (devices allow-list ← account) |
//! | PUT    | `/api/v1/hub/sync-devices` | `{enabled}` → 204 (periodic sync on/off) |
//!
//! A revoked registration answers `410 Gone` on calls that need the hub token.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ember_hub::{HubError, LinkError, RegistrationState};
use ember_transport::PeerId;
use serde::Deserialize;
use serde_json::json;

use super::{HubApiError, ServerHub};
use crate::computers::{ComputerError, Computers};

#[derive(Clone)]
struct AppState {
    hub: Option<Arc<ServerHub>>,
    computers: Arc<Computers>,
}

/// The shared routes. `hub` is `None` when the hub is off (no transport, or
/// `EMBER_HUB_URL=off`): every route then answers 503 with the reason.
pub fn router(hub: Option<Arc<ServerHub>>, computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/v1/hub", get(status))
        .route("/api/v1/hub/devices", get(devices))
        .route("/api/v1/hub/devices/{id}/computer", post(add_computer))
        .with_state(AppState { hub, computers })
}

/// The local-only routes (TCP listener only).
pub fn admin_router(hub: Option<Arc<ServerHub>>, computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/v1/hub/link", post(start_link).delete(cancel_link))
        .route("/api/v1/hub/check", post(check))
        .route("/api/v1/hub/registration", axum::routing::delete(forget))
        .route("/api/v1/hub/devices/{id}", axum::routing::delete(remove_device))
        .route("/api/v1/hub/link-codes/{code}", get(lookup_code).post(decide_code))
        .route("/api/v1/hub/sync-devices", post(sync_now).put(set_sync))
        .with_state(AppState { hub, computers })
}

struct ApiError(HubApiError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match &self.0 {
            HubApiError::NotRegistered => StatusCode::CONFLICT,
            HubApiError::Revoked(_) => StatusCode::GONE,
            HubApiError::AlreadyRegistered(_) => StatusCode::CONFLICT,
            HubApiError::NotFound(_) => StatusCode::NOT_FOUND,
            HubApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            HubApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            HubApiError::Link(LinkError::AlreadyRegistered(_)) => StatusCode::CONFLICT,
            HubApiError::Link(LinkError::Hub(e)) | HubApiError::Hub(e) => match e {
                HubError::BadRequest(_) => StatusCode::BAD_REQUEST,
                HubError::Forbidden(_) => StatusCode::FORBIDDEN,
                HubError::Conflict(_) => StatusCode::CONFLICT,
                HubError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::BAD_GATEWAY,
            },
            HubApiError::Link(_) => StatusCode::BAD_GATEWAY,
            HubApiError::Computer(ComputerError::BadRequest(_)) => StatusCode::BAD_REQUEST,
            HubApiError::Computer(ComputerError::NotFound(_)) => StatusCode::NOT_FOUND,
            HubApiError::Computer(_) | HubApiError::Device(_) | HubApiError::Other(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (code, Json(json!({ "error": format!("{:#}", self.0) }))).into_response()
    }
}

impl From<HubApiError> for ApiError {
    fn from(e: HubApiError) -> Self {
        ApiError(e)
    }
}

type ApiResult<T> = Result<T, ApiError>;

// An axum Response is the natural error for a handler helper; its size does not matter here.
#[allow(clippy::result_large_err)]
fn hub(s: &AppState) -> Result<&Arc<ServerHub>, Response> {
    s.hub.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "the hub is off: it needs EMBER_TRANSPORT=1, and EMBER_HUB_URL not set to off" })),
        )
            .into_response()
    })
}

macro_rules! hub_or_503 {
    ($s:expr) => {
        match hub(&$s) {
            Ok(h) => h.clone(),
            Err(r) => return Ok(r),
        }
    };
}

fn parse_peer(s: &str) -> Result<PeerId, ApiError> {
    s.parse().map_err(|e| ApiError(HubApiError::BadRequest(format!("endpoint id {s:?}: {e}"))))
}

async fn status(State(s): State<AppState>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    Ok(Json(hub.status()?).into_response())
}

#[derive(Deserialize, Default)]
struct LinkBody {
    #[serde(default)]
    name: String,
}

async fn start_link(State(s): State<AppState>, body: axum::body::Bytes) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    let body: LinkBody = if body.iter().all(u8::is_ascii_whitespace) {
        LinkBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError(HubApiError::BadRequest(format!("body: {e}"))))?
    };
    let pending = hub.start_link(&body.name).await?;
    Ok((StatusCode::ACCEPTED, Json(pending)).into_response())
}

async fn cancel_link(State(s): State<AppState>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    hub.cancel_link();
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn check(State(s): State<AppState>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    let state = match hub.check().await? {
        RegistrationState::Active(me) => json!({ "state": "active", "github_login": me.github_login }),
        RegistrationState::Revoked => json!({ "state": "revoked" }),
        RegistrationState::Rejected(e) => json!({ "state": "rejected", "error": e }),
        RegistrationState::Unreachable(e) => json!({ "state": "unreachable", "error": e }),
        RegistrationState::Unknown => json!({ "state": "unregistered" }),
    };
    Ok(Json(state).into_response())
}

#[derive(Deserialize, Default)]
struct ForgetQuery {
    #[serde(default)]
    local: Option<String>,
}

async fn forget(State(s): State<AppState>, Query(q): Query<ForgetQuery>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    let local_only = matches!(q.local.as_deref(), Some("1" | "true" | "yes"));
    hub.leave(local_only).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn devices(State(s): State<AppState>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    Ok(Json(hub.devices(&s.computers).await?).into_response())
}

async fn remove_device(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    hub.remove_device(parse_peer(&id)?).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct ComputerBody {
    #[serde(default)]
    name: Option<String>,
    token: String,
}

async fn add_computer(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<ComputerBody>,
) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    let c = hub.add_computer(&s.computers, parse_peer(&id)?, b.name.as_deref(), &b.token).await?;
    Ok((StatusCode::CREATED, Json(c)).into_response())
}

async fn lookup_code(State(s): State<AppState>, Path(code): Path<String>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    Ok(Json(hub.lookup_code(&code).await?).into_response())
}

#[derive(Deserialize)]
struct DecideBody {
    approve: bool,
}

async fn decide_code(
    State(s): State<AppState>,
    Path(code): Path<String>,
    Json(b): Json<DecideBody>,
) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    hub.decide_code(&code, b.approve).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn sync_now(State(s): State<AppState>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    Ok(Json(hub.sync_devices().await?).into_response())
}

#[derive(Deserialize)]
struct SyncBody {
    enabled: bool,
}

async fn set_sync(State(s): State<AppState>, Json(b): Json<SyncBody>) -> ApiResult<Response> {
    let hub = hub_or_503!(s);
    hub.set_sync_devices(b.enabled);
    Ok(StatusCode::NO_CONTENT.into_response())
}
