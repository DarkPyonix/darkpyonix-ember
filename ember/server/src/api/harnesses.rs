//! Per-computer harness detection (FR-A6.1).
//!
//! | Method | Path | Response |
//! | ------ | ---- | -------- |
//! | POST   | `/api/computers/{id}/harnesses/detect` | `[{harness, installed, version, path, detected_at}]` |
//!
//! Runs every probe now (no cache). `version` and `path` are `null` when unknown, never a made-up
//! value. Only the local computer is implemented: a registered computer answers 501
//! `not_implemented` until the node can probe its own CLIs (open, needs a second real node to
//! verify), and an unknown id answers 404. **[provisional]**

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

use crate::computers::{Computers, LOCAL};
use crate::session::Sessions;

#[derive(Clone)]
struct St {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
}

pub fn router(sessions: Arc<Sessions>, computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/computers/{id}/harnesses/detect", post(detect))
        .with_state(St { sessions, computers })
}

fn error(code: StatusCode, tag: &str, msg: String) -> Response {
    (code, Json(json!({ "error": msg, "code": tag }))).into_response()
}

async fn detect(State(s): State<St>, Path(id): Path<String>) -> Response {
    if id != LOCAL {
        match s.computers.registry().get(&id) {
            Ok(Some(_)) => {
                return error(
                    StatusCode::NOT_IMPLEMENTED,
                    "not_implemented",
                    format!("harness detection on computer {id} is not implemented yet: the node has no probe"),
                )
            }
            Ok(None) => {
                return error(StatusCode::NOT_FOUND, "not_found", format!("no such computer: {id}"))
            }
            Err(e) => {
                return error(StatusCode::INTERNAL_SERVER_ERROR, "internal", format!("{e:#}"))
            }
        }
    }
    let rows: Vec<_> = s
        .sessions
        .probe_agents()
        .await
        .into_iter()
        .map(|p| {
            json!({
                "harness": p.kind.as_str(),
                "installed": p.installed,
                "version": p.version,
                "path": p.path,
                "detected_at": p.detected_at,
            })
        })
        .collect();
    Json(rows).into_response()
}
