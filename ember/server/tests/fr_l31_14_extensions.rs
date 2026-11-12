//! FR-L31.14 (#446): the extension gallery search the server proxies, and the uninstall route.
//!
//! The gallery is a loopback axum server that records what it was asked and answers what the test
//! set; `tests/fixtures/open_vsx/search.json` is an answer recorded from
//! `https://open-vsx.org/api/-/search?query=rust-analyzer&size=2` on 2026-11-13. The OSE is a fake
//! `dpx-ose-server` (a shell script that records its argv), as in `fr_w4_ose_data_dir.rs`. No
//! real gallery and no real OSE is contacted.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use ember_server::api::ide_extensions::{
    self, parse_search, search_query, Gallery, SEARCH_BODY_CAP, SEARCH_OFFSET_MAX, SEARCH_PAGE, SEARCH_QUERY_MAX,
};
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::{self, Computers, Registry};
use ember_server::ose::{InstallError, OseManager};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";
const RECORDED: &str = include_str!("fixtures/open_vsx/search.json");

/// What the fake gallery answers next, and what it was asked.
#[derive(Clone)]
struct Upstream {
    answer: Arc<Mutex<(u16, Vec<(String, String)>, Vec<u8>)>>,
    seen: Arc<Mutex<Vec<(String, HeaderMap)>>>,
}

async fn upstream_search(State(u): State<Upstream>, RawQuery(query): RawQuery, headers: HeaderMap) -> Response {
    u.seen.lock().unwrap().push((query.unwrap_or_default(), headers));
    let (status, extra, body) = u.answer.lock().unwrap().clone();
    let mut res = (StatusCode::from_u16(status).unwrap(), body).into_response();
    for (k, v) in extra {
        res.headers_mut().insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            axum::http::HeaderValue::from_str(&v).unwrap(),
        );
    }
    res
}

struct Fx {
    app: Router,
    token: String,
    upstream: Upstream,
    argv: PathBuf,
    root: PathBuf,
    dir: tempfile::TempDir,
    computers: Arc<Computers>,
}

const RECORD: &str = r#"printf '%s\n' "$@" > "$(dirname "$0")/argv.txt"; echo "ran $$" >> "$(dirname "$0")/runs.txt""#;

/// A router with a fake gallery (answering `RECORDED` until told otherwise) and a fake OSE that
/// runs `script` after recording its argv.
async fn fx(script: &str) -> Fx {
    let upstream = Upstream {
        answer: Arc::new(Mutex::new((200, Vec::new(), RECORDED.as_bytes().to_vec()))),
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let gallery_app = Router::new().route("/api/-/search", get(upstream_search)).with_state(upstream.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, gallery_app).await;
    });

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("fake-ose");
    common::write_exec_ready(&bin, &format!("#!/bin/sh\n{RECORD}\n{script}\n"));
    let root = dir.path().join("ose-data");
    let ose = Arc::new(OseManager::with_data_root(Some(bin), root.clone()).with_install_timeout(Duration::from_millis(5000)));
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let router = ide_extensions::router_with_gallery(computers.clone(), ose, Gallery::with_base(&base));
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []);
    Fx { app: auth::protect(router, a), token, upstream, argv: dir.path().join("argv.txt"), root, dir, computers }
}

fn set_answer(f: &Fx, status: u16, headers: &[(&str, &str)], body: &[u8]) {
    *f.upstream.answer.lock().unwrap() =
        (status, headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body.to_vec());
}

async fn call(f: &Fx, method: Method, uri: &str, authorized: bool) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri).header("host", LISTEN);
    if authorized {
        b = b.header("authorization", format!("Bearer {}", f.token));
    }
    let r = f.app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn search(f: &Fx, query: &str) -> (StatusCode, Value) {
    call(f, Method::GET, &format!("/api/ide/extensions/search?{query}"), true).await
}

async fn uninstall(f: &Fx, computer: &str, id: &str) -> (StatusCode, Value) {
    call(f, Method::DELETE, &format!("/api/computers/{computer}/ide/extensions/{id}"), true).await
}

fn argv(f: &Fx) -> Vec<String> {
    std::fs::read_to_string(&f.argv).unwrap().lines().map(String::from).collect()
}

