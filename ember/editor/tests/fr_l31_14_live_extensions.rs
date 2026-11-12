//! FR-L31.14 (#446) against a real OSE server: the installed list comes from the management
//! channel `remoteExtensionsScanner`, at connect and again on request, and a disabled id is
//! listed but not handed to the extension host. Ignored by default and gated on
//! `EMBER_OSE_SERVER`, like `live_session.rs`:
//!
//! ```sh
//! export EMBER_OSE_SERVER="$(ember/editor-conn/scripts/fetch-ose-artifact.sh)"   # .../bin/dpx-ose-server
//! cargo test --manifest-path ember/editor/Cargo.toml --test fr_l31_14_live_extensions -- --ignored --nocapture
//! ```
//!
//! Not covered here: an install or an uninstall through the server CLI while this OSE runs
//! (whether the next scan shows it is unverified, see SPEC FR-L31.14).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ember_editor::{EditorSession, SessionConfig};

/// A built-in extension every OSE build carries (the one `live_session.rs` gets diagnostics from).
const BUILTIN: &str = "vscode.json-language-features";

struct Server {
    child: Option<Child>,
    port: u16,
    root: PathBuf,
}

impl Server {
    fn start(bin: &Path, root: &Path) -> Self {
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
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
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

async fn connect(addr: &str, ws: &Path, disabled: &[&str]) -> EditorSession {
    let mut config = SessionConfig::new(addr.to_string(), ws.to_str().unwrap());
    config.disabled_extensions = disabled.iter().map(|s| s.to_string()).collect();
    let (session, _updates) = tokio::time::timeout(Duration::from_secs(120), EditorSession::connect_tcp(config))
        .await
        .expect("connect timed out")
        .unwrap_or_else(|e| panic!("connect: {e}"));
    session
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server"]
async fn fr_l31_14_live_scan_lists_extensions_and_a_disabled_one_is_not_run() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-extensions] EMBER_OSE_SERVER not set; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("ember-ext-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0)));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let server = Server::start(&bin, &root);
    let addr = format!("127.0.0.1:{}", server.port);
    let deadline = Instant::now() + Duration::from_secs(120);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        assert!(Instant::now() < deadline, "server not listening on {addr}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Nothing disabled: the built-in extension is listed and running.
    let session = connect(&addr, &ws, &[]).await;
    let installed = session.installed_extensions().to_vec();
    assert!(!installed.is_empty(), "the scan lists the built-in extensions");
    let json = installed.iter().find(|e| e.id.eq_ignore_ascii_case(BUILTIN)).unwrap_or_else(|| panic!("{BUILTIN} not in the scan"));
    assert!(json.builtin);
    assert!(!json.version.is_empty() && !json.display_name.is_empty());
    assert!(session.running_extensions().iter().any(|e| e.id.eq_ignore_ascii_case(BUILTIN)));

    // The scan can be read again on the same management connection and answers the same list.
    let again = session.rescan_extensions().await.unwrap_or_else(|e| panic!("rescan: {e}"));
    assert_eq!(again, installed);
    drop(session);

    // Disabled: still listed, not handed to the extension host.
    let session = connect(&addr, &ws, &["VSCODE.JSON-language-features"]).await;
    assert!(session.installed_extensions().iter().any(|e| e.id.eq_ignore_ascii_case(BUILTIN)));
    assert!(!session.running_extensions().iter().any(|e| e.id.eq_ignore_ascii_case(BUILTIN)));
    assert_eq!(session.installed_extensions().len(), session.running_extensions().len() + 1);
}
