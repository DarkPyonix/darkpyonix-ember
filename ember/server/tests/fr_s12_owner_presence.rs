//! FR-S12: owner presence (INTENT D37 "Owner presence", #784).
//!
//! The routers are merged the way `main.rs` merges the local listener's app, then wrapped with
//! `auth::protect_with_owner`, so the server token and the owner key go through the real layers.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use ember_server::devices::{Devices, Scope};
use ember_server::mcp::McpRegistry;
use ember_server::owner::{self, Claimant, Owner, Presence, RouteClass, OWNER_HEADER, ROUTES};
use ember_server::schedules::{Scheduler, SystemClock};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";

/// The local listener's routers, as `main.rs` merges them (the transfers and IDE families,
/// which hold no owner route, are left out).
fn local_router(dir: &Path, auth: Arc<ServerAuth>) -> Router {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let accounts = Accounts::open(store.clone(), dir).unwrap();
    let enrol = computers::enrol::Enrol::new(store.clone());
    let devices = Devices::open(store.clone()).unwrap();
    let channels = ember_server::channels::api::router(store.clone(), sessions.clone());
    let mcp = McpRegistry::open(store, dir).unwrap();
    let scheduler = Scheduler::new(sessions.clone(), Arc::new(SystemClock));
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    let browsers = BrowserManager::new(BrowserConfig::from_env(dir));
    let chatgpt = ember_server::chatgpt::ChatGpt::new(
        accounts.clone(),
        ember_server::chatgpt::ChatGptConfig::from_env(8740),
    )
    .unwrap();
    ember_server::api::router(sessions.clone())
        .merge(computers::api::router(computers.clone(), sessions.clone()))
        .merge(computers::terms::router(computers.clone()))
        .merge(computers::enrol::router(
            enrol,
            computers::enrol::DoorUrl::Url("http://127.0.0.1:8740".into()),
            Arc::new(computers::enrol::ComputersRegistrar::new(computers.clone())),
        ))
        .merge(ember_server::api::harnesses::router(sessions.clone(), computers.clone()))
        .merge(ember_server::api::git::router(sessions.clone(), computers.clone()))
        .merge(ember_server::schedules::api::agent_router(scheduler.clone(), a2a.clone()))
        .merge(ember_server::schedules::api::router(scheduler))
        .merge(ember_server::mcp::api::router(mcp))
        .merge(ember_server::a2a::api::router(a2a))
        .merge(ember_server::accounts::api::router(accounts))
        .merge(ember_server::chatgpt::api::router(chatgpt))
        .merge(ember_server::browser::api::router(browsers))
        .merge(ember_server::hub::api::router(None, computers.clone()))
        .merge(ember_server::hub::api::admin_router(None, computers))
        .merge(ember_server::devices::api::router(devices))
        .merge(auth::rotate_router(auth))
        .merge(channels)
}

struct Fx {
    app: Router,
    token: String,
    owner: Arc<Owner>,
    dir: tempfile::TempDir,
}

fn fx_with(presence: Presence) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let a = ServerAuth::with_token_file(dir.path(), LISTEN.parse().unwrap(), []).unwrap();
    let token = std::fs::read_to_string(auth::token_path(dir.path())).unwrap().trim().to_string();
    let owner = Owner::new(presence);
    let app = auth::protect_with_owner(local_router(dir.path(), a.clone()), a, owner.clone());
    Fx { app, token, owner, dir }
}

fn fx() -> Fx {
    fx_with(Presence::Enforce)
}

fn who() -> Claimant {
    Claimant { pid: std::process::id(), exe: "test".into(), name: "test".into() }
}

