//! FR-S9 and FR-S10: the local token and the Origin/Host check (INTENT D37, #338).
//!
//! The routers are merged the way `main.rs` merges them, then wrapped once with
//! `ember_server::auth::protect`, so every family goes through the same layer.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::accounts::Accounts;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::auth::{self, ServerAuth};
use ember_server::browser::{BrowserConfig, BrowserManager};
use ember_server::computers::{self, Computers, Registry};
use ember_server::mcp::McpRegistry;
use ember_server::schedules::{Scheduler, SystemClock};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";

struct Fx {
    app: Router,
    token: String,
    _dir: tempfile::TempDir,
}

fn router(dir: &std::path::Path) -> Router {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let accounts = Accounts::open(store.clone(), dir).unwrap();
    let mcp = McpRegistry::open(store, dir).unwrap();
    let scheduler = Scheduler::new(sessions.clone(), Arc::new(SystemClock));
    let a2a = A2a::new(
        sessions.clone(),
        A2aStore::open_in_memory().unwrap(),
        A2aConfig::new("http://127.0.0.1:1"),
    );
    let browsers = BrowserManager::new(BrowserConfig::from_env(dir));
    ember_server::api::router(sessions.clone())
        .merge(computers::api::router(computers.clone(), sessions.clone()))
        .merge(computers::terms::router(computers.clone()))
        .merge(ember_server::api::git::router(sessions.clone(), computers))
        .merge(ember_server::schedules::api::agent_router(scheduler.clone(), a2a.clone()))
        .merge(ember_server::schedules::api::router(scheduler))
        .merge(ember_server::mcp::api::router(mcp))
        .merge(ember_server::a2a::api::router(a2a))
        .merge(ember_server::accounts::api::router(accounts))
        .merge(ember_server::browser::api::router(browsers))
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []);
    Fx { app: auth::protect(router(dir.path()), a), token, _dir: dir }
}

