//! HTTP client for the main server's `/api/v1` (see `crates/server/src/api/mod.rs`).
//!
//! An [`Api`] reaches the server one of two ways, behind the same methods:
//!
//! - **HTTP** ([`Api::new`]): a base URL over TCP.
//! - **Transport** ([`Api::over_transport`]): the peer-to-peer transport (SPEC `FR-N1`), dialing
//!   the server by its [`PeerAddr`] for service [`SERVER_SERVICE`]. The server admits only
//!   devices on its allow-list (`FR-N3`). Each request runs on a fresh stream of one cached
//!   connection; the push socket ([`Api::open_push`]) gets its own stream.

use std::fmt;

use ember_transport::{Dialer, PeerAddr};
use http::{header, Method, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;

use crate::wire::{
    ApprovalDecision, DetectedAgent, MentionCandidate, NewSession, Project, SearchHit, SessionDetail, SessionPatch,
    SessionRecord, StoredEvent, TeamMail, TeamMember, TeamView,
};

/// Transport service name of the server API (matches `ember_server::transport::SERVER_SERVICE`).
pub const SERVER_SERVICE: &str = "ember-server/1";

/// Host name used in requests over the transport (the stream already names the peer).
const PEER_HOST: &str = "ember-server";

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The peer-to-peer transport failed (dial, stream, or HTTP over a stream).
    #[error("peer connection failed: {0}")]
    Peer(String),
    #[error("server answered {status}: {message}")]
    Status { status: u16, message: String },
    #[error("invalid response: {0}")]
    Decode(String),
    #[error("push connection failed: {0}")]
    Push(String),
    #[error("invalid base url {0:?}")]
    BadUrl(String),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

fn peer_err(e: impl fmt::Display) -> ApiError {
    ApiError::Peer(e.to_string())
}

pub type ApiResult<T> = Result<T, ApiError>;

/// `GET /health`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub push_version: u32,
}

/// `POST /sessions/{id}/lease` answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lease {
    pub ttl_secs: u64,
}

/// Any byte stream the push WebSocket can run on (TCP or a transport stream).
pub trait PushIo: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> PushIo for T {}

/// The push WebSocket, whichever way the server is reached.
pub type PushSocket = WebSocketStream<Box<dyn PushIo>>;

#[derive(Debug, Clone)]
enum Reach {
    Http(reqwest::Client),
    Peer { dialer: Dialer, addr: PeerAddr },
}

/// A main server's HTTP API. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Api {
    reach: Reach,
    /// e.g. `http://127.0.0.1:8740`, no trailing slash; `peer:<id>` over the transport.
    base: String,
}

/// A status and a fully read body.
struct Raw {
    status: StatusCode,
    body: Bytes,
}

