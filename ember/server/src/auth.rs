//! The local token and the Origin/Host check (SPEC FR-S9, FR-S10; INTENT D37, #338).
//!
//! The server keeps a random token in `<data dir>/server-token` (mode 0600). [`protect`] wraps a
//! router so that every route, including the fallback, needs `Authorization: Bearer <token>`.
//! The layer is applied once to the local TCP router in `main.rs`. The router served over the
//! transport is not wrapped: those requests were already authenticated by the transport's device
//! gate (FR-N3, FR-N7).
//!
//! Exempt from the token (each still gets the Origin and Host check):
//! - `GET /api/health`, so a launcher can tell whether a server is up *[provisional]*;
//! - the agent routes, which carry the session's runtime token in the same header and check it
//!   themselves: `/api/a2a/*` except `/api/a2a/settings`, and the Antigravity hook;
//! - `POST /api/channels/<id>/post`, where an agent answers a mention with its runtime token
//!   (exactly this path, nothing else under `/api/channels/`; the handler checks the token);
//! - `GET /api/browsers/<project>/cdp/<nonce>` and `.../cdp/<nonce>/json/version` with the per-run
//!   nonce from [`agent_cdp_nonce`], the agent browser relay and its discovery document (agent MCP
//!   servers send no header);
//! - `GET /auth/callback`, the browser redirect of Sign in with ChatGPT, which cannot carry a
//!   header (the `state` value in it is checked by the handler).
//!
//! `/api/guest/*` is deliberately not exempt: the guest router (`channels::guest`) is never served
//! under this layer; its door is the separate `EMBER_GUEST_LISTEN` listener. Merged here by
//! mistake, its routes would answer 401 without the server token, which fails loudly.
//!
//! A WebSocket upgrade takes the token from the header, or from a `Sec-WebSocket-Protocol` entry
//! `ember.token.<token>`. Never from the query string, which ends up in logs and history. When
//! the client also offers `ember.v1`, the server answers with that one, so the token is not
//! echoed back.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The token file's name in the data folder.
pub const TOKEN_FILE: &str = "server-token";
/// Prefix of the `Sec-WebSocket-Protocol` entry that carries the token.
pub const WS_TOKEN_PREFIX: &str = "ember.token.";
/// The protocol entry the server selects when the client offers it.
pub const WS_PROTOCOL: &str = "ember.v1";

/// The path of the token file inside `data_dir`.
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE)
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Reads the token from `<data_dir>/server-token`, creating the file (mode 0600) with a fresh
/// random token when it is missing or empty. Never logs the token.
pub fn load_or_create_token(data_dir: &Path) -> std::io::Result<String> {
    let path = token_path(data_dir);
    if let Ok(mut f) = std::fs::File::open(&path) {
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        let s = s.trim().to_string();
        if !s.is_empty() {
            restrict(&path)?;
            return Ok(s);
        }
    }
    let token = new_token();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    restrict(&path)?;
    Ok(token)
}

