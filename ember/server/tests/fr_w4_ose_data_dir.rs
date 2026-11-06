//! FR-W4 (#459): a data dir per computer for the OSE, and the extension install route.
//!
//! A fake `dpx-ose-server` (a shell script written into a temp dir) records its argv, so no
//! real OSE is needed. The shared data dir behaviour of several real OSE processes is unknown
//! and not tested here.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::{self, Computers, Registry};
use ember_server::ose::{data_root_from, extension_spec, valid_computer_id, OseManager};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";

struct Fx {
    app: Router,
    router: Router,
    token: String,
    root: PathBuf,
    argv: PathBuf,
    dir: tempfile::TempDir,
    computers: Arc<Computers>,
}

fn fake(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-ose");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

const RECORD: &str = r#"printf '%s\n' "$@" > "$(dirname "$0")/argv.txt"; echo "ran $$" >> "$(dirname "$0")/runs.txt""#;

fn fx(script: &str, timeout_ms: u64) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake(dir.path(), &format!("{RECORD}\n{script}"));
    let root = dir.path().join("ose-data");
    let ose = Arc::new(
        OseManager::with_data_root(Some(bin), root.clone()).with_install_timeout(Duration::from_millis(timeout_ms)),
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let router = ember_server::api::ide_extensions::router(computers.clone(), ose);
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []);
    Fx {
        app: auth::protect(router.clone(), a),
        router,
        token,
        root,
        argv: dir.path().join("argv.txt"),
        dir,
        computers,
    }
}

async fn post(app: &Router, computer: &str, body: &str, auth_header: Option<&str>) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/computers/{computer}/ide/extensions"))
        .header("host", LISTEN)
        .header("content-type", "application/json");
    if let Some(h) = auth_header {
        b = b.header("authorization", h);
    }
    let r = app.clone().oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn argv(f: &Fx) -> Vec<String> {
    std::fs::read_to_string(&f.argv).unwrap().lines().map(String::from).collect()
}

fn ran(f: &Fx) -> bool {
    f.dir.path().join("runs.txt").exists()
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn arg_after<'a>(v: &'a [String], flag: &str) -> &'a str {
    &v[v.iter().position(|a| a == flag).unwrap() + 1]
}

