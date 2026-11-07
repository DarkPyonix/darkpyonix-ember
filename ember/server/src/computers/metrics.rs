//! Computer metrics of a registered computer, relayed from its node (FR-X7, FR-L21).
//!
//! | Method | Path | Node route |
//! | ------ | ---- | ---------- |
//! | GET    | `/api/computers/{id}/metrics?top=` | `GET /metrics` |
//!
//! The body is the node's `MetricsSample` (`ember_node::proto`), unchanged. A node error keeps
//! its status and body; an unreachable node is 502; an unknown computer is 404; the local
//! computer (this server) has no node client, so it answers 400. The server keeps no samples:
//! the monitor sheet polls about once a second while it is open. **[provisional]**

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::proto::{MetricsQuery, MetricsSample};
use serde_json::json;

use super::{ComputerError, Computers};

pub fn router(computers: Arc<Computers>) -> Router {
    Router::new().route("/api/computers/{id}/metrics", get(metrics)).with_state(computers)
}

fn plain(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn metrics(
    State(c): State<Arc<Computers>>,
    Path(id): Path<String>,
    Query(q): Query<MetricsQuery>,
) -> Result<Json<MetricsSample>, Response> {
    let node = c.client(&id).map_err(|e| {
        let code = match &e {
            ComputerError::NotFound(_) | ComputerError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ComputerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ComputerError::Unreachable(..) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        plain(code, format!("{e:#}"))
    })?;
    match node.metrics(q.top).await {
        Ok(s) => Ok(Json(s)),
        Err(ClientError::Api { status, body }) => {
            Err((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response())
        }
        Err(other) => Err(plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {other}"))),
    }
}
