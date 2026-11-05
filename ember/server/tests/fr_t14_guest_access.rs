//! FR-T14, FR-T15: accepting an invite and the guest routes. Details are [provisional].
//! The guest router is built directly (no listener serves it yet); the owner app is wrapped with
//! `auth::protect` to prove a guest credential is not a server token.

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
async fn fr_t14_accept_issues_a_credential_once_and_only_its_hash_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (invite, token) = fx.store.create_invite(&fx.channel, "user", Some(2)).unwrap();
    let (status, made) =
        call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": "  Mina  "}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let cred = made["credential"].as_str().unwrap();
    assert!(cred.starts_with("dpg_") && cred.len() >= 64);
    assert_eq!(made["channel_id"], fx.channel.as_str());
    assert!(made["guest_id"].is_string());
    assert_eq!(made["expires_at"].as_i64().unwrap(), invite.expires_at, "the credential follows the invite's expiry");

    let db = rusqlite::Connection::open(&fx.path).unwrap();
    let (hash, name, expires): (String, String, i64) = db
        .query_row("SELECT credential_hash, name, expires_at FROM channel_guests", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(hash, cred);
    assert_eq!(name, "Mina", "the name is trimmed");
    assert_eq!(expires, invite.expires_at);
    let row: String = db
        .query_row("SELECT id || channel_id || invite_id || name || credential_hash FROM channel_guests", [], |r| r.get(0))
        .unwrap();
    assert!(!row.contains(cred));
    let used: Option<i64> = db.query_row("SELECT used_at FROM channel_invites", [], |r| r.get(0)).unwrap();
    assert!(used.is_some());
}

