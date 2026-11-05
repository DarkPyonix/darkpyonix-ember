//! FR-T14: the guest door (`EMBER_GUEST_LISTEN`, PR 5a). Details are [provisional].
//! The door is built in process (the guest router wrapped by `GuestDoor::wrap`) and driven
//! with `oneshot`, so no socket is bound. Rate limits are PR 5b and not tested here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::guest::{guest_router, GUEST_ROUTES};
use ember_server::guest_door::{invite_link, GuestDoor, JOIN_CSP, JOIN_ROUTE};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";

fn door_with(vars: &[(&str, &str)]) -> Result<Option<GuestDoor>, String> {
    let m: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    GuestDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse::<SocketAddr>().unwrap()).map_err(|e| e.to_string())
}

fn door(vars: &[(&str, &str)]) -> GuestDoor {
    door_with(vars).unwrap().expect("door on")
}

fn store_and_sessions() -> (Arc<Store>, Arc<Sessions>) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    (store, sessions)
}

fn app(d: &GuestDoor) -> Router {
    let (store, sessions) = store_and_sessions();
    d.wrap(guest_router(store, sessions))
}

async fn send(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let (status, h) = (res.status(), res.headers().clone());
    (status, h, res.into_body().collect().await.unwrap().to_bytes().to_vec())
}

const LOCAL: &[(&str, &str)] = &[("host", "127.0.0.1:9000")];

#[test]
fn fr_t14_guest_door_is_off_without_the_variable() {
    assert!(door_with(&[]).unwrap().is_none());
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "  ")]).unwrap().is_none());
    // Settings of a door that is off do not turn it on.
    assert!(door_with(&[("EMBER_GUEST_PUBLIC_URL", "https://g.example.com")]).unwrap().is_none());
}

#[test]
fn fr_t14_bad_guest_listen_value_is_a_startup_error() {
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "not-an-address")]).is_err());
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "9000")]).is_err());
}

#[test]
fn fr_t14_guest_listen_equal_to_owner_listen_is_a_startup_error() {
    let e = door_with(&[("EMBER_GUEST_LISTEN", OWNER)]).unwrap_err();
    assert!(e.contains("EMBER_LISTEN"), "{e}");
}

#[test]
fn fr_t14_non_loopback_bind_needs_allow_remote() {
    for a in ["0.0.0.0:9000", "192.168.1.5:9000", "[::]:9000"] {
        let e = door_with(&[("EMBER_GUEST_LISTEN", a)]).unwrap_err();
        assert!(e.contains("EMBER_GUEST_LISTEN_ALLOW_REMOTE"), "{e}");
        assert!(door_with(&[("EMBER_GUEST_LISTEN", a), ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", "0")]).is_err());
        assert!(door_with(&[("EMBER_GUEST_LISTEN", a), ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", "1")]).unwrap().is_some());
    }
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]).unwrap().is_some());
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "[::1]:9000")]).unwrap().is_some());
}