async fn call_with(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("host", LISTEN);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    if let Some(k) = key {
        b = b.header(OWNER_HEADER, k);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A request path for a route template: each `{name}` becomes `name`.
fn sample(template: &str) -> String {
    template.replace("{*", "").replace(['{', '}'], "")
}

fn owner_routes() -> Vec<(&'static str, String)> {
    ROUTES.iter().filter(|(_, _, c)| *c == RouteClass::Owner).map(|(m, t, _)| (*m, sample(t))).collect()
}

fn is_owner_required(got: &(StatusCode, Value)) -> bool {
    got.0 == StatusCode::FORBIDDEN && got.1["code"] == "owner_required"
}

// ---- the owner layer on the local listener ----------------------------------------------

/// The right server token with no owner key, or a wrong one, gets 403 `owner_required` on every
/// tier 1 and tier 2 route.
#[tokio::test]
async fn fr_s12_server_token_alone_is_refused_on_each_owner_route() {
    let f = fx();
    let live = f.owner.grant(f.owner.next_claim(), who());
    let routes = owner_routes();
    assert!(routes.len() >= 40, "the owner list shrank to {}", routes.len());
    for (method, path) in &routes {
        let got = call_with(&f.app, method, path, Some(&f.token), None, Some(json!({}))).await;
        assert!(is_owner_required(&got), "{method} {path} without a key: {got:?}");
        for wrong in ["", "eok_", "eok_0000", f.token.as_str(), &live[..live.len() - 1]] {
            let got = call_with(&f.app, method, path, Some(&f.token), Some(wrong), Some(json!({}))).await;
            assert!(is_owner_required(&got), "{method} {path} with a wrong key: {got:?}");
        }
        // The server token is checked first: a key without it is a 401, not a way in.
        let got = call_with(&f.app, method, path, None, Some(&live), Some(json!({}))).await;
        assert_eq!(got.0, StatusCode::UNAUTHORIZED, "{method} {path} with a key and no token");
    }
}

/// With a live key the owner layer lets the call through to the handler.
#[tokio::test]
async fn fr_s12_a_live_key_opens_owner_routes() {
    let f = fx();
    let key = f.owner.grant(f.owner.next_claim(), who());
    let got = call_with(&f.app, "GET", "/api/devices", Some(&f.token), Some(&key), None).await;
    assert_eq!(got.0, StatusCode::OK, "{got:?}");
    assert!(got.1.is_array());
    for (method, path, body) in [
        ("PUT", "/api/sessions/nope/approve-all", json!({ "enabled": true })),
        ("POST", "/api/sessions/nope/approvals/a1", json!({ "decision": "deny" })),
        ("DELETE", "/api/computers/nope", json!({})),
        ("PATCH", "/api/devices/nope", json!({ "scope": "full" })),
        ("POST", "/api/hub/check", json!({})),
    ] {
        let got = call_with(&f.app, method, path, Some(&f.token), Some(&key), Some(body)).await;
        assert!(!is_owner_required(&got), "{method} {path}: {got:?}");
        assert_ne!(got.0, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

/// Reads, sessions, messages, terminals and question answers need the server token only.
#[tokio::test]
async fn fr_s12_non_owner_routes_keep_working_with_the_token() {
    let f = fx();
    for path in ["/api/sessions", "/api/projects", "/api/accounts", "/api/mcp"] {
        let got = call_with(&f.app, "GET", path, Some(&f.token), None, None).await;
        assert_eq!(got.0, StatusCode::OK, "GET {path}: {got:?}");
    }
    let cwd = f.dir.path().display().to_string();
    let got =
        call_with(&f.app, "POST", "/api/sessions", Some(&f.token), None, Some(json!({ "agent": "scripted", "cwd": cwd })))
            .await;
    assert_eq!(got.0, StatusCode::CREATED, "{got:?}");
    let id = got.1["id"].as_str().unwrap().to_string();
    for (method, path, body) in [
        ("POST", format!("/api/sessions/{id}/messages"), json!({ "text": "hello" })),
        ("POST", format!("/api/sessions/{id}/questions/q1/answer"), json!({ "decline": true })),
        ("POST", "/api/channels/c1/questions/q1/answer".to_string(), json!({ "decline": true })),
        ("GET", "/api/computers/local/terms".to_string(), json!({})),
        ("GET", "/api/hub/link-codes/abc".to_string(), json!({})),
        ("PUT", "/api/agents/defaults".to_string(), json!({})),
    ] {
        let got = call_with(&f.app, method, &path, Some(&f.token), None, Some(body)).await;
        assert!(!is_owner_required(&got), "{method} {path}: {got:?}");
        assert_ne!(got.0, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

/// Voiding a key ends its future authority only: what it switched on stays, the session lives.
#[tokio::test]
async fn fr_s12_voiding_a_key_stops_future_authority_only() {
    let f = fx();
    let cwd = f.dir.path().display().to_string();
    let got =
        call_with(&f.app, "POST", "/api/sessions", Some(&f.token), None, Some(json!({ "agent": "scripted", "cwd": cwd })))
            .await;
    assert_eq!(got.0, StatusCode::CREATED, "{got:?}");
    let id = got.1["id"].as_str().unwrap().to_string();
    let claim = f.owner.next_claim();
    let key = f.owner.grant(claim, who());
    let path = format!("/api/sessions/{id}/approve-all");
    let on = call_with(&f.app, "PUT", &path, Some(&f.token), Some(&key), Some(json!({ "enabled": true }))).await;
    assert_eq!(on.0, StatusCode::OK, "{on:?}");
    assert_eq!(on.1["approve_all"], true);

    f.owner.void(claim);
    let again = call_with(&f.app, "PUT", &path, Some(&f.token), Some(&key), Some(json!({ "enabled": false }))).await;
    assert!(is_owner_required(&again), "a voided key: {again:?}");
    // The session the key acted on is still there and still takes a message.
    let got = call_with(&f.app, "GET", &format!("/api/sessions/{id}"), Some(&f.token), None, None).await;
    assert_eq!(got.0, StatusCode::OK, "{got:?}");
    let got = call_with(
        &f.app,
        "POST",
        &format!("/api/sessions/{id}/messages"),
        Some(&f.token),
        None,
        Some(json!({ "text": "still here" })),
    )
    .await;
    assert_eq!(got.0, StatusCode::ACCEPTED, "{got:?}");
}

/// `EMBER_OWNER_PRESENCE=warn` serves an owner route without a key (one release, older apps).
#[tokio::test]
async fn fr_s12_warn_mode_serves_without_a_key() {
    let f = fx_with(Presence::Warn);
    let got = call_with(&f.app, "GET", "/api/devices", Some(&f.token), None, None).await;
    assert_eq!(got.0, StatusCode::OK, "{got:?}");
    // The token is still required.
    let got = call_with(&f.app, "GET", "/api/devices", None, None, None).await;
    assert_eq!(got.0, StatusCode::UNAUTHORIZED);
}

// ---- classification -----------------------------------------------------------------------

const ROUTING_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options", "trace", "any"];

/// Routes whose path is a constant in the source.
fn constant_path(expr: &str) -> Option<&'static str> {
    match expr {
        "ROTATE_ROUTE" => Some("/api/server-token/rotate"),
        "HOOK_ROUTE" => Some(ember_server::agents::antigravity::HOOK_ROUTE),
        "super::oauth::CALLBACK_PATH" => Some(ember_server::chatgpt::oauth::CALLBACK_PATH),
        "JOIN_ROUTE.1" => Some(ember_server::guest_door::JOIN_ROUTE.1),
        "ENROL_REDEEM_PATH" => Some(ember_server::enrol_door::ENROL_REDEEM_PATH),
        _ => None,
    }
}

/// The routing methods named in the second argument of a `.route(` call: an identifier from
/// [`ROUTING_METHODS`] directly followed by `(`.
fn methods_in(rest: &str) -> Vec<String> {
    let bytes = rest.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &rest[start..i];
            if ROUTING_METHODS.contains(&word) && rest[i..].trim_start().starts_with('(') {
                out.push(word.to_ascii_uppercase());
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Every `(method, path)` registered with `.route(` under `src/`.
fn scan_routes() -> BTreeSet<(String, String)> {
    fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, files);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut out = BTreeSet::new();
    for file in files {
        // The classification table itself and its tests are data, not registrations.
        if file.ends_with("owner/routes.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let mut from = 0;
        while let Some(at) = text[from..].find(".route(") {
            let start = from + at + ".route(".len();
            let mut depth = 1usize;
            let mut end = start;
            for (i, c) in text[start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            from = end.max(start);
            let call = text[start..end].trim();
            let (path, rest) = if let Some(q) = call.strip_prefix('"') {
                let Some(close) = q.find('"') else { continue };
                (q[..close].to_string(), &q[close + 1..])
            } else {
                let Some((expr, rest)) = call.split_once(',') else { continue };
                match constant_path(expr.trim()) {
                    Some(p) => (p.to_string(), rest),
                    // Not a router call (`Accounts::route(agent, None)`), unless it names a
                    // routing method.
                    None if methods_in(rest).is_empty() => continue,
                    None => panic!("{}: .route({expr}, ..) has a path this test cannot read", file.display()),
                }
            };
            let methods = methods_in(rest);
            assert!(!methods.is_empty(), "{}: .route({path:?}, ..) names no method", file.display());
            for m in methods {
                assert!(m != "ANY", "{}: {path} uses any(), which cannot be classified", file.display());
                out.insert((m, path.clone()));
            }
        }
    }
    out
}

/// A route registered in the source and missing from `owner::ROUTES` fails here, so a new route
/// cannot land without a class. A row for a route that is gone fails too.
#[test]
fn fr_s12_every_route_is_classified() {
    let registered = scan_routes();
    assert!(registered.len() > 150, "the scan found only {} routes", registered.len());
    let classified: BTreeSet<(String, String)> = ROUTES.iter().map(|(m, t, _)| (m.to_string(), t.to_string())).collect();
    assert_eq!(classified.len(), ROUTES.len(), "owner::ROUTES has a duplicate row");
    let unclassified: Vec<_> = registered.difference(&classified).collect();
    assert!(unclassified.is_empty(), "add these to owner::ROUTES with a class: {unclassified:?}");
    let stale: Vec<_> = classified.difference(&registered).collect();
    assert!(stale.is_empty(), "owner::ROUTES lists routes that are not registered: {stale:?}");
}

/// The table's classes agree with the layers: `Exempt` is exactly what `auth::token_exempt`
/// lets through, and an owner route is never exempt from the server token.
#[test]
fn fr_s12_classes_agree_with_the_token_layer() {
    for (m, template, class) in ROUTES {
        let method = Method::from_bytes(m.as_bytes()).unwrap();
        let path = sample(template).replace("/cdp/nonce", &format!("/cdp/{}", auth::agent_cdp_nonce()));
        assert_eq!(owner::classify(&method, &sample(template)), Some(*class), "{m} {template}");
        match class {
            RouteClass::Exempt => assert!(auth::token_exempt(&method, &path), "{m} {path} should be exempt"),
            RouteClass::Owner | RouteClass::Token => {
                assert!(!auth::token_exempt(&method, &path), "{m} {path} must need the server token")
            }
            RouteClass::OtherDoor => {}
        }
        assert_eq!(auth::owner_route(&method, &path), *class == RouteClass::Owner, "{m} {path}");
    }
}

/// The leader's tier 1 and tier 2 lists, spelled out, so the table cannot drop one silently.
#[test]
fn fr_s12_the_decided_owner_routes_are_owner_routes() {
    for (m, p) in [
        // Tier 1: tool approval.
        ("POST", "/api/sessions/s/approvals/a"),
        ("PUT", "/api/sessions/s/approve-all"),
        ("POST", "/api/channels/c/feed/a/answer"),
        // Tier 1: devices, every method.
        ("GET", "/api/devices"),
        ("POST", "/api/devices"),
        ("PATCH", "/api/devices/p"),
        ("DELETE", "/api/devices/p"),
        // Tier 1: accounts and sign-in.
        ("POST", "/api/accounts"),
        ("DELETE", "/api/accounts/a"),
        ("POST", "/api/accounts/a/default"),
        ("POST", "/api/accounts/a/check"),
        ("POST", "/api/accounts/a/clear-limit"),
        ("POST", "/api/providers"),
        ("DELETE", "/api/providers/p"),
        ("POST", "/api/chatgpt/signin"),
        ("POST", "/api/chatgpt/signin/complete"),
        ("DELETE", "/api/chatgpt/accounts/a"),
        ("POST", "/api/chatgpt/accounts/a/refresh"),
        // Tier 1: computers and enrol codes.
        ("POST", "/api/computers"),
        ("DELETE", "/api/computers/c"),
        ("POST", "/api/computers/enrol-codes"),
        ("GET", "/api/computers/enrol-codes/e"),
        ("DELETE", "/api/computers/enrol-codes/e"),
        ("POST", "/api/computers/enrol-codes/e/confirm"),
        ("POST", "/api/computers/enrol-codes/e/deny"),
        ("POST", "/api/hub/devices/d/computer"),
        // Tier 1: the token.
        ("POST", "/api/server-token/rotate"),
        // Tier 2: hub admin.
        ("POST", "/api/hub/link"),
        ("DELETE", "/api/hub/link"),
        ("POST", "/api/hub/check"),
        ("DELETE", "/api/hub/registration"),
        ("DELETE", "/api/hub/devices/d"),
        ("POST", "/api/hub/link-codes/c"),
        ("POST", "/api/hub/sync-devices"),
        ("PUT", "/api/hub/sync-devices"),
        // Tier 2: MCP writes and settings.
        ("POST", "/api/mcp"),
        ("PATCH", "/api/mcp/m"),
        ("DELETE", "/api/mcp/m"),
        ("PUT", "/api/agents/codex/settings"),
        ("PUT", "/api/a2a/settings"),
        ("PUT", "/api/browsers/p/egress"),
        // Tier 2: channel invites, guests and allowances.
        ("POST", "/api/channels/c/invites"),
        ("DELETE", "/api/channels/c/invites/i"),
        ("DELETE", "/api/channels/c/guests/g"),
        ("PUT", "/api/channels/c/guests/g/allow"),
        ("DELETE", "/api/channels/c/guests/g/allow"),
    ] {
        assert!(auth::owner_route(&Method::from_bytes(m.as_bytes()).unwrap(), p), "{m} {p}");
    }
    // Question answers stay ungated (leader, 2026-11-13).
    assert!(!auth::owner_route(&Method::POST, "/api/sessions/s/questions/q/answer"));
    assert!(!auth::owner_route(&Method::POST, "/api/channels/c/questions/q/answer"));
}

/// `main.rs` wraps the local listener with the owner layer, serves the transport through the
/// scope check, and keeps device management off the transport.
#[test]
fn fr_s12_main_wires_both_doors() {
    let main = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap();
    assert!(main.contains("auth::protect_with_owner(local_app, auth, owner.clone())"));
    assert!(main.contains("owner::door::OwnerDoor::bind(&data_dir, owner.clone(), cfg)"));
    let transport = main.find("serve_scoped(&n.transport, app.clone(), devices.clone())").expect("the transport serve");
    let devices = main.find("devices::api::router(devices.clone())").expect("the devices router");
    assert!(transport < devices, "the devices router must be merged after the transport app is taken");
    assert!(!main.contains("auth::protect(local_app"), "the local listener must use protect_with_owner");
}

// ---- the transport door -------------------------------------------------------------------

mod transport_door {
    use super::*;
    use ember_server::transport::{self as server_transport, SERVER_SERVICE};
    use ember_transport::http::http1_handshake;
    use ember_transport::mem::MemNetwork;
    use ember_transport::{Dialer, PeerAddr, Transport};
    use http_body_util::Full;

    /// The transport door as `main.rs` serves it: the shared app, without the devices router.
    fn serve(net: &MemNetwork) -> (Transport, Arc<Devices>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
        let devices = Devices::open(store).unwrap();
        let server_t = net.transport();
        let app = ember_server::api::router(sessions);
        tokio::spawn(server_transport::serve_scoped(&server_t, app, devices.clone()).unwrap());
        (server_t, devices)
    }

    fn device(net: &MemNetwork, devices: &Devices, scope: Scope) -> Dialer {
        let t = net.transport();
        devices.add(t.peer_id(), "device").unwrap();
        devices.set_scope(&t.peer_id(), scope).unwrap();
        Dialer::new(t)
    }

    async fn call(device: &Dialer, server: &Transport, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let fut = async {
            let stream = device.open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE).await.unwrap();
            let mut http = http1_handshake::<Full<hyper::body::Bytes>>(stream).await.unwrap();
            let req = hyper::Request::builder()
                .method(method)
                .uri(format!("http://ember-server{path}"))
                .header("content-type", "application/json")
                .body(Full::new(hyper::body::Bytes::from(body.to_string())))
                .unwrap();
            let resp = http.send_request(req).await.unwrap();
            let status = resp.status();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
        };
        tokio::time::timeout(Duration::from_secs(5), fut).await.expect("timed out")
    }

    /// On the transport the device key is the owner credential: a `full` device answers an
    /// approval with no owner key and no server token.
    #[tokio::test]
    async fn fr_s12_full_device_is_owner_on_the_transport() {
        let net = MemNetwork::new();
        let (server_t, devices) = serve(&net);
        let full = device(&net, &devices, Scope::Full);
        for (m, p, body) in [
            ("PUT", "/api/sessions/nope/approve-all", json!({ "enabled": true })),
            ("POST", "/api/sessions/nope/approvals/a1", json!({ "decision": "deny" })),
        ] {
            let got = call(&full, &server_t, m, p, body).await;
            assert_ne!(got.0, StatusCode::FORBIDDEN, "{m} {p}: {got:?}");
            assert_ne!(got.0, StatusCode::UNAUTHORIZED, "{m} {p}: {got:?}");
        }
        // The handler ran: the session does not exist.
        let got = call(&full, &server_t, "PUT", "/api/sessions/nope/approve-all", json!({ "enabled": true })).await;
        assert_eq!(got.0, StatusCode::NOT_FOUND, "{got:?}");
    }

    /// A narrow scope is the non-owner credential of the transport: 403 `scope_denied` on every
    /// owner route.
    #[tokio::test]
    async fn fr_s12_narrow_scope_is_not_owner_on_the_transport() {
        let net = MemNetwork::new();
        let (server_t, devices) = serve(&net);
        for scope in [Scope::ReadOnly, Scope::ChatOnly] {
            let d = device(&net, &devices, scope);
            for (m, p) in owner_routes() {
                let got = call(&d, &server_t, m, &p, json!({})).await;
                assert_eq!(got.0, StatusCode::FORBIDDEN, "{} {m} {p}: {got:?}", scope.as_str());
                assert_eq!(got.1["code"], "scope_denied", "{} {m} {p}: {got:?}", scope.as_str());
            }
        }
    }

    /// A `full` device approves tool calls but does not manage devices: the transport serves no
    /// devices, token, hub admin or enrol route at all.
    #[tokio::test]
    async fn fr_s12_a_full_device_cannot_manage_devices() {
        let net = MemNetwork::new();
        let (server_t, devices) = serve(&net);
        let full = device(&net, &devices, Scope::Full);
        let before = devices.list().unwrap().len();
        for (m, p, body) in [
            ("GET", "/api/devices", json!({})),
            ("POST", "/api/devices", json!({ "peer_id": "00", "name": "x" })),
            ("PATCH", "/api/devices/00", json!({ "scope": "full" })),
            ("DELETE", "/api/devices/00", json!({})),
            ("POST", "/api/server-token/rotate", json!({})),
            ("POST", "/api/computers/enrol-codes", json!({ "name": "x" })),
            ("POST", "/api/hub/link", json!({})),
        ] {
            let got = call(&full, &server_t, m, p, body).await;
            assert_eq!(got.0, StatusCode::NOT_FOUND, "{m} {p}: {got:?}");
        }
        assert_eq!(devices.list().unwrap().len(), before);
    }
}

// ---- the claim socket ---------------------------------------------------------------------

#[cfg(unix)]
mod door {
    use super::*;
    use ember_server::owner::door::{socket_path, DoorConfig, OwnerDoor, Proc, ProcTable};
    use ember_server::owner::Confirm;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::UnixStream;

    type Codes = Arc<Mutex<Vec<String>>>;

    /// A door whose console is captured. The process table holds this test process only, under
    /// `parent`; `roots` are the denied trees.
    fn config(confirm: Confirm, roots: Vec<u32>, parent: u32) -> (DoorConfig, Codes) {
        let codes: Codes = Arc::default();
        let seen = codes.clone();
        let me = std::process::id();
        let cfg = DoorConfig {
            confirm,
            roots: Arc::new(move || roots.clone()),
            procs: Arc::new(move || {
                let mut t = ProcTable::new();
                t.insert(me, Proc { ppid: parent, exe: "/test/app".into() });
                if parent > 1 {
                    t.insert(parent, Proc { ppid: 1, exe: "/test/parent".into() });
                }
                t
            }),
            console: Arc::new(move |code: &str, _who: &Claimant| seen.lock().unwrap().push(code.to_string())),
            confirm_timeout: Duration::from_secs(5),
        };
        (cfg, codes)
    }

    struct Conn {
        lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
        w: OwnedWriteHalf,
    }

    impl Conn {
        async fn open(dir: &Path, name: &str) -> Conn {
            let (r, w) = UnixStream::connect(socket_path(dir)).await.unwrap().into_split();
            let mut c = Conn { lines: BufReader::new(r).lines(), w };
            c.send(json!({ "op": "claim", "name": name })).await;
            c
        }

        /// A write to a connection the server already closed is not an error here: the
        /// refusal it sent first is still there to read.
        async fn send(&mut self, v: Value) {
            let _ = self.w.write_all(format!("{v}\n").as_bytes()).await;
        }

        /// The next line, or `Null` when the server closed the connection.
        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
                .await
                .expect("no line from the owner socket")
                .unwrap_or(None);
            line.map(|l| serde_json::from_str(&l).unwrap()).unwrap_or(Value::Null)
        }

        /// The next line that is not an event.
        async fn next_state(&mut self) -> Value {
            loop {
                let v = self.next().await;
                if v.get("event").is_none() {
                    return v;
                }
            }
        }

        /// The next event line.
        async fn next_event(&mut self) -> Value {
            loop {
                let v = self.next().await;
                if v.get("event").is_some() || v.is_null() {
                    return v;
                }
            }
        }
    }

    fn last_code(codes: &Codes) -> String {
        codes.lock().unwrap().last().cloned().expect("no code was shown on the console")
    }

    /// Claims and confirms with the console code. Returns the connection and the key.
    async fn claim_with_code(dir: &Path, codes: &Codes, name: &str) -> (Conn, String) {
        let mut c = Conn::open(dir, name).await;
        let v = c.next_state().await;
        assert_eq!(v["state"], "confirm", "{v}");
        c.send(json!({ "op": "confirm", "code": last_code(codes) })).await;
        let v = c.next_state().await;
        assert_eq!(v["state"], "granted", "{v}");
        let key = v["key"].as_str().unwrap().to_string();
        (c, key)
    }

    async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        for _ in 0..200 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn devices_status(f: &Fx, key: &str) -> StatusCode {
        call_with(&f.app, "GET", "/api/devices", Some(&f.token), Some(key), None).await.0
    }

    /// Strict by default: the first claim gets its key only after the code shown on the server
    /// console is sent back. The key opens owner routes while the connection lives.
    #[tokio::test]
    async fn fr_s12_first_claim_is_confirmed_by_the_console_code() {
        let f = fx();
        let (cfg, codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();

        let mut c = Conn::open(f.dir.path(), "ember-app").await;
        let v = c.next_state().await;
        assert_eq!(v["state"], "confirm", "{v}");
        assert_eq!(v["holders"], 0);
        assert!(v.get("key").is_none(), "no key before the confirmation: {v}");
        assert!(f.owner.holders().is_empty());
        let code = last_code(&codes);
        assert_eq!(code.len(), 9, "{code}");

        // Typed in lower case and without the dash, as a person would.
        c.send(json!({ "op": "confirm", "code": code.to_lowercase().replace('-', "") })).await;
        let v = c.next_state().await;
        assert_eq!(v["state"], "granted", "{v}");
        let key = v["key"].as_str().unwrap().to_string();
        assert!(key.starts_with("eok_"));
        assert_eq!(devices_status(&f, &key).await, StatusCode::OK);
        let holders = f.owner.holders();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].1.pid, std::process::id());
        assert_eq!(holders[0].1.name, "ember-app");
    }

    /// Three wrong codes end the claim with no key.
    #[tokio::test]
    async fn fr_s12_wrong_codes_end_the_claim() {
        let f = fx();
        let (cfg, _codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let mut c = Conn::open(f.dir.path(), "guesser").await;
        assert_eq!(c.next_state().await["state"], "confirm");
        c.send(json!({ "op": "confirm", "code": "AAAA-AAA1" })).await;
        let v = c.next_state().await;
        assert_eq!((v["state"].as_str(), v["left"].as_u64()), (Some("wrong_code"), Some(2)), "{v}");
        c.send(json!({ "op": "confirm", "code": "" })).await;
        assert_eq!(c.next_state().await["left"], 1);
        c.send(json!({ "op": "confirm", "code": "AAAA-AAA1" })).await;
        let v = c.next_state().await;
        assert_eq!((v["state"].as_str(), v["reason"].as_str()), (Some("refused"), Some("wrong_code")), "{v}");
        assert!(c.next().await.is_null(), "the connection closes");
        assert!(f.owner.holders().is_empty());
    }

    /// A later claim is confirmed in an app that already holds a key, and each holder is told
    /// about the other claim. A denied claim gets no key.
    #[tokio::test]
    async fn fr_s12_a_later_claim_is_confirmed_by_a_holder() {
        let f = fx();
        let (cfg, codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let (mut app, app_key) = claim_with_code(f.dir.path(), &codes, "ember-app").await;

        let mut cli = Conn::open(f.dir.path(), "ember-server owner").await;
        let v = cli.next_state().await;
        assert_eq!((v["state"].as_str(), v["holders"].as_u64()), (Some("confirm"), Some(1)), "{v}");
        let pending = app.next_event().await;
        assert_eq!(pending["event"], "claim_pending", "{pending}");
        assert_eq!(pending["name"], "ember-server owner");
        assert_eq!(pending["pid"], std::process::id());
        app.send(json!({ "op": "approve", "claim": pending["claim"] })).await;
        let v = cli.next_state().await;
        assert_eq!(v["state"], "granted", "{v}");
        let cli_key = v["key"].as_str().unwrap().to_string();
        assert_ne!(cli_key, app_key);
        assert_eq!(devices_status(&f, &cli_key).await, StatusCode::OK);
        // The first holder sees the foreign claim.
        let mut saw_claimed = false;
        for _ in 0..3 {
            let ev = app.next_event().await;
            if ev["event"] == "owner_claimed" {
                assert_eq!(ev["claim"], pending["claim"]);
                saw_claimed = true;
                break;
            }
        }
        assert!(saw_claimed, "the holder was not told about the new key");

        // A claim the holder denies.
        let mut other = Conn::open(f.dir.path(), "someone").await;
        assert_eq!(other.next_state().await["state"], "confirm");
        let pending = loop {
            let ev = app.next_event().await;
            assert!(!ev.is_null(), "the holder's connection closed");
            if ev["event"] == "claim_pending" {
                break ev;
            }
        };
        app.send(json!({ "op": "deny", "claim": pending["claim"] })).await;
        let v = other.next_state().await;
        assert_eq!((v["state"].as_str(), v["reason"].as_str()), (Some("refused"), Some("denied")), "{v}");
        assert_eq!(f.owner.holders().len(), 2);
    }

    /// A claimant that has no key cannot approve a claim, its own included.
    #[tokio::test]
    async fn fr_s12_a_pending_claim_cannot_approve_itself() {
        let f = fx();
        let (cfg, _codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let mut c = Conn::open(f.dir.path(), "sneaky").await;
        let v = c.next_state().await;
        assert_eq!(v["state"], "confirm");
        c.send(json!({ "op": "approve", "claim": v["claim"] })).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(f.owner.holders().is_empty());
    }

    /// A process inside an agent or terminal tree gets no key, whatever it sends. A plain
    /// process gets to the confirmation.
    #[tokio::test]
    async fn fr_s12_claim_refused_from_an_agent_tree() {
        let me = std::process::id();
        // This process is the root itself (an agent the server started directly).
        // Then: its parent is the root (a shell under a shim).
        for (roots, parent) in [(vec![me], 1), (vec![4242], 4242)] {
            let f = fx();
            let (cfg, codes) = config(Confirm::Off, roots, parent);
            let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
            let mut c = Conn::open(f.dir.path(), "agent shell").await;
            let v = c.next_state().await;
            assert_eq!((v["state"].as_str(), v["reason"].as_str()), (Some("refused"), Some("agent_tree")), "{v}");
            assert!(c.next().await.is_null());
            assert!(f.owner.holders().is_empty());
            assert!(codes.lock().unwrap().is_empty(), "a refused peer is shown no code");
        }
        // The same process outside every denied tree.
        let f = fx();
        let (cfg, _codes) = config(Confirm::Off, vec![4242], 77);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let mut c = Conn::open(f.dir.path(), "plain").await;
        assert_eq!(c.next_state().await["state"], "granted");
    }

    /// A claimant the process table does not show is refused.
    #[tokio::test]
    async fn fr_s12_claim_refused_when_the_process_is_unknown() {
        let f = fx();
        let (mut cfg, _codes) = config(Confirm::Off, vec![], 1);
        cfg.procs = Arc::new(ProcTable::new);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let mut c = Conn::open(f.dir.path(), "ghost").await;
        let v = c.next_state().await;
        assert_eq!((v["state"].as_str(), v["reason"].as_str()), (Some("refused"), Some("unknown_process")), "{v}");
    }

    /// The real process table shows this test process and its parent.
    #[test]
    fn fr_s12_ps_table_shows_this_process() {
        let t = ember_server::owner::door::ps_table();
        let me = t.get(&std::process::id()).expect("ps does not list this process");
        // SAFETY: getppid has no preconditions and cannot fail.
        assert_eq!(me.ppid, unsafe { libc::getppid() } as u32);
        assert!(!me.exe.is_empty());
    }

    /// The key lives while its connection does: closing the socket voids it.
    #[tokio::test]
    async fn fr_s12_key_dies_with_its_socket() {
        let f = fx();
        let (cfg, codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let (c, key) = claim_with_code(f.dir.path(), &codes, "ember-app").await;
        assert_eq!(devices_status(&f, &key).await, StatusCode::OK);
        drop(c);
        let owner = f.owner.clone();
        until("the key to be voided", || owner.holders().is_empty()).await;
        let got = call_with(&f.app, "GET", "/api/devices", Some(&f.token), Some(&key), None).await;
        assert!(is_owner_required(&got), "{got:?}");
    }

    /// Every key dies with the server: the door closed, the socket file gone, the old key 403.
    #[tokio::test]
    async fn fr_s12_key_dies_with_the_server() {
        let f = fx();
        let (cfg, codes) = config(Confirm::Strict, vec![], 1);
        let door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let (mut c, key) = claim_with_code(f.dir.path(), &codes, "ember-app").await;
        assert!(socket_path(f.dir.path()).exists());
        drop(door);
        assert!(!socket_path(f.dir.path()).exists(), "the socket file is removed at stop");
        let got = call_with(&f.app, "GET", "/api/devices", Some(&f.token), Some(&key), None).await;
        assert!(is_owner_required(&got), "{got:?}");
        // The app sees its connection close and claims again.
        assert!(c.next_event().await.is_null());
        // A new server on the same folder does not know the old key.
        let (cfg, codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        assert!(is_owner_required(
            &call_with(&f.app, "GET", "/api/devices", Some(&f.token), Some(&key), None).await
        ));
        let (_c, fresh) = claim_with_code(f.dir.path(), &codes, "ember-app").await;
        assert_ne!(fresh, key);
        assert_eq!(devices_status(&f, &fresh).await, StatusCode::OK);
    }

    /// `EMBER_OWNER_CONFIRM=off` is the explicit opt-out: the peer checks only.
    #[tokio::test]
    async fn fr_s12_confirm_off_grants_without_a_code() {
        let f = fx();
        let (cfg, codes) = config(Confirm::Off, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let mut c = Conn::open(f.dir.path(), "ember-app").await;
        let v = c.next_state().await;
        assert_eq!(v["state"], "granted", "{v}");
        assert!(codes.lock().unwrap().is_empty());
        assert_eq!(devices_status(&f, v["key"].as_str().unwrap()).await, StatusCode::OK);
    }

    /// The socket is private to the server's user, and a socket file left by a killed server
    /// does not stop the next start; a live one does.
    #[tokio::test]
    async fn fr_s12_socket_is_0600_and_a_stale_file_is_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let f = fx();
        let path = socket_path(f.dir.path());
        // A file left behind: nothing answers on it.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let (cfg, _) = config(Confirm::Strict, vec![], 1);
        let door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let (cfg, _) = config(Confirm::Strict, vec![], 1);
        let second = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg);
        assert!(second.is_err(), "a second server on the same folder must not take the socket");
        drop(door);
    }

    #[derive(Clone)]
    struct LogBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn files_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
            if md.is_dir() {
                files_under(&p, out);
            } else if md.is_file() {
                out.push(p);
            }
        }
    }

    /// After a claim and a call, the key is in no file of the data folder, in no log line and
    /// not on the console.
    #[tokio::test]
    async fn fr_s12_owner_key_never_on_disk_or_in_logs() {
        let logs = LogBuf(Arc::default());
        let sink = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || sink.clone())
            .finish();
        // The test runtime is single-threaded, so the door's tasks log through this guard too.
        let _guard = tracing::subscriber::set_default(subscriber);

        let f = fx();
        let (cfg, codes) = config(Confirm::Strict, vec![], 1);
        let _door = OwnerDoor::bind(f.dir.path(), f.owner.clone(), cfg).unwrap();
        let (c, key) = claim_with_code(f.dir.path(), &codes, "ember-app").await;
        assert_eq!(devices_status(&f, &key).await, StatusCode::OK);
        // A refused call logs too.
        assert!(is_owner_required(
            &call_with(&f.app, "GET", "/api/devices", Some(&f.token), Some("eok_wrong"), None).await
        ));
        drop(c);
        let owner = f.owner.clone();
        until("the key to be voided", || owner.holders().is_empty()).await;

        let secret = key.strip_prefix("eok_").unwrap();
        let mut files = Vec::new();
        files_under(f.dir.path(), &mut files);
        assert!(!files.is_empty(), "the data folder holds at least the server token");
        for p in files {
            let bytes = std::fs::read(&p).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(secret), "{} holds the owner key", p.display());
        }
        let logged = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(logged.contains("owner key claimed"), "the claim is logged: {logged}");
        assert!(logged.contains("owner key voided"), "the void is logged: {logged}");
        assert!(!logged.contains(secret), "a log line holds the owner key");
        for line in codes.lock().unwrap().iter() {
            assert!(!line.contains(secret));
        }
    }
}

// ---- the issue's "done when" test ---------------------------------------------------------

/// An agent's shell reads `<data dir>/server-token` and calls every owner route with curl. Each
/// call is answered 403 `owner_required`, while the same token still reads a normal route.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_s12_agent_shell_cannot_use_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let a = ServerAuth::with_token_file(dir.path(), addr, []).unwrap();
    let owner = Owner::new(Presence::Enforce);
    // A live key exists (the app is open); the shell does not hold it.
    let _app_key = owner.grant(owner.next_claim(), who());
    let app = auth::protect_with_owner(local_router(dir.path(), a.clone()), a, owner);
    tokio::spawn(async move { axum::serve(listener, app).await });

    // What an agent's shell has: the data folder's path and curl. No owner key, no variable.
    const SCRIPT: &str = r#"
T=$(cat "$EMBER_DATA_DIR/server-token") || exit 3
curl -s -m 10 -X "$1" -H "Authorization: Bearer $T" -H 'content-type: application/json' -d '{}' \
  -w '\n%{http_code}' "$2"
"#;
    let shell = |method: &'static str, url: String| {
        let data_dir = dir.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("sh")
                .args(["-c", SCRIPT, "sh", method, url.as_str()])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("EMBER_DATA_DIR", &data_dir)
                .output()
                .expect("sh");
            assert!(out.status.success(), "the shell failed (is curl installed?): {out:?}");
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let (body, code) = text.rsplit_once('\n').expect("status line");
            (code.trim().parse::<u16>().expect("status code"), serde_json::from_str::<Value>(body).unwrap_or(Value::Null))
        })
    };

    // The token the shell read is the right one.
    let (code, _) = shell("GET", format!("http://{addr}/api/health")).await.unwrap();
    assert_eq!(code, 200);
    let routes = owner_routes();
    for (method, path) in &routes {
        let (code, body) = shell(*method, format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(code, 403, "{method} {path}: {body}");
        assert_eq!(body["code"], "owner_required", "{method} {path}: {body}");
    }
    // Not an owner route: the token alone still works there. A GET with a body is fine for curl.
    let out = tokio::task::spawn_blocking({
        let data_dir = dir.path().to_path_buf();
        move || {
            let url = format!("http://{addr}/api/sessions");
            std::process::Command::new("sh")
                .args([
                    "-c",
                    r#"curl -s -m 10 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $(cat "$EMBER_DATA_DIR/server-token")" "$1""#,
                    "sh",
                    url.as_str(),
                ])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("EMBER_DATA_DIR", &data_dir)
                .output()
                .expect("sh")
        }
    })
    .await
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "200");
}
