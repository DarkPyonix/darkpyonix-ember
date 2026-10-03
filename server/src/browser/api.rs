//! HTTP and WebSocket routes for the remote browser. Protocol: `docs/design/REMOTE-BROWSER.md`.
//!
//! | Route (under `/api/v1/browsers`) | |
//! | --- | --- |
//! | `GET /` | list browsers |
//! | `GET /{project}` | one browser's info and state |
//! | `POST /{project}` `{egress?}` | start (or keep running); `egress` given → switch to it |
//! | `PUT /{project}/egress` `{egress}` | switch egress (restart on the same profile) |
//! | `DELETE /{project}` | stop |
//! | `DELETE /{project}/data` | clear the profile (FR-R4) |
//! | `POST /{project}/input` | one input event (same JSON as on the view socket) |
//! | `GET /{project}/view` (WebSocket) | screencast frames out, input in |
//! | `GET /{project}/cdp` (WebSocket) | DevTools relay for agents |
//! | `GET /{project}/cdp/json/version` | DevTools discovery document pointing at the relay |
//! | `GET /{project}/agent-config?mcp=` | MCP config for Claude Code and Codex |

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio_tungstenite::tungstenite::Message as TMessage;

use super::agent::{self, BrowserMcp};
use super::{BrowserInstance, BrowserManager, InputEvent, STREAM_VERSION};

pub fn router(browsers: Arc<BrowserManager>) -> Router {
    Router::new()
        .route("/api/v1/browsers", get(list))
        .route("/api/v1/browsers/{project}", get(info).post(open).delete(stop))
        .route("/api/v1/browsers/{project}/egress", put(egress))
        .route("/api/v1/browsers/{project}/data", axum::routing::delete(clear))
        .route("/api/v1/browsers/{project}/input", post(input))
        .route("/api/v1/browsers/{project}/view", get(view))
        .route("/api/v1/browsers/{project}/cdp", get(cdp))
        .route("/api/v1/browsers/{project}/cdp/json/version", get(cdp_version))
        .route("/api/v1/browsers/{project}/agent-config", get(agent_config))
        .with_state(browsers)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;
type S = State<Arc<BrowserManager>>;

fn bad(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
}

async fn instance(m: &BrowserManager, project: &str) -> ApiResult<Arc<BrowserInstance>> {
    m.instance(project).await.map_err(bad)
}

async fn list(State(m): S) -> impl IntoResponse {
    Json(json!({ "v": STREAM_VERSION, "chrome": m.config().chrome, "browsers": m.list().await }))
}

async fn info(State(m): S, Path(project): Path<String>) -> ApiResult<impl IntoResponse> {
    let b = m
        .get(&project)
        .await
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no browser for {project}")))?;
    Ok(Json(b.info().await))
}

#[derive(Deserialize, Default)]
struct OpenBody {
    /// Absent: keep the current egress. `null`: direct. A string: that proxy.
    #[serde(default, deserialize_with = "some_option")]
    egress: Option<Option<String>>,
}

/// Distinguish an absent field (`None`) from an explicit `null` (`Some(None)`).
fn some_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

async fn open(
    State(m): S,
    Path(project): Path<String>,
    body: Option<Json<OpenBody>>,
) -> ApiResult<impl IntoResponse> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let b = instance(&m, &project).await?;
    match body.egress {
        Some(e) => b.set_egress(e).await.map_err(bad)?,
        None => b.ensure_running().await?,
    }
    Ok(Json(b.info().await))
}

#[derive(Deserialize)]
struct EgressBody {
    egress: Option<String>,
}

async fn egress(
    State(m): S,
    Path(project): Path<String>,
    Json(body): Json<EgressBody>,
) -> ApiResult<impl IntoResponse> {
    let b = instance(&m, &project).await?;
    b.set_egress(body.egress).await.map_err(bad)?;
    Ok(Json(b.info().await))
}