#[test]
fn fr_t14_public_url_with_a_path_or_odd_scheme_is_rejected() {
    for u in ["https://g.example.com/x", "https://g.example.com?a=1", "https://g.example.com#f", "ftp://g.example.com", "g.example.com", "https://u@g.example.com", "https://"] {
        assert!(door_with(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_PUBLIC_URL", u)]).is_err(), "{u}");
    }
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_PUBLIC_URL", "https://g.example.com/")]);
    assert_eq!(d.public_url.as_deref(), Some("https://g.example.com"));
}

#[tokio::test]
async fn fr_t14_guest_door_refuses_a_foreign_host() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let a = app(&d);
    for host in ["evil.example.com", "10.0.0.7:9000", "127.0.0.2:9000", "user@127.0.0.1", ""] {
        let (s, _, _) = send(&a, "GET", "/join", &[("host", host)]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "host {host:?}");
    }
    let (s, _, _) = send(&a, "GET", "/join", &[]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "no Host");
    for host in ["127.0.0.1:9000", "localhost:9000", "LOCALHOST"] {
        let (s, _, _) = send(&a, "GET", "/join", &[("host", host)]).await;
        assert_eq!(s, StatusCode::OK, "host {host:?}");
    }
    // Even an unknown path is refused by Host before the router answers.
    let (s, _, _) = send(&a, "GET", "/nothing", &[("host", "evil.example.com")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_t14_guest_door_unspecified_bind_does_not_accept_any_ip_literal_host() {
    let d = door(&[
        ("EMBER_GUEST_LISTEN", "0.0.0.0:9000"),
        ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", "1"),
        ("EMBER_GUEST_ALLOWED_HOSTS", "Guests.Example.com, 192.168.1.5"),
        ("EMBER_GUEST_PUBLIC_URL", "https://public.example.org"),
    ]);
    let a = app(&d);
    for host in ["10.9.9.9:9000", "127.0.0.1:9000", "localhost"] {
        let (s, _, _) = send(&a, "GET", "/join", &[("host", host)]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{host}");
    }
    for host in ["guests.example.com", "192.168.1.5:9000", "public.example.org"] {
        let (s, _, _) = send(&a, "GET", "/join", &[("host", host)]).await;
        assert_eq!(s, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn fr_t14_guest_door_refuses_a_foreign_and_a_null_origin() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let a = app(&d);
    for origin in ["https://evil.example.com", "null", "garbage", "file://x", "http://127.0.0.1:9000/x"] {
        let (s, _, _) = send(&a, "POST", "/api/guest/accept", &[("host", "127.0.0.1:9000"), ("origin", origin)]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "origin {origin:?}");
    }
}

#[tokio::test]
async fn fr_t14_guest_door_refuses_a_loopback_origin_that_is_not_the_host() {
    let d = door(&[
        ("EMBER_GUEST_LISTEN", "127.0.0.1:9000"),
        ("EMBER_GUEST_ALLOWED_HOSTS", "g.example.com"),
    ]);
    let a = app(&d);
    for (host, origin) in [
        ("127.0.0.1:9000", "http://localhost:9000"),
        ("127.0.0.1:9000", "http://127.0.0.1:3000"),
        ("g.example.com", "http://localhost:3000"),
        ("g.example.com", "http://127.0.0.1:9000"),
        ("g.example.com", "http://[::1]:9000"),
    ] {
        let (s, _, _) = send(&a, "POST", "/api/guest/accept", &[("host", host), ("origin", origin)]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{host} / {origin}");
    }
}

#[tokio::test]
async fn fr_t14_guest_door_accepts_no_origin_and_same_host_origin() {
    let d = door(&[
        ("EMBER_GUEST_LISTEN", "127.0.0.1:9000"),
        ("EMBER_GUEST_ALLOWED_HOSTS", "g.example.com"),
    ]);
    let a = app(&d);
    for (host, origin) in [
        ("127.0.0.1:9000", None),
        ("127.0.0.1:9000", Some("http://127.0.0.1:9000")),
        ("g.example.com", Some("https://g.example.com")),
        ("G.Example.com", Some("https://g.example.com")),
    ] {
        let mut h = vec![("host", host)];
        if let Some(o) = origin {
            h.push(("origin", o));
        }
        let (s, _, _) = send(&a, "GET", "/join", &h).await;
        assert_eq!(s, StatusCode::OK, "{host} / {origin:?}");
    }
}

#[tokio::test]
async fn fr_t14_guest_door_has_no_websocket_route() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let a = app(&d);
    for path in ["/api/push", "/api/guest/channels/x/messages", "/join"] {
        let (s, _, _) = send(
            &a,
            "GET",
            path,
            &[("host", "127.0.0.1:9000"), ("upgrade", "websocket"), ("connection", "Upgrade")],
        )
        .await;
        assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND, "{path}: {s}");
    }
}

#[tokio::test]
async fn fr_t14_join_page_has_csp_nosniff_and_no_referrer() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let (s, h, body) = send(&app(&d), "GET", "/join", LOCAL).await;
    assert_eq!(s, StatusCode::OK);
    let get = |n: &str| h.get(n).unwrap().to_str().unwrap().to_string();
    assert_eq!(get("content-security-policy"), JOIN_CSP);
    assert!(JOIN_CSP.contains("default-src 'none'") && JOIN_CSP.contains("frame-ancestors 'none'"));
    assert!(JOIN_CSP.contains("connect-src 'self'") && !JOIN_CSP.contains("unsafe"));
    assert_eq!(get("referrer-policy"), "no-referrer");
    assert_eq!(get("x-content-type-options"), "nosniff");
    assert_eq!(get("cache-control"), "no-store");
    assert!(get("content-type").starts_with("text/html"));
    let page = String::from_utf8(body).unwrap();
    assert!(!page.contains("<script") && !page.contains("<style") && !page.contains("http://"));
    // The API and the fallback carry nosniff and no-store too.
    let (_, h, _) = send(&app(&d), "GET", "/api/guest/channels/x", LOCAL).await;
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");
    let (_, h, _) = send(&app(&d), "GET", "/nothing", LOCAL).await;
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
}

/// Fills `{id}` so a pattern can be requested.
fn concrete(p: &str) -> String {
    p.replace("{id}", "x")
}

#[tokio::test]
async fn fr_t14_guest_door_serves_exactly_the_route_table() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let a = app(&d);
    let mut table: Vec<(&str, &str)> = GUEST_ROUTES.to_vec();
    table.push(JOIN_ROUTE);
    let methods = ["GET", "POST", "PUT", "DELETE", "PATCH"];
    let paths: std::collections::BTreeSet<&str> = table.iter().map(|r| r.1).collect();
    for path in paths {
        for m in methods {
            let (s, _, _) = send(&a, m, &concrete(path), LOCAL).await;
            let listed = table.contains(&(m, path));
            if listed {
                // A row in the table answers (401 without a credential, 400/415/422 on a bad accept
                // body, 200 for /join): never the router's "no such route".
                assert!(s != StatusCode::NOT_FOUND && s != StatusCode::METHOD_NOT_ALLOWED, "{m} {path}: {s}");
            } else {
                assert!(s == StatusCode::METHOD_NOT_ALLOWED || s == StatusCode::NOT_FOUND, "{m} {path}: {s}");
                assert_ne!(s, StatusCode::OK, "{m} {path}");
            }
        }
    }
    // The documented four plus /join; a fifth guest route must be added to the table on purpose.
    assert_eq!(GUEST_ROUTES.len(), 4);
    // Siblings of the table's paths are not served.
    for p in [
        "/api/guest/channels/x/feed",
        "/api/guest/channels/x/approvals",
        "/api/guest/channels",
        "/api/guest/channels/x/messages/1",
        "/api/guest/accept/x",
        "/join/x",
        "/api/guest",
    ] {
        for m in methods {
            let (s, _, _) = send(&a, m, p, LOCAL).await;
            assert!(s == StatusCode::NOT_FOUND || s == StatusCode::METHOD_NOT_ALLOWED, "{m} {p}: {s}");
        }
    }
}

#[tokio::test]
async fn fr_t14_every_owner_route_is_404_on_the_guest_door() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let a = app(&d);
    let owner = [
        "/api/sessions",
        "/api/sessions/x",
        "/api/channels",
        "/api/channels/x/invites",
        "/api/channels/x/guests",
        "/api/computers",
        "/api/devices",
        "/api/hub",
        "/api/hub/link",
        "/api/accounts",
        "/api/projects",
        "/api/push",
        "/api/browsers/x/cdp",
        "/api/a2a/x",
        "/api/files",
        "/api/mcp",
        "/api/health",
        "/auth/callback",
        "/",
    ];
    let creds: [&[(&str, &str)]; 3] = [
        LOCAL,
        &[("host", "127.0.0.1:9000"), ("authorization", "Bearer some-server-token")],
        &[("host", "127.0.0.1:9000"), ("authorization", "Bearer dpg_whatever")],
    ];
    for path in owner {
        for m in ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"] {
            for h in creds {
                let (s, _, _) = send(&a, m, path, h).await;
                assert!(s == StatusCode::NOT_FOUND || s == StatusCode::METHOD_NOT_ALLOWED, "{m} {path}: {s}");
            }
        }
    }
}

#[tokio::test]
async fn fr_t14_the_server_token_on_a_guest_route_is_401() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let a = app(&d);
    let (s, _, _) = send(
        &a,
        "GET",
        "/api/guest/channels/x/messages",
        &[("host", "127.0.0.1:9000"), ("authorization", "Bearer the-server-token")],
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[test]
fn fr_t14_invite_link_uses_the_public_url_when_set_else_the_listen_address_with_a_warning() {
    let on = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let (link, warn, gd) = invite_link(Some(&on), "dpi_t");
    assert_eq!(link, "http://127.0.0.1:9000/join#dpi_t");
    assert!(warn);
    assert_eq!(gd, json!({"state": "on", "listen": "127.0.0.1:9000", "public_url": null}));

    let public = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_PUBLIC_URL", "https://g.example.com/")]);
    let (link, warn, gd) = invite_link(Some(&public), "dpi_t");
    assert_eq!(link, "https://g.example.com/join#dpi_t");
    assert!(!warn);
    assert_eq!(gd["state"], "on");
    assert_eq!(gd["public_url"], "https://g.example.com");

    let (link, warn, gd) = invite_link(None, "dpi_t");
    assert_eq!(link, "/join#dpi_t");
    assert!(!warn);
    assert_eq!(gd["state"], "off");
}

#[tokio::test]
async fn fr_t14_the_invite_response_carries_the_link_warning_and_the_door_state() {
    let (store, sessions) = store_and_sessions();
    store.create_session("core", AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
    let ch = store.channels(None).unwrap().remove(0).id;
    for (d, link_prefix, warn, state) in [
        (None, "/join#", false, "off"),
        (Some(door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")])), "http://127.0.0.1:9000/join#", true, "on"),
        (
            Some(door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_PUBLIC_URL", "https://g.example.com")])),
            "https://g.example.com/join#",
            false,
            "on",
        ),
    ] {
        let owner = ember_server::channels::api::router_with_door(store.clone(), sessions.clone(), d);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/channels/{ch}/invites"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let res = owner.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let v: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let token = v["token"].as_str().unwrap();
        assert_eq!(v["link"], format!("{link_prefix}{token}"));
        assert_eq!(v["link_warning"], warn);
        assert_eq!(v["guest_door"]["state"], state);
    }
}
