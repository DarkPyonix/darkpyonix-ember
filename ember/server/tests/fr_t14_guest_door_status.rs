//! FR-T14: `GET /api/guest-door`, the owner's view of the guest door.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::auth::{self, ServerAuth};
use ember_server::channels::guest::guest_router;
use ember_server::guest_door::GuestDoor;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";

fn door(vars: &[(&str, &str)]) -> GuestDoor {
    let m: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    GuestDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse::<SocketAddr>().unwrap())
        .unwrap()
        .expect("door on")
}

fn parts() -> (Arc<Store>, Arc<Sessions>) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    (store, sessions)
}

fn owner(d: Option<GuestDoor>) -> Router {
    let (store, sessions) = parts();
    ember_server::channels::api::router_with_door(store, sessions, d)
}

async fn get(app: &Router, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut req = Request::builder().method("GET").uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let st = res.status();
    let b = res.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

#[tokio::test]
async fn fr_t14_door_off_gives_the_off_shape() {
    let (st, v) = get(&owner(None), "/api/guest-door", &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!({"state": "off", "listen": null, "public_url": null, "link_warning": false}));
}

#[tokio::test]
async fn fr_t14_door_on_without_a_public_url_warns() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let (st, v) = get(&owner(Some(d)), "/api/guest-door", &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!({"state": "on", "listen": "127.0.0.1:9000", "public_url": null, "link_warning": true}));
}

#[tokio::test]
async fn fr_t14_door_on_with_a_public_url_does_not_warn() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_PUBLIC_URL", "https://g.example.com")]);
    let (st, v) = get(&owner(Some(d)), "/api/guest-door", &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        v,
        json!({"state": "on", "listen": "127.0.0.1:9000", "public_url": "https://g.example.com", "link_warning": false})
    );
}

#[tokio::test]
async fn fr_t14_guest_door_route_needs_the_server_token() {
    let a = ServerAuth::new("tok".to_string(), OWNER.parse().unwrap(), []);
    let app = auth::protect(owner(None), a);
    let (st, _) = get(&app, "/api/guest-door", &[("host", "127.0.0.1:8740")]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = get(&app, "/api/guest-door", &[("host", "127.0.0.1:8740"), ("authorization", "Bearer wrong")]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, v) = get(&app, "/api/guest-door", &[("host", "127.0.0.1:8740"), ("authorization", "Bearer tok")]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["state"], "off");
}

#[tokio::test]
async fn fr_t14_the_guest_door_does_not_serve_the_status_route() {
    let d = door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]);
    let (store, sessions) = parts();
    let app = d.wrap(guest_router(store, sessions));
    for m in ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"] {
        let req = Request::builder()
            .method(m)
            .uri("/api/guest-door")
            .header("host", "127.0.0.1:9000")
            .body(Body::empty())
            .unwrap();
        let s = app.clone().oneshot(req).await.unwrap().status();
        assert!(s == StatusCode::NOT_FOUND || s == StatusCode::METHOD_NOT_ALLOWED, "{m}: {s}");
    }
}

#[tokio::test]
async fn fr_t14_the_invite_response_and_the_status_route_agree() {
    let none = None;
    let on = Some(door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000")]));
    let public = Some(door(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_PUBLIC_URL", "https://g.example.com")]));
    for d in [none, on, public] {
        let (store, sessions) = parts();
        store.create_session("core", AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
        let ch = store.channels(None).unwrap().remove(0).id;
        let app = ember_server::channels::api::router_with_door(store, sessions, d);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/channels/{ch}/invites"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let inv: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let (_, status) = get(&app, "/api/guest-door", &[]).await;
        assert_eq!(status["link_warning"], inv["link_warning"]);
        for k in ["state", "listen", "public_url"] {
            assert_eq!(status[k], inv["guest_door"][k], "{k}");
        }
    }
}
