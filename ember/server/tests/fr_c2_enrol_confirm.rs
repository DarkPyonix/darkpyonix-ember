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
    // An unknown field is refused, never silently ignored.
    let o = open(&f, "laptop").await;
    let (st, _, _) =
        call(&f.app, Method::POST, &path(&o.id, "/confirm"), Some(&h), Some(json!({"expect_fingerprints": "ab12"}))).await;
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


// ---- C5: expect_fingerprint, registered_by, duplicate peer ----

/// A registrar over [`Fake`] that says how it would register every node.
struct By {
    inner: Fake,
    by: Option<&'static str>,
}

impl Registrar for By {
    fn name_taken(&self, name: &str) -> bool {
        self.inner.name_taken(name)
    }
    fn register(&self, name: &str, token: &str, i: &NodeInfo, o: &str) -> Result<String, RegisterError> {
        self.inner.register(name, token, i, o)
    }
    fn remove(&self, id: &str) {
        self.inner.remove(id)
    }
    fn registers_by(&self, _i: &NodeInfo, _o: &str) -> Option<&'static str> {
        self.by
    }
}

fn fx_by(by: Option<&'static str>) -> (Fx, Arc<By>) {
    let reg = Arc::new(By { inner: Fake::default(), by });
    (fx_with(|_| reg.clone()), reg)
}

fn peer_node(url: Option<&str>) -> (NodeInfo, String) {
    let id = ember_transport::SecretKey::generate().peer_id().to_string();
    let mut i = info();
    i.url = url.map(String::from);
    i.peer_id = Some(id.clone());
    (i, id)
}

fn redeem_info(f: &Fx, name: &str, i: &NodeInfo) -> String {
    let m = f.enrol.mint(name, now_ms(), None, BASE).unwrap();
    f.enrol.redeem(m.code.expose(), i, now_ms(), SRC).unwrap().enrol_id
}

async fn status_of(f: &Fx, id: &str) -> Value {
    let h = format!("Bearer {}", f.token);
    let (st, b, _) = call(&f.app, Method::GET, &path(id, ""), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK);
    b
}