#[cfg(unix)]
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Constant-time equality: both sides are hashed to 32 bytes, then compared without an early
/// exit, so the time depends on neither the length nor the position of the first difference.
pub fn token_eq(a: &str, b: &str) -> bool {
    let (da, db) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    da.iter().zip(db.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The per-run nonce of the agent browser relay. The MCP servers an agent runs (chrome-devtools-mcp,
/// Playwright MCP) connect to the CDP relay with a bare URL and no way this repository could
/// verify to add a header, so the URL itself carries a random path segment,
/// `/api/browsers/<project>/cdp/<nonce>`. The nonce lives in memory only, is new on every server
/// start, is given only to agent configs, and opens exactly that one route (never a query string).
pub fn agent_cdp_nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(new_token)
}

/// True for `GET /api/browsers/<project>/cdp/<nonce>` and for the discovery document under it,
/// `GET /api/browsers/<project>/cdp/<nonce>/json/version`, with the right nonce (constant time).
/// A tool that discovers the relay through `json/version` (an HTTP endpoint instead of a ws URL)
/// sends no header either, and the document it gets must lead to a URL that opens.
pub fn agent_cdp_path_ok(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/browsers/") else { return false };
    let mut it = rest.split('/');
    match (it.next(), it.next(), it.next(), it.next(), it.next(), it.next()) {
        (Some(p), Some("cdp"), Some(n), None, ..) if !p.is_empty() => token_eq(n, agent_cdp_nonce()),
        (Some(p), Some("cdp"), Some(n), Some("json"), Some("version"), None) if !p.is_empty() => {
            token_eq(n, agent_cdp_nonce())
        }
        _ => false,
    }
}

/// What the middleware checks against.
pub struct ServerAuth {
    token: String,
    listen_ip: IpAddr,
    extra_hosts: HashSet<String>,
}

impl ServerAuth {
    /// `listen` is the local listener's address; `extra_hosts` are configured host names
    /// (`EMBER_ALLOWED_HOSTS`, the agent URL's host) accepted in `Host` besides the listen
    /// address and, for a loopback or unspecified listener, the loopback names.
    pub fn new(token: String, listen: SocketAddr, extra_hosts: impl IntoIterator<Item = String>) -> Arc<ServerAuth> {
        Arc::new(ServerAuth {
            token,
            listen_ip: listen.ip(),
            extra_hosts: extra_hosts
                .into_iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        })
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        if self.extra_hosts.contains(&host) {
            return true;
        }
        let ip = host.parse::<IpAddr>().ok();
        if ip == Some(self.listen_ip) {
            return true;
        }
        let loopbackish = self.listen_ip.is_loopback() || self.listen_ip.is_unspecified();
        if loopbackish && (host == "localhost" || ip.is_some_and(|a| a.is_loopback())) {
            return true;
        }
        // A listener on every interface is reached by any of the machine's addresses. An IP
        // literal cannot be a DNS rebinding name, so it is accepted; names must be configured.
        self.listen_ip.is_unspecified() && ip.is_some()
    }
}

/// Wraps `router` (every route and the fallback) in the Origin/Host check and the token check.
pub fn protect(router: Router, auth: Arc<ServerAuth>) -> Router {
    router.layer(middleware::from_fn_with_state(auth, guard))
}

fn refuse(status: StatusCode, msg: &str) -> Response {
    let mut r = (status, Json(json!({ "error": msg }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    r
}

/// True for exactly `/api/channels/<id>/post` (four segments, a non-empty id): the route where an
/// agent answers a mention with its runtime token (FR-T12). The rest of `/api/channels/*` carries
/// the user routes and keeps the server token.
fn agent_channel_post_path(path: &str) -> bool {
    let mut it = path.split('/');
    matches!(
        (it.next(), it.next(), it.next(), it.next(), it.next(), it.next(), it.next()),
        (Some(""), Some("api"), Some("channels"), Some(id), Some("post"), None, None) if !id.is_empty()
    )
}

/// True for the routes that check a session's runtime token themselves, and `GET /auth/callback`.
fn token_exempt(method: &axum::http::Method, path: &str) -> bool {
    if method == axum::http::Method::GET
        && (path == "/api/health" || path == crate::chatgpt::oauth::CALLBACK_PATH)
    {
        return true;
    }
    if method == axum::http::Method::GET && agent_cdp_path_ok(path) {
        return true;
    }
    if method == axum::http::Method::POST && agent_channel_post_path(path) {
        return true;
    }
    (path.starts_with("/api/a2a/") && path != "/api/a2a/settings")
        || path == crate::agents::antigravity::HOOK_ROUTE
}

fn is_upgrade(h: &HeaderMap) -> bool {
    h.get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn protocol_entries(h: &HeaderMap) -> Vec<String> {
    h.get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The presented token: the bearer header, else (WebSocket upgrades only) the protocol entry.
fn presented(h: &HeaderMap) -> Option<String> {
    if let Some(t) = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Some(t.to_string());
    }
    if is_upgrade(h) {
        return protocol_entries(h)
            .iter()
            .find_map(|e| e.strip_prefix(WS_TOKEN_PREFIX).map(str::to_string));
    }
    None
}

async fn guard(State(auth): State<Arc<ServerAuth>>, req: Request<Body>, next: Next) -> Response {
    let headers = req.headers();
    // FR-S10: Host first (DNS rebinding), then Origin (cross-site pages).
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    match host {
        Some(h) if auth.host_allowed(&crate::api::origin::host_of(&h)) => {}
        _ => return refuse(StatusCode::FORBIDDEN, "host not allowed"),
    }
    if !crate::api::origin::origin_allowed(headers) {
        return refuse(StatusCode::FORBIDDEN, "origin not allowed");
    }
    if !token_exempt(req.method(), req.uri().path()) {
        match presented(headers) {
            Some(t) if token_eq(&t, &auth.token) => {}
            _ => return refuse(StatusCode::UNAUTHORIZED, "missing or wrong server token"),
        }
    }
    let offered_v1 = protocol_entries(headers).iter().any(|e| e == WS_PROTOCOL);
    let mut resp = next.run(req).await;
    if offered_v1
        && resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && !resp.headers().contains_key(header::SEC_WEBSOCKET_PROTOCOL)
    {
        resp.headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static(WS_PROTOCOL));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_s9_only_the_channel_post_route_is_exempt_under_channels() {
        use axum::http::Method;
        assert!(token_exempt(&Method::POST, "/api/channels/c1/post"));
        assert!(!token_exempt(&Method::GET, "/api/channels/c1/post"));
        for p in ["/api/channels", "/api/channels/c1", "/api/channels/c1/messages", "/api/channels//post",
            "/api/channels/c1/post/x", "/api/channels/c1/x/post", "/api/channels/post"] {
            assert!(!token_exempt(&Method::POST, p), "{p}");
        }
    }

    #[test]
    fn fr_t14_the_guest_family_is_not_exempt() {
        use axum::http::Method;
        for m in [Method::GET, Method::POST] {
            for p in ["/api/guest/accept", "/api/guest/channels/c1/messages", "/api/guest/", "/api/guest"] {
                assert!(!token_exempt(&m, p), "{m} {p}");
            }
        }
    }

    #[test]
    fn fr_s9_token_eq_compares_whole_values() {
        assert!(token_eq("abc", "abc"));
        assert!(!token_eq("abc", "abd"));
        assert!(!token_eq("abc", "abcd"));
        assert!(!token_eq("", "abc"));
    }
}
