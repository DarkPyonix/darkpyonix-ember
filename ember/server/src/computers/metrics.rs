//! Computer metrics of a computer, relayed from its node (FR-X7, FR-L21).
//!
//! | Method | Path | Node route |
//! | ------ | ---- | ---------- |
//! | GET    | `/api/computers/{id}/metrics?top=` | `GET /metrics` |
//!
//! The body is the node's `MetricsSample` (`ember_node::proto`), unchanged. A node error keeps
//! its status and body; an unreachable node is 502; an unknown computer is 404. The local
//! computer (this server) is answered by the server's in-process node
//! ([`Computers::relay_client`], #981), and its sample carries the pids of the agents this server
//! started (`sessions`, [`crate::agents::proc::agent_pids`], #389): every agent process runs on
//! this machine, so a registered computer's sample has none. The server keeps no samples: the
//! monitor sheet polls about once a second while it is open. **[provisional]**

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::proto::{MetricsQuery, MetricsSample, SessionProcess};
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
    let local = id == super::LOCAL;
    let node = c.relay_client(&id).map_err(|e| {
        let code = match &e {
            ComputerError::NotFound(_) | ComputerError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ComputerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ComputerError::Unreachable(..) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        plain(code, format!("{e:#}"))
    })?;
    match node.metrics(q.top).await {
        Ok(mut s) => {
            if local {
                s.sessions = crate::agents::proc::agent_pids()
                    .into_iter()
                    .map(|(session, pid)| SessionProcess { pid, session })
                    .collect();
            }
            Ok(Json(s))
        }
        Err(ClientError::Api { status, body }) => {
            Err((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response())
        }
        Err(other) => Err(plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {other}"))),
    }
}
