//! `EditorSession` against a real OSE server. Ignored by default and gated on `EMBER_OSE_SERVER`
//! (the same server binary `editor-conn/tests/live_ose.rs` uses):
//!
//! ```sh
//! export EMBER_OSE_SERVER="$(editor-conn/scripts/fetch-ose-artifact.sh)"   # .../bin/dpx-ose-server
//! cargo test --manifest-path editor/Cargo.toml --test live_session -- --ignored --nocapture
//! ```
//!
//! Flow: start the server on a free port (flags as `proxy/dpx`), `EditorSession::connect_tcp` on
//! a temp workspace, open a valid JSON file, send the widget event that makes it invalid
//! (`CodeChanged` inserting `,,`), and wait for an Error **underline decoration** produced by the
//! built-in JSON language server through `$changeMany` → the diagnostics bridge. Then save
//! (`CodeSaveRequested` → `writeFile` + `$acceptModelSaved`) and check the bytes on disk, change
//! the file behind the server's back and expect a reload (`SetText` with a new generation).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ember_editor::{
    DecorationKind, EditorSession, SessionConfig, SessionUpdate, Severity, WidgetCommand, WidgetEvent, WidgetPos, WidgetRange,
};
use tokio::sync::mpsc;

/// Line 3 is `  "ok": true`; `true` ends at UTF-16 column 12 (0-based).
const DOC_TEXT: &str = "{\n  \"name\": \"ember\",\n  \"ok\": true\n}\n";
const DOC_TEXT_AFTER_EDIT: &str = "{\n  \"name\": \"ember\",\n  \"ok\": true,,\n}\n";
const DOC_TEXT_EXTERNAL: &str = "{\n  \"changed\": \"outside\"\n}\n";

struct Server {
    child: Option<Child>,
    port: u16,
    root: PathBuf,
}

