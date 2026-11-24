//! SPEC FR-W4a and FR-W4b (#896): the OSE on a node.
//!
//! `POST /ide/ose {folder}` starts or reuses the OSE of a folder inside the node's allowed roots
//! and answers the launch id with the placeholder token, `GET /ide/ose/{launch_id}/relay` is the
//! binary WebSocket relay to the node's loopback OSE with the placeholder replaced by the real
//! token, and `POST /ide/extensions` installs an extension. All three sit behind the node's token.
//!
//! The OSE is a fake here: a loopback TCP listener that records what it receives and echoes it
//! (registered with `OseManager::register_launch_for_test`), or a shell script standing in for
//! the `dpx-ose-server` binary. A real Code-OSS runtime is not covered.
//!
//! The VS Code target (`POST /ide/ose {folder, target: "vscode"}`, D55, #1064) starts the OSE on
//! a 0700 Unix socket with no token; its relay joins the socket and replaces nothing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::ose::{OseManager, OseTarget, LOCAL_COMPUTER};
use ember_node::ose_api::OseRelayStream;
use ember_node::proto::ErrorCode;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

#[cfg(unix)]
mod common;

const TOKEN: &str = "node-token";
const REAL_TOKEN: &str = "1111111111111111222222222222222233333333333333334444444444444444";
const WAIT: Duration = Duration::from_secs(20);

struct Fx {
    client: NodeClient,
    base: String,
    /// Allowed root (canonical).
    root: PathBuf,
    /// A sibling directory outside the root.
    outside: PathBuf,
    ose: Arc<OseManager>,
    /// The OSE data root given to the manager.
    data: PathBuf,
    dir: tempfile::TempDir,
}

/// A node on a loopback port whose OSE manager is what `make` builds from `(temp dir, data root)`.
async fn start(make: impl FnOnce(&Path, &Path) -> OseManager) -> Fx {
    start_in(tempfile::tempdir().unwrap(), make).await
}

/// Like [`start`] in a temp dir with a short path. A Unix socket address holds at most 104 bytes
/// on macOS (108 on Linux), and the default temp dir there (`/private/var/folders/...`) leaves no
/// room for `<data>/local/sockets/<16 hex>.sock`, so the VS Code target's tests use `/tmp`.
#[cfg(unix)]
async fn start_short(make: impl FnOnce(&Path, &Path) -> OseManager) -> Fx {
    start_in(tempfile::Builder::new().prefix("ew").tempdir_in("/tmp").unwrap(), make).await
}

async fn start_in(dir: tempfile::TempDir, make: impl FnOnce(&Path, &Path) -> OseManager) -> Fx {
    let base_dir = dir.path().canonicalize().unwrap();
    let root = base_dir.join("root");
    let outside = base_dir.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let data = base_dir.join("ose-data");
    let ose = Arc::new(make(&base_dir, &data));
    let node = Node::with_ose(NodeConfig::new(TOKEN, vec![root.clone()]), ose.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fx { client: NodeClient::new(&base, TOKEN).unwrap(), base, root, outside, ose, data, dir }
}

/// A manager with no OSE binary: launches exist only through `register_launch_for_test`.
fn no_binary(_dir: &Path, data: &Path) -> OseManager {
    OseManager::with_data_root(None, data.to_path_buf())
}

/// What the fake OSE saw: every byte it received, and how many connections are open now.
#[derive(Clone, Default)]
struct FakeOse {
    received: Arc<Mutex<Vec<u8>>>,
    live: Arc<AtomicUsize>,
}

/// A loopback TCP "OSE" that records what it receives and echoes it back.
async fn start_fake_ose() -> (SocketAddr, FakeOse) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fake = FakeOse::default();
    let bg = fake.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let fake = bg.clone();
            fake.live.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            fake.received.lock().unwrap().extend_from_slice(&buf[..n]);
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                fake.live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (addr, fake)
}

/// The launch id is the last segment of the relay path the manager builds.
fn launch_id_of(relay: &str) -> String {
    relay.rsplit('/').next().unwrap().to_string()
}

