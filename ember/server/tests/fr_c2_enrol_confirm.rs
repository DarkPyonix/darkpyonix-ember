//! FR-C2a to FR-C2d, FR-C2g (step 1 of the enrol work, INTENT D39 "Confirm at join", all
//! [provisional]): the owner's confirm and deny routes and the status route, on the local
//! token-protected router. There is no enrol door yet, so the redeem is driven through the core
//! (`Enrol::redeem`) and the node's poll through `Enrol::take_delivery`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::enrol::{
    ComputersRegistrar, Delivery, Enrol, NodeInfo, RegisterError, Registrar, CONFIRM_WINDOW_MS,
};
use ember_server::computers::{default_connector, Computers, Registry};
use ember_server::store::{now_ms, Store};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";
const BASE: &str = "http://192.168.0.5:8740";
const SRC: &str = "192.168.0.9:50123";

/// A registry that lives in a Vec.
#[derive(Default)]
struct Fake {
    registered: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn count(&self) -> usize {
        self.registered.lock().unwrap().len()
    }
}

impl Registrar for Fake {
    fn name_taken(&self, name: &str) -> bool {
        self.registered.lock().unwrap().iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
    fn register(&self, name: &str, token: &str, _i: &NodeInfo, _o: &str) -> Result<String, RegisterError> {
        if self.name_taken(name) {
            return Err(RegisterError::NameTaken);
        }
        let mut r = self.registered.lock().unwrap();
        r.push((name.to_string(), token.to_string()));
        Ok(format!("computer-{}", r.len()))
    }
    fn remove(&self, _id: &str) {}
}

struct Fx {
    app: Router,
    enrol: Arc<Enrol>,
    token: String,
    _dir: tempfile::TempDir,
}

fn fx_with(registrar: impl FnOnce(&Arc<Store>) -> Arc<dyn Registrar>) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let enrol = Enrol::new(store.clone());
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse::<SocketAddr>().unwrap(), []);
    let reg = registrar(&store);
    let app = auth::protect(ember_server::computers::enrol::router(enrol.clone(), ember_server::computers::enrol::DoorUrl::Url(BASE.into()), reg), a);
    Fx { app, enrol, token, _dir: dir }
}

fn fx(fake: Arc<Fake>) -> Fx {
    fx_with(|_| fake)
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

fn info() -> NodeInfo {
    NodeInfo {
        hostname: "ibrew-laptop".into(),
        os: "macos".into(),
        arch: "aarch64".into(),
        node_version: "0.1.0".into(),
        url: Some("http://192.168.0.9:8741".into()),
        peer_id: None,
    }
}

struct Opened {
    id: String,
    code: String,
    poll_secret: String,
}

/// Mint through the route and redeem through the core (the door does not exist yet).
async fn open(f: &Fx, name: &str) -> Opened {
    let h = format!("Bearer {}", f.token);
    let (st, body, _) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({ "name": name }))).await;
    assert_eq!(st, StatusCode::CREATED);
    let id = body["id"].as_str().unwrap().to_string();
    let code = body["code"].as_str().unwrap().to_string();
    let r = f.enrol.redeem(&code, &info(), now_ms(), SRC).unwrap();
    assert_eq!(r.enrol_id, id);
    Opened { id, code, poll_secret: r.poll_secret.expose().to_string() }
}

fn path(id: &str, tail: &str) -> String {
    format!("/api/computers/enrol-codes/{id}{tail}")
}

#[tokio::test]
async fn fr_c2_confirm_deny_and_status_routes_need_the_server_token() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let o = open(&f, "laptop").await;
    let creds = [
        None,
        Some("Bearer nope".to_string()),
        Some("Bearer dpg_somecredential".to_string()),
        Some("Bearer dpe_whatever".to_string()),
        // The real code and the real poll secret are not the owner's credential either.
        Some(format!("Bearer {}", o.code)),
        Some(format!("Bearer {}", o.poll_secret)),
    ];
    let routes = [
        (Method::GET, path(&o.id, ""), None),
        (Method::POST, path(&o.id, "/confirm"), Some(json!({}))),
        (Method::POST, path(&o.id, "/deny"), None),
    ];
    for (m, p, b) in &routes {
        for a in &creds {
            let (st, _, text) = call(&f.app, m.clone(), p, a.as_deref(), b.clone()).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{m} {p} {a:?}");
            assert!(!text.contains(&o.poll_secret));
        }
    }
    // Nothing happened.
    assert_eq!(fake.count(), 0);
    assert!(matches!(f.enrol.take_delivery(&o.id, &o.poll_secret, now_ms()).unwrap(), Delivery::Pending));
}

