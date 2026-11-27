//! The Ember 에디터 window's flow against a real OSE, without a keyboard (SPEC FR-E12, #32 phase A
//! step 3). Ignored by default and gated on `EMBER_OSE_SERVER`, like `live_session.rs`:
//!
//! ```sh
//! export EMBER_OSE_SERVER="$(ember/editor-conn/scripts/fetch-ose-artifact.sh)"   # .../bin/dpx-ose-server
//! cargo test --locked --manifest-path ember/editor/Cargo.toml --test live_window_flow -- --ignored --nocapture
//! ```
//!
//! The test plays the window's part: every session update goes into the [`WidgetBridge`] the
//! window keeps, and typing goes through the bridge's `on_code_changed`, the same call the
//! `CodeEditor`'s change event makes in `ember/app`. It types a YAML syntax error, waits for the
//! error of the unmodified Open VSX extension `redhat.vscode-yaml` (pinned in
//! `.github/scripts/ose/regression/extensions.json`, chosen by the leader, Q-E6) in the bridge's problems
//! list, saves, and checks that the file on disk is the bridge's text. It needs the network to
//! install the extension from Open VSX, as the `NFR-W1` run does.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ember_editor::{EditorSession, SessionConfig, SessionUpdate, Severity, WidgetBridge, WidgetEvent, WidgetPos, WidgetRange};

/// The linter of the window-flow test and its pinned version (`regression/extensions.json`).
const LINTER: &str = "redhat.vscode-yaml@2.0.0";
const DOC_TEXT: &str = "name: ember\nok: true\n";
/// Typed at the start of line 2 (0-based), after `ok: true\n`: an unclosed flow sequence.
const TYPED: &str = "broken: [unclosed\n";

struct Server {
    child: Option<Child>,
    port: u16,
    root: PathBuf,
}

/// The server settings of the NFR-W1 run (`.github/scripts/ose/regress.sh`): Open VSX extensions carry no
/// Microsoft signature, and `--install-extension` refuses them unless signature checks are off.
/// Written before the install as well as before the server starts. Returns the server data dir.
fn write_server_settings(root: &Path) -> PathBuf {
    let data = root.join("data");
    for scope in ["Machine", "User"] {
        let dir = data.join("data").join(scope);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), "{ \"extensions.verifySignature\": false }\n").unwrap();
    }
    data
}