impl Api {
    pub fn new(base_url: &str) -> ApiResult<Api> {
        let base = base_url.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(ApiError::BadUrl(base_url.to_string()));
        }
        Ok(Api { reach: Reach::Http(reqwest::Client::new()), base })
    }

    /// The server reached over the transport at `addr` (dialed through `dialer`, which holds
    /// this device's identity).
    pub fn over_transport(dialer: Dialer, addr: impl Into<PeerAddr>) -> Api {
        let addr = addr.into();
        Api { base: format!("peer:{}", addr.peer), reach: Reach::Peer { dialer, addr } }
    }

    /// The base URL, or `peer:<id>` over the transport.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// The server's transport address, when reached over the transport.
    pub fn peer(&self) -> Option<&PeerAddr> {
        match &self.reach {
            Reach::Peer { addr, .. } => Some(addr),
            Reach::Http(_) => None,
        }
    }

    /// The push WebSocket URL (`ws(s)://…/api/v1/push`).
    pub fn push_url(&self) -> String {
        match &self.reach {
            Reach::Http(_) => {
                let rest = self.base.strip_prefix("http").unwrap_or(&self.base);
                format!("ws{rest}/api/v1/push")
            }
            Reach::Peer { .. } => format!("ws://{PEER_HOST}/api/v1/push"),
        }
    }

    /// `path` is below `/api/v1` and may carry a query string.
    async fn send(&self, method: Method, path: &str, json: Option<serde_json::Value>) -> ApiResult<Raw> {
        match &self.reach {
            Reach::Http(http) => {
                let mut req = http.request(method, format!("{}/api/v1{path}", self.base));
                if let Some(body) = json {
                    req = req.json(&body);
                }
                let res = req.send().await?;
                let status = res.status();
                Ok(Raw { status, body: res.bytes().await? })
            }
            Reach::Peer { dialer, addr } => {
                let stream = dialer.open_bi(addr, SERVER_SERVICE).await.map_err(peer_err)?;
                let mut sender =
                    ember_transport::http::http1_handshake::<Full<Bytes>>(stream).await.map_err(peer_err)?;
                let mut req = hyper::Request::builder()
                    .method(method)
                    .uri(format!("http://{PEER_HOST}/api/v1{path}"))
                    .header(header::HOST, PEER_HOST);
                let body = match json {
                    Some(v) => {
                        req = req.header(header::CONTENT_TYPE, "application/json");
                        Full::new(Bytes::from(serde_json::to_vec(&v).expect("json value serialises")))
                    }
                    None => Full::default(),
                };
                let req = req.body(body).map_err(peer_err)?;
                let resp = sender.send_request(req).await.map_err(peer_err)?;
                let status = resp.status();
                let body = resp.into_body().collect().await.map_err(peer_err)?.to_bytes();
                Ok(Raw { status, body })
            }
        }
    }

    fn check(raw: Raw) -> ApiResult<Raw> {
        if raw.status.is_success() {
            return Ok(raw);
        }
        let body = String::from_utf8_lossy(&raw.body).into_owned();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        Err(ApiError::Status { status: raw.status.as_u16(), message })
    }

    fn decode<T: DeserializeOwned>(raw: Raw) -> ApiResult<T> {
        serde_json::from_slice(&Self::check(raw)?.body).map_err(|e| ApiError::Decode(e.to_string()))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        Self::decode(self.send(Method::GET, path, None).await?)
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> ApiResult<Raw> {
        Self::check(self.send(Method::POST, path, Some(body)).await?)
    }

    /// Opens the push WebSocket (`/api/v1/push`).
    pub async fn open_push(&self) -> ApiResult<PushSocket> {
        let url = self.push_url();
        let io: Box<dyn PushIo> = match &self.reach {
            Reach::Http(_) => {
                let rest = url
                    .strip_prefix("ws://")
                    .ok_or_else(|| ApiError::Push(format!("{url}: only plain ws:// is supported")))?;
                let authority = rest.split('/').next().unwrap_or_default();
                let (host, port) = authority_addr(authority);
                let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .map_err(|e| ApiError::Push(e.to_string()))?;
                let _ = tcp.set_nodelay(true);
                Box::new(tcp)
            }
            Reach::Peer { dialer, addr } => Box::new(dialer.open_bi(addr, SERVER_SERVICE).await.map_err(peer_err)?),
        };
        let (ws, _) = tokio_tungstenite::client_async(url, io).await.map_err(|e| ApiError::Push(e.to_string()))?;
        Ok(ws)
    }

    pub async fn health(&self) -> ApiResult<Health> {
        self.get("/health").await
    }

    pub async fn agents(&self) -> ApiResult<Vec<DetectedAgent>> {
        self.get("/agents").await
    }

    /// All sessions, or one project's, most recently updated first.
    pub async fn sessions(&self, project: Option<&str>) -> ApiResult<Vec<SessionRecord>> {
        match project {
            Some(p) => {
                let q = serde_urlencoded::to_string([("project", p)]).map_err(|e| ApiError::Decode(e.to_string()))?;
                self.get(&format!("/sessions?{q}")).await
            }
            None => self.get("/sessions").await,
        }
    }

    pub async fn session(&self, id: &str) -> ApiResult<SessionDetail> {
        self.get(&format!("/sessions/{}", seg(id))).await
    }

    pub async fn create_session(&self, new: &NewSession) -> ApiResult<SessionRecord> {
        let body = serde_json::to_value(new).expect("NewSession serialises");
        Self::decode(self.post("/sessions", body).await?)
    }

    /// Stored events with `seq > after`, oldest first.
    pub async fn events(&self, id: &str, after: i64) -> ApiResult<Vec<StoredEvent>> {
        self.get(&format!("/sessions/{}/events?after={after}", seg(id))).await
    }

    pub async fn send_message(&self, id: &str, text: &str) -> ApiResult<()> {
        self.post(&format!("/sessions/{}/messages", seg(id)), json!({ "text": text })).await?;
        Ok(())
    }

    pub async fn answer(&self, id: &str, approval_id: &str, decision: ApprovalDecision) -> ApiResult<()> {
        let path = format!("/sessions/{}/approvals/{}", seg(id), seg(approval_id));
        self.post(&path, json!({ "decision": decision })).await?;
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> ApiResult<()> {
        self.post(&format!("/sessions/{}/interrupt", seg(id)), json!({})).await?;
        Ok(())
    }

    /// Keep `id`'s agent alive while it is being viewed (FR-S6).
    pub async fn lease(&self, id: &str) -> ApiResult<Lease> {
        Self::decode(self.post(&format!("/sessions/{}/lease", seg(id)), json!({})).await?)
    }

    // ---- projects and computer assignment (FR-L4) ------------------------------------------

    /// Every project with its assigned computers.
    pub async fn projects(&self) -> ApiResult<Vec<Project>> {
        self.get("/projects").await
    }

    /// Create a project (no error when it exists).
    pub async fn create_project(&self, name: &str) -> ApiResult<Project> {
        Self::decode(self.post("/projects", json!({ "name": name })).await?)
    }

    /// Assign `computer_id` (`local` = the main server) to `project`. Idempotent.
    pub async fn assign_computer(&self, project: &str, computer_id: &str) -> ApiResult<Project> {
        let path = format!("/projects/{}/computers/{}", seg(project), seg(computer_id));
        Self::decode(self.send(Method::PUT, &path, None).await?)
    }

    /// Unassign `computer_id` from `project`. Idempotent.
    pub async fn unassign_computer(&self, project: &str, computer_id: &str) -> ApiResult<Project> {
        let path = format!("/projects/{}/computers/{}", seg(project), seg(computer_id));
        Self::decode(self.send(Method::DELETE, &path, None).await?)
    }

    // ---- session metadata, search, export, fork (FR-L9, FR-S4, FR-S5) -----------------------

    /// Rename, pin or archive a session; answers the updated record.
    pub async fn patch_session(&self, id: &str, patch: &SessionPatch) -> ApiResult<SessionRecord> {
        let body = serde_json::to_value(patch).expect("SessionPatch serialises");
        Self::decode(self.send(Method::PATCH, &format!("/sessions/{}", seg(id)), Some(body)).await?)
    }

    /// Full-text search across every session's messages, best match first.
    pub async fn search(&self, query: &str, limit: Option<usize>) -> ApiResult<Vec<SearchHit>> {
        let mut q = vec![("q", query.to_string())];
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        let q = serde_urlencoded::to_string(q).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&format!("/search?{q}")).await
    }

    /// The session as the server's self-contained export file (`format: "ember-transcript"`).
    pub async fn export_session(&self, id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/sessions/{}/export", seg(id))).await
    }

    /// Fork a session (FR-S5). Every server so far answers 501 (`ApiError::Status`); check
    /// [`SessionDetail::can_fork`] first.
    pub async fn fork_session(&self, id: &str, after_seq: Option<i64>) -> ApiResult<SessionRecord> {
        let body = match after_seq {
            Some(seq) => json!({ "after_seq": seq }),
            None => json!({}),
        };
        Self::decode(self.post(&format!("/sessions/{}/fork", seg(id)), body).await?)
    }

    // ---- teams and mentions (FR-T6, FR-T7) ---------------------------------------------------

    /// The team `id` leads or belongs to (`None` when it is in none).
    pub async fn session_team(&self, id: &str) -> ApiResult<Option<TeamView>> {
        self.get(&format!("/sessions/{}/team", seg(id))).await
    }

    pub async fn team(&self, team_id: &str) -> ApiResult<TeamView> {
        self.get(&format!("/teams/{}", seg(team_id))).await
    }

    /// A team's mail, oldest first: the last `limit` (server default 20), or the first after
    /// mail number `after`.
    pub async fn team_mail(&self, team_id: &str, after: i64, limit: Option<usize>) -> ApiResult<Vec<TeamMail>> {
        let mut q = vec![("after", after.to_string())];
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        let q = serde_urlencoded::to_string(q).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&format!("/teams/{}/mail?{q}", seg(team_id))).await
    }

    /// End a teammate as the user: its agent stops and it leaves the team.
    pub async fn end_teammate(&self, team_id: &str, session_id: &str) -> ApiResult<TeamMember> {
        let path = format!("/teams/{}/members/{}/end", seg(team_id), seg(session_id));
        Self::decode(self.post(&path, json!({})).await?)
    }

    /// Sessions a message typed in `id` may mention as `@@...`, filtered by `query` (title
    /// substring or id prefix).
    pub async fn mention_candidates(&self, id: &str, query: &str, limit: Option<usize>) -> ApiResult<Vec<MentionCandidate>> {
        let mut q = vec![("q", query.to_string())];
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        let q = serde_urlencoded::to_string(q).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&format!("/sessions/{}/mentions?{q}", seg(id))).await
    }
}

/// `host:port` (default port 80) for `TcpStream::connect`, with IPv6 brackets removed.
fn authority_addr(authority: &str) -> (String, u16) {
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(80);
            return (host.to_string(), port);
        }
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse() {
            Ok(port) => (h.to_string(), port),
            Err(_) => (authority.to_string(), 80),
        },
        None => (authority.to_string(), 80),
    }
}

/// Percent-encode one path segment (ids are UUIDs today, but approval ids come from agents).
fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let api = Api::new("http://h:1/").unwrap();
        assert_eq!(api.push_url(), "ws://h:1/api/v1/push");
        assert_eq!(Api::new("https://h").unwrap().push_url(), "wss://h/api/v1/push");
        assert!(Api::new("h:1").is_err());
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
        assert_eq!(authority_addr("h:1"), ("h".to_string(), 1));
        assert_eq!(authority_addr("h"), ("h".to_string(), 80));
        assert_eq!(authority_addr("[::1]:8740"), ("::1".to_string(), 8740));
    }
}