/// Reads binary messages until `want` bytes arrived.
async fn read_bytes(ws: &mut OseRelayStream, want: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < want {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no echo in time") {
            Some(Ok(Message::Binary(b))) => out.extend_from_slice(&b),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("the relay ended after {} of {want} bytes: {other:?}", out.len()),
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn fr_w4b_node_ide_ose_requires_node_token_and_returns_placeholder() {
    let f = start(no_binary).await;
    let (ose_addr, _) = start_fake_ose().await;
    let conn = f.ose.register_launch_for_test(LOCAL_COMPUTER, &f.root, ose_addr, REAL_TOKEN);
    let launch_id = launch_id_of(conn.relay.as_deref().unwrap());

    // Every route refuses a wrong token and a missing one.
    let bad = NodeClient::new(&f.base, "wrong").unwrap();
    let e = bad.ide_ose(&f.root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
    let e = bad.ide_ose_relay(&launch_id).await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
    let (status, _) = bad.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 401);
    let r = reqwest::Client::new()
        .post(format!("{}/ide/ose", f.base))
        .json(&json!({ "folder": f.root }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // With the token: the launch id and the placeholder, never the real token.
    let launch = f.client.ide_ose(&f.root).await.unwrap();
    assert_eq!(launch.launch_id, launch_id);
    assert_eq!(launch.launch_id.len(), 32);
    assert_eq!(launch.connection_token, conn.connection_token);
    assert_eq!(launch.connection_token.len(), 64);
    assert_ne!(launch.connection_token, REAL_TOKEN);
    assert_eq!(launch.endpoint, format!("http://{ose_addr}"));
    let (status, body) = f.client.post_raw("/ide/ose", &json!({ "folder": f.root })).await.unwrap();
    assert_eq!(status, 200);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains(REAL_TOKEN), "the real token is in the answer: {text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["connection_token"], conn.connection_token.as_str());

    // The same folder is the same launch.
    assert_eq!(f.client.ide_ose(&f.root).await.unwrap().launch_id, launch_id);
}

#[tokio::test]
async fn fr_w4b_node_ide_relay_substitutes_placeholder_and_pumps_bytes() {
    let f = start(no_binary).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let conn = f.ose.register_launch_for_test(LOCAL_COMPUTER, &f.root, ose_addr, REAL_TOKEN);
    let launch_id = launch_id_of(conn.relay.as_deref().unwrap());
    let placeholder = conn.connection_token.clone();

    // An unknown launch is refused before the upgrade.
    let e = f.client.ide_ose_relay("0123456789abcdef0123456789abcdef").await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 404, .. }), "{e:?}");
    assert_eq!(fake.live.load(Ordering::SeqCst), 0, "an unknown launch must dial nothing");

    let mut ws = f.client.ide_ose_relay(&launch_id).await.unwrap();

    // First message: the placeholder inside the 8 KiB window is replaced by the real token.
    let mut first = format!("GET /?tkn={placeholder} HTTP/1.1\r\n").into_bytes();
    first.resize(9000, b'x');
    ws.send(Message::Binary(first.clone().into())).await.unwrap();
    let echo = read_bytes(&mut ws, first.len()).await;
    let mut expected = format!("GET /?tkn={REAL_TOKEN} HTTP/1.1\r\n").into_bytes();
    expected.resize(9000, b'x');
    assert_eq!(echo, expected);

    // Second message: past the window the bytes pass unchanged.
    let second = format!("late:{placeholder}").into_bytes();
    ws.send(Message::Binary(second.clone().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, second.len()).await, second);

    let seen = fake.received.lock().unwrap().clone();
    assert!(find(&seen[..9000], REAL_TOKEN.as_bytes()), "the OSE did not get the real token");
    assert!(!find(&seen[..9000], placeholder.as_bytes()), "the placeholder reached the OSE inside the window");
    assert_eq!(seen.len(), first.len() + second.len());

    // Closing the client side drops the TCP connection to the OSE.
    ws.close(None).await.unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while fake.live.load(Ordering::SeqCst) != 0 {
        assert!(std::time::Instant::now() < deadline, "the OSE connection stayed open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A text frame closes the relay with 1003.
    let mut ws = f.client.ide_ose_relay(&launch_id).await.unwrap();
    ws.send(Message::text("not binary")).await.unwrap();
    let code = loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no close in time") {
            Some(Ok(Message::Close(Some(frame)))) => break u16::from(frame.code),
            Some(Ok(Message::Close(None))) | Some(Err(_)) | None => panic!("closed without the 1003 frame"),
            Some(Ok(_)) => continue,
        }
    };
    assert_eq!(code, 1003);
}

#[tokio::test]
async fn fr_w4b_node_ide_relay_limit_is_429() {
    let f = start(|_, data| OseManager::with_data_root(None, data.to_path_buf()).with_relay_limits(1, 64)).await;
    let (ose_addr, _) = start_fake_ose().await;
    let conn = f.ose.register_launch_for_test(LOCAL_COMPUTER, &f.root, ose_addr, REAL_TOKEN);
    let launch_id = launch_id_of(conn.relay.as_deref().unwrap());

    let _held = f.client.ide_ose_relay(&launch_id).await.unwrap();
    let e = f.client.ide_ose_relay(&launch_id).await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 429, .. }), "{e:?}");
}

