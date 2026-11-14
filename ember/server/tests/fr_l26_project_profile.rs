//! Per-project profile override (SPEC FR-L26 and FR-L20, #384 PR 3): `profile_overrides` on
//! `PATCH /api/projects/{name}`, its storage, and its use when a member session is created.
//! Written before the code.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::members::{create_member_session, MemberDef};
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
    app: axum::Router,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let accounts =
        Accounts::with_secrets(store.clone(), &dir.path().join("accounts"), SecretBox::ephemeral()).unwrap();
    let app = ember_server::api::router(sessions.clone());
    Fixture { _dir: dir, store, sessions, accounts, app }
}

fn make_project(f: &Fixture, name: &str) {
    f.store.ensure_project(name).unwrap();
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
    (status, value)
}

async fn patch(f: &Fixture, name: &str, body: Value) -> (StatusCode, Value) {
    call(&f.app, Method::PATCH, &format!("/api/projects/{name}"), Some(body)).await
}

#[tokio::test]
async fn fr_l26_profile_override_roundtrips_and_a_new_project_has_none() {
    let f = fixture();
    make_project(&f, "p");
    let (_, got) = call(&f.app, Method::GET, "/api/projects/p", None).await;
    assert_eq!(got["profile_overrides"], json!({}));

    let claude = f.accounts.create(AgentKind::ClaudeCode, "work").unwrap();
    let codex = f.accounts.create(AgentKind::Codex, "side").unwrap();
    let mut rx = f.sessions.subscribe();
    let (st, p) = patch(&f, "p", json!({ "profile_overrides": { "claude-code": claude.id, "codex": codex.id } })).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["profile_overrides"], json!({ "claude-code": claude.id, "codex": codex.id }));

    let (_, got) = call(&f.app, Method::GET, "/api/projects/p", None).await;
    assert_eq!(got, p, "GET returns the same");
    let (_, listed) = call(&f.app, Method::GET, "/api/projects", None).await;
    assert_eq!(listed[0], p, "the list returns it too");

    let mut pushed = false;
    while let Ok(push) = rx.try_recv() {
        if let Push::ProjectUpdated { project } = push {
            pushed |= project.name == "p" && serde_json::to_value(&project).unwrap()["profile_overrides"] == p["profile_overrides"];
        }
    }
    assert!(pushed, "project_updated carries profile_overrides");
}

#[tokio::test]
async fn fr_l26_profile_override_patch_replaces_the_whole_map_and_null_removes() {
    let f = fixture();
    make_project(&f, "p");
    let claude = f.accounts.create(AgentKind::ClaudeCode, "work").unwrap();
    let codex = f.accounts.create(AgentKind::Codex, "side").unwrap();
    patch(&f, "p", json!({ "profile_overrides": { "claude-code": claude.id, "codex": codex.id } })).await;

    // The map replaces the stored one: codex is gone because it is not mentioned.
    let (_, p) = patch(&f, "p", json!({ "profile_overrides": { "claude-code": claude.id } })).await;
    assert_eq!(p["profile_overrides"], json!({ "claude-code": claude.id }));

    // null for a harness stores nothing for it.
    let (st, p) = patch(&f, "p", json!({ "profile_overrides": { "claude-code": null, "codex": codex.id } })).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["profile_overrides"], json!({ "codex": codex.id }));

    // An empty object clears all.
    let (_, p) = patch(&f, "p", json!({ "profile_overrides": {} })).await;
    assert_eq!(p["profile_overrides"], json!({}));

    // A patch without the field leaves it alone.
    patch(&f, "p", json!({ "profile_overrides": { "codex": codex.id } })).await;
    let (_, p) = patch(&f, "p", json!({ "repository": "r" })).await;
    assert_eq!(p["profile_overrides"], json!({ "codex": codex.id }));
}