impl Server {
    fn start(bin: &Path, root: &Path) -> Self {
        let data = root.join("data");
        let machine = data.join("data").join("Machine");
        std::fs::create_dir_all(&machine).unwrap();
        std::fs::write(machine.join("settings.json"), "{ \"extensions.verifySignature\": false }\n").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let log = std::fs::File::create(root.join("server.log")).unwrap();
        let log_err = log.try_clone().unwrap();
        let mut cmd = Command::new(bin);
        cmd.args(["--host", "127.0.0.1", "--port", &port.to_string(), "--without-connection-token", "--accept-server-license-terms"])
            .arg("--server-data-dir")
            .arg(&data)
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

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let pgid = child.id().to_string();
        let _ = Command::new("pkill").args(["-TERM", "-g", &pgid]).status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = Command::new("pkill").args(["-KILL", "-g", &pgid]).status();
        let _ = child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let failing = std::thread::panicking();
        self.stop();
        if failing || std::env::var_os("EMBER_OSE_KEEP").is_some() {
            if let Ok(log) = std::fs::read_to_string(self.root.join("server.log")) {
                let lines: Vec<&str> = log.lines().collect();
                for l in &lines[lines.len().saturating_sub(60)..] {
                    eprintln!("  {l}");
                }
            }
            eprintln!("[live-session] kept {}", self.root.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

async fn wait_for<T>(
    what: &str,
    secs: u64,
    rx: &mut mpsc::UnboundedReceiver<SessionUpdate>,
    mut pick: impl FnMut(&SessionUpdate) -> Option<T>,
) -> T {
    let fut = async {
        loop {
            match rx.recv().await {
                Some(u) => {
                    if let SessionUpdate::ConnectionLost { reason } = &u {
                        panic!("connection lost while waiting for {what}: {reason}");
                    }
                    if let Some(v) = pick(&u) {
                        return v;
                    }
                }
                None => panic!("session ended while waiting for {what}"),
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(v) => v,
        Err(_) => panic!("timed out after {secs} s waiting for {what}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server"]
async fn live_session_json_diagnostics_save_and_reload() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-session] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("ember-sess-{}", &uuid_short()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let doc_path = ws.join("data.json");
    std::fs::write(&doc_path, DOC_TEXT).unwrap();

    let server = Server::start(&bin, &root);
    let addr = format!("127.0.0.1:{}", server.port);
    let deadline = Instant::now() + Duration::from_secs(120);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        assert!(Instant::now() < deadline, "server not listening on {addr}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let t = Instant::now();
    let config = SessionConfig::new(addr.clone(), ws.to_str().unwrap());
    let (session, mut rx) = tokio::time::timeout(Duration::from_secs(120), EditorSession::connect_tcp(config))
        .await
        .expect("connect timed out")
        .unwrap_or_else(|e| panic!("connect: {e}"));
    println!("[live-session] connected in {:.1} s", t.elapsed().as_secs_f64());

    let uri = session.uri(doc_path.to_str().unwrap());
    session.open(uri.clone()).unwrap();
    let generation = wait_for("Opened", 30, &mut rx, |u| match u {
        SessionUpdate::Opened { generation, text, language_id, .. } => {
            assert_eq!(text, DOC_TEXT);
            assert_eq!(language_id, "json");
            Some(*generation)
        }
        SessionUpdate::OpenFailed { error, .. } => panic!("open failed: {error}"),
        _ => None,
    })
    .await;

    // `"ok": true` → `"ok": true,,` : insert at widget (line 2, col 12).
    let edit = WidgetEvent::CodeChanged { version: 1, range: WidgetRange::empty(WidgetPos::new(2, 12)), text: ",,".into() };
    session.widget_event(uri.clone(), generation, edit).unwrap();

    let t = Instant::now();
    let errors = wait_for("an Error underline for data.json", 120, &mut rx, |u| match u {
        SessionUpdate::Decorations { uri: du, decorations, .. } if du.path == uri.path => {
            let errs: Vec<_> = decorations
                .iter()
                .filter(|d| d.kind == DecorationKind::Underline && d.severity == Some(Severity::Error))
                .cloned()
                .collect();
            (!errs.is_empty()).then_some(errs)
        }
        SessionUpdate::Desync { error, .. } => panic!("desync: {error}"),
        _ => None,
    })
    .await;
    println!("[live-session] diagnostics after {:.1} s", t.elapsed().as_secs_f64());
    for d in &errors {
        println!("[live-session]   id {:#x} v{} {:?}", d.id.get(), d.version, d.range);
        assert_eq!(d.version, 1);
        assert_ne!(d.id.get(), 0);
    }
    assert!(errors.iter().any(|d| d.range.start.line == 2), "an error on the edited line: {errors:?}");

    // Save.
    session.widget_event(uri.clone(), generation, WidgetEvent::CodeSaveRequested { version: 1 }).unwrap();
    wait_for("Saved", 30, &mut rx, |u| match u {
        SessionUpdate::Saved { .. } => Some(()),
        SessionUpdate::SaveFailed { error, .. } => panic!("save failed: {error}"),
        _ => None,
    })
    .await;
    assert_eq!(std::fs::read_to_string(&doc_path).unwrap(), DOC_TEXT_AFTER_EDIT);

    // Change it behind the server's back: a clean buffer reloads with a new generation. The
    // watcher starts asynchronously, so rewrite every 3 s until the reload arrives.
    let (gen2, text) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            std::fs::write(&doc_path, DOC_TEXT_EXTERNAL).unwrap();
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(u) = tokio::time::timeout_at(until, rx.recv()).await {
                match u {
                    Some(SessionUpdate::Command { command: WidgetCommand::SetText { generation, text }, .. }) => {
                        return (generation, text);
                    }
                    Some(SessionUpdate::ConnectionLost { reason }) => panic!("connection lost: {reason}"),
                    Some(_) => {}
                    None => panic!("session ended while waiting for the reload"),
                }
            }
        }
    })
    .await
    .expect("no reload after the external change");
    assert_ne!(gen2, generation);
    assert_eq!(text, DOC_TEXT_EXTERNAL);

    session.close(uri).unwrap();
    session.shutdown().await;
    drop(server);
}

fn uuid_short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}
