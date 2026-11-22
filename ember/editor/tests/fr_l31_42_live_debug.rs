//! Debugging against a real OSE server: a Python file stops at a breakpoint, a variable is read and
//! the run continues. The adapter is the unmodified `ms-python.debugpy` extension from Open VSX,
//! run by the server's extension host, as in Code-OSS. Ignored by default and gated on
//! `EMBER_OSE_SERVER`, like `live_session.rs`; it needs the network to install the extensions and
//! `python3` on the PATH:
//!
//! ```sh
//! export EMBER_OSE_SERVER="$(ember/editor-conn/scripts/fetch-ose-artifact.sh)"   # .../bin/dpx-ose-server
//! cargo test --locked --manifest-path ember/editor/Cargo.toml --test fr_l31_42_live_debug -- --ignored --nocapture
//! ```
//!
//! `EMBER_DEBUGPY_EXTENSION` and `EMBER_PYTHON_EXTENSION` override the extension ids to install
//! (`publisher.name` or `publisher.name@version`), for a version that fits the server.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ember_editor::debugger::{ConfigSelector, DebugRequest, DebugUpdate, ScopeInfo, SessionInfo, SessionState, VariableInfo};
use ember_editor::{EditorSession, SessionConfig, SessionUpdate};
use tokio::sync::mpsc::UnboundedReceiver;

const MAIN_PY: &str = "x = 41\ny = x + 1\nprint(y)\n";
const LAUNCH_JSON: &str = r#"{
    "version": "0.2.0",
    "configurations": [
        { "type": "debugpy", "request": "launch", "name": "Python: Current File", "program": "${file}", "console": "internalConsole" }
    ]
}"#;
const CONFIG_NAME: &str = "Python: Current File";

struct Server {
    child: Option<Child>,
    port: u16,
    root: PathBuf,
}