async fn call(
    app: &Router,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    // A test-supplied Host replaces the default; appending a second Host header would leave the
    // first one in force for `headers.get(HOST)`.
    let host = headers.iter().find(|(k, _)| *k == "host").map(|(_, v)| *v).unwrap_or(LISTEN);
    let mut b = Request::builder().method(method).uri(path).header("host", host);
    for (k, v) in headers {
        if *k != "host" {
            b = b.header(*k, *v);
        }
    }
    let r = app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// One route from each family served by the local listener.
const FAMILIES: &[(&str, &str)] = &[
    ("GET", "/api/sessions"),
    ("POST", "/api/sessions"),
    ("GET", "/api/projects"),
    ("GET", "/api/agents"),
    ("GET", "/api/computers"),
    ("GET", "/api/computers/c1/terms"),
    ("GET", "/api/accounts"),
    ("GET", "/api/mcp"),
    ("GET", "/api/schedules"),
    ("GET", "/api/browsers"),
    ("GET", "/api/a2a/settings"),
    ("PUT", "/api/a2a/settings"),
    ("POST", "/api/projects/p/git/status"),
    ("GET", "/api/no-such-route"),
];

fn m(s: &str) -> Method {
    Method::from_bytes(s.as_bytes()).unwrap()
}

#[tokio::test]
async fn fr_s9_no_token_is_401_json_on_every_family() {
    let f = fx();
    for (method, path) in FAMILIES {
        let (st, body) = call(&f.app, m(method), path, &[]).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert!(body["error"].is_string(), "{method} {path}: {body}");
    }
}

#[tokio::test]
async fn fr_s9_wrong_token_is_401() {
    let f = fx();
    for (method, path) in FAMILIES {
        for h in ["Bearer nope", "Bearer ", "Basic abc", "nope"] {
            let (st, _) = call(&f.app, m(method), path, &[("authorization", h)]).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{method} {path} with {h:?}");
        }
    }
    // A prefix of the right token is wrong too.
    let short = format!("Bearer {}", &f.token[..f.token.len() - 1]);
    let (st, _) = call(&f.app, Method::GET, "/api/sessions", &[("authorization", &short)]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_s9_right_token_passes_every_family() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    for (method, path) in FAMILIES {
        let (st, _) = call(&f.app, m(method), path, &[("authorization", &h)]).await;
        assert_ne!(st, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_ne!(st, StatusCode::FORBIDDEN, "{method} {path}");
    }
    let (st, body) = call(&f.app, Method::GET, "/api/sessions", &[("authorization", &h)]).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.is_array() || body.is_object());
}

#[tokio::test]
async fn fr_s9_health_is_exempt_and_only_health() {
    let f = fx();
    let (st, body) = call(&f.app, Method::GET, "/api/health", &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let (st, _) = call(&f.app, Method::POST, "/api/health", &[]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_s9_agent_routes_keep_their_runtime_token_auth() {
    let f = fx();
    // Not refused by the server token layer: the route's own check answers, with its `code`.
    for path in ["/api/a2a/targets", "/api/a2a/team", "/api/a2a/schedules"] {
        let (st, body) = call(&f.app, Method::GET, path, &[("authorization", "Bearer runtime-token")]).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["code"], "unauthorized", "{path}: {body}");
    }
    // The server token is not a runtime token.
    let h = format!("Bearer {}", f.token);
    let (st, body) = call(&f.app, Method::GET, "/api/a2a/targets", &[("authorization", &h)]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
}

#[tokio::test]
async fn fr_s9_token_file_is_created_0600_and_reused() {
    let dir = tempfile::tempdir().unwrap();
    let path = auth::token_path(dir.path());
    assert!(!path.exists());
    let t1 = auth::load_or_create_token(dir.path()).unwrap();
    assert_eq!(path.file_name().unwrap(), "server-token");
    assert!(t1.len() >= 32, "token is long enough");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // A restart reads the same token.
    assert_eq!(auth::load_or_create_token(dir.path()).unwrap(), t1);
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), t1);
    // A file that was left too open is tightened.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(auth::load_or_create_token(dir.path()).unwrap(), t1);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Another data folder gets another token.
    let other = tempfile::tempdir().unwrap();
    assert_ne!(auth::load_or_create_token(other.path()).unwrap(), t1);
}

#[tokio::test]
async fn fr_s9_token_eq_is_exact() {
    assert!(auth::token_eq("a", "a"));
    assert!(!auth::token_eq("a", "b"));
    assert!(!auth::token_eq("a", "ab"));
}

#[tokio::test]
async fn fr_s10_cross_origin_is_refused_on_every_family() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    for (method, path) in FAMILIES {
        for origin in ["https://evil.example", "null", "http://127.0.0.1.evil.example"] {
            let (st, body) = call(
                &f.app,
                m(method),
                path,
                &[("authorization", &h), ("origin", origin)],
            )
            .await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{method} {path} origin {origin}");
            assert!(body["error"].is_string());
        }
    }
    // The exempt routes get the check too.
    let (st, _) = call(&f.app, Method::GET, "/api/health", &[("origin", "https://evil.example")]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &f.app,
        Method::GET,
        "/api/a2a/targets",
        &[("authorization", "Bearer x"), ("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_s10_no_origin_and_loopback_origin_pass_the_check_but_still_need_the_token() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    let (st, _) = call(&f.app, Method::GET, "/api/sessions", &[("authorization", &h)]).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &f.app,
        Method::GET,
        "/api/sessions",
        &[("authorization", &h), ("origin", "http://localhost:5173")],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Passing the Origin check is not authentication.
    let (st, _) = call(&f.app, Method::GET, "/api/sessions", &[("origin", "http://localhost:5173")]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_s10_foreign_host_is_refused() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    for host in ["evil.example", "evil.example:8740", "127.0.0.1.evil.example:8740", "192.168.1.9:8740"] {
        for (method, path) in [("GET", "/api/sessions"), ("GET", "/api/health"), ("GET", "/api/a2a/targets")] {
            let (st, _) = call(&f.app, m(method), path, &[("authorization", &h), ("host", host)]).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{path} host {host}");
        }
    }
    for host in ["127.0.0.1:8740", "localhost:8740", "localhost"] {
        let (st, _) = call(&f.app, Method::GET, "/api/sessions", &[("authorization", &h), ("host", host)]).await;
        assert_eq!(st, StatusCode::OK, "host {host}");
    }
}

#[tokio::test]
async fn fr_s10_configured_host_names_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), "0.0.0.0:8740".parse().unwrap(), ["Mini.local".to_string()]);
    let app = auth::protect(router(dir.path()), a);
    let h = format!("Bearer {token}");
    for host in ["mini.local:8740", "192.168.1.9:8740", "127.0.0.1:8740"] {
        let (st, _) = call(&app, Method::GET, "/api/sessions", &[("authorization", &h), ("host", host)]).await;
        assert_eq!(st, StatusCode::OK, "host {host}");
    }
    let (st, _) = call(&app, Method::GET, "/api/sessions", &[("authorization", &h), ("host", "other.local")]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

// ---- WebSocket upgrades ----

async fn serve(f: Fx) -> (SocketAddr, String, tempfile::TempDir) {
    // The Host check needs the real port, so protect again with the bound address.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let dir = f._dir;
    let app = auth::protect(router(dir.path()), ServerAuth::new(f.token.clone(), addr, []));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (addr, f.token, dir)
}

fn ws_request(addr: SocketAddr, path: &str, headers: &[(&str, String)]) -> Request<()> {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    for (k, v) in headers {
        req.headers_mut().insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
    }
    req
}

async fn status_of(req: Request<()>) -> Result<(), u16> {
    match tokio_tungstenite::connect_async(req).await {
        Ok(_) => Ok(()),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[tokio::test]
async fn fr_s9_websocket_takes_the_header_or_the_protocol_entry_never_the_query() {
    let (addr, token, _dir) = serve(fx()).await;
    // No token.
    assert_eq!(status_of(ws_request(addr, "/api/push", &[])).await, Err(401));
    // Query string is not read.
    assert_eq!(status_of(ws_request(addr, &format!("/api/push?token={token}"), &[])).await, Err(401));
    // Wrong tokens.
    let wrong = [("authorization", "Bearer nope".to_string())];
    assert_eq!(status_of(ws_request(addr, "/api/push", &wrong)).await, Err(401));
    let wrong = [("sec-websocket-protocol", "ember.v1, ember.token.nope".to_string())];
    assert_eq!(status_of(ws_request(addr, "/api/push", &wrong)).await, Err(401));
    // Header.
    let ok = [("authorization", format!("Bearer {token}"))];
    assert_eq!(status_of(ws_request(addr, "/api/push", &ok)).await, Ok(()));
    // Protocol entry; the server selects ember.v1 and does not echo the token.
    let ok = [("sec-websocket-protocol", format!("ember.v1, ember.token.{token}"))];
    let (_ws, resp) = tokio_tungstenite::connect_async(ws_request(addr, "/api/push", &ok)).await.unwrap();
    assert_eq!(resp.headers().get("sec-websocket-protocol").unwrap(), "ember.v1");
    // A cross-site page with the right token is still refused (FR-S10).
    let evil = [
        ("authorization", format!("Bearer {token}")),
        ("origin", "https://evil.example".to_string()),
    ];
    assert_eq!(status_of(ws_request(addr, "/api/push", &evil)).await, Err(403));
}

#[tokio::test]
async fn fr_s9_protocol_entry_is_not_a_token_on_plain_http() {
    let f = fx();
    let h = format!("ember.token.{}", f.token);
    let (st, _) = call(&f.app, Method::GET, "/api/sessions", &[("sec-websocket-protocol", &h)]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

// The agent browser relay (FR-S9): the MCP servers an agent runs send no header, so the config
// carries a per-run nonce path segment that opens exactly one route.

const WS_UPGRADE: &[(&str, &str)] = &[
    ("connection", "upgrade"),
    ("upgrade", "websocket"),
    ("sec-websocket-version", "13"),
    ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
];

#[test]
fn fr_s9_agent_config_carries_the_cdp_nonce() {
    use ember_server::browser::agent::{agent_cdp_ws_url, agent_config, BrowserMcp};
    let nonce = auth::agent_cdp_nonce();
    assert!(nonce.len() >= 32);
    let ws = agent_cdp_ws_url("http://127.0.0.1:8740", "acme");
    assert_eq!(ws, format!("ws://127.0.0.1:8740/api/browsers/acme/cdp/{nonce}"));
    assert!(!ws.contains('?'), "never a query string");
    let cfg = agent_config(&ws, BrowserMcp::ChromeDevtools);
    assert!(cfg.claude_code_mcp_config.to_string().contains(nonce));
    assert!(cfg.codex_toml.contains(nonce));
}

#[tokio::test]
async fn fr_s9_cdp_route_with_the_right_nonce_passes_the_middleware() {
    let f = fx();
    let path = format!("/api/browsers/acme/cdp/{}", auth::agent_cdp_nonce());
    let (st, _) = call(&f.app, Method::GET, &path, WS_UPGRADE).await;
    assert_ne!(st, StatusCode::UNAUTHORIZED, "{path}");
    assert_ne!(st, StatusCode::FORBIDDEN, "{path}");
}

#[tokio::test]
async fn fr_s9_cdp_route_with_a_wrong_nonce_is_401() {
    let f = fx();
    for p in [
        "/api/browsers/acme/cdp/wrong",
        "/api/browsers/acme/cdp/",
        "/api/browsers/acme/cdp",
        "/api/browsers/acme/view",
    ] {
        let (st, _) = call(&f.app, Method::GET, p, WS_UPGRADE).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{p}");
    }
    // The nonce opens only the cdp route, and only for GET.
    let n = auth::agent_cdp_nonce();
    for (method, p) in [
        (Method::POST, format!("/api/browsers/acme/cdp/{n}")),
        (Method::GET, format!("/api/browsers/acme/view/{n}")),
        (Method::GET, format!("/api/sessions/{n}")),
    ] {
        let (st, _) = call(&f.app, method.clone(), &p, &[]).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{method} {p}");
    }
}

#[tokio::test]
async fn fr_s10_cdp_nonce_route_still_checks_origin_and_host() {
    let f = fx();
    let path = format!("/api/browsers/acme/cdp/{}", auth::agent_cdp_nonce());
    let mut h = WS_UPGRADE.to_vec();
    h.push(("origin", "https://evil.example"));
    let (st, _) = call(&f.app, Method::GET, &path, &h).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let mut h = WS_UPGRADE.to_vec();
    h.push(("host", "evil.example"));
    let (st, _) = call(&f.app, Method::GET, &path, &h).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

// Discovery through `/json/version` (FR-S9). A tool given an HTTP endpoint instead of a ws URL
// reads `<endpoint>/json/version` and connects to its `webSocketDebuggerUrl`. It sends no header,
// so both the document and the URL in it must open without the server token.

/// The layer in front of a router that answers 200 on every path, so only the middleware's verdict
/// shows (the real handlers would start a browser).
fn gate_only() -> Router {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token, LISTEN.parse().unwrap(), []);
    auth::protect(Router::new().fallback(|| async { "ok" }), a)
}

#[test]
fn fr_s9_cdp_version_document_points_at_the_nonce_url() {
    use ember_server::browser::agent::{agent_cdp_ws_url, cdp_version_doc};
    let doc = cdp_version_doc("http://127.0.0.1:8740", "acme");
    let url = doc["webSocketDebuggerUrl"].as_str().unwrap();
    assert_eq!(url, agent_cdp_ws_url("http://127.0.0.1:8740", "acme"));
    assert!(url.ends_with(auth::agent_cdp_nonce()), "{url}");
}

#[test]
fn fr_s9_the_url_in_the_discovery_document_passes_the_token_layer() {
    use ember_server::browser::agent::cdp_version_doc;
    let doc = cdp_version_doc("http://127.0.0.1:8740", "acme");
    let url = doc["webSocketDebuggerUrl"].as_str().unwrap();
    let path = url.strip_prefix("ws://127.0.0.1:8740").unwrap();
    assert!(auth::agent_cdp_path_ok(path), "{path}");
}

#[tokio::test]
async fn fr_s9_cdp_version_under_the_nonce_needs_no_token() {
    let app = gate_only();
    let ok = format!("/api/browsers/acme/cdp/{}/json/version", auth::agent_cdp_nonce());
    let (st, _) = call(&app, Method::GET, &ok, &[]).await;
    assert_eq!(st, StatusCode::OK, "{ok}");
}

#[tokio::test]
async fn fr_s9_cdp_version_without_the_nonce_or_with_a_wrong_one_is_401() {
    let app = gate_only();
    let n = auth::agent_cdp_nonce();
    for p in [
        "/api/browsers/acme/cdp/json/version".to_string(),
        "/api/browsers/acme/cdp/wrong/json/version".to_string(),
        format!("/api/browsers/acme/cdp/{n}/json/other"),
        format!("/api/browsers/acme/cdp/{n}/json/version/more"),
        format!("/api/browsers/acme/cdp/{n}/json"),
        "/api/browsers/acme/agent-config".to_string(),
    ] {
        let (st, _) = call(&app, Method::GET, &p, &[]).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{p}");
    }
    let (st, _) = call(&app, Method::POST, &format!("/api/browsers/acme/cdp/{n}/json/version"), &[]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "the nonce opens GET only");
}

#[tokio::test]
async fn fr_s10_cdp_version_under_the_nonce_still_checks_origin_and_host() {
    let app = gate_only();
    let p = format!("/api/browsers/acme/cdp/{}/json/version", auth::agent_cdp_nonce());
    let (st, _) = call(&app, Method::GET, &p, &[("origin", "https://evil.example")]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&app, Method::GET, &p, &[("host", "evil.example")]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}
