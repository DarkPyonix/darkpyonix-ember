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
//!
//! The token is the non-owner credential (SPEC `FR-S12`, #784): the routes that grant authority,
//! admit a party or change a secret also need owner presence, the in-memory key of
//! [`crate::owner`]. [`protect_with_owner`] applies both layers, and is what `main.rs` uses.

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
    write_token(&path, &token)?;
    Ok(token)
}

/// Writes `token` to `path` (mode 0600) through a temporary file in the same folder and a rename,
/// so a reader sees the old token or the new one, never half a file.
fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let res = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        restrict(&tmp)?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Replaces `<data_dir>/server-token` with a fresh random token and returns it (#369). Used after
/// revoking a device that could have read the old one (INTENT D37). A running server whose
/// [`ServerAuth`] watches the file ([`ServerAuth::with_token_file`]) takes the new token on the
/// next request that presents it; `POST /api/server-token/rotate` does both at once.
pub fn rotate_token(data_dir: &Path) -> std::io::Result<String> {
    let token = new_token();
    write_token(&token_path(data_dir), &token)?;
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

/// The variable that lets the owner listener (`EMBER_LISTEN`) bind a non-loopback address
/// (FR-S10, INTENT D37). Without it such a bind is a startup error, because the listener speaks
/// plain HTTP and the bearer token would cross the network in clear.
pub const ALLOW_NON_LOOPBACK_LISTEN: &str = "EMBER_ALLOW_NON_LOOPBACK_LISTEN";

/// What the listen guard decided for `EMBER_LISTEN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenCheck {
    /// A loopback address: no warning.
    Loopback,
    /// A non-loopback address the owner allowed with `EMBER_ALLOW_NON_LOOPBACK_LISTEN=1`:
    /// the server starts and logs a warning.
    NonLoopbackAllowed,
}

/// Parses `EMBER_LISTEN` (`None` gives the default `127.0.0.1:8740`). `localhost:<port>` is
/// read as `127.0.0.1:<port>`, so the guard never needs DNS.
pub fn parse_listen(value: Option<&str>) -> Result<SocketAddr, String> {
    let v = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or("127.0.0.1:8740");
    if let Some(port) = v.strip_prefix("localhost:") {
        let port: u16 = port.parse().map_err(|_| format!("EMBER_LISTEN: bad port in {v:?}"))?;
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    v.parse().map_err(|_| format!("EMBER_LISTEN: {v:?} is not ip:port"))
}

/// The listen guard (FR-S10). A loopback address passes. Any other address (`0.0.0.0`, `::`, a
/// LAN address, an IPv4-mapped IPv6 address) is refused unless `allow` is exactly `"1"`. Remote
/// devices are meant to come in through the transport door, which checks the device key.
pub fn check_listen(addr: SocketAddr, allow: Option<&str>) -> Result<ListenCheck, String> {
    if addr.ip().is_loopback() {
        return Ok(ListenCheck::Loopback);
    }
    if allow.map(str::trim) == Some("1") {
        return Ok(ListenCheck::NonLoopbackAllowed);
    }
    Err(format!(
        "EMBER_LISTEN={addr} is not a loopback address. The owner listener speaks plain HTTP and the \
         server token would cross the network in clear; remote devices come in through the transport \
         (EMBER_TRANSPORT=1). Set {ALLOW_NON_LOOPBACK_LISTEN}=1 to bind it anyway."
    ))
}

/// What the middleware checks against.
pub struct ServerAuth {
    /// The token in force. Replaced by a rotation (#369); the old one is refused from then on.
    token: std::sync::RwLock<String>,
    /// The token file, when the server follows it: a presented token that differs from the one
    /// in force makes the server re-read the file if it changed since (a CLI rotation).
    file: Option<PathBuf>,
    /// The file's modification time when it was last read.
    file_mtime: std::sync::Mutex<Option<std::time::SystemTime>>,
    listen_ip: IpAddr,
    extra_hosts: HashSet<String>,
}

impl ServerAuth {
    /// `listen` is the local listener's address; `extra_hosts` are configured host names
    /// (`EMBER_ALLOWED_HOSTS`, the agent URL's host) accepted in `Host` besides the listen
    /// address and, for a loopback or unspecified listener, the loopback names.
    pub fn new(token: String, listen: SocketAddr, extra_hosts: impl IntoIterator<Item = String>) -> Arc<ServerAuth> {
        Arc::new(ServerAuth {
            token: std::sync::RwLock::new(token),
            file: None,
            file_mtime: std::sync::Mutex::new(None),
            listen_ip: listen.ip(),
            extra_hosts: extra_hosts
                .into_iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        })
    }

    /// Like [`ServerAuth::new`], with the token read from `<data_dir>/server-token` (created when
    /// missing) and followed: [`rotate_token`] run by another process (the CLI) takes effect on
    /// the next request, and [`ServerAuth::rotate`] writes the file (#369).
    pub fn with_token_file(
        data_dir: &Path,
        listen: SocketAddr,
        extra_hosts: impl IntoIterator<Item = String>,
    ) -> std::io::Result<Arc<ServerAuth>> {
        let token = load_or_create_token(data_dir)?;
        let path = token_path(data_dir);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let a = ServerAuth::new(token, listen, extra_hosts);
        let mut a = Arc::try_unwrap(a).unwrap_or_else(|_| unreachable!("fresh Arc"));
        a.file = Some(path);
        a.file_mtime = std::sync::Mutex::new(mtime);
        Ok(Arc::new(a))
    }

    /// True when `presented` is the token in force (constant time). On a mismatch a followed
    /// token file is re-read once if its modification time changed, so a token rotated by the CLI
    /// is accepted and the old one is refused from then on.
    pub fn check(&self, presented: &str) -> bool {
        if token_eq(presented, &self.token.read().unwrap_or_else(|e| e.into_inner())) {
            return true;
        }
        let Some(path) = &self.file else { return false };
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else { return false };
        {
            let mut seen = self.file_mtime.lock().unwrap_or_else(|e| e.into_inner());
            if *seen == Some(mtime) {
                return false;
            }
            *seen = Some(mtime);
        }
        let Ok(text) = std::fs::read_to_string(path) else { return false };
        let fresh = text.lines().next().unwrap_or_default().trim().to_string();
        if fresh.is_empty() {
            return false;
        }
        let ok = token_eq(presented, &fresh);
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        ok
    }

    /// Rotates the token: a fresh one is written to the followed file (when there is one) and put
    /// in force at once; the old token gets 401 from the next request (#369). Returns the token
    /// file's path, never the token.
    pub fn rotate(&self) -> std::io::Result<Option<PathBuf>> {
        let token = new_token();
        if let Some(path) = &self.file {
            write_token(path, &token)?;
            let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            *self.file_mtime.lock().unwrap_or_else(|e| e.into_inner()) = mtime;
        }
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = token;
        Ok(self.file.clone())
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

/// The one list of owner routes, for both doors (`FR-S12`).
pub use crate::owner::owner_route;

/// [`protect`], plus owner presence on the owner routes (`FR-S12`, #784): the Origin and Host
/// check, then the server token (401), then the owner key on an owner route (403
/// `owner_required`). The local listener's app is wrapped with this.
pub fn protect_with_owner(router: Router, auth: Arc<ServerAuth>, owner: Arc<crate::owner::Owner>) -> Router {
    protect(crate::owner::require(router, owner), auth)
}

/// `POST /api/server-token/rotate` (#369): rotates the server token. Mount it on the local
/// listener only, inside [`protect_with_owner`]: it is an owner route (`FR-S12`), so the server
/// token alone does not rotate it.
/// Answers `{rotated: true, token_file}`; the new token is never in the answer, so a client on
/// this computer re-reads the file (FR-S11) and a device that only had the old token is locked out.
pub fn rotate_router(auth: Arc<ServerAuth>) -> Router {
    Router::new().route(ROTATE_ROUTE, axum::routing::post(rotate_route)).with_state(auth)
}

/// The rotate route's path.
pub const ROTATE_ROUTE: &str = "/api/server-token/rotate";

async fn rotate_route(State(auth): State<Arc<ServerAuth>>) -> Response {
    match auth.rotate() {
        Ok(file) => {
            tracing::info!("server token rotated");
            Json(json!({ "rotated": true, "token_file": file.map(|p| p.display().to_string()) })).into_response()
        }
        Err(e) => {
            tracing::error!("server token rotation failed: {e}");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "token rotation failed")
        }
    }
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
/// Public so a test can walk every exempt family (FR-S9).
pub fn token_exempt(method: &axum::http::Method, path: &str) -> bool {
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
            Some(t) if auth.check(&t) => {}
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

    /// FR-A5: the question route is an owner route: never exempt, for any method.
    #[test]
    fn fr_a5_the_question_answer_route_is_not_token_exempt() {
        use axum::http::Method;
        for m in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
            for p in ["/api/sessions/s1/questions/q1/answer", "/api/sessions/s1/questions/q1", "/api/sessions/s1/approvals/a1"] {
                assert!(!token_exempt(&m, p), "{m} {p}");
            }
        }
    }

    /// FR-T11, FR-A5: answering a question from the channel feed is an owner route like the
    /// approval answer beside it: under `/api/channels/` only the agent `post` route is exempt.
    #[test]
    fn fr_t11_the_channel_question_answer_route_is_not_token_exempt() {
        use axum::http::Method;
        for m in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
            for p in [
                "/api/channels/c1/questions/q1/answer",
                "/api/channels/c1/questions/post",
                "/api/channels/c1/questions/q1/answer/post",
                "/api/channels/c1/feed/a1/answer",
            ] {
                assert!(!token_exempt(&m, p), "{m} {p}");
            }
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

    /// FR-S8 (#311): the per-harness settings routes are owner routes, never exempt. The nearby
    /// exempt `/api/agents/antigravity/hook` is matched by its whole path only.
    #[test]
    fn fr_s8_settings_paths_are_not_token_exempt() {
        use axum::http::Method;
        for m in [Method::GET, Method::PUT, Method::POST, Method::DELETE] {
            for p in [
                "/api/agents/settings",
                "/api/agents/codex/settings",
                "/api/agents/claude-code/settings",
                "/api/agents/antigravity/settings",
                "/api/agents/scripted/settings",
            ] {
                assert!(!token_exempt(&m, p), "{m} {p}");
            }
        }
    }

    /// FR-C2e: the enrol routes and the mint and owner routes are never exempt from the token.
    /// The enrol door is not behind `protect` at all (its own listener), so an exemption here would
    /// only ever open an owner route.
    #[test]
    fn fr_c2_redeem_is_not_token_exempt() {
        use axum::http::Method;
        for m in [Method::GET, Method::POST, Method::DELETE, Method::PUT] {
            for p in [
                "/enrol/redeem",
                "/enrol/redeem/",
                "/enrol/status/x",
                "/enrol/status/",
                "/enrol",
                "/api/computers/enrol-codes",
                "/api/computers/enrol-codes/x",
                "/api/computers/enrol-codes/x/confirm",
                "/api/computers/enrol-codes/x/deny",
            ] {
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
