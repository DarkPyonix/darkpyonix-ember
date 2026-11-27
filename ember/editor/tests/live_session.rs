//! `EditorSession` against a real OSE server. Ignored by default and gated on `EMBER_OSE_SERVER`
//! (the same server binary `ember/editor-conn/tests/live_ose.rs` uses):
//!
//! ```sh
//! export EMBER_OSE_SERVER="$(ember/editor-conn/scripts/fetch-ose-artifact.sh)"   # .../bin/dpx-ose-server
//! cargo test --manifest-path ember/editor/Cargo.toml --test live_session -- --ignored --nocapture
//! ```
//!
//! Flow: start the server on a free port (flags as `ember/proxy/dpx`), `EditorSession::connect_tcp` on
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
    DecorationKind, EditorSession, HoverPhase, SessionConfig, SessionUpdate, Severity, WidgetCommand, WidgetEvent, WidgetPos,
    WidgetRange,
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
        Self::start_with(bin, root, None)
    }

    /// As [`Server::start`], with `--extensions-dir` when `extensions` is given (the probe case).
    fn start_with(bin: &Path, root: &Path, extensions: Option<&Path>) -> Self {
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
            .args(extensions.map(|e| vec!["--extensions-dir".into(), e.as_os_str().to_owned()]).unwrap_or_default())
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
    let root = std::env::temp_dir().join(format!("ember-sess-{}", uuid_short()));
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

/// Every log the server wrote below `root`: its stdout and stderr, and the files of its data dir.
fn server_logs(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "log") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(root, &mut out);
    out
}