#[tokio::test]
async fn fr_w4a_node_ose_refuses_folder_outside_allowed_roots_with_403() {
    let f = start(no_binary).await;

    let e = f.client.ide_ose(&f.outside).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 403, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));

    let e = f.client.ide_ose(f.root.join("missing")).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 404, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::NotFound));

    let e = f.client.ide_ose("relative/folder").await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 400, .. }), "{e:?}");

    let file = f.root.join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let e = f.client.ide_ose(&file).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 400, .. }), "{e:?}");

    // Inside the roots, but this node has no OSE binary: 503 with the reason.
    let e = f.client.ide_ose(&f.root).await.unwrap_err();
    match &e {
        ClientError::Api { status: 503, body } => assert!(body.error.contains("not installed"), "{body:?}"),
        other => panic!("{other:?}"),
    }
    let (status, body) = f.client.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 503, "{}", String::from_utf8_lossy(&body));
    // Nothing was created for a refused launch.
    assert!(!f.data.exists(), "a refused launch made the data dir");
}

/// A node built without an OSE manager (`Node::new`: the server's in-process local node, tests)
/// has the routes and no binary.
#[tokio::test]
async fn fr_w4a_node_without_an_ose_manager_answers_503() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    let e = NodeClient::new(&base, TOKEN).unwrap().ide_ose(&root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 503, .. }), "{e:?}");
}

/// True, with a message, when `python3` is not on this machine: the stand-in OSE binaries listen
/// through it, so a test that starts one is skipped instead of failing.
#[cfg(unix)]
fn python3_missing(test: &str) -> bool {
    let have = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have {
        eprintln!("python3 is missing: skipping {test}");
    }
    !have
}

/// A stand-in for `dpx-ose-server`: writes its environment and arguments beside itself, then
/// listens on `--port` like the real one.
#[cfg(unix)]
fn fake_ose_binary(dir: &Path, data: &Path) -> OseManager {
    let script = dir.join("fake-ose");
    common::write_exec_ready(
        &script,
        r##"#!/bin/sh
env > "$0.env"
printf '%s\n' "$@" > "$0.args"
exec python3 -c 'import socket,sys; p=int(sys.argv[sys.argv.index("--port")+1]); s=socket.socket(); s.bind(("127.0.0.1",p)); s.listen();
while True: s.accept()[0].close()' "$@"
"##,
    );
    OseManager::with_data_root(Some(script), data.to_path_buf())
}

#[cfg(unix)]
#[tokio::test]
async fn fr_w4a_node_ose_spawns_per_folder_under_allowed_roots_and_reuses_live_process() {
    if python3_missing("fr_w4a_node_ose_spawns_per_folder_under_allowed_roots_and_reuses_live_process") {
        return;
    }
    let f = start(fake_ose_binary).await;
    let sub = f.root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let first = f.client.ide_ose(&f.root).await.unwrap();
    let again = f.client.ide_ose(&f.root).await.unwrap();
    assert_eq!(first.launch_id, again.launch_id, "a live OSE of the folder is reused");
    assert_eq!(first.connection_token, again.connection_token);
    assert_eq!(first.endpoint, again.endpoint);

    let other = f.client.ide_ose(&sub).await.unwrap();
    assert_ne!(other.launch_id, first.launch_id, "another folder is another OSE");
    assert_ne!(other.endpoint, first.endpoint);

    // The endpoint is a loopback listener of this node, and the relay reaches it.
    let port: u16 = first.endpoint.rsplit(':').next().unwrap().parse().unwrap();
    assert!(first.endpoint.starts_with("http://127.0.0.1:"), "{}", first.endpoint);
    tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("the OSE listens");
    f.client.ide_ose_relay(&first.launch_id).await.expect("the relay opens");
}