#[tokio::test]
async fn fr_c2_status_route_answers_the_node_info_for_the_owner() {
    let f = fx(Arc::new(Fake::default()));
    let h = format!("Bearer {}", f.token);
    let (_, body, _) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({"name": "desk"}))).await;
    let id = body["id"].as_str().unwrap().to_string();
    let (_, b, _) = call(&f.app, Method::GET, &path(&id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "waiting");
    assert!(b.get("node_info").is_none() && b.get("observed_addr").is_none());

    let code = body["code"].as_str().unwrap();
    // A node that reports no url: the server proposes the observed address with port 8741.
    let mut bare = info();
    bare.url = None;
    let r = f.enrol.redeem(code, &bare, now_ms(), SRC).unwrap();
    let (st, b, text) = call(&f.app, Method::GET, &path(&id, ""), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(b["status"], "awaiting_confirmation");
    assert_eq!(b["name"], "desk");
    assert_eq!(b["node_info"]["hostname"], "ibrew-laptop");
    assert_eq!(b["observed_addr"], SRC);
    assert_eq!(b["proposed_url"], "http://192.168.0.9:8741");
    assert_eq!(b["confirm_deadline_ms"], r.expires_ms);
    // The secrets are in no answer.
    assert!(!text.contains(r.poll_secret.expose()) && !text.contains(code));
}

#[tokio::test]
async fn fr_c2_confirm_registers_the_computer_once_and_the_node_gets_the_token_once() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    let o = open(&f, "laptop").await;
    assert!(matches!(f.enrol.take_delivery(&o.id, &o.poll_secret, now_ms()).unwrap(), Delivery::Pending));

    let (st, body, text) = call(&f.app, Method::POST, &path(&o.id, "/confirm"), Some(&h), Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(body["computer_id"], "computer-1");
    assert_eq!(body["name"], "laptop");
    let token = fake.registered.lock().unwrap()[0].1.clone();
    assert!(!text.contains(&token), "the confirm answer carries no credential");
    let (_, b, text) = call(&f.app, Method::GET, &path(&o.id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "awaiting_confirmation");
    assert!(!text.contains(&token));

    match f.enrol.take_delivery(&o.id, &o.poll_secret, now_ms()).unwrap() {
        Delivery::Joined { token: t, .. } => assert_eq!(t.expose(), token),
        other => panic!("{other:?}"),
    }
    assert!(matches!(f.enrol.take_delivery(&o.id, &o.poll_secret, now_ms()).unwrap(), Delivery::Delivered));
    let (_, b, _) = call(&f.app, Method::GET, &path(&o.id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "joined");

    // A second confirm is wrong_state, and nothing more was registered.
    let (st, body, _) = call(&f.app, Method::POST, &path(&o.id, "/confirm"), Some(&h), Some(json!({}))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], "wrong_state");
    assert_eq!(fake.count(), 1);
}

#[tokio::test]
async fn fr_c2_confirm_with_no_body_uses_the_minted_name() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    let o = open(&f, "no-body").await;
    let (st, body, _) = call(&f.app, Method::POST, &path(&o.id, "/confirm"), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["name"], "no-body");
}

#[tokio::test]
async fn fr_c2_confirm_name_clash_is_409_and_the_row_stays_pending_then_rename_works() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    let first = open(&f, "laptop").await;
    call(&f.app, Method::POST, &path(&first.id, "/confirm"), Some(&h), Some(json!({}))).await;
    let second = open(&f, "laptop").await;

    let (st, body, _) = call(&f.app, Method::POST, &path(&second.id, "/confirm"), Some(&h), Some(json!({}))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], "name_taken");
    assert!(body["message"].as_str().unwrap().contains("laptop"));
    let (_, b, _) = call(&f.app, Method::GET, &path(&second.id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "awaiting_confirmation");
    assert!(matches!(f.enrol.take_delivery(&second.id, &second.poll_secret, now_ms()).unwrap(), Delivery::Pending));
    assert_eq!(fake.count(), 1);

    // Invalid replacement names are 400 and change nothing.
    let (st, _, _) = call(&f.app, Method::POST, &path(&second.id, "/confirm"), Some(&h), Some(json!({"name": "a\nb"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, body, _) = call(&f.app, Method::POST, &path(&second.id, "/confirm"), Some(&h), Some(json!({"name": "laptop-2"}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["name"], "laptop-2");
    assert_eq!(fake.count(), 2);
}

#[tokio::test]
async fn fr_c2_deny_registers_nothing_and_the_node_sees_denied() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    let o = open(&f, "laptop").await;
    let (st, body, _) = call(&f.app, Method::POST, &path(&o.id, "/deny"), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "denied");
    let (_, b, _) = call(&f.app, Method::GET, &path(&o.id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "denied");
    assert!(b.get("node_info").is_none(), "the details go once the owner has decided");
    assert!(matches!(f.enrol.take_delivery(&o.id, &o.poll_secret, now_ms()).unwrap(), Delivery::Denied));
    assert_eq!(fake.count(), 0);
    // Deny again and confirm after deny are wrong_state.
    for (p, b) in [(path(&o.id, "/deny"), None), (path(&o.id, "/confirm"), Some(json!({})))] {
        let (st, body, _) = call(&f.app, Method::POST, &p, Some(&h), b).await;
        assert_eq!(st, StatusCode::CONFLICT, "{p}");
        assert_eq!(body["error"], "wrong_state");
    }
    assert_eq!(fake.count(), 0);
}

#[tokio::test]
async fn fr_c2_confirm_after_the_deadline_is_410_and_registers_nothing() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    // Minted and redeemed 11 minutes ago, through the core with an explicit clock.
    let past = now_ms() - CONFIRM_WINDOW_MS - 60_000;
    let m = f.enrol.mint("old", past, None, BASE).unwrap();
    let r = f.enrol.redeem(m.code.expose(), &info(), past + 1, SRC).unwrap();
    let (st, body, _) = call(&f.app, Method::POST, &path(&r.enrol_id, "/confirm"), Some(&h), Some(json!({}))).await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(body["error"], "expired");
    assert_eq!(fake.count(), 0);
    let (_, b, _) = call(&f.app, Method::GET, &path(&r.enrol_id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "expired");
    let (st, _, _) = call(&f.app, Method::POST, &path(&r.enrol_id, "/deny"), Some(&h), None).await;
    assert_eq!(st, StatusCode::GONE);
}

#[tokio::test]
async fn fr_c2_unknown_ids_are_404_and_unknown_body_fields_are_refused() {
    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    for tail in ["/confirm", "/deny"] {
        let (st, _, _) = call(&f.app, Method::POST, &path("nope", tail), Some(&h), Some(json!({}))).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{tail}");
    }
    // `expect_fingerprint` is specified but not built: it is refused, never silently ignored.
    let o = open(&f, "laptop").await;
    let (st, _, _) =
        call(&f.app, Method::POST, &path(&o.id, "/confirm"), Some(&h), Some(json!({"expect_fingerprint": "ab12"}))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fake.count(), 0);
    let (_, b, _) = call(&f.app, Method::GET, &path(&o.id, ""), Some(&h), None).await;
    assert_eq!(b["status"], "awaiting_confirmation");
    // Wrong methods.
    for (m, p) in [(Method::GET, path(&o.id, "/confirm")), (Method::GET, path(&o.id, "/deny")), (Method::DELETE, path(&o.id, "/confirm"))] {
        let (st, _, _) = call(&f.app, m.clone(), &p, Some(&h), None).await;
        assert_eq!(st, StatusCode::METHOD_NOT_ALLOWED, "{m} {p}");
    }
}

#[tokio::test]
async fn fr_c2_the_admin_router_has_no_redeem_and_no_poll_route() {
    let f = fx(Arc::new(Fake::default()));
    let h = format!("Bearer {}", f.token);
    for (m, p) in [
        (Method::POST, "/enrol/redeem".to_string()),
        (Method::GET, "/enrol/status/x".to_string()),
        (Method::POST, path("x", "/redeem")),
        (Method::GET, path("x", "/poll")),
    ] {
        let (st, _, _) = call(&f.app, m.clone(), &p, Some(&h), Some(json!({"code": "dpe_x"}))).await;
        assert!(st == StatusCode::NOT_FOUND || st == StatusCode::METHOD_NOT_ALLOWED, "{m} {p}: {st}");
    }
}

#[tokio::test]
async fn fr_c2_confirm_with_the_real_registry_adds_the_computer_with_the_delivered_token() {
    let mut computers: Option<Arc<Computers>> = None;
    let f = fx_with(|store| {
        let c = Computers::new(Registry::new(store.clone()), default_connector());
        computers = Some(c.clone());
        Arc::new(ComputersRegistrar::new(c))
    });
    let computers = computers.unwrap();
    let h = format!("Bearer {}", f.token);
    let o = open(&f, "real-box").await;
    let (st, body, text) = call(&f.app, Method::POST, &path(&o.id, "/confirm"), Some(&h), Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let computer_id = body["computer_id"].as_str().unwrap().to_string();
    let listed = computers.registry().list().unwrap();
    let c = listed.iter().find(|c| c.id == computer_id).expect("registered");
    assert_eq!(c.name, "real-box");
    assert_eq!(c.url, "http://192.168.0.9:8741");
    match f.enrol.take_delivery(&o.id, &o.poll_secret, now_ms()).unwrap() {
        Delivery::Joined { token, computer_id: cid, .. } => {
            assert_eq!(token.expose(), c.token);
            assert_eq!(cid, computer_id);
        }
        other => panic!("{other:?}"),
    }

    // A second enrolment under the same name is a clash against the real registry.
    let again = open(&f, "REAL-BOX").await;
    let (st, body, _) = call(&f.app, Method::POST, &path(&again.id, "/confirm"), Some(&h), Some(json!({}))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], "name_taken");
    assert_eq!(computers.registry().list().unwrap().len(), 1);

    // A node that reported no address is registered at the observed address plus port 8741.
    let m = f.enrol.mint("guessed", now_ms(), None, BASE).unwrap();
    let mut bare = info();
    bare.url = None;
    let r = f.enrol.redeem(m.code.expose(), &bare, now_ms(), "192.168.0.44:60001").unwrap();
    let (st, body, _) = call(&f.app, Method::POST, &path(&r.enrol_id, "/confirm"), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK);
    let guessed = computers.registry().get(body["computer_id"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(guessed.url, "http://192.168.0.44:8741");
}
