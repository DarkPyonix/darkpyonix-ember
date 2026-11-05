//! FR-T14, FR-T15 hardening of the guest routes (review of #400). Details are [provisional].

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::auth::{self, ServerAuth};
use ember_server::channels::guest::guest_router;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const SERVER_TOKEN: &str = "server-token-for-tests";

struct Fx {
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    channel: String,
    session_id: String,
    guest: Router,
    path: std::path::PathBuf,
}

fn fixture(dir: &Path) -> Fx {
    let path = dir.join("ember.db");
    let store = Arc::new(Store::open(&path).unwrap());
    let session_id = store.create_session("core", AgentKind::Scripted, "/secret/cwd", None, "alpha").unwrap().id;
    let channel = store.channels(None).unwrap().remove(0).id;
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let guest = guest_router(store.clone(), sessions.clone());
    Fx { store, sessions, channel, session_id, guest, path }
}

async fn call(app: &Router, method: &str, uri: &str, bearer: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", "127.0.0.1:8740");
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Accept a fresh invite over HTTP: (guest id, credential, invite id).
async fn join(fx: &Fx, name: &str) -> (String, String, String) {
    let (invite, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    let (status, made) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": name}))).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    (made["guest_id"].as_str().unwrap().into(), made["credential"].as_str().unwrap().into(), invite.id)
}

fn owner_app(fx: &Fx) -> Router {
    let routes = ember_server::channels::api::router(fx.store.clone(), fx.sessions.clone())
        .merge(ember_server::api::router(fx.sessions.clone()));
    auth::protect(routes, ServerAuth::new(SERVER_TOKEN.into(), "127.0.0.1:8740".parse().unwrap(), []))
}

#[tokio::test]
async fn fr_t14_accept_refuses_reserved_and_member_names_without_burning_the_invite() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    // "alpha" is the title of the channel's member agent.
    for bad in ["user", "USER", "  나  ", "System", "AGENT", "alpha", "ALPHA", " Alpha "] {
        let (status, body) =
            call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": bad}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert_eq!(body["code"], "invalid_name", "{bad:?}");
    }
    let (status, _) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": "Mina"}))).await;
    assert_eq!(status, StatusCode::CREATED, "the invite was not burned by the refusals");
}

#[tokio::test]
async fn fr_t14_accept_refuses_format_and_separator_characters() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    for bad in [
        "Mi\u{200B}na", "\u{200E}Mina", "Mi\u{202A}na", "Mi\u{202E}na", "Mi\u{2066}na", "Mi\u{2069}na",
        "Mi\u{200D}na", "\u{FEFF}Mina", "Mi\u{2028}na", "Mi\u{2029}na", "Mi\u{0085}na", "Mi\u{00AD}na",
        "\u{200B}", " \t\u{3000} ", "\u{200B}\u{200B}",
    ] {
        let (status, body) =
            call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": bad}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert_eq!(body["code"], "invalid_name", "{bad:?}");
    }
    // Ordinary non-ASCII names pass.
    let (status, _) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": "민아 Ünal"}))).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn fr_t14_a_guest_is_limited_to_ten_posts_a_minute() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let (_, other, _) = join(&fx, "Joon").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    for i in 0..10 {
        let (status, _) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": format!("m{i}")}))).await;
        assert_eq!(status, StatusCode::CREATED, "post {i}");
    }
    let (status, body) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": "eleventh"}))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "rate_limited");
    assert_eq!(fx.store.read_channel_messages(&fx.channel, 0).unwrap().len(), 10, "the refused post was not stored");
    // Reading is not limited, and another guest has its own budget.
    assert_eq!(call(&fx.guest, "GET", &uri, Some(&cred), None).await.0, StatusCode::OK);
    let (status, _) = call(&fx.guest, "POST", &uri, Some(&other), Some(json!({"text": "hi"}))).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn fr_t14_a_guest_is_capped_at_500_messages_per_channel() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let (_, other, _) = join(&fx, "Joon").await;
    let guest = fx.store.guest_by_credential(&cred).unwrap();
    for i in 0..500 {
        fx.store.post_guest_message(&guest, &format!("m{i}")).unwrap();
    }
    // The owner's and agents' messages do not count toward the guest's cap.
    fx.store.post_channel_message(&fx.channel, "user", "owner").unwrap();
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    let (status, body) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": "501st"}))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "quota_exceeded");
    let (status, _) = call(&fx.guest, "POST", &uri, Some(&other), Some(json!({"text": "hi"}))).await;
    assert_eq!(status, StatusCode::CREATED, "the cap is per guest");
}