/// FR-L31.24 (#789): an external change reaches the session when the workspace is on an external
/// volume. `EMBER_LIVE_WS_ROOT` names a directory to create the workspace in; on macOS point it
/// below `/Volumes/<disk>` (unset: the temp dir, where the test only repeats the reload check).
/// The server must not log that it refuses to watch a workspace path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server"]
async fn live_fr_l31_24_external_change_under_ws_root() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-session] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let base = std::env::var_os("EMBER_LIVE_WS_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("ember-watch-{}", uuid_short()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    println!("[live-session] workspace root {}", root.display());
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let doc_path = ws.join("sub").join("data.json");
    std::fs::write(&doc_path, DOC_TEXT).unwrap();

    let mut server = Server::start(&bin, &root);
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
    let uri = session.uri(doc_path.to_str().unwrap());
    session.open(uri.clone()).unwrap();
    let generation = wait_for("Opened", 30, &mut rx, |u| match u {
        SessionUpdate::Opened { generation, .. } => Some(*generation),
        SessionUpdate::OpenFailed { error, .. } => panic!("open failed: {error}"),
        _ => None,
    })
    .await;

    let reloaded = tokio::time::timeout(Duration::from_secs(30), async {
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
    .await;

    session.close(uri).unwrap();
    session.shutdown().await;
    server.stop();
    let logs = server_logs(&root);
    // The server's own data dir is below `root` here, and the OSE watches it with `fs.watch()` by
    // itself; only the workspace is the session's business.
    let ws_str = ws.to_str().unwrap();
    let refused: Vec<&str> =
        logs.lines().filter(|l| l.contains("possibly being a network share") && l.contains(ws_str)).collect();
    assert!(refused.is_empty(), "the OSE refused to watch a workspace path: {refused:#?}");
    let (gen2, text) = reloaded.expect("no reload after the external change");
    assert_ne!(gen2, generation);
    assert_eq!(text, DOC_TEXT_EXTERNAL);
}

fn uuid_short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}

/// Explorer edits through the OSE file channel (D38, #438): create, rename and delete on disk, never
/// overwriting, and refusing paths the explorer must not touch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server"]
async fn live_session_explorer_create_rename_delete() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-session] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("ember-explorer-{}", uuid_short()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ws_str = ws.to_str().unwrap().to_string();

    let server = Server::start(&bin, &root);
    let addr = format!("127.0.0.1:{}", server.port);
    let deadline = Instant::now() + Duration::from_secs(120);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        assert!(Instant::now() < deadline, "server not listening on {addr}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let config = SessionConfig::new(addr.clone(), ws_str.clone());
    let (session, mut rx) = tokio::time::timeout(Duration::from_secs(120), EditorSession::connect_tcp(config))
        .await
        .expect("connect timed out")
        .unwrap_or_else(|e| panic!("connect: {e}"));

    // Create a file and a directory; an existing name is an error and the file is left as it was.
    let file = session.create_file(&ws_str, "a.txt").await.expect("create_file");
    assert_eq!(file, format!("{ws_str}/a.txt"));
    assert!(ws.join("a.txt").is_file());
    std::fs::write(ws.join("a.txt"), "keep").unwrap();
    assert!(session.create_file(&ws_str, "a.txt").await.is_err(), "an existing file is not overwritten");
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "keep");
    let dir = session.create_dir(&ws_str, "sub").await.expect("create_dir");
    assert!(ws.join("sub").is_dir());
    session.create_file(&dir, "inner.txt").await.expect("create in a subdirectory");

    // Rename: new name in the same directory; an existing target is an error, nothing is lost.
    let renamed = session.rename_path(&file, "b.txt").await.expect("rename");
    assert_eq!(renamed, format!("{ws_str}/b.txt"));
    assert!(!ws.join("a.txt").exists() && ws.join("b.txt").is_file());
    std::fs::write(ws.join("c.txt"), "other").unwrap();
    assert!(session.rename_path(&renamed, "c.txt").await.is_err(), "rename never overwrites");
    assert_eq!(std::fs::read_to_string(ws.join("c.txt")).unwrap(), "other");
    assert!(ws.join("b.txt").is_file());

    // Delete a file, then a directory with its contents.
    session.delete_path(&renamed, false).await.expect("delete file");
    assert!(!ws.join("b.txt").exists());
    session.delete_path(&dir, true).await.expect("delete directory");
    assert!(!ws.join("sub").exists());

    // Symbolic links: a link is renamed and deleted as itself, never followed, and nothing is made inside one.
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious.txt"), "do not touch").unwrap();
    let link = ws.join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let link_str = link.to_str().unwrap().to_string();
    assert!(
        matches!(session.create_file(&link_str, "x.txt").await, Err(ember_editor::Error::Path(ember_editor::paths::PathError::Symlink))),
        "nothing is created inside a symlinked directory"
    );
    assert!(!outside.join("x.txt").exists());
    let moved = session.rename_path(&link_str, "link2").await.expect("rename the link itself");
    assert!(std::fs::symlink_metadata(ws.join("link2")).unwrap().file_type().is_symlink());
    assert!(!ws.join("link").exists() && outside.join("precious.txt").is_file());
    session.delete_path(&moved, true).await.expect("delete the link");
    assert!(std::fs::symlink_metadata(ws.join("link2")).is_err(), "the link is gone");
    assert_eq!(std::fs::read_to_string(outside.join("precious.txt")).unwrap(), "do not touch", "what the link pointed at is untouched");

    // Symbolic link in the middle of a path:
    let mid_dir = ws.join("mid_parent");
    std::fs::create_dir_all(&mid_dir).unwrap();
    let mid_link = mid_dir.join("mid_link");
    std::os::unix::fs::symlink(&outside, &mid_link).unwrap();
    let mid_path = format!("{ws_str}/mid_parent/mid_link/nested");
    assert!(
        matches!(session.create_file(&mid_path, "escape.txt").await, Err(ember_editor::Error::Path(ember_editor::paths::PathError::Symlink))),
        "symlink in the middle of a path is refused"
    );

    // Open file rename: dirty document is refused, clean document succeeds
    let open_file = session.create_file(&ws_str, "open.txt").await.expect("create open.txt");
    let open_uri = session.uri(&open_file);
    session.open(open_uri.clone()).unwrap();
    let generation = wait_for("Opened open.txt", 30, &mut rx, |u| match u {
        SessionUpdate::Opened { uri, generation, .. } if uri.path == open_uri.path => Some(*generation),
        _ => None,
    }).await;
    session.widget_event(open_uri.clone(), generation, WidgetEvent::CodeChanged {
        version: 1,
        range: WidgetRange::empty(WidgetPos::new(0, 0)),
        text: "dirty text".into(),
    }).unwrap();
    assert!(
        matches!(session.rename_path(&open_file, "open2.txt").await, Err(ember_editor::Error::Path(ember_editor::paths::PathError::Dirty))),
        "rename of an open dirty file is refused"
    );
    session.widget_event(open_uri.clone(), generation, WidgetEvent::CodeSaveRequested { version: 1 }).unwrap();
    wait_for("Saved open.txt", 30, &mut rx, |u| match u {
        SessionUpdate::Saved { uri } if uri.path == open_uri.path => Some(()),
        _ => None,
    }).await;
    let open_renamed = session.rename_path(&open_file, "open2.txt").await.expect("rename clean file");
    assert_eq!(open_renamed, format!("{ws_str}/open2.txt"));
    assert!(!ws.join("open.txt").exists() && ws.join("open2.txt").is_file());
    session.close(open_uri).unwrap();

    // Paths the explorer must not touch are refused before anything is sent.
    use ember_editor::paths::PathError;
    let refused = |r: Result<String, ember_editor::Error>| matches!(r, Err(ember_editor::Error::Path(_)));
    assert!(refused(session.create_file(&ws_str, "../escape.txt").await));
    assert!(refused(session.create_file(root.to_str().unwrap(), "x.txt").await));
    assert!(refused(session.rename_path(&ws_str, "x").await));
    assert!(matches!(session.delete_path(&ws_str, true).await, Err(ember_editor::Error::Path(PathError::FolderRoot))));
    assert!(ws.is_dir());
    assert!(!root.join("escape.txt").exists());
    session.shutdown().await;
}