async fn stop(State(m): S, Path(project): Path<String>) -> ApiResult<impl IntoResponse> {
    if let Some(b) = m.get(&project).await {
        b.stop().await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn clear(State(m): S, Path(project): Path<String>) -> ApiResult<impl IntoResponse> {
    m.clear_data(&project).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn input(
    State(m): S,
    Path(project): Path<String>,
    Json(ev): Json<InputEvent>,
) -> ApiResult<impl IntoResponse> {
    let b = instance(&m, &project).await?;
    b.input(ev).await.map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- view stream ----

async fn view(
    State(m): S,
    Path(project): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<impl IntoResponse> {
    let b = instance(&m, &project).await?;
    Ok(ws.on_upgrade(move |socket| view_loop(socket, b)))
}

fn state_msg(kind: &str, b: &BrowserInstance) -> Message {
    let st = b.state().borrow().clone();
    let msg = json!({ "type": kind, "v": STREAM_VERSION, "project": b.project, "state": st });
    Message::Text(msg.to_string().into())
}

async fn view_loop(socket: WebSocket, b: Arc<BrowserInstance>) {
    let (mut tx, mut rx) = socket.split();
    let _viewer = b.add_viewer();
    let mut st = b.state();
    let mut frames = b.frames();
    if tx.send(state_msg("hello", &b)).await.is_err() {
        return;
    }
    // A start failure shows up as `state.error`; the viewer stays connected.
    {
        let b = b.clone();
        tokio::spawn(async move {
            let _ = b.ensure_running().await;
        });
    }
    st.borrow_and_update();
    loop {
        tokio::select! {
            r = st.changed() => {
                if r.is_err() { return; }
                st.borrow_and_update();
                if tx.send(state_msg("state", &b)).await.is_err() { return; }
            }
            f = frames.recv() => match f {
                Ok(frame) => {
                    if tx.send(Message::Binary(frame)).await.is_err() { return; }
                }
                // Dropped frames are fine for a video stream: the next one supersedes them.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            },
            m = rx.next() => {
                let text = match m {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => continue,
                };
                let res = match serde_json::from_str::<InputEvent>(text.as_str()) {
                    Ok(ev) => b.input(ev).await,
                    Err(e) => Err(anyhow::anyhow!("bad input message: {e}")),
                };
                if let Err(e) = res {
                    let msg = json!({ "type": "error", "v": STREAM_VERSION, "message": format!("{e:#}") });
                    if tx.send(Message::Text(msg.to_string().into())).await.is_err() { return; }
                }
            }
        }
    }
}

// ---- DevTools relay for agents ----

async fn cdp(
    State(m): S,
    Path(project): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<impl IntoResponse> {
    let b = instance(&m, &project).await?;
    let upstream = b.devtools_ws().await?;
    Ok(ws.on_upgrade(move |socket| relay(socket, b, upstream)))
}

async fn relay(socket: WebSocket, b: Arc<BrowserInstance>, upstream: String) {
    let up = match tokio_tungstenite::connect_async(&upstream).await {
        Ok((up, _)) => up,
        Err(e) => {
            tracing::warn!("CDP relay: {e}");
            return;
        }
    };
    b.agent_connected(1);
    let (mut up_tx, mut up_rx) = up.split();
    let (mut tx, mut rx) = socket.split();
    let agent_to_chrome = async {
        while let Some(Ok(m)) = rx.next().await {
            let m = match m {
                Message::Text(t) => TMessage::Text(t.as_str().into()),
                Message::Binary(d) => TMessage::Binary(d),
                Message::Close(_) => break,
                _ => continue,
            };
            // Every agent command marks activity, and waits while the user has taken over.
            b.agent_command().await;
            if up_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = up_tx.close().await;
    };
    let chrome_to_agent = async {
        while let Some(Ok(m)) = up_rx.next().await {
            let m = match m {
                TMessage::Text(t) => Message::Text(t.as_str().into()),
                TMessage::Binary(d) => Message::Binary(d),
                TMessage::Close(_) => break,
                _ => continue,
            };
            if tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = tx.close().await;
    };
    tokio::select! {
        _ = agent_to_chrome => {}
        _ = chrome_to_agent => {}
    }
    b.agent_connected(-1);
}

fn base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:8740");
    format!("http://{host}")
}

async fn cdp_version(
    State(m): S,
    Path(project): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    instance(&m, &project).await?;
    Ok(Json(json!({
        "Browser": "ember-remote-browser",
        "Protocol-Version": "1.3",
        "webSocketDebuggerUrl": agent::cdp_ws_url(&base_url(&headers), &project),
    })))
}

#[derive(Deserialize)]
struct AgentConfigQuery {
    mcp: Option<String>,
}

async fn agent_config(
    State(m): S,
    Path(project): Path<String>,
    Query(q): Query<AgentConfigQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    instance(&m, &project).await?;
    let mcp = match q.mcp.as_deref() {
        None => BrowserMcp::ChromeDevtools,
        Some(s) => BrowserMcp::parse(s)
            .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("unknown mcp {s}")))?,
    };
    let ws = agent::cdp_ws_url(&base_url(&headers), &project);
    Ok(Json(agent::agent_config(&ws, mcp)))
}
