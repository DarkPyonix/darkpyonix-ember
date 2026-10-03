//! HTTP API for devices (FR-N3). **Local only**: `main.rs` merges this router into the TCP
//! listener's app, never into the app served over the transport.
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/v1/devices` | → `[Device]` |
//! | POST   | `/api/v1/devices` | `{peer_id, name}` → 201 `Device` |
//! | DELETE | `/api/v1/devices/{peer_id}` | → `{closed}` (connections closed); 404 if unknown |

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use ember_transport::PeerId;
use serde::Deserialize;
use serde_json::json;

use super::{DeviceError, Devices};

pub fn router(devices: Arc<Devices>) -> Router {
    Router::new()
        .route("/api/v1/devices", get(list).post(add))
        .route("/api/v1/devices/{peer}", delete(remove))
        .with_state(devices)
}

struct ApiError(DeviceError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match &self.0 {
            DeviceError::NotFound(_) => StatusCode::NOT_FOUND,
            DeviceError::BadRequest(_) => StatusCode::BAD_REQUEST,
            DeviceError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (code, Json(json!({ "error": format!("{:#}", self.0) }))).into_response()
    }
}

impl From<DeviceError> for ApiError {
    fn from(e: DeviceError) -> Self {
        ApiError(e)
    }
}

fn parse_peer(s: &str) -> Result<PeerId, ApiError> {
    s.parse().map_err(|e| ApiError(DeviceError::BadRequest(format!("peer id {s:?}: {e}"))))
}

async fn list(State(d): State<Arc<Devices>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(d.list()?))
}

#[derive(Deserialize)]
struct AddBody {
    peer_id: String,
    name: String,
}

async fn add(State(d): State<Arc<Devices>>, Json(b): Json<AddBody>) -> Result<impl IntoResponse, ApiError> {
    let peer = parse_peer(&b.peer_id)?;
    Ok((StatusCode::CREATED, Json(d.add(peer, &b.name)?)))
}

async fn remove(State(d): State<Arc<Devices>>, Path(peer): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let peer = parse_peer(&peer)?;
    let closed = d.remove(&peer)?;
    Ok(Json(json!({ "closed": closed })))
}
