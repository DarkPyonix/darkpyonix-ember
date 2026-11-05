//! FR-T14: channel invites, the store and the owner routes. Details are [provisional].

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app(store: &Arc<Store>) -> Router {
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    ember_server::channels::api::router(store.clone(), sessions)
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
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

/// A file-backed store (so a test can read the tables directly) with one default channel.
fn fixture(dir: &Path) -> (Arc<Store>, String, Router, std::path::PathBuf) {
    let path = dir.join("ember.db");
    let store = Arc::new(Store::open(&path).unwrap());
    store.create_session("core", AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
    let channel = store.channels(None).unwrap().remove(0).id;
    let app = app(&store);
    (store, channel, app, path)
}

#[tokio::test]
async fn fr_t14_the_token_is_returned_once_and_only_its_hash_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let (_, ch, app, path) = fixture(dir.path());
    let (status, made) = call(&app, "POST", &format!("/api/channels/{ch}/invites"), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let token = made["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("dpi_") && token.len() >= 64);
    assert_eq!(made["link"], format!("/join#{token}"));
    assert!(made["id"].is_string() && made["expires_at"].is_i64());

    let db = rusqlite::Connection::open(&path).unwrap();
    let (hash, cols): (String, String) = db
        .query_row("SELECT token_hash, id || channel_id || created_by FROM channel_invites", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(hash.len(), 64, "SHA-256 hex");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(hash, token);
    assert!(!cols.contains(&token));
    // No cell of the table holds the plaintext.
    let all: Vec<String> = db
        .prepare("SELECT token_hash || id || created_by || channel_id FROM channel_invites")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(all.iter().all(|s| !s.contains(&token)));
}

#[tokio::test]
async fn fr_t14_ttl_defaults_to_24_hours_and_out_of_range_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let (_, ch, app, _) = fixture(dir.path());
    let uri = format!("/api/channels/{ch}/invites");
    let (_, made) = call(&app, "POST", &uri, None).await;
    let (_, list) = call(&app, "GET", &uri, None).await;
    let created = list[0]["created_at"].as_i64().unwrap();
    assert_eq!(made["expires_at"].as_i64().unwrap() - created, 24 * 3_600_000);

    let (_, made) = call(&app, "POST", &uri, Some(json!({"ttl_hours": 168}))).await;
    assert!(made["token"].is_string());
    for bad in [json!(0), json!(169), json!(-1), json!(1.5)] {
        let (status, _) = call(&app, "POST", &uri, Some(json!({"ttl_hours": bad}))).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {status}");
    }
    let (status, _) = call(&app, "POST", &uri, Some(json!({"ttl_hours": 0}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, "POST", &uri, Some(json!({"ttl_hours": 169}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn fr_t14_listing_never_shows_the_token_or_its_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (_, ch, app, _) = fixture(dir.path());
    let uri = format!("/api/channels/{ch}/invites");
    let (_, made) = call(&app, "POST", &uri, None).await;
    let (status, list) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let text = list.to_string();
    assert!(!text.contains(made["token"].as_str().unwrap()));
    assert!(!text.contains("token") && !text.contains("hash"));
    assert_eq!(list[0]["id"], made["id"]);
    assert!(list[0]["used_at"].is_null() && list[0]["revoked_at"].is_null());
}

#[tokio::test]
async fn fr_t14_revoking_sets_revoked_at_and_the_token_no_longer_finds_an_invite() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ch, app, _) = fixture(dir.path());
    let uri = format!("/api/channels/{ch}/invites");
    let (_, made) = call(&app, "POST", &uri, None).await;
    let token = made["token"].as_str().unwrap();
    assert!(store.invite_by_token(token).unwrap().is_some());
    let (status, _) = call(&app, "DELETE", &format!("{uri}/{}", made["id"].as_str().unwrap()), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(store.invite_by_token(token).unwrap().is_none());
    let (_, list) = call(&app, "GET", &uri, None).await;
    assert!(list[0]["revoked_at"].is_i64());
    let (status, _) = call(&app, "DELETE", &format!("{uri}/nope"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_t14_revoking_an_invite_revokes_the_guest_credential_made_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ch, _, path) = fixture(dir.path());
    let (invite, _) = store.create_invite(&ch, "user", None).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "INSERT INTO channel_guests (id, channel_id, invite_id, name, credential_hash, created_at, expires_at, allow_until)
         VALUES ('g1', ?1, ?2, 'Mina', 'h', 1, 2, 99)",
        rusqlite::params![ch, invite.id],
    )
    .unwrap();
    store.revoke_invite(&ch, &invite.id).unwrap();
    let (revoked, allow): (Option<i64>, Option<i64>) =
        db.query_row("SELECT revoked_at, allow_until FROM channel_guests WHERE id = 'g1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(revoked.is_some());
    assert!(allow.is_none());
    assert_eq!(store.list_invites(&ch).unwrap()[0].guest_name.as_deref(), Some("Mina"));
}

#[tokio::test]
async fn fr_t14_an_expired_or_used_invite_is_not_found_by_its_token() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ch, _, path) = fixture(dir.path());
    let (invite, token) = store.create_invite(&ch, "user", Some(1)).unwrap();
    assert!(store.invite_by_token_at(&token, invite.expires_at - 1).unwrap().is_some());
    assert!(store.invite_by_token_at(&token, invite.expires_at).unwrap().is_none());
    assert!(store.invite_by_token("dpi_unknown").unwrap().is_none());
    let (_, token2) = store.create_invite(&ch, "user", None).unwrap();
    rusqlite::Connection::open(&path).unwrap().execute("UPDATE channel_invites SET used_at = 5", []).unwrap();
    assert!(store.invite_by_token(&token2).unwrap().is_none());
}

#[tokio::test]
async fn fr_t14_an_unknown_channel_is_404_on_every_invite_route() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, app, _) = fixture(dir.path());
    for (m, uri) in [("POST", "/api/channels/nope/invites"), ("GET", "/api/channels/nope/invites"), ("DELETE", "/api/channels/nope/invites/x")] {
        let (status, _) = call(&app, m, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{m} {uri}");
    }
}

#[tokio::test]
async fn fr_t14_the_migration_applies_to_an_old_database() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, _, path) = fixture(dir.path());
    let latest = store.schema_version().unwrap();
    drop(store);
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE channel_guests; DROP TABLE channel_invites;").unwrap();
        db.pragma_update(None, "user_version", latest - 1).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), latest);
    let ch = store.channels(None).unwrap().remove(0).id;
    assert!(store.create_invite(&ch, "user", None).is_ok());
    assert!(store.list_invites(&ch).unwrap().len() == 1);
}
