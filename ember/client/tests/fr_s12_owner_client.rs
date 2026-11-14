//! FR-S12: owner presence on the client (INTENT D37 "Owner presence", #784): the owner key is
//! sent on owner routes only, a 403 `owner_required` is reported once, and a claim over the
//! server's real claim door (`owner.sock` on unix, the named pipe on Windows) yields a key the
//! server accepts.

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, Method, StatusCode, Uri};
use ember_client::api::Api;
use ember_client::owner::{self, OwnerKey};
use ember_client::wire::ApprovalDecision;
use ember_server::owner::{RouteClass, ROUTES};

fn sample(template: &str) -> String {
    template.replace("{*", "").replace(['{', '}'], "")
}

/// The client's list is the server's: every `Owner` row, and nothing else of the server's
/// routes, counts as an owner route here.
#[test]
fn fr_s12_client_owner_routes_match_the_server() {
    let server: Vec<(&str, &str)> =
        ROUTES.iter().filter(|(_, _, c)| *c == RouteClass::Owner).map(|(m, t, _)| (*m, *t)).collect();
    assert_eq!(owner::OWNER_ROUTES, server.as_slice(), "ember/client/src/owner.rs OWNER_ROUTES is out of date");
    for (m, template, class) in ROUTES {
        let method = Method::from_bytes(m.as_bytes()).unwrap();
        let path = sample(template);
        assert_eq!(owner::owner_route(&method, &path), *class == RouteClass::Owner, "{m} {path}");
        assert_eq!(
            owner::owner_route(&method, &path),
            ember_server::auth::owner_route(&method, &path),
            "{m} {path}"
        );
    }
    assert_eq!(owner::OWNER_HEADER, ember_server::owner::OWNER_HEADER);
    assert_eq!(owner::SOCKET_FILE, ember_server::owner::SOCKET_FILE);
    assert_eq!(owner::PIPE_PREFIX, ember_server::owner::PIPE_PREFIX);
}