/// SPEC FR-E12 (#32 phase A step 2): hover, CodeLens and inline completion from a real extension
/// reach the session through the extension host and Ember's bridges. The probe extension
/// (`.github/scripts/ose/regression/probe/`, packed by `pack.py`) answers each with a fixed marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server, and python3"]
async fn live_session_probe_hover_codelens_inline() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-session] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("ember-probe-{}", uuid_short()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let doc_path = ws.join("probe.txt");
    std::fs::write(&doc_path, "hello probe world\nsecond line\n").unwrap();

    // Pack and install the probe the way the regression harness installs extensions.
    let vsix = root.join("ember-probe.vsix");
    let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/scripts/ose/regression/probe/pack.py");
    let st = Command::new("python3").arg("-I").arg(&pack).arg(&vsix).status().expect("python3");
    assert!(st.success(), "pack.py failed");
    let ext = root.join("ext");
    let data = root.join("data");
    let out = Command::new(&bin)
        .arg("--install-extension")
        .arg(&vsix)
        .arg("--extensions-dir")
        .arg(&ext)
        .arg("--server-data-dir")
        .arg(&data)
        .arg("--accept-server-license-terms")
        .stdin(Stdio::null())
        .output()
        .expect("install the probe");
    assert!(out.status.success(), "install failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    let server = Server::start_with(&bin, &root, Some(&ext));
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

    let uri = session.uri(doc_path.to_str().unwrap());
    session.open(uri.clone()).unwrap();
    let generation = wait_for("Opened", 30, &mut rx, |u| match u {
        SessionUpdate::Opened { generation, language_id, .. } => {
            assert_eq!(language_id, "plaintext");
            Some(*generation)
        }
        SessionUpdate::OpenFailed { error, .. } => panic!("open failed: {error}"),
        _ => None,
    })
    .await;

    // CodeLens: asked for when the document opens and its provider registers.
    let t = Instant::now();
    wait_for("the probe's CodeLens", 120, &mut rx, |u| match u {
        SessionUpdate::Decorations { uri: du, decorations, .. } if du.path == uri.path => decorations
            .iter()
            .any(|d| d.kind == DecorationKind::CodeLens && d.text.contains("ember-probe lens"))
            .then_some(()),
        _ => None,
    })
    .await;
    println!("[live-session] CodeLens after {:.1} s", t.elapsed().as_secs_f64());

    // Hover: rest on `probe` (line 0, column 7) until the provider answers.
    let t = Instant::now();
    let hover = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let rest = WidgetEvent::CodeHovered { decoration: 0, pos: WidgetPos::new(0, 7), phase: HoverPhase::Rest };
            session.widget_event(uri.clone(), generation, rest).unwrap();
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(Some(u)) = tokio::time::timeout_at(until, rx.recv()).await {
                if let SessionUpdate::Hover(h) = &u {
                    if let Some(m) = h.contents.iter().find(|m| m.value.contains("ember-probe hover")) {
                        return m.value.clone();
                    }
                }
            }
        }
    })
    .await
    .expect("no hover from the probe");
    assert_eq!(hover, "ember-probe hover: probe");
    println!("[live-session] hover after {:.1} s", t.elapsed().as_secs_f64());

    // Inline completion: ask at the end of line 1 until ghost text arrives.
    let t = Instant::now();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            session.trigger_inline_completion(uri.clone(), generation, WidgetPos::new(1, 11)).unwrap();
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(Some(u)) = tokio::time::timeout_at(until, rx.recv()).await {
                if let SessionUpdate::Decorations { uri: du, decorations, .. } = &u {
                    if du.path == uri.path
                        && decorations.iter().any(|d| d.kind == DecorationKind::GhostText && d.text.contains("ember-probe-ghost"))
                    {
                        return;
                    }
                }
            }
        }
    })
    .await
    .expect("no ghost text from the probe");
    println!("[live-session] ghost text after {:.1} s", t.elapsed().as_secs_f64());

    session.close(uri).unwrap();
    session.shutdown().await;
    drop(server);
}

