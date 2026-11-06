//! FR-C1: the admin routes for enrol codes (INTENT D39, provisional). The redeem core has no
//! route; these tests cover minting, status and cancel behind the local token.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::enrol::Enrol;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";
const BASE: &str = "http://192.168.0.5:8740";

struct Fx {
    app: Router,
    enrol: Arc<Enrol>,
    token: String,
    _dir: tempfile::TempDir,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let enrol = Enrol::new(store);
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse::<SocketAddr>().unwrap(), []);
    let app = auth::protect(ember_server::computers::enrol::router(enrol.clone(), BASE.into()), a);
    Fx { app, enrol, token, _dir: dir }
}

async fn call(app: &Router, method: Method, path: &str, auth: Option<&str>, body: Option<Value>) -> (StatusCode, Value, String) {
    let mut b = Request::builder().method(method).uri(path).header("host", LISTEN);
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let st = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null), text)
}

#[tokio::test]
async fn fr_c1_admin_routes_need_the_token() {
    let f = fx();
    let routes = [
        (Method::POST, "/api/computers/enrol-codes", Some(json!({"name": "x"}))),
        (Method::GET, "/api/computers/enrol-codes/e1", None),
        (Method::DELETE, "/api/computers/enrol-codes/e1", None),
    ];
    for (m, p, b) in routes {
        for a in [None, Some("Bearer nope"), Some("Bearer dpg_somecredential"), Some("Bearer dpe_whatever")] {
            let (st, _, _) = call(&f.app, m.clone(), p, a, b.clone()).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{m} {p} {a:?}");
        }
    }
    assert!(f.enrol.status("e1", 0).is_err());
}

#[tokio::test]
async fn fr_c1_mint_answers_201_with_the_code_once() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    let (st, body, text) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({"name": "laptop"}))).await;
    assert_eq!(st, StatusCode::CREATED, "{text}");
    let code = body["code"].as_str().unwrap().to_string();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(code.starts_with("dpe_"));
    assert_eq!(body["command"], format!("ember-node enrol {code} --server {BASE}"));
    assert!(body["expires_ms"].as_i64().unwrap() > 0);
    assert!(body["warning"].as_str().unwrap().contains(BASE));

    // The code is in no later answer.
    let (st, body, text) = call(&f.app, Method::GET, &format!("/api/computers/enrol-codes/{id}"), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "waiting");
    assert!(!text.contains(&code));
}

#[tokio::test]
async fn fr_c1_mint_refuses_bad_input() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    for b in [json!({"name": ""}), json!({"name": "local"}), json!({"name": "ok", "ttl_secs": 0})] {
        let (st, body, _) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(b.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
        assert!(body["error"].is_string());
    }
    // A larger ttl is clamped to 10 minutes, not refused.
    let (st, body, _) =
        call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({"name": "ok", "ttl_secs": 999999}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let exp = body["expires_ms"].as_i64().unwrap();
    assert!(exp <= ember_server::store::now_ms() + 600_000);
}

#[tokio::test]
async fn fr_c1_status_and_cancel_by_id() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    let (_, body, _) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({"name": "a"}))).await;
    let id = body["id"].as_str().unwrap().to_string();
    let path = format!("/api/computers/enrol-codes/{id}");
    let (st, _, _) = call(&f.app, Method::DELETE, &path, Some(&h), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _, _) = call(&f.app, Method::GET, &path, Some(&h), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = call(&f.app, Method::DELETE, &path, Some(&h), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A redeemed code cannot be cancelled, and reads joined once completed.
    let (_, body, _) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({"name": "b"}))).await;
    let (id, code) = (body["id"].as_str().unwrap().to_string(), body["code"].as_str().unwrap().to_string());
    let r = f.enrol.redeem(&code, ember_server::store::now_ms(), "src").unwrap();
    let path = format!("/api/computers/enrol-codes/{id}");
    let (st, _, _) = call(&f.app, Method::DELETE, &path, Some(&h), None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    f.enrol.complete(&r.enrol_id, "c1").unwrap();
    let (_, body, _) = call(&f.app, Method::GET, &path, Some(&h), None).await;
    assert_eq!(body["status"], "joined");
}

#[tokio::test]
async fn fr_c1_there_is_no_redeem_route() {
    let f = fx();
    let h = format!("Bearer {}", f.token);
    for (m, p) in [
        (Method::POST, "/api/computers/enrol-codes/redeem"),
        (Method::POST, "/api/computers/enrol"),
        (Method::POST, "/api/computers/enrol-codes/x/redeem"),
        (Method::GET, "/api/computers/enrol-codes"),
    ] {
        for a in [None, Some(h.as_str())] {
            let (st, _, _) = call(&f.app, m.clone(), p, a, Some(json!({"code": "dpe_x"}))).await;
            assert!(!st.is_success(), "{m} {p}: {st}");
        }
    }
}