#[cfg(unix)]
#[tokio::test]
async fn fr_w4a_node_ose_writes_0600_token_file_and_clears_env() {
    use std::os::unix::fs::PermissionsExt;

    if python3_missing("fr_w4a_node_ose_writes_0600_token_file_and_clears_env") {
        return;
    }
    let f = start(fake_ose_binary).await;
    let launch = f.client.ide_ose(&f.root).await.unwrap();

    let script = f.dir.path().canonicalize().unwrap().join("fake-ose");
    let args = std::fs::read_to_string(format!("{}.args", script.display())).unwrap();
    let args: Vec<&str> = args.lines().collect();
    let at = args.iter().position(|a| *a == "--connection-token-file").expect("the token file flag");
    let token_file = PathBuf::from(args[at + 1]);
    assert!(token_file.starts_with(f.data.join(LOCAL_COMPUTER).join("tokens")), "{}", token_file.display());

    let md = std::fs::metadata(&token_file).unwrap();
    assert_eq!(md.permissions().mode() & 0o777, 0o600);
    let real = std::fs::read_to_string(&token_file).unwrap();
    assert_eq!(real.len(), 64, "64 hex, no trailing newline");
    assert!(real.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(real, launch.connection_token, "the answer carries the placeholder, not the real token");
    assert!(!args.iter().any(|a| a.contains(&real)), "the real token is on the command line");
    let dir_at = args.iter().position(|a| *a == "--server-data-dir").expect("the data dir flag");
    assert_eq!(Path::new(args[dir_at + 1]), f.data.join(LOCAL_COMPUTER));

    // The child's environment is the allowlist: nothing of the test process's own variables
    // (cargo sets `CARGO_*` for every test binary) and no token.
    let env = std::fs::read_to_string(format!("{}.env", script.display())).unwrap();
    let names: Vec<&str> = env.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
    assert!(std::env::vars().any(|(k, _)| k.starts_with("CARGO_")), "the test expects cargo's variables");
    assert!(!names.iter().any(|n| n.starts_with("CARGO_")), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("TOKEN")), "{names:?}");
    assert!(!env.contains(&real) && !env.contains(TOKEN), "a token is in the OSE environment");
}

