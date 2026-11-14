//! Git per project (FR-G4): `POST /api/projects/{name}/git/<op>`, relayed to the node.
//!
//! | Op | Body (without `repo`) |
//! | -- | --------------------- |
//! | `status`, `conflicts` | `{}` |
//! | `toplevel` | `{}` (FR-L31.9: the work tree root and the session directory's path below it) |
//! | `diff` | `{path?, staged, include_untracked?}` |
//! | `stage`, `unstage` | `{paths}` |
//! | `apply` | `{patch, reverse}` |
//! | `commit` | `{summary, description?, amend}` |
//! | `checkout` | `{branch}` |
//! | `branch` | `{name, from?}` |
//! | `fetch`, `pull`, `push` | `{remote?}` |
//! | `branches` | `{}` (FR-G9) |
//! | `prs` | `{state?, limit?}` (FR-G8, through `gh` on the project's computer) |
//! | `pr` | `{number}` (FR-G10) |
//! | `pr/review` | `{number, action, body?, confirm}` (FR-G11) |
//! | `pr/merge` | `{number, method, delete_branch, head_sha, confirm}` (FR-G11) |
//! | `pr/create` | `{title, body?, base?, head?, draft, confirm}` (FR-G12) |
//! | `commit/show` | `{sha}` (FR-G14) |
//! | `revert`, `cherry-pick` | `{sha, confirm}` (FR-G15) |
//! | `merge` | `{}` (FR-G16) |
//! | `merge/abort`, `merge/commit` | `{confirm}` (FR-G17) |
//! | `blame` | `{path, line, contents?}` (FR-L31.8: one line's last commit for the editor) |
//!
//! The server does not interpret the body. It adds `repo`, the working directory of the
//! project's most recent unarchived session (or of the session named by `?session=<id>`), and
//! posts it to `/git/<op>` on that session's computer (the project's first assigned remote
//! computer when the session has none recorded, else this server, through an in-process node).
//! The node's status and body come back unchanged, so a 404 `not_a_repo` or a 409 reaches the
//! client as the node sent it. An unreachable node is 502; an unknown project, op or a project
//! without a session is 404.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use ember_node::api::Node;
use ember_node::config::NodeConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::computers::{Computers, LOCAL};
use crate::session::Sessions;

/// Every operation the node serves under `/git/`.
pub const OPS: &[&str] =
    &["status", "toplevel", "diff", "stage", "unstage", "apply", "commit", "checkout", "branch", "fetch", "pull", "push", "conflicts", "log", "branches", "prs", "pr", "pr/review", "pr/merge", "pr/create", "commit/show", "revert", "cherry-pick", "merge", "merge/abort", "merge/commit", "stash/list", "stash/push", "stash/apply", "stash/pop", "stash/drop", "conflict", "resolve", "blame"];

#[derive(Clone)]
struct GitState {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    local: Arc<Mutex<Option<Router>>>,
}

/// A fresh slot for the in-process node used by [`local_call`].
pub(crate) fn new_local() -> Arc<Mutex<Option<Router>>> {
    Arc::new(Mutex::new(None))
}

pub fn router(sessions: Arc<Sessions>, computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/projects/{name}/git/{*op}", post(relay))
        .with_state(GitState { sessions, computers, local: Arc::new(Mutex::new(None)) })
}

fn error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

#[derive(Deserialize)]
struct Q {
    session: Option<String>,
}

async fn relay(
    State(s): State<GitState>,
    Path((name, op)): Path<(String, String)>,
    Query(q): Query<Q>,
    body: Option<Json<Value>>,
) -> Response {
    if !OPS.contains(&op.as_str()) {
        return error(StatusCode::NOT_FOUND, format!("unknown git operation {op}"));
    }
    let store = s.sessions.store();
    match store.project(&name) {
        Ok(Some(_)) => {}
        Ok(None) => return error(StatusCode::NOT_FOUND, format!("project {name} not found")),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
    let mut body = match body.map(|b| b.0).unwrap_or_else(|| json!({})) {
        Value::Object(m) => m,
        _ => return error(StatusCode::BAD_REQUEST, "the body must be a JSON object"),
    };
    // Pick the session whose directory is the repository.
    let rec = match &q.session {
        Some(id) => match store.session(id) {
            Ok(Some(r)) if r.project == name => Some(r),
            Ok(_) => return error(StatusCode::NOT_FOUND, format!("session {id} not in project {name}")),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        },
        None => match store.sessions(Some(&name)) {
            Ok(all) => all.iter().find(|r| !r.archived).or(all.first()).cloned(),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        },
    };
    let Some(rec) = rec else {
        return error(StatusCode::NOT_FOUND, format!("project {name} has no session, so no repository"));
    };
    let computer_id = match s.computers.registry().session_computer(&rec.id) {
        Ok(Some(sc)) => sc.computer_id,
        Ok(None) => store
            .project(&name)
            .ok()
            .flatten()
            .and_then(|p| p.computers.into_iter().find(|c| c != LOCAL))
            .unwrap_or_else(|| LOCAL.to_string()),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    body.insert("repo".into(), Value::String(rec.cwd));
    let body = Value::Object(body);
    let path = format!("/git/{op}");

    let (status, bytes) = if computer_id == LOCAL {
        match local_call(&s.local, &path, &body).await {
            Ok(r) => r,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        }
    } else {
        let client = match s.computers.client(&computer_id) {
            Ok(c) => c,
            Err(e) => return error(StatusCode::BAD_GATEWAY, format!("{e:#}")),
        };
        match client.post_raw(&path, &body).await {
            Ok(r) => r,
            Err(e) => return error(StatusCode::BAD_GATEWAY, format!("computer {computer_id}: {e}")),
        }
    };
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut resp = (status, bytes).into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    resp
}

/// This server as a computer: an in-process node rooted at `$HOME`, built on first use.
pub(crate) async fn local_call(local: &Mutex<Option<Router>>, path: &str, body: &Value) -> anyhow::Result<(u16, Vec<u8>)> {
    const TOKEN: &str = "local";
    let app = {
        let mut slot = local.lock().unwrap();
        if slot.is_none() {
            let roots = std::env::var_os("HOME").map(std::path::PathBuf::from).into_iter().collect();
            *slot = Some(ember_node::api::router(Node::new(NodeConfig::new(TOKEN, roots))?));
        }
        slot.clone().expect("just set")
    };
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))?;
    let resp = app.oneshot(req).await?;
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024).await?;
    Ok((status, bytes.to_vec()))
}