impl Server {
    fn start(bin: &Path, root: &Path, extensions: &Path) -> Self {
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
            eprintln!("[live-debug] kept {}", self.root.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// What the debug updates said so far.
#[derive(Default)]
struct Seen {
    sessions: Vec<SessionInfo>,
    terminated: bool,
    console: String,
    scopes: Vec<ScopeInfo>,
    variables: Vec<VariableInfo>,
    failed: Option<String>,
}

impl Seen {
    fn stopped(&self) -> bool {
        self.sessions.iter().any(|s| matches!(s.state, SessionState::Stopped { .. }))
    }

    fn take(&mut self, update: DebugUpdate) {
        match update {
            DebugUpdate::Sessions { sessions, .. } => {
                self.terminated |= sessions.iter().any(|s| s.state == SessionState::Terminated);
                self.sessions = sessions;
            }
            DebugUpdate::Console { entry, .. } => self.console.push_str(&entry.text),
            DebugUpdate::Scopes { scopes, .. } => self.scopes = scopes,
            DebugUpdate::Variables { variables, .. } => self.variables = variables,
            DebugUpdate::StartFailed { message } => self.failed = Some(message),
            _ => {}
        }
    }
}

/// Read updates into `seen` until `done` says so; `false` on timeout.
async fn pump(rx: &mut UnboundedReceiver<SessionUpdate>, seen: &mut Seen, until: Duration, mut done: impl FnMut(&Seen) -> bool) -> bool {
    let end = tokio::time::Instant::now() + until;
    loop {
        if done(seen) {
            return true;
        }
        match tokio::time::timeout_at(end, rx.recv()).await {
            Ok(Some(SessionUpdate::ConnectionLost { reason })) => panic!("connection lost: {reason}"),
            Ok(Some(SessionUpdate::Debug(update))) => seen.take(update),
            Ok(Some(_)) => {}
            Ok(None) => panic!("session ended"),
            Err(_) => return false,
        }
    }
}

fn install(bin: &Path, root: &Path, extensions: &Path, spec: &str) {
    let out = Command::new(bin)
        .args(["--install-extension", spec, "--extensions-dir"])
        .arg(extensions)
        .arg("--server-data-dir")
        .arg(root.join("data"))
        .arg("--accept-server-license-terms")
        .stdin(Stdio::null())
        .output()
        .expect("install an extension");
    assert!(out.status.success(), "installing {spec} failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server (EMBER_OSE_SERVER), python3 and the network for Open VSX"]
async fn fr_l31_42_live_python_stops_at_a_breakpoint_reads_a_variable_and_runs_on() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-debug] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    if Command::new("python3").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(true, |s| !s.success()) {
        eprintln!("[live-debug] python3 is not on the PATH; skipping");
        return;
    }
    let root = std::env::temp_dir().join(format!("ember-debug-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join(".vscode")).unwrap();
    let main_path = ws.join("main.py");
    std::fs::write(&main_path, MAIN_PY).unwrap();
    std::fs::write(ws.join(".vscode").join("launch.json"), LAUNCH_JSON).unwrap();

    let ext = root.join("ext");
    for (var, default) in [("EMBER_PYTHON_EXTENSION", "ms-python.python"), ("EMBER_DEBUGPY_EXTENSION", "ms-python.debugpy")] {
        install(&bin, &root, &ext, &std::env::var(var).unwrap_or_else(|_| default.to_owned()));
    }

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
    let mut seen = Seen::default();

    // The active editor is what `${file}` stands for.
    let uri = session.uri(main_path.to_str().unwrap());
    session.open(uri.clone()).unwrap();
    let end = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match tokio::time::timeout_at(end, rx.recv()).await {
            Ok(Some(SessionUpdate::Opened { .. })) => break,
            Ok(Some(SessionUpdate::ConnectionLost { reason })) => panic!("connection lost: {reason}"),
            Ok(Some(_)) => {}
            Ok(None) => panic!("session ended"),
            Err(_) => panic!("main.py did not open"),
        }
    }
    session.focus(uri.clone()).unwrap();
    session.debug(DebugRequest::ToggleBreakpoint { uri: uri.clone(), line: 2 }).unwrap();

    // The debug types arrive when the extension host has read the extensions; start again until
    // the type is known.
    let t = Instant::now();
    let mut started = false;
    for attempt in 0..30 {
        seen.failed = None;
        session.start_debugging(ConfigSelector::Name(CONFIG_NAME.into())).unwrap();
        let outcome = pump(&mut rx, &mut seen, Duration::from_secs(20), |s| s.failed.is_some() || s.stopped()).await;
        match (&seen.failed, outcome) {
            (None, true) => {
                started = true;
                break;
            }
            (Some(why), _) if why.contains("is not supported") => {
                eprintln!("[live-debug] attempt {attempt}: {why}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            (Some(why), _) => panic!("the session did not start: {why}"),
            (None, false) => break,
        }
    }
    assert!(started, "no stop at the breakpoint; the console said {:?}", seen.console);
    println!("[live-debug] stopped after {:.1} s", t.elapsed().as_secs_f64());

    // The stop loaded the stack and the scopes of its top frame; read `x` from the locals.
    let found = pump(&mut rx, &mut seen, Duration::from_secs(30), |s| !s.scopes.is_empty()).await;
    assert!(found, "no scopes after the stop");
    let locals = seen.scopes.iter().find(|s| s.name.to_lowercase().contains("local")).unwrap_or(&seen.scopes[0]).variables_reference;
    let id = seen.sessions.iter().find(|s| matches!(s.state, SessionState::Stopped { .. })).expect("a stopped session").id.clone();
    let thread = match &seen.sessions.iter().find(|s| s.id == id).unwrap().state {
        SessionState::Stopped { thread_id, .. } => thread_id.expect("the stop names a thread"),
        other => panic!("not stopped: {other:?}"),
    };
    session.debug(DebugRequest::Variables { session: id.clone(), reference: locals }).unwrap();
    let got = pump(&mut rx, &mut seen, Duration::from_secs(30), |s| s.variables.iter().any(|v| v.name == "x")).await;
    assert!(got, "no variable x; got {:?}", seen.variables);
    let x = seen.variables.iter().find(|v| v.name == "x").unwrap();
    assert_eq!(x.value, "41", "{:?}", seen.variables);

    // Continue: the program prints 42 and the session ends.
    session.debug(DebugRequest::Continue { session: id, thread_id: thread }).unwrap();
    let ended = pump(&mut rx, &mut seen, Duration::from_secs(60), |s| s.terminated).await;
    assert!(ended, "the session did not end; the console said {:?}", seen.console);
    assert!(seen.console.contains("42"), "the console said {:?}", seen.console);

    session.shutdown().await;
    drop(server);
}