fn ran(f: &Fx) -> bool {
    f.dir.path().join("runs.txt").exists()
}

fn arg_after<'a>(v: &'a [String], flag: &str) -> &'a str {
    &v[v.iter().position(|a| a == flag).unwrap() + 1]
}

fn exists(p: &Path) -> bool {
    p.exists()
}

// ---- parsing ----

#[test]
fn fr_l31_14_a_recorded_open_vsx_answer_is_rebuilt_into_fixed_fields() {
    let page = parse_search(RECORDED.as_bytes()).unwrap();
    assert_eq!(page.offset, 0);
    assert_eq!(page.total, 566);
    assert_eq!(page.extensions.len(), 2);
    let first = &page.extensions[0];
    assert_eq!(first.id, "rust-lang.rust-analyzer");
    assert_eq!(first.namespace, "rust-lang");
    assert_eq!(first.name, "rust-analyzer");
    assert_eq!(first.display_name, "rust-analyzer");
    assert_eq!(first.description, "Rust language support for Visual Studio Code");
    assert_eq!(first.version, "0.4.3116");
    assert_eq!(first.downloads, 7_713_255);
    assert!(first.verified && !first.deprecated);
    // No URL of the gallery is passed on to the client. A description is the publisher's own text
    // and may name a link (the recorded second row does), so the check is on the gallery's host.
    let out = serde_json::to_string(&page).unwrap();
    assert!(!out.contains("open-vsx.org"), "{out}");
    assert!(!out.contains("files") && !out.contains("icon"));
}

#[test]
fn fr_l31_14_text_is_capped_and_an_id_the_install_route_refuses_is_dropped() {
    let long = "가".repeat(2000);
    let body = json!({
        "offset": 30, "totalSize": 99,
        "extensions": [
            { "namespace": "ok", "name": "one", "version": "1.0.0", "description": long, "displayName": "a\nb\u{0007}c" },
            { "namespace": "--flag", "name": "x", "version": "1.0.0" },
            { "namespace": "a", "name": "b c", "version": "1.0.0" },
            { "namespace": "a/..", "name": "b", "version": "1.0.0" },
            { "namespace": "", "name": "b" },
            { "name": "only-name" },
            "not an object",
            { "namespace": "ok", "name": "two", "downloadCount": -5, "verified": "yes", "version": "9".repeat(500) }
        ]
    });
    let page = parse_search(body.to_string().as_bytes()).unwrap();
    let ids: Vec<&str> = page.extensions.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["ok.one", "ok.two"]);
    assert_eq!((page.offset, page.total), (30, 99));
    assert_eq!(page.extensions[0].description.chars().count(), 400);
    assert_eq!(page.extensions[0].display_name, "a b c", "control characters become spaces");
    let two = &page.extensions[1];
    assert_eq!(two.display_name, "two", "no displayName falls back to the name");
    assert_eq!((two.downloads, two.verified, two.deprecated), (0, false, false));
    assert_eq!(two.version.len(), 64);
}

#[test]
fn fr_l31_14_at_most_one_page_of_results_is_kept_and_other_json_is_an_error() {
    let many: Vec<Value> = (0..100).map(|i| json!({ "namespace": "p", "name": format!("e{i}"), "version": "1.0.0" })).collect();
    let page = parse_search(json!({ "extensions": many }).to_string().as_bytes()).unwrap();
    assert_eq!(page.extensions.len(), SEARCH_PAGE);
    assert_eq!(page.total, SEARCH_PAGE as u64, "no totalSize reads as what was kept");
    for bad in ["", "<html>", "[]", "{}", r#"{"extensions":"x"}"#, r#"{"error":"down"}"#] {
        assert!(parse_search(bad.as_bytes()).is_err(), "{bad:?}");
    }
}