/// Packs a minimal storage-probe VSIX into `vsix_path` for the live OSE storage persistence test.
fn pack_storage_probe_vsix(vsix_path: &Path) {
    let script = r#"
import json, sys, zipfile

package_json = {
    "name": "ember-storage-probe",
    "displayName": "Ember Storage Probe",
    "version": "0.0.1",
    "publisher": "ember-test",
    "engines": {"vscode": "^1.80.0"},
    "main": "./extension.js",
    "activationEvents": [
        "*",
        "onCommand:emberStorageProbe.write",
        "onCommand:emberStorageProbe.report"
    ],
    "contributes": {
        "commands": [
            {"command": "emberStorageProbe.write", "title": "Write Storage"},
            {"command": "emberStorageProbe.report", "title": "Report Storage"}
        ]
    }
}

extension_js = r'''
const vscode = require('vscode');

exports.activate = function (context) {
  context.subscriptions.push(vscode.commands.registerCommand('emberStorageProbe.write', async (gVal, wVal) => {
    await context.globalState.update('gKey', gVal);
    await context.workspaceState.update('wKey', wVal);
    vscode.window.showInformationMessage('WROTE:' + JSON.stringify({ g: context.globalState.get('gKey', null), w: context.workspaceState.get('wKey', null) }));
  }));
  context.subscriptions.push(vscode.commands.registerCommand('emberStorageProbe.report', async () => {
    vscode.window.showInformationMessage('REPORT:' + JSON.stringify({ g: context.globalState.get('gKey', null), w: context.workspaceState.get('wKey', null) }));
  }));
};

exports.deactivate = function () {};
'''

manifest = '''<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="ember-storage-probe" Version="0.0.1" Publisher="ember-test"/>
    <DisplayName>Ember Storage Probe</DisplayName>
    <Description>Test probe for MainThreadStorage persistence</Description>
  </Metadata>
  <Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation>
  <Dependencies/>
  <Assets><Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/></Assets>
</PackageManifest>'''

content_types = '''<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".json" ContentType="application/json"/>
  <Default Extension=".js" ContentType="application/javascript"/>
  <Default Extension=".vsixmanifest" ContentType="text/xml"/>
</Types>'''

with zipfile.ZipFile(sys.argv[1], 'w', zipfile.ZIP_DEFLATED) as z:
    z.writestr('[Content_Types].xml', content_types)
    z.writestr('extension.vsixmanifest', manifest)
    z.writestr('extension/package.json', json.dumps(package_json, indent=2))
    z.writestr('extension/extension.js', extension_js)
"#;
    let st = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(vsix_path)
        .status()
        .expect("python3");
    assert!(st.success(), "failed to pack storage probe vsix");
}