/// The client finds the server's Windows pipe by the same name the server creates. Run on every
/// platform: the name is only a hash.
#[test]
fn fr_s12_client_pipe_name_matches_the_server() {
    for p in [r"\\?\C:\Users\me\.ember", "/home/me/.ember", "", "D:\\Ember Data\\서버"] {
        assert_eq!(owner::pipe_name_of(p), ember_server::owner::pipe_name_of(p), "{p}");
    }
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(owner::pipe_name(dir.path()), ember_server::owner::pipe_name(dir.path()));
}

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A server that records each path with the owner header it carried and answers `status` with
/// `body`.
async fn recorder(status: StatusCode, body: &'static str) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap| {
        let log = log.clone();
        async move {
            let key = headers.get(owner::OWNER_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string);
            log.lock().unwrap().push((uri.path().to_string(), key));
            (status, [("content-type", "application/json")], body)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn fr_s12_client_sends_the_key_on_owner_routes_only() {
    let (url, seen) = recorder(StatusCode::ACCEPTED, "[]").await;
    let api = Api::with_token(&url, Some("server-token".into())).unwrap();
    let clone = api.clone();

    // No key yet: no header anywhere.
    api.answer("s1", "a1", ApprovalDecision::Deny).await.unwrap();
    assert!(!api.has_owner_key());

    assert!(api.set_owner_key(Some(&OwnerKey::new("eok_abc"))));
    assert!(clone.has_owner_key(), "clones share the key");
    clone.answer("s1", "a1", ApprovalDecision::Deny).await.unwrap();
    api.set_approve_all("s1", true).await.unwrap();
    let _ = api.sessions(None).await;
    api.interrupt("s1").await.unwrap();

    assert!(api.set_owner_key(None));
    api.answer("s1", "a1", ApprovalDecision::Deny).await.unwrap();

    let seen = seen.lock().unwrap().clone();
    let key = Some("eok_abc".to_string());
    assert_eq!(
        seen,
        vec![
            ("/api/sessions/s1/approvals/a1".to_string(), None),
            ("/api/sessions/s1/approvals/a1".to_string(), key.clone()),
            ("/api/sessions/s1/approve-all".to_string(), key),
            ("/api/sessions".to_string(), None),
            ("/api/sessions/s1/interrupt".to_string(), None),
            ("/api/sessions/s1/approvals/a1".to_string(), None),
        ]
    );
    assert!(!format!("{api:?}").contains("eok_abc"), "the key is not in debug output");
}

#[tokio::test]
async fn fr_s12_owner_required_is_reported_and_not_retried() {
    let (url, seen) = recorder(StatusCode::FORBIDDEN, r#"{"code":"owner_required","error":"needs owner presence"}"#).await;
    let api = Api::with_token(&url, Some("server-token".into())).unwrap();
    let err = api.answer("s1", "a1", ApprovalDecision::AllowOnce).await.unwrap_err();
    assert!(err.is_owner_required(), "{err:?}");
    assert!(!err.is_scope_denied());
    assert_eq!(seen.lock().unwrap().len(), 1, "a refused owner call is sent once");

    let (url, _) = recorder(StatusCode::FORBIDDEN, r#"{"code":"scope_denied","error":"scope"}"#).await;
    let api = Api::with_token(&url, None).unwrap();
    let err = api.answer("s1", "a1", ApprovalDecision::AllowOnce).await.unwrap_err();
    assert!(err.is_scope_denied() && !err.is_owner_required(), "{err:?}");
}

/// The key goes only where the server token goes: a loopback or listed host.
#[test]
fn fr_s12_the_key_is_not_kept_for_a_remote_host() {
    let api = Api::with_token("http://192.0.2.1:8740", None).unwrap();
    assert!(!api.set_owner_key(Some(&OwnerKey::new("eok_abc"))));
    assert!(!api.has_owner_key());
    // A value that cannot be a header is refused too.
    let local = Api::with_token("http://127.0.0.1:1", None).unwrap();
    assert!(!local.set_owner_key(Some(&OwnerKey::new("eok_\n"))));
    assert!(!local.has_owner_key());
}

/// Against the real server layers and the real claim door (the socket on unix, the pipe on
/// Windows): no key, 403; claim, confirm with the console code, then the same call reaches the
/// handler; drop the link, 403 again.
#[cfg(any(unix, windows))]
#[tokio::test]
async fn fr_s12_client_claims_a_key_the_server_accepts() {
    use ember_client::owner::{OwnerLink, OwnerMessage};
    use ember_server::agents::scripted::ScriptedAdapter;
    use ember_server::agents::AgentAdapter;
    use ember_server::auth::{self, ServerAuth};
    use ember_server::owner::door::{DoorConfig, OwnerDoor, Proc, ProcTable};
    use ember_server::owner::{Claimant, Confirm, Owner, Presence};
    use ember_server::session::Sessions;
    use ember_server::store::Store;

    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_auth = ServerAuth::with_token_file(dir.path(), addr, []).unwrap();
    let token = std::fs::read_to_string(auth::token_path(dir.path())).unwrap().trim().to_string();
    let server_owner = Owner::new(Presence::Enforce);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let app = auth::protect_with_owner(ember_server::api::router(sessions), server_auth, server_owner.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let codes: Arc<Mutex<Vec<String>>> = Arc::default();
    let shown = codes.clone();
    let me = std::process::id();
    let cfg = DoorConfig {
        confirm: Confirm::Strict,
        roots: Arc::new(Vec::new),
        procs: Arc::new(move || {
            let mut t = ProcTable::new();
            t.insert(me, Proc { ppid: 1, exe: "/test/ember".into() });
            t
        }),
        console: Arc::new(move |code: &str, _who: &Claimant| shown.lock().unwrap().push(code.to_string())),
        confirm_timeout: std::time::Duration::from_secs(5),
    };
    let _door = OwnerDoor::bind(dir.path(), server_owner.clone(), cfg).unwrap();

    let api = Api::with_token(&format!("http://{addr}"), Some(token)).unwrap();
    // The server token alone: refused, and reported as owner_required.
    let err = api.set_approve_all("nope", true).await.unwrap_err();
    assert!(err.is_owner_required(), "{err:?}");
    // A normal route works with the token alone.
    assert!(api.sessions(None).await.unwrap().is_empty());

    let endpoint = owner::OwnerEndpoint::for_dir(Some(dir.path()), None).unwrap();
    #[cfg(unix)]
    assert_eq!(endpoint, owner::OwnerEndpoint::Socket(ember_server::owner::door::socket_path(dir.path())));
    #[cfg(windows)]
    assert_eq!(endpoint, owner::OwnerEndpoint::Pipe(ember_server::owner::pipe_name(dir.path())));
    let mut link = OwnerLink::claim(&endpoint, "Ember").await.unwrap();
    let Some(OwnerMessage::Confirm { holders: 0, .. }) = link.next().await.unwrap() else {
        panic!("the first claim must ask for the console code");
    };
    link.confirm("WRONG-CODE").await.unwrap();
    assert_eq!(link.next().await.unwrap(), Some(OwnerMessage::WrongCode { left: 2 }));
    let code = codes.lock().unwrap().last().cloned().unwrap();
    link.confirm(&code).await.unwrap();
    let Some(OwnerMessage::Granted { key, .. }) = link.next().await.unwrap() else {
        panic!("the right code must grant the key");
    };
    assert!(api.set_owner_key(Some(&key)));

    // The owner layer lets the call through: the handler answers that the session is unknown.
    let err = api.set_approve_all("nope", true).await.unwrap_err();
    assert!(!err.is_owner_required(), "{err:?}");
    assert!(matches!(err, ember_client::api::ApiError::Status { status: 404, .. }), "{err:?}");

    // The key dies with the link.
    drop(link);
    for _ in 0..200 {
        if server_owner.holders().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(server_owner.holders().is_empty(), "the server did not void the key");
    let err = api.set_approve_all("nope", true).await.unwrap_err();
    assert!(err.is_owner_required(), "{err:?}");
}
