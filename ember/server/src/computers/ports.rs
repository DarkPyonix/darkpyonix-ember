//! Listening ports of a registered computer's sessions, relayed from its node (FR-P7).
//!
//! | Method | Path | Node route |
//! | ------ | ---- | ---------- |
//! | GET    | `/api/computers/{id}/ports` | `GET /ports` |
//!
//! The body is the node's `PortsResponse` (`ember_node::proto`), unchanged. A node error keeps
//! its status and body; an unreachable node is 502; an unknown computer is 404; the local
//! computer (this server) has no node client, so it answers 400. The server keeps no state. **[provisional]**

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::proto::PortsResponse;
use serde_json::json;

use super::{ComputerError, Computers};

pub fn router(computers: Arc<Computers>) -> Router {
    Router::new().route("/api/computers/{id}/ports", get(ports)).with_state(computers)
}

fn plain(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn ports(
    State(c): State<Arc<Computers>>,
    Path(id): Path<String>,
) -> Result<Json<PortsResponse>, Response> {
    let node = c.client(&id).map_err(|e| {
        let code = match &e {
            ComputerError::NotFound(_) | ComputerError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ComputerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ComputerError::Unreachable(..) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        plain(code, format!("{e:#}"))
    })?;
    match node.ports().await {
        Ok(s) => Ok(Json(s)),
        Err(ClientError::Api { status, body }) => {
            Err((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response())
        }
        Err(other) => Err(plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {other}"))),
    }
}