#[test]
fn fr_l31_14_the_query_text_is_trimmed_and_bounded() {
    assert_eq!(search_query("  rust analyzer ").unwrap(), "rust analyzer");
    assert_eq!(search_query("한글 확장").unwrap(), "한글 확장");
    assert!(search_query("").is_err());
    assert!(search_query("   ").is_err());
    assert!(search_query("a\nb").is_err());
    assert!(search_query("a\u{0000}b").is_err());
    assert!(search_query(&"x".repeat(SEARCH_QUERY_MAX)).is_ok());
    assert!(search_query(&"x".repeat(SEARCH_QUERY_MAX + 1)).is_err());
    assert!(search_query(&"가".repeat(SEARCH_QUERY_MAX)).is_ok(), "the bound counts characters, not bytes");
}

// ---- the search route ----

#[tokio::test]
async fn fr_l31_14_search_asks_the_gallery_with_fixed_parameters_and_no_credential() {
    let f = fx("").await;
    let (st, body) = search(&f, "q=rust%20analyzer%26size%3D999&offset=30").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 566);
    assert_eq!(body["extensions"][0]["id"], "rust-lang.rust-analyzer");
    assert_eq!(body["extensions"][1]["id"], "loong-vsx.rust-analyzer");
    assert!(body["extensions"][0].get("files").is_none());

    let seen = f.upstream.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (query, headers) = &seen[0];
    // The client's text is one encoded value: it cannot add or override a parameter.
    assert_eq!(query, "query=rust+analyzer%26size%3D999&size=30&offset=30&sortBy=relevance&sortOrder=desc");
    assert!(headers.get("authorization").is_none(), "the server token never goes to the gallery");
    assert!(headers.get("cookie").is_none());
    assert!(!format!("{headers:?}").contains(&f.token));
}

#[tokio::test]
async fn fr_l31_14_search_refuses_bad_input_without_asking_the_gallery() {
    let f = fx("").await;
    let too_long = format!("q={}", "x".repeat(SEARCH_QUERY_MAX + 1));
    let too_far = format!("q=a&offset={}", SEARCH_OFFSET_MAX + 1);
    for query in [
        "", "q=", "q=%20%20", "offset=3", "q=a&offset=-1", "q=a&offset=x", "q=a%0Ab", too_long.as_str(), too_far.as_str(),
        "q=a&url=http://127.0.0.1/", "q=a&size=500", "q=a&host=evil.example",
    ] {
        let (st, body) = search(&f, query).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{query:?}: {body}");
        assert!(body["error"].is_string(), "{query:?}");
    }
    assert!(f.upstream.seen.lock().unwrap().is_empty(), "a refused request never reaches the gallery");
    let (st, _) = search(&f, &format!("q=a&offset={SEARCH_OFFSET_MAX}")).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn fr_l31_14_a_failing_gallery_is_502_and_a_redirect_is_not_followed() {
    let f = fx("").await;
    set_answer(&f, 500, &[], b"down");
    let (st, body) = search(&f, "q=a").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body["error"].as_str().unwrap().contains("500"));

    // A redirect would send the server to a host the gallery chose: it is an error instead.
    let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("http://{}/api/-/search", elsewhere.local_addr().unwrap());
    set_answer(&f, 302, &[("location", &target)], b"");
    let (st, body) = search(&f, "q=a").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");
    let accepted = tokio::time::timeout(Duration::from_millis(300), elsewhere.accept()).await;
    assert!(accepted.is_err(), "the redirect target must never be dialled");

    set_answer(&f, 200, &[], b"<html>maintenance</html>");
    let (st, _) = search(&f, "q=a").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);

    let huge = vec![b' '; SEARCH_BODY_CAP + 1];
    set_answer(&f, 200, &[], &huge);
    let (st, body) = search(&f, "q=a").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body["error"].as_str().unwrap().contains("too large"));
}

#[tokio::test]
async fn fr_l31_14_an_unreachable_gallery_is_502() {
    let f = fx("").await;
    // A port nothing listens on: bound, read, released.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let ose = Arc::new(OseManager::with_data_root(None, f.dir.path().join("other")));
    let app = ide_extensions::router_with_gallery(computers, ose, Gallery::with_base(&format!("http://127.0.0.1:{port}")));
    let req = Request::builder().uri("/api/ide/extensions/search?q=a").body(Body::empty()).unwrap();
    let r = app.oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("cannot be reached"), "{message}");
    assert!(!message.contains("127.0.0.1"), "the upstream URL is not echoed: {message}");
}