#[tokio::test]
async fn fr_t14_a_used_invite_cannot_be_accepted_twice() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    let body = json!({"token": token, "name": "A"});
    assert_eq!(call(&fx.guest, "POST", "/api/guest/accept", None, Some(body.clone())).await.0, StatusCode::CREATED);
    let (status, again) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(body)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(again["code"], "not_found");
    let db = rusqlite::Connection::open(&fx.path).unwrap();
    let n: i64 = db.query_row("SELECT COUNT(*) FROM channel_guests", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn fr_t14_an_expired_or_revoked_or_unknown_invite_cannot_be_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (invite, token) = fx.store.create_invite(&fx.channel, "user", Some(1)).unwrap();
    assert!(fx.store.accept_invite_at(&token, "A", invite.expires_at).is_err());
    assert!(fx.store.accept_invite_at(&token, "A", invite.expires_at + 1).is_err());

    let (invite2, token2) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    fx.store.revoke_invite(&fx.channel, &invite2.id).unwrap();
    let (status, _) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token2, "name": "A"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    for t in ["dpi_unknown", "", "dpg_x"] {
        let (status, _) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": t, "name": "A"}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{t:?}");
    }
    let db = rusqlite::Connection::open(&fx.path).unwrap();
    let n: i64 = db.query_row("SELECT COUNT(*) FROM channel_guests", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn fr_t14_a_bad_name_is_400_and_does_not_burn_the_invite() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    for bad in ["", "   ", &"x".repeat(65), "bad\nname"] {
        let (status, _) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": bad}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    let ok = "y".repeat(64);
    let (status, _) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": ok}))).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn fr_t14_revoking_the_invite_makes_the_credential_401_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, invite_id) = join(&fx, "Mina").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    assert_eq!(call(&fx.guest, "GET", &uri, Some(&cred), None).await.0, StatusCode::OK);
    fx.store.revoke_invite(&fx.channel, &invite_id).unwrap();
    for (m, body) in [("GET", None), ("POST", Some(json!({"text": "hi"})))] {
        let (status, _) = call(&fx.guest, m, &uri, Some(&cred), body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{m}");
    }
}

#[tokio::test]
async fn fr_t14_an_expired_credential_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (invite, token) = fx.store.create_invite(&fx.channel, "user", Some(1)).unwrap();
    let (_, cred) = fx.store.accept_invite(&token, "A").unwrap();
    assert!(fx.store.guest_by_credential_at(&cred, invite.expires_at - 1).is_some());
    assert!(fx.store.guest_by_credential_at(&cred, invite.expires_at).is_none());
    assert!(fx.store.guest_by_credential("dpg_nope").is_none());
    assert!(fx.store.guest_by_credential(SERVER_TOKEN).is_none());
}

#[tokio::test]
async fn fr_t14_a_wrong_or_missing_credential_is_401() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    let sum = format!("/api/guest/channels/{}", fx.channel);
    let flipped = if cred.ends_with('a') { 'b' } else { 'a' };
    let tampered = format!("{}{flipped}", &cred[..cred.len() - 1]);
    for u in [&uri, &sum] {
        for bearer in [None, Some("dpg_wrong"), Some(""), Some(tampered.as_str()), Some("Basic x")] {
            let (status, body) = call(&fx.guest, "GET", u, bearer, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{u} {bearer:?}");
            assert_eq!(body["code"], "unauthorized");
            assert!(body["error"].is_string());
        }
    }
    // A raw Authorization header that is not "Bearer <x>".
    let req = Request::builder().uri(&uri).header("authorization", cred.as_str()).body(Body::empty()).unwrap();
    assert_eq!(fx.guest.clone().oneshot(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let (status, _) = call(&fx.guest, "POST", &uri, None, Some(json!({"text": "hi"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_t14_the_server_token_is_401_on_guest_routes() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    // The token that opens the owner app does not open the guest routes.
    let owner = owner_app(&fx);
    assert_eq!(call(&owner, "GET", "/api/channels", Some(SERVER_TOKEN), None).await.0, StatusCode::OK);
    for (m, u, body) in [
        ("GET", uri.clone(), None),
        ("POST", uri.clone(), Some(json!({"text": "hi"}))),
        ("GET", format!("/api/guest/channels/{}", fx.channel), None),
    ] {
        let (status, _) = call(&fx.guest, m, &u, Some(SERVER_TOKEN), body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{m} {u}");
    }
    assert_eq!(call(&fx.guest, "GET", &uri, Some(SERVER_TOKEN), None).await.1["code"], "unauthorized");
}

#[tokio::test]
async fn fr_t14_a_guest_credential_is_401_on_the_owner_door() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let owner = owner_app(&fx);
    let ch = &fx.channel;
    let sid = &fx.session_id;
    for (m, u) in [
        ("GET", "/api/channels".to_string()),
        ("GET", format!("/api/channels/{ch}")),
        ("GET", format!("/api/channels/{ch}/messages")),
        ("POST", format!("/api/channels/{ch}/messages")),
        ("GET", format!("/api/channels/{ch}/feed")),
        ("POST", format!("/api/channels/{ch}/invites")),
        ("GET", "/api/sessions".to_string()),
        ("GET", format!("/api/sessions/{sid}")),
        ("GET", "/api/projects".to_string()),
        ("GET", "/api/computers".to_string()),
    ] {
        let (status, _) = call(&owner, m, &u, Some(&cred), Some(json!({"text": "hi"}))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{m} {u}");
    }
    // The guest family is exempt from the server-token check, but the owner app has no such route.
    let (status, _) = call(&owner, "GET", &format!("/api/guest/channels/{ch}/messages"), Some(&cred), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the owner app does not carry the guest routes");
}

#[tokio::test]
async fn fr_t14_a_guest_reads_and_posts_in_its_own_channel_only() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let other = fx
        .store
        .create_channel(ember_server::channels::NewChannel { project: "core".into(), name: "other".into(), agents: vec![] })
        .unwrap()
        .id;
    for (m, u, body) in [
        ("GET", format!("/api/guest/channels/{other}"), None),
        ("GET", format!("/api/guest/channels/{other}/messages"), None),
        ("POST", format!("/api/guest/channels/{other}/messages"), Some(json!({"text": "hi"}))),
        ("GET", "/api/guest/channels/nope/messages".to_string(), None),
    ] {
        let (status, _) = call(&fx.guest, m, &u, Some(&cred), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{m} {u}");
    }
    let n = fx.store.read_channel_messages(&other, 0).unwrap().len();
    assert_eq!(n, 0, "nothing was written to the other channel");
}

#[tokio::test]
async fn fr_t15_a_guest_post_is_attributed_to_the_guest_whatever_the_body_says() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (gid, cred, _) = join(&fx, "Mina").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    let (status, m) = call(
        &fx.guest,
        "POST",
        &uri,
        Some(&cred),
        Some(json!({"text": "hello", "from": "user", "author": "user", "guest_id": "other"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{m}");
    assert_eq!(m["author"], format!("guest:{gid}"));
    assert_eq!(m["name"], "Mina");
    assert_eq!(m["kind"], "guest");
    // The log (owner's view) holds the same author.
    let log = fx.store.read_channel_messages(&fx.channel, 0).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].from, format!("guest:{gid}"));
    // The guest reads it back.
    let (_, list) = call(&fx.guest, "GET", &uri, Some(&cred), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["author"], format!("guest:{gid}"));
    assert_eq!(list[0]["text"], "hello");
    let (_, after) = call(&fx.guest, "GET", &format!("{uri}?after={}", log[0].seq), Some(&cred), None).await;
    assert_eq!(after.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn fr_t15_a_guest_mention_is_gated_stored_and_delivered_to_no_one() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    let (status, bad) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": "@nobody hi"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["code"], "not_a_member");
    assert!(fx.store.read_channel_messages(&fx.channel, 0).unwrap().is_empty());
    let (status, ok) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": "@alpha hi"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{ok}");
    assert_eq!(ok["mentions"], json!(["alpha"]));
    assert!(!ok.to_string().contains(&fx.session_id));
    // Delivery to agents is not part of the guest route: the only rows are the guest's own.
    let log = fx.store.read_channel_messages(&fx.channel, 0).unwrap();
    assert_eq!(log.len(), 1, "no system or agent message was added");
}

#[tokio::test]
async fn fr_t14_a_guest_cannot_reach_the_feed_or_answer_an_approval() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let ch = &fx.channel;
    for (m, u) in [
        ("GET", format!("/api/guest/channels/{ch}/feed")),
        ("POST", format!("/api/guest/channels/{ch}/feed/a1/answer")),
        ("GET", format!("/api/channels/{ch}/feed")),
        ("POST", format!("/api/channels/{ch}/feed/a1/answer")),
        ("GET", "/api/sessions".to_string()),
        ("POST", format!("/api/guest/channels/{ch}/invites")),
        ("POST", format!("/api/guest/channels/{ch}/read")),
        ("POST", format!("/api/guest/channels/{ch}/mentions/preview")),
    ] {
        let (status, _) = call(&fx.guest, m, &u, Some(&cred), Some(json!({"decision": "allow_once"}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{m} {u}");
    }
    // The guest cannot edit or delete messages either.
    let uri = format!("/api/guest/channels/{ch}/messages");
    for m in ["PUT", "PATCH", "DELETE"] {
        let (status, _) = call(&fx.guest, m, &uri, Some(&cred), Some(json!({"text": "x"}))).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{m}");
    }
}

#[tokio::test]
async fn fr_t14_the_channel_summary_and_log_carry_names_and_no_session_ids() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (gid, cred, _) = join(&fx, "Mina").await;
    fx.store.post_channel_message(&fx.channel, "user", "@alpha status?").unwrap();
    fx.store.post_channel_message(&fx.channel, &fx.session_id, "all good").unwrap();
    fx.store.post_channel_system_message(&fx.channel, "@alpha not reached: x").unwrap();

    let (status, sum) = call(&fx.guest, "GET", &format!("/api/guest/channels/{}", fx.channel), Some(&cred), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = sum["members"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"alpha") && names.contains(&"나"));
    assert_eq!(sum["you"]["id"], gid.as_str());
    let (_, log) = call(&fx.guest, "GET", &format!("/api/guest/channels/{}/messages", fx.channel), Some(&cred), None).await;
    assert_eq!(log.as_array().unwrap().len(), 3);
    assert_eq!(log[1]["kind"], "agent");
    assert_eq!(log[1]["name"], "alpha");
    assert_eq!(log[0]["mentions"], json!(["alpha"]));
    for text in [sum.to_string(), log.to_string()] {
        assert!(!text.contains(&fx.session_id), "a session id leaked: {text}");
        assert!(!text.contains("/secret/cwd") && !text.contains("cwd") && !text.contains("harness"));
        assert!(!text.contains("scripted"));
    }
}

#[tokio::test]
async fn fr_t14_empty_or_oversize_text_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred, _) = join(&fx, "Mina").await;
    let uri = format!("/api/guest/channels/{}/messages", fx.channel);
    for text in ["".to_string(), "   \n".to_string(), "x".repeat(8193)] {
        let (status, _) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": text}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "len {}", text.len());
    }
    let (status, _) = call(&fx.guest, "POST", &uri, Some(&cred), Some(json!({"text": "x".repeat(8192)}))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(fx.store.read_channel_messages(&fx.channel, 0).unwrap().len(), 1);
}