#[tokio::test]
async fn fr_t14_a_guest_sees_the_history_from_before_it_joined() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    fx.store.post_channel_message(&fx.channel, "user", "before you").unwrap();
    fx.store.post_channel_message(&fx.channel, &fx.session_id, "me too").unwrap();
    let (_, cred, _) = join(&fx, "Mina").await;
    let (_, log) = call(&fx.guest, "GET", &format!("/api/guest/channels/{}/messages", fx.channel), Some(&cred), None).await;
    let texts: Vec<&str> = log.as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["before you", "me too"]);
}

#[tokio::test]
async fn fr_t14_other_guests_show_their_name_and_never_their_id() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (a_id, a_cred, _) = join(&fx, "Mina").await;
    let (b_id, b_cred, _) = join(&fx, "Joon").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    call(&fx.guest, "POST", &uri, Some(&a_cred), Some(json!({"text": "from mina"}))).await;
    call(&fx.guest, "POST", &uri, Some(&b_cred), Some(json!({"text": "from joon"}))).await;
    fx.store.post_channel_message(&fx.channel, "user", "from owner").unwrap();

    let (_, seen_by_b) = call(&fx.guest, "GET", &uri, Some(&b_cred), None).await;
    let rows = seen_by_b.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0]["author"].as_str(), rows[0]["name"].as_str(), rows[0]["you"].as_bool()), (Some("guest"), Some("Mina"), Some(false)));
    assert_eq!((rows[1]["author"].as_str(), rows[1]["name"].as_str(), rows[1]["you"].as_bool()), (Some("guest"), Some("Joon"), Some(true)));
    assert_eq!(rows[2]["you"], false);
    let text = seen_by_b.to_string();
    assert!(!text.contains("guest:") && !text.contains(&a_id) && !text.contains(&b_id), "{text}");

    let (_, seen_by_a) = call(&fx.guest, "GET", &uri, Some(&a_cred), None).await;
    assert_eq!(seen_by_a[0]["you"], true);
    assert_eq!(seen_by_a[1]["you"], false);
    assert!(!seen_by_a.to_string().contains(&b_id));
}

#[tokio::test]
async fn fr_t14_the_guest_router_merged_under_auth_protect_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    // The mistake this pins: serving the guest router behind the owner door. Without the server
    // token, /api/guest/accept is 401 there, because /api/guest/* is not exempt.
    let wrong = auth::protect(fx.guest.clone(), ServerAuth::new(SERVER_TOKEN.into(), "127.0.0.1:8740".parse().unwrap(), []));
    let (_, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    let (status, _) = call(&wrong, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": "Mina"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // And on the real owner app, with no token, the same path is 401 (not 404 or 201).
    let owner = owner_app(&fx);
    let (status, _) = call(&owner, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": "Mina"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(fx.store.invite_by_token(&token).unwrap().is_some(), "the invite was not used");
}

#[test]
fn fr_t14_new_secret_has_244_bits_and_the_comment_says_so() {
    let s = ember_server::channels::invites::new_secret("dpg_");
    assert_eq!(s.len(), 4 + 64);
    let src = include_str!("../src/channels/invites.rs");
    assert!(src.contains("244 random bits") && !src.contains("256 random bits"));
}