#[tokio::test]
async fn fr_w4_ose_data_dir_install_passes_the_computer_data_dir_as_separate_argv() {
    let f = fx("echo installing; echo warn >&2", 5000);
    let h = format!("Bearer {}", f.token);
    let (st, body) = post(&f.app, "local", r#"{"id":"redhat.java","version":"1.2.3-pre.1"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["installed"], true);
    assert_eq!(body["id"], "redhat.java");
    let out = body["output"].as_str().unwrap();
    assert!(out.contains("installing") && out.contains("warn"), "{out}");
    let v = argv(&f);
    let local = f.root.join("local");
    assert_eq!(v[0], "--install-extension");
    assert_eq!(v[1], "redhat.java@1.2.3-pre.1");
    assert_eq!(arg_after(&v, "--server-data-dir"), local.to_str().unwrap());
    assert_eq!(arg_after(&v, "--extensions-dir"), local.join("extensions").to_str().unwrap());
    assert_eq!(mode(&local), 0o700);
    assert_eq!(mode(&f.root), 0o700);
}

#[tokio::test]
async fn fr_w4_ose_data_dir_bad_ids_are_400_and_the_binary_never_runs() {
    let f = fx("", 5000);
    let h = format!("Bearer {}", f.token);
    let long = format!("{}.b", "a".repeat(300));
    let bad: Vec<String> = vec![
        "../x".into(), "a b".into(), "a;b.c".into(), "x".into(), "--foo.bar".into(),
        "https://example.com/a.vsix".into(), "".into(), long, "/tmp/a.vsix".into(), "a.b.c".into(),
        "$(touch pwn).x".into(), "A.b".into(), "a.b\n--foo".into(),
    ];
    for id in &bad {
        let (st, body) = post(&f.app, "local", &json!({ "id": id }).to_string(), Some(&h)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{id:?}: {body}");
    }
    for version in ["1.2", "x", "--help", "1.2.3 ; ls", "1.2.3-", "../1.2.3", ""] {
        let (st, _) = post(&f.app, "local", &json!({ "id": "a.b", "version": version }).to_string(), Some(&h)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "version {version:?}");
    }
    // Extra fields (a path or URL smuggled in) and broken bodies are 400 too.
    for body in [r#"{"id":"a.b","path":"/x.vsix"}"#, r#"{"id":"a.b","url":"http://x"}"#, "{", "", "[]"] {
        let (st, _) = post(&f.app, "local", body, Some(&h)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    }
    assert!(!ran(&f), "the fake binary must never run for a bad request");
    assert!(!f.root.exists(), "no data dir is created for a bad request");
}

#[test]
fn fr_w4_ose_data_dir_spec_and_computer_id_validation() {
    assert_eq!(extension_spec("ms-python.python", None).unwrap(), "ms-python.python");
    assert_eq!(extension_spec("a1.b-2", Some("10.0.1+build.5")).unwrap(), "a1.b-2@10.0.1+build.5");
    assert!(extension_spec("-a.b", None).is_err());
    assert!(extension_spec("a.-b", None).is_err());
    for ok in ["local", "mini-2", "a.b_c"] {
        assert!(valid_computer_id(ok), "{ok}");
    }
    for bad in ["", ".", "..", "a..b", "../x", "a/b", "a\\b", "a b", "é", &"a".repeat(65)] {
        assert!(!valid_computer_id(bad), "{bad:?}");
    }
}

#[test]
fn fr_w4_ose_data_dir_a_computer_id_cannot_escape_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let m = OseManager::with_data_root(None, dir.path().join("root"));
    for bad in ["..", "../x", "a/../b", "", "/etc", "x/y"] {
        assert!(m.computer_data_dir(bad).is_err(), "{bad:?}");
        assert!(m.connection_for(bad, dir.path()).is_err());
    }
    assert!(!dir.path().join("x").exists());
    let ok = m.computer_data_dir("local").unwrap();
    assert_eq!(ok, dir.path().join("root/local"));
    assert_eq!(mode(&ok), 0o700);
    let err = m.install_extension("../x", "a.b", None).unwrap_err();
    assert!(matches!(err, ember_server::ose::InstallError::Bad(_)));
}

#[test]
fn fr_w4_ose_data_dir_root_resolution() {
    let p = |s: &str| Some(PathBuf::from(s));
    assert_eq!(data_root_from(p("/o"), p("/d"), p("/h")), PathBuf::from("/o"));
    assert_eq!(data_root_from(None, p("/d"), p("/h")), PathBuf::from("/d/ose-data"));
    assert_eq!(data_root_from(None, None, p("/h")), PathBuf::from("/h/.ember/ose-data"));
    assert_eq!(data_root_from(p(""), p(""), p("/h")), PathBuf::from("/h/.ember/ose-data"));
}

#[tokio::test]
async fn fr_w4_ose_data_dir_start_passes_server_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("fake-ose");
    std::fs::write(
        &bin,
        r##"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/start-argv.txt"
exec python3 -c 'import socket,sys; p=int(sys.argv[sys.argv.index("--port")+1]); s=socket.socket(); s.bind(("127.0.0.1",p)); s.listen();
while True: s.accept()[0].close()' "$@"
"##,
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = dir.path().join("ose-data");
    let folder = dir.path().join("proj");
    std::fs::create_dir(&folder).unwrap();
    let m = OseManager::with_data_root(Some(bin), root.clone());
    let m = tokio::task::spawn_blocking(move || {
        m.connection(&folder).unwrap();
        m
    })
    .await
    .unwrap();
    let v: Vec<String> =
        std::fs::read_to_string(dir.path().join("start-argv.txt")).unwrap().lines().map(String::from).collect();
    assert_eq!(arg_after(&v, "--server-data-dir"), root.join("local").to_str().unwrap());
    assert!(v.contains(&"--connection-token".to_string()));
    assert_eq!(mode(&root.join("local")), 0o700);
    drop(m);
}

#[tokio::test]
async fn fr_w4_ose_data_dir_timeout_kills_the_child_by_handle() {
    let f = fx(r#"echo $$ > "$(dirname "$0")/pid.txt"; echo started; exec sleep 60"#, 400);
    let h = format!("Bearer {}", f.token);
    let (st, body) = post(&f.app, "local", r#"{"id":"a.b"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body["error"].as_str().unwrap().contains("timed out"));
    assert!(body["output"].as_str().unwrap().contains("started"));
    let pid = std::fs::read_to_string(f.dir.path().join("pid.txt")).unwrap().trim().to_string();
    let alive = std::process::Command::new("kill").args(["-0", &pid]).status().unwrap().success();
    assert!(!alive, "child {pid} survived the timeout");
}

#[tokio::test]
async fn fr_w4_ose_data_dir_failure_is_502_with_the_output_and_output_is_capped() {
    let f = fx("echo boom >&2; head -c 100000 /dev/zero | tr '\\0' x; exit 1", 5000);
    let h = format!("Bearer {}", f.token);
    let (st, body) = post(&f.app, "local", r#"{"id":"a.b"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{}", body["error"]);
    let out = body["output"].as_str().unwrap();
    assert!(out.len() <= 16 * 1024 && out.len() > 1000);
    assert_eq!(body["installed"], false);
}

#[tokio::test]
async fn fr_w4_ose_data_dir_missing_binary_is_503() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(OseManager::with_data_root(Some(dir.path().join("absent")), dir.path().join("d")));
    let store = Arc::new(Store::open_in_memory().unwrap());
    let c = Computers::new(Registry::new(store), computers::default_connector());
    let app = ember_server::api::ide_extensions::router(c, ose);
    let (st, _) = post(&app, "local", r#"{"id":"a.b"}"#, None).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn fr_w4_ose_data_dir_unknown_computer_404_bad_id_400_non_local_400() {
    let f = fx("", 5000);
    let h = format!("Bearer {}", f.token);
    let (st, _) = post(&f.app, "nope", r#"{"id":"a.b"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = post(&f.app, "a..b", r#"{"id":"a.b"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = post(&f.app, "%2E%2E", r#"{"id":"a.b"}"#, Some(&h)).await;
    assert!(st == StatusCode::BAD_REQUEST || st == StatusCode::NOT_FOUND, "{st}");
    let c = f.computers.registry().insert("peer", "http://127.0.0.1:1", "tok").unwrap();
    let (st, body) = post(&f.app, &c.id, r#"{"id":"a.b"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "OSE server is only available on the local computer");
    assert!(!ran(&f));
}

#[tokio::test]
async fn fr_w4_ose_data_dir_needs_the_server_token_and_a_guest_token_is_401() {
    let f = fx("", 5000);
    for h in [None, Some("Bearer dpg_whatever"), Some("Bearer nope")] {
        let (st, _) = post(&f.app, "local", r#"{"id":"a.b"}"#, h).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{h:?}");
    }
    assert!(!ran(&f));
    // Without the layer the route itself answers, so the 401 above comes from auth::protect.
    let (st, _) = post(&f.router, "local", r#"{"id":"a.b"}"#, None).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn fr_w4_ose_data_dir_a_second_concurrent_install_is_409() {
    let f = fx(r#"echo go > "$(dirname "$0")/started.txt"; exec sleep 60"#, 1500);
    let h = format!("Bearer {}", f.token);
    let app = f.app.clone();
    let h2 = h.clone();
    let first = tokio::spawn(async move { post(&app, "local", r#"{"id":"a.b"}"#, Some(&h2)).await });
    let started = f.dir.path().join("started.txt");
    for _ in 0..200 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(started.exists(), "the first install never started");
    let (st, _) = post(&f.app, "local", r#"{"id":"c.d"}"#, Some(&h)).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = first.await.unwrap();
    assert_eq!(st, StatusCode::BAD_GATEWAY, "the first one ends by timeout");
    // The lock is released: the next install runs (and times out the same way).
    let _ = std::fs::remove_file(&started);
    let (st, _) = post(&f.app, "local", r#"{"id":"c.d"}"#, Some(&h)).await;
    assert_ne!(st, StatusCode::CONFLICT);
}