/// FR-W4a (#896): an OSE that is slow to start does not hold up the node's other requests. The
/// test runtime has ONE worker thread, so a handler that waited for the start (or for the
/// manager's lock, which the start holds) on that worker would stall the health call below.
#[cfg(unix)]
#[tokio::test]
async fn fr_w4a_a_starting_ose_does_not_stall_other_node_requests() {
    if python3_missing("fr_w4a_a_starting_ose_does_not_stall_other_node_requests") {
        return;
    }
    // Listens only after 3 s: the launch holds the manager's lock for that long.
    let f = start(|dir, data| {
        let script = dir.join("slow-ose");
        common::write_exec_ready(
            &script,
            r##"#!/bin/sh
sleep 3
exec python3 -c 'import socket,sys; p=int(sys.argv[sys.argv.index("--port")+1]); s=socket.socket(); s.bind(("127.0.0.1",p)); s.listen();
while True: s.accept()[0].close()' "$@"
"##,
        );
        OseManager::with_data_root(Some(script), data.to_path_buf())
    })
    .await;

    let (client, root) = (f.client.clone(), f.root.clone());
    let launch = tokio::spawn(async move { client.ide_ose(&root).await });
    tokio::time::sleep(Duration::from_millis(700)).await;
    // A relay lookup now waits for the lock the start holds.
    let client = f.client.clone();
    let lookup = tokio::spawn(async move { client.ide_ose_relay("0123456789abcdef0123456789abcdef").await.err() });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let asked = std::time::Instant::now();
    f.client.health().await.expect("the node answers while an OSE starts");
    assert!(asked.elapsed() < Duration::from_secs(1), "health took {:?} while an OSE started", asked.elapsed());
    assert!(!launch.is_finished(), "the slow OSE started before the check: the test proved nothing");

    let launched = tokio::time::timeout(WAIT, launch).await.expect("the launch ends").unwrap();
    assert!(launched.is_ok(), "{launched:?}");
    let refused = tokio::time::timeout(WAIT, lookup).await.expect("the lookup ends").unwrap();
    assert!(matches!(&refused, Some(ClientError::Api { status: 404, .. })), "{refused:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn fr_w4b_node_ide_extensions_installs_and_enforces_mutex_and_timeout() {
    // `$1` is `--install-extension`, `$2` the extension spec.
    let f = start(|dir, data| {
        let script = dir.join("fake-ose");
        common::write_exec_ready(
            &script,
            r##"#!/bin/sh
case "$2" in
  slow.ext) sleep 2; echo "installed $2" ;;
  hang.ext) sleep 60 ;;
  fail.ext) echo "no such extension" >&2; exit 3 ;;
  *) echo "installed $2" ;;
esac
"##,
        );
        OseManager::with_data_root(Some(script), data.to_path_buf()).with_install_timeout(Duration::from_secs(5))
    })
    .await;
    let post = |body: Value| {
        let client = f.client.clone();
        async move {
            let (status, bytes) = client.post_raw("/ide/extensions", &body).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
        }
    };

    let (status, v) = post(json!({ "id": "good.ext", "version": "1.2.3" })).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["installed"], true);
    assert_eq!(v["id"], "good.ext");
    assert!(v["output"].as_str().unwrap().contains("installed good.ext@1.2.3"), "{v}");

    // The id pattern is the server's: no path, no flag, no unknown field.
    for bad in [json!({ "id": "../x" }), json!({ "id": "-a.b" }), json!({ "id": "a.b", "path": "/x" })] {
        let (status, v) = post(bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {v}");
        assert!(v["error"].is_string(), "{v}");
    }

    // A failed run is 502 with what it printed.
    let (status, v) = post(json!({ "id": "fail.ext" })).await;
    assert_eq!(status, 502, "{v}");
    assert_eq!(v["installed"], false);
    assert!(v["output"].as_str().unwrap().contains("no such extension"), "{v}");

    // One install at a time on the node.
    let slow = tokio::spawn(post(json!({ "id": "slow.ext" })));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, v) = post(json!({ "id": "good.ext" })).await;
    assert_eq!(status, 409, "{v}");
    let (status, v) = slow.await.unwrap();
    assert_eq!(status, 200, "{v}");

    // A run past the timeout is killed and answered 502.
    let started = std::time::Instant::now();
    let (status, v) = post(json!({ "id": "hang.ext" })).await;
    assert_eq!(status, 502, "{v}");
    assert_eq!(v["installed"], false);
    assert!(v["error"].as_str().unwrap().starts_with("install timed out"), "{v}");
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
}

/// SPEC FR-E10 (#32): the node's launch answer carries the Code-OSS commit of its OSE build,
/// read from the `product.json` beside the launcher's `bin/`.
#[cfg(unix)]
#[tokio::test]
async fn fr_w4_ide_reports_ose_commit() {
    if python3_missing("fr_w4_ide_reports_ose_commit") {
        return;
    }
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let f = start(|dir, data| {
        let build = dir.join("dpx-ose-test");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("product.json"), format!(r#"{{"commit":"{COMMIT}"}}"#)).unwrap();
        let fake = fake_ose_binary(&build.join("bin"), data);
        drop(fake);
        OseManager::with_data_root(Some(build.join("bin").join("fake-ose")), data.to_path_buf())
    })
    .await;
    let launch = f.client.ide_ose(&f.root).await.unwrap();
    assert_eq!(launch.ose_commit.as_deref(), Some(COMMIT));
    let again = f.client.ide_ose(&f.root).await.unwrap();
    assert_eq!(again.ose_commit.as_deref(), Some(COMMIT), "a reused OSE reports it too");
}

/// A stand-in for `dpx-ose-server` that serves both targets: with `--socket-path` it listens on
/// that Unix socket (the VS Code target), else on `--port`. It writes its arguments to
/// `<script>.<socket|tcp>.args` and its pid to `<script>.<socket|tcp>.pid`, and every connection
/// echoes what it receives and appends it to `<script>.recv`.
#[cfg(unix)]
fn fake_dual_ose_binary(dir: &Path, data: &Path) -> OseManager {
    let script = dir.join("fake-ose");
    common::write_exec_ready(
        &script,
        r##"#!/bin/sh
case " $* " in *" --socket-path "*) m=socket ;; *) m=tcp ;; esac
printf '%s\n' "$@" > "$0.$m.args"
echo $$ > "$0.$m.pid"
exec python3 -c '
import socket, sys, threading
rec = sys.argv[1]
a = sys.argv
if "--socket-path" in a:
    s = socket.socket(socket.AF_UNIX)
    s.bind(a[a.index("--socket-path") + 1])
else:
    s = socket.socket()
    s.bind(("127.0.0.1", int(a[a.index("--port") + 1])))
s.listen()
lock = threading.Lock()
def echo(c):
    while True:
        d = c.recv(65536)
        if not d:
            break
        with lock:
            with open(rec, "ab") as f:
                f.write(d)
        c.sendall(d)
    c.close()
while True:
    c = s.accept()[0]
    threading.Thread(target=echo, args=(c,), daemon=True).start()
' "$0.recv" "$@"
"##,
    );
    OseManager::with_data_root(Some(script), data.to_path_buf())
}