#[tokio::test]
async fn fr_l26_profile_override_is_validated() {
    let f = fixture();
    make_project(&f, "p");
    let claude = f.accounts.create(AgentKind::ClaudeCode, "work").unwrap();
    let codex = f.accounts.create(AgentKind::Codex, "side").unwrap();

    let (st, body) = patch(&f, "p", json!({ "profile_overrides": { "no-such-harness": claude.id } })).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_field")), "{body}");
    assert!(body["error"].as_str().unwrap().contains("no-such-harness"), "{body}");

    let (st, body) = patch(&f, "p", json!({ "profile_overrides": { "claude-code": "missing-id" } })).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_field")), "{body}");
    assert!(body["error"].as_str().unwrap().contains("missing-id"), "the reason names the account: {body}");

    // An account of another harness.
    let (st, body) = patch(&f, "p", json!({ "profile_overrides": { "claude-code": codex.id } })).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_field")), "{body}");
    assert!(body["error"].as_str().unwrap().contains(&codex.id), "{body}");

    // Wrong JSON types.
    for bad in [json!(null), json!([]), json!("x"), json!({ "codex": 5 }), json!({ "codex": "" })] {
        let (st, body) = patch(&f, "p", json!({ "profile_overrides": bad })).await;
        assert_eq!((st, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_field")), "{bad}: {body}");
    }

    // Nothing was stored, and a bad entry rolls back the rest of the patch.
    let (st, _) = patch(&f, "p", json!({ "repository": "r", "profile_overrides": { "codex": "missing-id" } })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let p = f.store.project("p").unwrap().unwrap();
    assert!(p.profile_overrides.is_empty() && p.repository.is_none());
}

#[tokio::test]
async fn fr_l26_leader_stays_not_yet() {
    let f = fixture();
    make_project(&f, "p");
    for body in [json!({ "leader_session": null })] {
        let (st, resp) = patch(&f, "p", body.clone()).await;
        assert_eq!((st, resp["code"].as_str()), (StatusCode::BAD_REQUEST, Some("not_yet")), "{body}");
    }
}

#[tokio::test]
async fn fr_l26_rename_keeps_profile_overrides() {
    let f = fixture();
    make_project(&f, "old");
    let claude = f.accounts.create(AgentKind::ClaudeCode, "work").unwrap();
    patch(&f, "old", json!({ "profile_overrides": { "claude-code": claude.id } })).await;
    let (st, p) = patch(&f, "old", json!({ "name": "fresh" })).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["profile_overrides"], json!({ "claude-code": claude.id }));
    let (_, got) = call(&f.app, Method::GET, "/api/projects/fresh", None).await;
    assert_eq!(got["profile_overrides"], json!({ "claude-code": claude.id }));
}

#[tokio::test]
async fn fr_l26_deleted_account_is_reported_as_null_not_dangling() {
    let f = fixture();
    make_project(&f, "p");
    let claude = f.accounts.create(AgentKind::ClaudeCode, "work").unwrap();
    let codex = f.accounts.create(AgentKind::Codex, "side").unwrap();
    patch(&f, "p", json!({ "profile_overrides": { "claude-code": claude.id, "codex": codex.id } })).await;
    f.accounts.delete(&claude.id).unwrap();

    let (_, got) = call(&f.app, Method::GET, "/api/projects/p", None).await;
    assert_eq!(got["profile_overrides"], json!({ "claude-code": null, "codex": codex.id }));
    let (_, listed) = call(&f.app, Method::GET, "/api/projects", None).await;
    assert_eq!(listed[0]["profile_overrides"], got["profile_overrides"]);

    // The override is not used: the harness falls back to the router.
    assert_eq!(f.store.project_account_override("p", AgentKind::ClaudeCode).unwrap(), None);
    assert_eq!(f.store.project_account_override("p", AgentKind::Codex).unwrap().as_deref(), Some(codex.id.as_str()));
}

fn new_session(project: &str) -> impl FnOnce(AgentKind) -> NewSession + '_ {
    move |agent| NewSession {
        project: project.into(),
        agent,
        cwd: std::env::temp_dir(),
        model: None,
        title: "m".into(),
    }
}

fn pipeline_def() -> MemberDef {
    MemberDef { harness: "codex".into(), account: None, pipeline: true, proxy: None }
}

/// The scripted adapter stands in for no real harness here, so the sessions use a fake codex
/// adapter registered through `Sessions::new`.
fn codex_fixture() -> Fixture {
    struct FakeCodex;
    #[async_trait::async_trait]
    impl AgentAdapter for FakeCodex {
        fn kind(&self) -> AgentKind {
            AgentKind::Codex
        }
        async fn detect(&self) -> ember_server::agents::Detected {
            ScriptedAdapter.detect().await
        }
        async fn start(
            &self,
            req: ember_server::agents::StartRequest,
            events: tokio::sync::mpsc::Sender<ember_server::events::AgentEvent>,
        ) -> anyhow::Result<Box<dyn ember_server::agents::AgentRun>> {
            ScriptedAdapter.start(req, events).await
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(FakeCodex) as Arc<dyn AgentAdapter>]);
    let accounts =
        Accounts::with_secrets(store.clone(), &dir.path().join("accounts"), SecretBox::ephemeral()).unwrap();
    accounts.install(&sessions);
    let app = ember_server::api::router(sessions.clone());
    Fixture { _dir: dir, store, sessions, accounts, app }
}

#[tokio::test]
async fn fr_l26_override_decides_the_account_of_a_pipeline_member() {
    let f = codex_fixture();
    make_project(&f, "acme");
    let a = f.accounts.create(AgentKind::Codex, "a").unwrap();
    let b = f.accounts.create(AgentKind::Codex, "b").unwrap();

    // Without an override the router picks (a: oldest, equal use).
    let rec = create_member_session(&f.sessions, &pipeline_def(), new_session("acme")).unwrap();
    assert_eq!(rec.account_id.as_deref(), Some(a.id.as_str()));

    patch(&f, "acme", json!({ "profile_overrides": { "codex": b.id } })).await;
    let rec = create_member_session(&f.sessions, &pipeline_def(), new_session("acme")).unwrap();
    assert_eq!(rec.account_id.as_deref(), Some(b.id.as_str()), "the project's override wins over the router");

    // Another project is not affected.
    make_project(&f, "other");
    let rec = create_member_session(&f.sessions, &pipeline_def(), new_session("other")).unwrap();
    assert_eq!(rec.account_id.as_deref(), Some(a.id.as_str()));

    // An explicit account in the member definition beats the override.
    let fixed = MemberDef { harness: "codex".into(), account: Some(a.id.clone()), pipeline: false, proxy: None };
    let rec = create_member_session(&f.sessions, &fixed, new_session("acme")).unwrap();
    assert_eq!(rec.account_id.as_deref(), Some(a.id.as_str()));

    // Existing sessions never change account.
    assert_eq!(f.sessions.store().sessions(Some("acme")).unwrap().iter().filter(|s| s.account_id.as_deref() == Some(a.id.as_str())).count(), 2);
}

/// The project routes sit behind the server token like every owner route (FR-S9): no token, a
/// wrong token and a guest credential are all 401 and change nothing.
#[tokio::test]
async fn fr_l26_project_patch_needs_the_server_token() {
    use ember_server::auth::{self, ServerAuth};
    const LISTEN: &str = "127.0.0.1:8740";
    let f = fixture();
    make_project(&f, "p");
    let claude = f.accounts.create(AgentKind::ClaudeCode, "work").unwrap();
    f.store
        .patch_project(
            "p",
            &serde_json::from_value(json!({ "profile_overrides": { "claude-code": claude.id } })).unwrap(),
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let app = auth::protect(f.app.clone(), ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []));

    let send = |bearer: Option<String>| {
        let app = app.clone();
        async move {
            let mut b = Request::builder()
                .method(Method::PATCH)
                .uri("/api/projects/p")
                .header("host", LISTEN)
                .header("content-type", "application/json");
            if let Some(t) = bearer {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            app.oneshot(b.body(Body::from(json!({ "profile_overrides": {} }).to_string())).unwrap())
                .await
                .unwrap()
                .status()
        }
    };
    let guest = ember_server::channels::invites::new_secret("dpg_");
    for bearer in [None, Some("wrong-token".to_string()), Some(guest)] {
        assert_eq!(send(bearer.clone()).await, StatusCode::UNAUTHORIZED, "{bearer:?}");
        let p = f.store.project("p").unwrap().unwrap();
        assert_eq!(p.profile_overrides.len(), 1, "a refused patch changes nothing");
    }
    assert_eq!(send(Some(token)).await, StatusCode::OK);
    assert!(f.store.project("p").unwrap().unwrap().profile_overrides.is_empty());
}