/// Live OSE test: `MainThreadStorage` persists `globalState` across sessions and workspaces,
/// and persists `workspaceState` per workspace while isolating different workspaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server, and python3"]
async fn live_session_main_thread_storage_persists_across_restarts() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-session] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("ember-storage-live-{}", uuid_short()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws_a = root.join("ws-a");
    let ws_b = root.join("ws-b");
    std::fs::create_dir_all(&ws_a).unwrap();
    std::fs::create_dir_all(&ws_b).unwrap();

    let vsix = root.join("ember-storage-probe.vsix");
    pack_storage_probe_vsix(&vsix);
    let ext = root.join("ext");
    let data = root.join("data");
    let out = Command::new(&bin)
        .arg("--install-extension")
        .arg(&vsix)
        .arg("--extensions-dir")
        .arg(&ext)
        .arg("--server-data-dir")
        .arg(&data)
        .arg("--accept-server-license-terms")
        .stdin(Stdio::null())
        .output()
        .expect("install storage probe");
    assert!(
        out.status.success(),
        "install failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let server = Server::start_with(&bin, &root, Some(&ext));
    let addr = format!("127.0.0.1:{}", server.port);
    let deadline = Instant::now() + Duration::from_secs(120);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        assert!(Instant::now() < deadline, "server not listening on {addr}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Helper: invoke a probe command, retrying briefly while the extension finishes registering
    // its runtime command handler after activation.
    async fn run_probe_cmd(
        session: &EditorSession,
        rx: &mut mpsc::UnboundedReceiver<SessionUpdate>,
        cmd: &str,
        args: Vec<serde_json::Value>,
        prefix: &str,
    ) -> String {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                session.execute_command(cmd, args.clone()).unwrap();
                let until = tokio::time::Instant::now() + Duration::from_secs(2);
                while let Ok(Some(u)) = tokio::time::timeout_at(until, rx.recv()).await {
                    match u {
                        SessionUpdate::ExtensionMessage(m) if m.message.starts_with(prefix) => {
                            return m.message;
                        }
                        SessionUpdate::CommandFailed { id, error } => {
                            panic!("command {id} failed: {error}");
                        }
                        SessionUpdate::ConnectionLost { reason } => {
                            panic!("connection lost while running {cmd}: {reason}");
                        }
                        SessionUpdate::RunCommand { command } if command.id == cmd => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            break;
                        }
                        _ => {}
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {prefix} from {cmd}"))
    }

    // Session 1 (ws-a): write both globalState and workspaceState.
    let (s1, mut rx1) = tokio::time::timeout(
        Duration::from_secs(120),
        EditorSession::connect_tcp(SessionConfig::new(addr.clone(), ws_a.to_str().unwrap())),
    )
    .await
    .expect("connect s1 timed out")
    .unwrap_or_else(|e| panic!("connect s1: {e}"));
    let wrote = run_probe_cmd(
        &s1,
        &mut rx1,
        "emberStorageProbe.write",
        vec![serde_json::json!("g-persisted"), serde_json::json!("w-alpha")],
        "WROTE:",
    )
    .await;
    println!("[live-session] s1 {wrote}");
    s1.shutdown().await;

    // Session 2 (ws-a): both globalState and workspaceState must be restored.
    let (s2, mut rx2) = tokio::time::timeout(
        Duration::from_secs(120),
        EditorSession::connect_tcp(SessionConfig::new(addr.clone(), ws_a.to_str().unwrap())),
    )
    .await
    .expect("connect s2 timed out")
    .unwrap_or_else(|e| panic!("connect s2: {e}"));
    let report_a = run_probe_cmd(&s2, &mut rx2, "emberStorageProbe.report", vec![], "REPORT:").await;
    println!("[live-session] s2 {report_a}");
    let payload_a: serde_json::Value = serde_json::from_str(report_a.trim_start_matches("REPORT:")).unwrap();
    assert_eq!(payload_a["g"], "g-persisted");
    assert_eq!(payload_a["w"], "w-alpha");
    s2.shutdown().await;

    // Session 3 (ws-b): globalState persists, workspaceState is isolated.
    let (s3, mut rx3) = tokio::time::timeout(
        Duration::from_secs(120),
        EditorSession::connect_tcp(SessionConfig::new(addr.clone(), ws_b.to_str().unwrap())),
    )
    .await
    .expect("connect s3 timed out")
    .unwrap_or_else(|e| panic!("connect s3: {e}"));
    let report_b = run_probe_cmd(&s3, &mut rx3, "emberStorageProbe.report", vec![], "REPORT:").await;
    println!("[live-session] s3 {report_b}");
    let payload_b: serde_json::Value = serde_json::from_str(report_b.trim_start_matches("REPORT:")).unwrap();
    assert_eq!(payload_b["g"], "g-persisted");
    assert!(payload_b["w"].is_null(), "workspaceState leaked across workspaces: {payload_b}");
    s3.shutdown().await;
    drop(server);
}