/// FR-W4a, FR-W4b (4) (D55, #1064): the VS Code target's OSE listens on a 0700 Unix socket in a
/// 0700 dir of the node's data dir, with no token and no TCP port; the answer is the launch id
/// alone. The same folder's Ember target is another process, with its token and port.
#[cfg(unix)]
#[tokio::test]
async fn fr_w4a_vscode_target_socket_is_0700_and_no_tcp_port() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    if python3_missing("fr_w4a_vscode_target_socket_is_0700_and_no_tcp_port") {
        return;
    }
    let f = start_short(fake_dual_ose_binary).await;
    let script = f.dir.path().canonicalize().unwrap().join("fake-ose");

    // The answer: a 32-hex launch id, and no endpoint and no token of any kind.
    let (status, body) = f.client.post_raw("/ide/ose", &json!({ "folder": f.root, "target": "vscode" })).await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let v: Value = serde_json::from_slice(&body).unwrap();
    let launch_id = v["launch_id"].as_str().unwrap().to_string();
    assert_eq!(launch_id.len(), 32);
    assert!(launch_id.bytes().all(|b| b.is_ascii_hexdigit()), "{launch_id}");
    assert!(v.get("endpoint").is_none(), "{v}");
    assert!(v.get("connection_token").is_none(), "{v}");
    let typed = f.client.ide_ose_for(&f.root, OseTarget::Vscode).await.unwrap();
    assert_eq!(typed.launch_id, launch_id, "a live socket OSE of the folder is reused");
    assert!(typed.endpoint.is_empty() && typed.connection_token.is_empty(), "{typed:?}");

    // The command line: a socket and no token; no host, no port, no token file.
    let args = std::fs::read_to_string(format!("{}.socket.args", script.display())).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert!(args.contains(&"--without-connection-token"), "{args:?}");
    for absent in ["--host", "--port", "--connection-token-file", "--connection-token"] {
        assert!(!args.contains(&absent), "{absent} in {args:?}");
    }
    let at = args.iter().position(|a| *a == "--socket-path").expect("the socket flag");
    let socket = PathBuf::from(args[at + 1]);
    let sockets = f.data.join(LOCAL_COMPUTER).join("sockets");
    assert_eq!(socket.parent(), Some(sockets.as_path()), "{}", socket.display());
    let name = socket.file_name().unwrap().to_string_lossy().into_owned();
    let stem = name.strip_suffix(".sock").expect("a .sock name");
    assert_eq!(stem.len(), 16, "{name}");
    assert!(stem.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
    let dir_at = args.iter().position(|a| *a == "--server-data-dir").expect("the data dir flag");
    assert_eq!(Path::new(args[dir_at + 1]), f.data.join(LOCAL_COMPUTER));

    // The socket and its dir are the node user's alone.
    let md = std::fs::symlink_metadata(&socket).unwrap();
    assert!(md.file_type().is_socket(), "{} is not a socket", socket.display());
    assert_eq!(md.permissions().mode() & 0o777, 0o700, "socket mode");
    let dmd = std::fs::symlink_metadata(&sockets).unwrap();
    assert!(dmd.is_dir() && !dmd.file_type().is_symlink());
    assert_eq!(dmd.permissions().mode() & 0o777, 0o700, "sockets dir mode");

    // No token file was written for it.
    let tokens = f.data.join(LOCAL_COMPUTER).join("tokens");
    let token_files = std::fs::read_dir(&tokens).map(|d| d.count()).unwrap_or(0);
    assert_eq!(token_files, 0, "a token file was written for the VS Code target");

    // The OSE process holds no listening TCP socket (checked when lsof is on this machine).
    let pid = std::fs::read_to_string(format!("{}.socket.pid", script.display())).unwrap();
    match std::process::Command::new("lsof").args(["-nP", "-a", "-p", pid.trim(), "-iTCP", "-sTCP:LISTEN"]).output() {
        Ok(out) => assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "the VS Code target's OSE listens on TCP: {}",
            String::from_utf8_lossy(&out.stdout)
        ),
        Err(_) => eprintln!("lsof is missing: the TCP listener check is skipped"),
    }

    // The Ember target of the same folder is a different OSE, with its port and placeholder.
    let ember = f.client.ide_ose(&f.root).await.unwrap();
    assert_ne!(ember.launch_id, launch_id);
    assert!(ember.endpoint.starts_with("http://127.0.0.1:"), "{}", ember.endpoint);
    assert_eq!(ember.connection_token.len(), 64);

    // An unknown target is refused.
    let (status, _) = f.client.post_raw("/ide/ose", &json!({ "folder": f.root, "target": "emacs" })).await.unwrap();
    assert!((400..500).contains(&status), "{status}");

    // When the OSE goes away, so does its socket.
    f.ose.shutdown();
    assert!(std::fs::symlink_metadata(&socket).is_err(), "the socket outlived its OSE");
}