#[tokio::test]
async fn fr_c2_confirm_accepts_a_matching_expect_fingerprint_in_any_case_and_spacing() {
    let h_of = |f: &Fx| format!("Bearer {}", f.token);
    for style in 0..3 {
        let (f, reg) = fx_by(Some("url"));
        let (i, id) = peer_node(None);
        let eid = redeem_info(&f, "box", &i);
        let fp = ember_transport::fingerprint_hex(&id).unwrap();
        let given = match style {
            0 => fp.clone(),
            1 => fp.to_ascii_uppercase(),
            _ => fp.replace(' ', ""),
        };
        let (st, _, text) = call(
            &f.app,
            Method::POST,
            &path(&eid, "/confirm"),
            Some(&h_of(&f)),
            Some(json!({ "expect_fingerprint": given })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{given:?}: {text}");
        assert_eq!(reg.inner.count(), 1);
    }
}

#[tokio::test]
async fn fr_c2_confirm_with_a_wrong_fingerprint_is_409_and_nothing_is_registered() {
    let (f, reg) = fx_by(Some("url"));
    let h = format!("Bearer {}", f.token);
    let (i, _) = peer_node(None);
    let eid = redeem_info(&f, "box", &i);
    let other = ember_transport::fingerprint_hex(&ember_transport::SecretKey::generate().peer_id().to_string()).unwrap();
    let (st, body, _) =
        call(&f.app, Method::POST, &path(&eid, "/confirm"), Some(&h), Some(json!({ "expect_fingerprint": other }))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], "fingerprint_mismatch");
    assert!(body["message"].is_string());
    assert_eq!(reg.inner.count(), 0);
    assert_eq!(status_of(&f, &eid).await["status"], "awaiting_confirmation");
}

#[tokio::test]
async fn fr_c2_expect_fingerprint_for_a_node_without_a_peer_id_is_a_mismatch() {
    let (f, reg) = fx_by(Some("url"));
    let h = format!("Bearer {}", f.token);
    let eid = redeem_info(&f, "box", &info());
    let (st, body, _) = call(
        &f.app,
        Method::POST,
        &path(&eid, "/confirm"),
        Some(&h),
        Some(json!({ "expect_fingerprint": "abab abab abab abab" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], "fingerprint_mismatch");
    assert_eq!(reg.inner.count(), 0);
}

#[tokio::test]
async fn fr_c2_a_malformed_expect_fingerprint_is_400() {
    let (f, reg) = fx_by(Some("url"));
    let h = format!("Bearer {}", f.token);
    let (i, _) = peer_node(None);
    let eid = redeem_info(&f, "box", &i);
    for bad in ["", "abab", "not hex at all!!!!", "abab abab abab abab ab"] {
        let (st, _, text) =
            call(&f.app, Method::POST, &path(&eid, "/confirm"), Some(&h), Some(json!({ "expect_fingerprint": bad }))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad:?}: {text}");
    }
    assert_eq!(reg.inner.count(), 0);
    assert_eq!(status_of(&f, &eid).await["status"], "awaiting_confirmation");
}

#[tokio::test]
async fn fr_c2_the_peer_path_needs_expect_fingerprint_and_a_url_path_does_not() {
    let (f, reg) = fx_by(Some("peer"));
    let h = format!("Bearer {}", f.token);
    let (i, id) = peer_node(None);
    let eid = redeem_info(&f, "box", &i);
    for body in [None, Some(json!({})), Some(json!({ "name": "box" }))] {
        let (st, b, _) = call(&f.app, Method::POST, &path(&eid, "/confirm"), Some(&h), body).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(b["error"], "fingerprint_required");
        assert_eq!(
            b["message"],
            "This computer is registered by its peer id; confirm with expect_fingerprint set to the fingerprint shown on the other machine"
        );
    }
    assert_eq!(reg.inner.count(), 0);
    assert_eq!(status_of(&f, &eid).await["status"], "awaiting_confirmation");
    let fp = ember_transport::fingerprint_hex(&id).unwrap();
    let (st, _, text) =
        call(&f.app, Method::POST, &path(&eid, "/confirm"), Some(&h), Some(json!({ "expect_fingerprint": fp }))).await;
    assert_eq!(st, StatusCode::OK, "{text}");

    // The url path confirms as before, with no fingerprint.
    let (f, _) = fx_by(Some("url"));
    let h = format!("Bearer {}", f.token);
    let (i, _) = peer_node(Some("http://192.168.0.9:8741"));
    let eid = redeem_info(&f, "box", &i);
    let (st, _, text) = call(&f.app, Method::POST, &path(&eid, "/confirm"), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn fr_c2_status_route_carries_registered_by_only_while_awaiting_and_only_with_a_registrar() {
    let (f, _) = fx_by(Some("url"));
    let h = format!("Bearer {}", f.token);
    // A url and a peer id together: both are shown and the url is what would be used.
    let (i, id) = peer_node(Some("http://192.168.0.9:8741"));
    let eid = redeem_info(&f, "box", &i);
    let b = status_of(&f, &eid).await;
    assert_eq!(b["registered_by"], "url");
    assert_eq!(b["node_info"]["peer_id"], id.as_str());
    assert_eq!(b["node_info"]["url"], "http://192.168.0.9:8741");
    let (st, _, _) = call(&f.app, Method::POST, &path(&eid, "/deny"), Some(&h), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(status_of(&f, &eid).await.get("registered_by").is_none());

    let (f, _) = fx_by(Some("peer"));
    let (i, _) = peer_node(None);
    let eid = redeem_info(&f, "box", &i);
    assert_eq!(status_of(&f, &eid).await["registered_by"], "peer");

    let (f, _) = fx_by(Some("observed"));
    let mut bare = info();
    bare.url = None;
    let eid = redeem_info(&f, "box", &bare);
    assert_eq!(status_of(&f, &eid).await["registered_by"], "observed");

    // A registrar that cannot say (the trait default, here the plain Fake): no key.
    let f = fx(Arc::new(Fake::default()));
    let eid = redeem_info(&f, "box", &info());
    assert!(status_of(&f, &eid).await.get("registered_by").is_none());
}

/// A real `Computers` with a transport dialer on the in-memory network.
fn computers_with_transport(store: &Arc<Store>) -> Arc<Computers> {
    let net = ember_transport::mem::MemNetwork::new();
    let st = ember_server::transport::ServerTransport::new(net.transport());
    Computers::with_transport(
        Registry::new(store.clone()),
        ember_server::computers::connector(Some(st.dialer.clone())),
        None,
        Some(st.dialer.clone()),
    )
}

#[tokio::test]
async fn fr_c2_post_computers_with_a_duplicate_peer_is_400_and_the_real_confirm_refuses_it_too() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = computers_with_transport(&store);
    let sessions = ember_server::session::Sessions::new(
        store.clone(),
        vec![Arc::new(ember_server::agents::scripted::ScriptedAdapter) as Arc<dyn ember_server::agents::AgentAdapter>],
    );
    let api = ember_server::computers::api::router(computers.clone(), sessions);
    let id = ember_transport::SecretKey::generate().peer_id().to_string();
    let (st, _, text) =
        call(&api, Method::POST, "/api/computers", None, Some(json!({ "name": "one", "peer": id, "token": "t1" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{text}");
    // The same id again, as a full address with other relays, under another name.
    let (st, body, _) = call(
        &api,
        Method::POST,
        "/api/computers",
        None,
        Some(json!({ "name": "two", "peer": { "peer": id, "relays": ["https://relay.example"] }, "token": "t2" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "a computer with this peer id is already registered");
    assert_eq!(computers.registry().list().unwrap().len(), 1);

    // The enrol confirm over the same registry: a node with that peer id is refused (400) and the
    // enrolment stays awaiting confirmation.
    let enrol = Enrol::new(store.clone());
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse::<SocketAddr>().unwrap(), []);
    let app = auth::protect(
        ember_server::computers::enrol::router(
            enrol.clone(),
            ember_server::computers::enrol::DoorUrl::Url(BASE.into()),
            Arc::new(ComputersRegistrar::new(computers.clone())),
        ),
        a,
    );
    let h = format!("Bearer {token}");
    let mut i = info();
    i.url = None;
    i.peer_id = Some(id.clone());
    let m = enrol.mint("dup", now_ms(), None, BASE).unwrap();
    let eid = enrol.redeem(m.code.expose(), &i, now_ms(), SRC).unwrap().enrol_id;
    let fp = ember_transport::fingerprint_hex(&id).unwrap();
    let (st, body, _) =
        call(&app, Method::POST, &path(&eid, "/confirm"), Some(&h), Some(json!({ "expect_fingerprint": fp }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "a computer with this peer id is already registered");
    assert_eq!(computers.registry().list().unwrap().len(), 1);
    let (_, b, _) = call(&app, Method::GET, &path(&eid, ""), Some(&h), None).await;
    assert_eq!(b["status"], "awaiting_confirmation");
    assert_eq!(b["registered_by"], "peer");
}