impl Server {
    fn start(bin: &Path, root: &Path, extensions: &Path) -> Self {
        let data = write_server_settings(root);
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let log = std::fs::File::create(root.join("server.log")).unwrap();
        let log_err = log.try_clone().unwrap();
        let mut cmd = Command::new(bin);
        cmd.args(["--host", "127.0.0.1", "--port", &port.to_string(), "--without-connection-token", "--accept-server-license-terms"])
            .arg("--server-data-dir")
            .arg(&data)
            .arg("--extensions-dir")
            .arg(extensions)
            .args(["--telemetry-level", "off"])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        Self { child: Some(child), port, root: root.to_path_buf() }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pgid = child.id().to_string();
            let _ = Command::new("pkill").args(["-TERM", "-g", &pgid]).status();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = Command::new("pkill").args(["-KILL", "-g", &pgid]).status();
            let _ = child.wait();
        }
        if std::thread::panicking() || std::env::var_os("EMBER_OSE_KEEP").is_some() {
            if let Ok(log) = std::fs::read_to_string(self.root.join("server.log")) {
                let lines: Vec<&str> = log.lines().collect();
                for l in &lines[lines.len().saturating_sub(60)..] {
                    eprintln!("  {l}");
                }
            }
            eprintln!("[window-flow] kept {}", self.root.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Feed updates into the bridge, as the window does, until `done` says so; `false` on timeout.
async fn pump(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>,
    bridge: &mut WidgetBridge,
    until: Duration,
    mut done: impl FnMut(&WidgetBridge, &SessionUpdate) -> bool,
) -> bool {
    let end = tokio::time::Instant::now() + until;
    loop {
        let u = match tokio::time::timeout_at(end, rx.recv()).await {
            Ok(Some(u)) => u,
            Ok(None) => panic!("session ended"),
            Err(_) => return false,
        };
        if let SessionUpdate::ConnectionLost { reason } = &u {
            panic!("connection lost: {reason}");
        }
        let seen = u.clone();
        let _ = bridge.apply(u);
        if done(bridge, &seen) {
            return true;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server (EMBER_OSE_SERVER) and the network for Open VSX"]
async fn live_window_flow_type_save_linter() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        // FR-E12: in CI the live job always sets it, so a missing server there is a failure.
        assert!(std::env::var_os("CI").is_none(), "[window-flow] EMBER_OSE_SERVER not set in CI");
        eprintln!("[window-flow] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("ember-flow-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let doc_path = ws.join("config.yaml");
    std::fs::write(&doc_path, DOC_TEXT).unwrap();

    // The unmodified linter from Open VSX, installed the way the NFR-W1 run installs it.
    let ext = root.join("ext");
    let data = write_server_settings(&root);
    let out = Command::new(&bin)
        .args(["--install-extension", LINTER, "--extensions-dir"])
        .arg(&ext)
        .arg("--server-data-dir")
        .arg(&data)
        .arg("--accept-server-license-terms")
        .stdin(Stdio::null())
        .output()
        .expect("install the linter");
    assert!(out.status.success(), "installing {LINTER} failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    let server = Server::start(&bin, &root, &ext);
    let addr = format!("127.0.0.1:{}", server.port);
    let deadline = Instant::now() + Duration::from_secs(120);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        assert!(Instant::now() < deadline, "server not listening on {addr}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let config = SessionConfig::new(addr.clone(), ws.to_str().unwrap());
    let (session, mut rx) = tokio::time::timeout(Duration::from_secs(120), EditorSession::connect_tcp(config))
        .await
        .expect("connect timed out")
        .unwrap_or_else(|e| panic!("connect: {e}"));

    // The window's side: every update goes into the bridge.
    let mut bridge = WidgetBridge::new();
    let uri = session.uri(doc_path.to_str().unwrap());
    session.open(uri.clone()).unwrap();
    let opened = pump(&mut rx, &mut bridge, Duration::from_secs(30), |_, u| matches!(u, SessionUpdate::Opened { .. })).await;
    assert!(opened, "the document did not open");
    let doc = bridge.document(&uri).expect("the bridge has the document");
    assert_eq!(doc.text, DOC_TEXT);
    assert_eq!(doc.language_id, "yaml");

    // Type through the bridge, as the CodeEditor's change event does.
    let range = WidgetRange::empty(WidgetPos::new(2, 0));
    let (u, generation, event) = bridge.on_code_changed(&uri, 1, range, TYPED.into()).expect("the bridge knows the document");
    session.widget_event(u, generation, event).unwrap();
    let typed = format!("{DOC_TEXT}{TYPED}");
    assert_eq!(bridge.document(&uri).unwrap().text, typed);

    // The linter's error reaches the problems list.
    let t = Instant::now();
    let key = uri.key();
    let found = pump(&mut rx, &mut bridge, Duration::from_secs(180), |b, _| {
        b.problems().iter().any(|p| p.uri.key() == key && p.severity == Severity::Error)
    })
    .await;
    assert!(found, "no error from {LINTER} within 180 s");
    let problems: Vec<_> = bridge.problems().iter().filter(|p| p.uri.key() == key).collect();
    println!("[window-flow] problems after {:.1} s: {problems:?}", t.elapsed().as_secs_f64());
    assert!(problems.iter().any(|p| p.severity == Severity::Error && p.range.start.line >= 2), "{problems:?}");

    // Save: the file on disk is what the window shows.
    session.widget_event(uri.clone(), generation, WidgetEvent::CodeSaveRequested { version: 1 }).unwrap();
    let saved = pump(&mut rx, &mut bridge, Duration::from_secs(30), |_, u| match u {
        SessionUpdate::Saved { .. } => true,
        SessionUpdate::SaveFailed { error, .. } => panic!("save failed: {error}"),
        _ => false,
    })
    .await;
    assert!(saved, "no save answer");
    assert_eq!(std::fs::read_to_string(&doc_path).unwrap(), typed);

    session.close(uri).unwrap();
    session.shutdown().await;
    drop(server);
}