/// FR-W4b (4) (D55, #1064): the relay of a VS Code launch joins the folder's Unix socket and
/// carries the bytes unchanged both ways: even the Ember launch's placeholder of the same folder
/// reaches that OSE as it was sent. The limits are the Ember relay's.
#[cfg(unix)]
#[tokio::test]
async fn fr_w4b_vscode_target_relay_joins_the_socket_without_replacement() {
    if python3_missing("fr_w4b_vscode_target_relay_joins_the_socket_without_replacement") {
        return;
    }
    let f = start_short(|dir, data| fake_dual_ose_binary(dir, data).with_relay_limits(1, 64)).await;
    let recv = f.dir.path().canonicalize().unwrap().join("fake-ose.recv");
    let placeholder = f.client.ide_ose(&f.root).await.unwrap().connection_token;
    let vscode = f.client.ide_ose_for(&f.root, OseTarget::Vscode).await.unwrap();

    let mut ws = f.client.ide_ose_relay(&vscode.launch_id).await.unwrap();
    let mut first = format!("GET /?tkn={placeholder} HTTP/1.1\r\n").into_bytes();
    first.resize(9000, b'x');
    ws.send(Message::Binary(first.clone().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, first.len()).await, first, "the relay changed the bytes");
    let second = format!("again:{placeholder}").into_bytes();
    ws.send(Message::Binary(second.clone().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, second.len()).await, second);

    let seen = std::fs::read(&recv).unwrap();
    assert_eq!(seen.len(), first.len() + second.len(), "the OSE saw other bytes");
    assert!(find(&seen[..first.len()], placeholder.as_bytes()), "the bytes were rewritten on the way");
    assert_eq!(&seen[..first.len()], &first[..]);

    // The per-launch limit holds for the socket relay too.
    let e = f.client.ide_ose_relay(&vscode.launch_id).await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 429, .. }), "{e:?}");

    // A text frame closes it with 1003.
    ws.send(Message::text("not binary")).await.unwrap();
    let code = loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no close in time") {
            Some(Ok(Message::Close(Some(frame)))) => break u16::from(frame.code),
            Some(Ok(Message::Close(None))) | Some(Err(_)) | None => panic!("closed without the 1003 frame"),
            Some(Ok(_)) => continue,
        }
    };
    assert_eq!(code, 1003);
}