#[tokio::test]
async fn fr_l31_14_search_and_uninstall_need_the_server_token() {
    let f = fx("").await;
    let (st, _) = call(&f, Method::GET, "/api/ide/extensions/search?q=a", false).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(&f, Method::DELETE, "/api/computers/local/ide/extensions/a.b", false).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(f.upstream.seen.lock().unwrap().is_empty());
    assert!(!ran(&f));
}

// ---- the uninstall route ----

#[tokio::test]
async fn fr_l31_14_uninstall_runs_the_ose_cli_with_the_computer_data_dir() {
    let f = fx("echo \"Extension 'redhat.java' was successfully uninstalled!\"").await;
    let (st, body) = uninstall(&f, "local", "redhat.java").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["uninstalled"], true);
    assert_eq!(body["id"], "redhat.java");
    assert!(body["output"].as_str().unwrap().contains("successfully uninstalled"));
    let v = argv(&f);
    let local = f.root.join("local");
    assert_eq!(v[0], "--uninstall-extension");
    assert_eq!(v[1], "redhat.java");
    assert_eq!(arg_after(&v, "--server-data-dir"), local.to_str().unwrap());
    assert_eq!(arg_after(&v, "--extensions-dir"), local.join("extensions").to_str().unwrap());
    assert!(!v.iter().any(|a| a == "--install-extension"));
}

#[tokio::test]
async fn fr_l31_14_uninstall_refuses_bad_ids_and_the_binary_never_runs() {
    let f = fx("").await;
    for id in ["x", "a.b.c", "--force.x", "a.b%401.2.3", "a%20b.c", "..%2Fa.b", "a.b%0A--force", "%24(id).x", "-a.b"] {
        let (st, body) = uninstall(&f, "local", id).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{id:?}: {body}");
    }
    assert!(!ran(&f), "the fake binary must never run for a bad id");
    assert!(!exists(&f.root), "no data dir is created for a bad request");
    let m = OseManager::with_data_root(None, f.dir.path().join("r"));
    assert!(matches!(m.uninstall_extension("../x", "a.b"), Err(InstallError::Bad(_))));
    assert!(matches!(m.uninstall_extension("local", "a.b@1.0.0"), Err(InstallError::Bad(_))));
    assert!(matches!(m.uninstall_extension("local", "a.b"), Err(InstallError::NotInstalled)));
}

#[tokio::test]
async fn fr_l31_14_a_failed_uninstall_is_502_with_the_output() {
    let f = fx("echo \"Extension 'a.b' is not installed.\" >&2; exit 1").await;
    let (st, body) = uninstall(&f, "local", "a.b").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["uninstalled"], false);
    assert_eq!(body["id"], "a.b");
    assert!(body["error"].as_str().unwrap().starts_with("uninstall failed"));
    assert!(body["output"].as_str().unwrap().contains("is not installed"));
}

#[tokio::test]
async fn fr_l31_14_uninstall_on_another_computer_is_400_and_an_unknown_one_404() {
    let f = fx("").await;
    let (st, _) = uninstall(&f, "nope", "a.b").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let c = f.computers.registry().insert("peer", "http://127.0.0.1:1", "tok").unwrap();
    let (st, body) = uninstall(&f, &c.id, "a.b").await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "OSE server is only available on the local computer");
    assert!(!ran(&f));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_l31_14_an_uninstall_during_an_install_is_409() {
    let f = fx(r#"case "$1" in --install-extension) sleep 2;; esac"#).await;
    let app = f.app.clone();
    let token = f.token.clone();
    let install = tokio::spawn(async move {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/computers/local/ide/extensions")
            .header("host", LISTEN)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"id":"a.b"}"#))
            .unwrap();
        app.oneshot(req).await.unwrap().status()
    });
    // Wait until the install's child has started (it records its run first).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ran(&f) {
        assert!(std::time::Instant::now() < deadline, "the install never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (st, body) = uninstall(&f, "local", "a.b").await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(install.await.unwrap(), StatusCode::OK);
    let (st, _) = uninstall(&f, "local", "a.b").await;
    assert_eq!(st, StatusCode::OK, "the computer is free again after the install");
}
