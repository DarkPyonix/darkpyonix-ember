//! End-to-end test against a real OSE server (Code-OSS [`PINNED_VERSION`], the build CI makes from
//! `build/ose/VERSION`). Ignored by default and gated on `EMBER_OSE_SERVER`:
//!
//! ```sh
//! export EMBER_OSE_SERVER="$(crates/editor-conn/scripts/fetch-ose-artifact.sh)"   # .../bin/dpx-ose-server
//! cargo test --manifest-path crates/editor-conn/Cargo.toml --test live_ose -- --ignored --nocapture
//! ```
//!
//! Optional: `EMBER_OSE_VERBOSE=1` prints every extension-host → Ember call (and extension
//! `console.*` output); `EMBER_OSE_KEEP=1` keeps the temp directory (server data dir, logs,
//! workspace) even on success. On failure it is always kept and the server / extension-host logs
//! are printed.
//!
//! Flow (each step prints its duration):
//!
//! 0. Start `dpx-ose-server` with the flags `web/proxy/dpx` uses (`--host 127.0.0.1 --port P
//!    --without-connection-token --accept-server-license-terms --server-data-dir D
//!    --telemetry-level off`) on a free port, with `extensions.verifySignature: false` in
//!    `D/data/Machine/settings.json` like `build/ose/smoke.sh`, in its own process group; wait for the
//!    port.
//! 1. `verify_server` (`GET /version`) → commit must equal [`PINNED_COMMIT`].
//! 2. Management connection over plain TCP (`skipWebSocketFrames`): handshake, IPC client,
//!    `remoteextensionsenvironment.getEnvironmentData`, then `remoteFilesystem`
//!    mkdir / writeFile / stat / readFile / readdir / rename / stat (EntryNotFound) / delete in a
//!    temp workspace folder, then watch a file, change it with `std::fs`, and receive `fileChange`.
//! 3. `remoteExtensionsScanner.scanExtensions`: the built-in `vscode.json-language-features` must
//!    be in it (the server scans `<appRoot>/extensions` itself; nothing extra is installed).
//! 4. Extension-host connection: handshake, Ready → init data → Initialized,
//!    `$initializeConfiguration` (defaults from every scanned extension's
//!    `contributes.configuration`), `$initializeWorkspace` (the temp folder), an editor tab model
//!    (best effort), then `DocumentBridge::open_in_editor` (`$activateByEvent("onLanguage:json")`
//!    and `$acceptDocumentsAndEditorsDelta`) for a valid JSON file, a `DocumentBridge` edit that
//!    makes it invalid (`$acceptModelChanged`), and finally `MainThreadDiagnostics.$changeMany`
//!    with error markers for that file, produced by the JSON language server.
//! 5. Shutdown: `Terminate` to the extension host, `Disconnect` on both connections, check the
//!    server is still alive, then SIGTERM its process group (SIGKILL after 10 s).
//!
//! Every extension-host → Ember request is answered with [`exthost::default_reply`] except the
//! few overridden in [`live_reply`]; the test is also a probe for which replies the extension
//! host really needs (run with `EMBER_OSE_VERBOSE=1` to see them).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use ember_editor_conn::document::{DocumentBridge, EditorChange, TextEdit};
use ember_editor_conn::exthost::{
    self, InitDataParams, MainThreadCall, MarkerData, Range, WorkspaceData, WorkspaceFolder,
};
use ember_editor_conn::handshake::{self, ConnectOptions, ConnectionType, ExtensionHostStartParams};
use ember_editor_conn::ipc::IpcClient;
use ember_editor_conn::management::{self, RemoteAgentConnectionContext};
use ember_editor_conn::remote_fs::{
    self, DeleteOptions, FileChange, FileChangeType, RemoteFs, WatchEvent, WatchOptions, WriteOptions,
};
use ember_editor_conn::rpc::{Arg, Reply, RpcEvent, RpcPeer};
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::{Error, PINNED_COMMIT, PINNED_VERSION};

const JSON_EXTENSION: &str = "vscode.json-language-features";
/// Valid JSON; line 3 is `  "ok": true` (`true` spans columns 9..13).
const DOC_TEXT: &str = "{\n  \"name\": \"ember\",\n  \"ok\": true\n}\n";
const DOC_TEXT_AFTER_EDIT: &str = "{\n  \"name\": \"ember\",\n  \"ok\": true,,\n}\n";

// ---- small helpers ----------------------------------------------------------------------------

/// Step timings, printed as they happen and as a table at the end.
struct Steps {
    t0: Instant,
    last: Instant,
    rows: Vec<(String, Duration)>,
}

impl Steps {
    fn new() -> Self {
        let now = Instant::now();
        Self { t0: now, last: now, rows: Vec::new() }
    }

    fn done(&mut self, name: &str) {
        let now = Instant::now();
        let d = now - self.last;
        self.last = now;
        println!(
            "[live-ose] {name:<58} {:>9.1} ms   (t+{:.2} s)",
            d.as_secs_f64() * 1000.0,
            (now - self.t0).as_secs_f64()
        );
        self.rows.push((name.to_owned(), d));
    }

    fn summary(&self) {
        println!("[live-ose] ---- timings ----");
        for (name, d) in &self.rows {
            println!("[live-ose] {name:<58} {:>9.1} ms", d.as_secs_f64() * 1000.0);
        }
        println!("[live-ose] {:<58} {:>9.1} ms", "TOTAL", self.t0.elapsed().as_secs_f64() * 1000.0);
    }
}

async fn within<T>(what: &str, secs: u64, fut: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(v) => v,
        Err(_) => panic!("timed out after {secs} s: {what}"),
    }
}

async fn dial(addr: &str) -> TcpStream {
    let s = TcpStream::connect(addr).await.unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    let _ = s.set_nodelay(true);
    s
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0").local_addr().unwrap().port()
}

fn path_str(p: &Path) -> String {
    p.to_str().unwrap_or_else(|| panic!("non-UTF-8 path {}", p.display())).to_owned()
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}

/// Print the last `n` lines of every `*.log` under `dir` (server log, `remoteagent.log`,
/// `exthost*/remoteexthost.log`, the JSON language server's output channel, …).
fn dump_logs(dir: &Path, n: usize) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "log") {
                let lines: Vec<String> = std::fs::File::open(&p)
                    .map(|f| BufReader::new(f).lines().map_while(Result::ok).collect())
                    .unwrap_or_default();
                eprintln!("[live-ose] ==== {} (last {n} of {} lines) ====", p.display(), lines.len());
                for l in &lines[lines.len().saturating_sub(n)..] {
                    eprintln!("  {l}");
                }
            }
        }
    }
}

// ---- the server process -----------------------------------------------------------------------

/// A `dpx-ose-server` child in its own process group. Dropping it stops the whole group (the
/// launcher is a shell script that does not `exec` node, and node spawns the extension host and
/// language servers), prints the logs if the test is failing, and removes the temp dir on success.
struct Server {
    child: Option<Child>,
    port: u16,
    root: PathBuf,
    keep: bool,
}

impl Server {
    fn start(bin: &Path, root: &Path) -> Self {
        let data = root.join("data");
        // build/ose/smoke.sh and web/proxy/dpx/vscode/runtime.py: Code-OSS has no Marketplace signature
        // verifier, so OSE runs with signature verification off.
        let machine = data.join("data").join("Machine");
        std::fs::create_dir_all(&machine).unwrap();
        std::fs::write(machine.join("settings.json"), "{ \"extensions.verifySignature\": false }\n").unwrap();

        let port = free_port();
        let log = std::fs::File::create(root.join("server.log")).unwrap();
        let log_err = log.try_clone().unwrap();
        let mut cmd = Command::new(bin);
        cmd.arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--without-connection-token")
            .arg("--accept-server-license-terms")
            .arg("--server-data-dir")
            .arg(&data)
            .arg("--telemetry-level")
            .arg("off")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        Self { child: Some(child), port, root: root.to_path_buf(), keep: std::env::var_os("EMBER_OSE_KEEP").is_some() }
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// `Some(status)` if the launcher has exited.
    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.as_mut().and_then(|c| c.try_wait().ok().flatten())
    }

    /// Signal the whole process group (pgid = launcher pid, see `process_group(0)`).
    fn signal_group(&mut self, sig: &str) {
        let Some(pid) = self.pid() else { return };
        let ok = Command::new("pkill")
            .arg(format!("-{sig}"))
            .arg("-g")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !ok {
            if let Some(c) = self.child.as_mut() {
                let _ = c.kill();
            }
        }
    }

    /// SIGTERM the group, wait up to 10 s, then SIGKILL. Returns the launcher's exit status.
    fn stop(&mut self) -> Option<ExitStatus> {
        self.child.as_ref()?;
        self.signal_group("TERM");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(st) = self.exited() {
                self.signal_group("KILL"); // stragglers (extension host, language servers)
                self.child = None;
                return Some(st);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.signal_group("KILL");
        let st = self.child.as_mut().and_then(|c| c.wait().ok());
        self.child = None;
        st
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let failing = std::thread::panicking();
        if failing {
            dump_logs(&self.root, 60);
        }
        self.stop();
        if failing || self.keep {
            eprintln!("[live-ose] kept {}", self.root.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

async fn wait_listening(server: &mut Server, addr: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if let Some(st) = server.exited() {
            panic!("server exited before listening: {st}");
        }
        if Instant::now() > deadline {
            panic!("server not listening on {addr} after {secs} s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---- extension-host request handling ----------------------------------------------------------

enum DiagEvent {
    Changed { owner: String, entries: Vec<(UriComponents, Option<Vec<MarkerData>>)> },
    Gone(String),
}

/// [`exthost::default_reply`] plus the replies this test found necessary beyond it. Kept here,
/// not in the crate, until the session object (EDITOR-CONNECTION.md §6 step 3) owns them:
///
/// * `MainThreadLanguages.$getLanguages` → `string[]` (`languages.getLanguages()`; `undefined`
///   would throw in callers that iterate it).
/// * `MainThreadOutputService.$register` → the channel id string (`window.createOutputChannel`,
///   used by every language client for its log).
fn live_reply(proxy: Option<&str>, method: &str, seq: u64) -> Reply {
    match (proxy, method) {
        (Some("MainThreadLanguages"), "$getLanguages") => Reply::Json(json!(["plaintext", "json", "jsonc"])),
        (Some("MainThreadOutputService"), "$register") => Reply::Json(json!(format!("ember-live-output-{seq}"))),
        _ => exthost::default_reply(proxy, method),
    }
}

fn args_preview(args: &[Arg]) -> String {
    let v: Vec<Value> = args.iter().map(|a| a.as_json().cloned().unwrap_or(Value::Null)).collect();
    let s = Value::Array(v).to_string();
    if s.len() > 400 {
        format!("{}…", &s[..s.char_indices().nth(400).map_or(s.len(), |(i, _)| i)])
    } else {
        s
    }
}

/// Answer every extension-host request, forward diagnostics, count methods.
async fn respond_loop(
    peer: RpcPeer,
    mut events: mpsc::UnboundedReceiver<RpcEvent>,
    diag: mpsc::UnboundedSender<DiagEvent>,
    seen: Arc<Mutex<BTreeMap<String, u32>>>,
    verbose: bool,
) {
    let mut seq = 0u64;
    while let Some(ev) = events.recv().await {
        match ev {
            RpcEvent::Request(r) => {
                seq += 1;
                let name = format!("{}.{}", r.proxy.unwrap_or("<unknown proxy>"), r.method);
                *seen.lock().unwrap().entry(name.clone()).or_default() += 1;
                match (r.proxy, r.method.as_str()) {
                    (Some("MainThreadErrors"), "$onUnexpectedError")
                    | (Some("MainThreadExtensionService"), "$onExtensionActivationError")
                    | (Some("MainThreadExtensionService"), "$onExtensionRuntimeError") => {
                        eprintln!("[live-ose] ext host reports {name}: {}", args_preview(&r.args));
                    }
                    _ if verbose => println!("[live-ose]   EH→ {name} {}", args_preview(&r.args)),
                    _ => {}
                }
                match MainThreadCall::parse(&r) {
                    Ok(MainThreadCall::DiagnosticsChangeMany { owner, entries }) => {
                        let _ = diag.send(DiagEvent::Changed { owner, entries });
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("[live-ose] cannot parse {name}: {e}; args {}", args_preview(&r.args)),
                }
                peer.respond(r.req, Ok(live_reply(r.proxy, &r.method, seq)));
            }
            RpcEvent::Cancel(_) => {}
            RpcEvent::Lost(reason) => {
                let _ = diag.send(DiagEvent::Gone(format!("lost ({reason:?})")));
            }
            RpcEvent::Disconnected => {
                let _ = diag.send(DiagEvent::Gone("disconnected".into()));
                break;
            }
        }
    }
}

// ---- the test -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OSE server: EMBER_OSE_SERVER=<unpacked build>/bin/dpx-ose-server"]
async fn live_ose_end_to_end() {
    let Some(bin) = std::env::var_os("EMBER_OSE_SERVER").map(PathBuf::from) else {
        eprintln!("[live-ose] EMBER_OSE_SERVER not set; skipping (crates/editor-conn/scripts/fetch-ose-artifact.sh prints it)");
        return;
    };
    assert!(bin.is_file(), "EMBER_OSE_SERVER={} is not a file", bin.display());
    let verbose = std::env::var_os("EMBER_OSE_VERBOSE").is_some();
    let mut steps = Steps::new();

    // ---- 0. server ----------------------------------------------------------------------------
    // Short name: macOS temp dirs are long and the server puts sockets/locks under its data dir.
    let root = std::env::temp_dir().join(format!("ember-ose-{}", short_id()));
    std::fs::create_dir_all(&root).unwrap();
    // Canonical (/var → /private/var on macOS) so server-reported paths compare equal.
    let root = std::fs::canonicalize(&root).unwrap();
    println!("[live-ose] server {} (Code-OSS {PINNED_VERSION}), temp dir {}", bin.display(), root.display());

    let mut server = Server::start(&bin, &root);
    let addr = format!("127.0.0.1:{}", server.port);
    // The remote authority the workbench would use; both URI transformers key on it.
    let authority = addr.clone();
    steps.done("0. spawn dpx-ose-server");
    wait_listening(&mut server, &addr, 120).await;
    steps.done("0. server listening");

    // ---- 1. version gate ----------------------------------------------------------------------
    let base = ConnectOptions::new(addr.clone());
    let commit = within("GET /version", 30, async { handshake::verify_server(dial(&addr).await, &base).await })
        .await
        .unwrap_or_else(|e| panic!("verify_server: {e}"));
    assert_eq!(commit, PINNED_COMMIT);
    steps.done("1. verify_server (commit == PINNED_COMMIT)");

    // One reconnection token per logical connection, never reused.
    let opts_for = || {
        let mut o = base.clone();
        o.commit = Some(commit.clone());
        o.reconnection_token = uuid::Uuid::new_v4().to_string();
        o
    };

    // ---- 2. management connection -------------------------------------------------------------
    let mopts = opts_for();
    let (mconn, first) = within("management handshake", 30, async {
        handshake::connect(dial(&addr).await, &mopts, ConnectionType::Management, None).await
    })
    .await
    .unwrap_or_else(|e| panic!("management connect: {e}"));
    assert_eq!(first["type"], "ok", "management handshake reply: {first}");
    steps.done("2. management: upgrade + auth/sign/connectionType");

    let ctx = RemoteAgentConnectionContext { remote_authority: authority.clone(), client_id: format!("ember-live-{}", short_id()) }
        .to_ipc()
        .unwrap();
    let (ipc, _ipc_lifecycle) = IpcClient::start(mconn, ctx);
    let env = within("getEnvironmentData", 30, management::get_environment_data(&ipc, &authority, None))
        .await
        .unwrap_or_else(|e| panic!("getEnvironmentData: {e}"));
    println!(
        "[live-ose]   env: pid {} os {} arch {} appRoot {} logs {}",
        env.pid, env.os, env.arch, env.app_root.path, env.extension_host_logs_path.path
    );
    assert_eq!(env.app_root.scheme, "vscode-remote", "server must transform URIs for our authority");
    steps.done("2. IPC initialize + getEnvironmentData");

    // remoteFilesystem
    let fs = RemoteFs::new(ipc.clone());
    let ws_path = root.join("ws");
    let ws_str = path_str(&ws_path);
    let ws_uri = UriComponents::remote(&authority, &ws_str);
    let child = |name: &str| UriComponents::remote(&authority, &format!("{ws_str}/{name}"));

    within("mkdir", 15, fs.mkdir(&ws_uri)).await.unwrap_or_else(|e| panic!("mkdir: {e}"));
    assert!(ws_path.is_dir());
    steps.done("2. fs.mkdir");

    let a = child("a.txt");
    let content = b"hello ember\n".to_vec();
    within("writeFile", 15, fs.write_file(&a, content.clone(), &WriteOptions::default()))
        .await
        .unwrap_or_else(|e| panic!("writeFile: {e}"));
    assert_eq!(std::fs::read(ws_path.join("a.txt")).unwrap(), content);
    steps.done("2. fs.writeFile");

    let st = within("stat", 15, fs.stat(&a)).await.unwrap_or_else(|e| panic!("stat: {e}"));
    assert!(st.is_file() && !st.is_dir(), "{st:?}");
    assert_eq!(st.size, content.len() as u64);
    steps.done("2. fs.stat");

    let read = within("readFile", 15, fs.read_file(&a)).await.unwrap_or_else(|e| panic!("readFile: {e}"));
    assert_eq!(read, content);
    steps.done("2. fs.readFile");

    let entries = within("readdir", 15, fs.readdir(&ws_uri)).await.unwrap_or_else(|e| panic!("readdir: {e}"));
    assert!(
        entries.iter().any(|(n, t)| n == "a.txt" && t & remote_fs::file_type::FILE != 0),
        "readdir: {entries:?}"
    );
    steps.done("2. fs.readdir");

    let b = child("b.txt");
    within("rename", 15, fs.rename(&a, &b, false)).await.unwrap_or_else(|e| panic!("rename: {e}"));
    assert!(!ws_path.join("a.txt").exists() && ws_path.join("b.txt").exists());
    match within("stat (renamed away)", 15, fs.stat(&a)).await {
        Err(Error::Remote(e)) => assert_eq!(e.fs_code(), Some("EntryNotFound"), "{e}"),
        other => panic!("stat of renamed file: expected EntryNotFound, got {other:?}"),
    }
    steps.done("2. fs.rename (+ stat → EntryNotFound)");

    within("delete", 15, fs.delete(&b, &DeleteOptions::default())).await.unwrap_or_else(|e| panic!("delete: {e}"));
    assert!(!ws_path.join("b.txt").exists());
    steps.done("2. fs.delete");

    // watch: subscribe first, then watch one file non-recursively, change it behind the
    // server's back with std::fs, expect a fileChange for it.
    let watched = child("watched.txt");
    let watched_local = ws_path.join("watched.txt");
    within("writeFile watched", 15, fs.write_file(&watched, b"v0\n".to_vec(), &WriteOptions::default()))
        .await
        .unwrap_or_else(|e| panic!("writeFile watched: {e}"));
    let mut changes = within("listen fileChange", 15, fs.subscribe_changes())
        .await
        .unwrap_or_else(|e| panic!("listen fileChange: {e}"));
    let watch_req = within("watch", 15, fs.watch(&watched, &WatchOptions::default()))
        .await
        .unwrap_or_else(|e| panic!("watch: {e}"));
    steps.done("2. fs.listen(fileChange) + fs.watch");
    // The watcher is set up asynchronously after `watch` returns; give it a moment, then keep
    // rewriting the file every 3 s until an event for it arrives.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let change: FileChange = within("fileChange event for watched.txt", 30, async {
        let mut n = 0u32;
        loop {
            n += 1;
            std::fs::write(&watched_local, format!("v{n} from std::fs\n")).unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(ev) = tokio::time::timeout_at(deadline, changes.recv()).await {
                match ev {
                    Some(WatchEvent::Changes(list)) => {
                        if let Some(c) = list.into_iter().find(|c| c.resource.path.ends_with("/watched.txt")) {
                            return c;
                        }
                    }
                    Some(WatchEvent::Error(e)) => eprintln!("[live-ose]   watcher error: {e}"),
                    None => panic!("fileChange subscription closed"),
                }
            }
        }
    })
    .await;
    println!("[live-ose]   fileChange: {:?} {}", change.kind, change.resource.key());
    assert!(matches!(change.kind, FileChangeType::Updated | FileChangeType::Added), "{change:?}");
    assert_eq!(change.resource.scheme, "vscode-remote");
    steps.done("2. fileChange received after std::fs::write");
    within("unwatch", 15, fs.unwatch(&watch_req)).await.unwrap_or_else(|e| panic!("unwatch: {e}"));

    // ---- 3. extensions --------------------------------------------------------------------------
    let extensions = within("scanExtensions", 60, management::scan_extensions(&ipc, "en"))
        .await
        .unwrap_or_else(|e| panic!("scanExtensions: {e}"));
    let builtin = extensions.iter().filter(|d| d.get("isBuiltin").and_then(Value::as_bool) == Some(true)).count();
    println!("[live-ose]   scanExtensions: {} extensions ({builtin} built-in)", extensions.len());
    let json_ext = extensions
        .iter()
        .find(|d| management::extension_id(d).is_some_and(|id| id.eq_ignore_ascii_case(JSON_EXTENSION)));
    let Some(json_ext) = json_ext else {
        let ids: Vec<&str> = extensions.iter().filter_map(management::extension_id).collect();
        panic!("{JSON_EXTENSION} not in scanExtensions (built-ins live in <appRoot>/extensions): {ids:?}");
    };
    println!(
        "[live-ose]   {JSON_EXTENSION}: main {} activationEvents {}",
        json_ext.get("main").unwrap_or(&Value::Null),
        json_ext.get("activationEvents").unwrap_or(&Value::Null)
    );
    steps.done("3. scanExtensions (json-language-features present)");

    // The document, on disk too (the language server may read neighbours; the ext host only
    // needs the delta).
    let doc_uri = child("data.json");
    within("writeFile data.json", 15, fs.write_file(&doc_uri, DOC_TEXT.as_bytes().to_vec(), &WriteOptions::default()))
        .await
        .unwrap_or_else(|e| panic!("writeFile data.json: {e}"));

    // ---- 4. extension-host connection -------------------------------------------------------
    let eopts = opts_for();
    let start_params = serde_json::to_value(ExtensionHostStartParams { language: "en".into(), ..Default::default() }).unwrap();
    let (econn, first) = within("ext-host handshake", 30, async {
        handshake::connect(dial(&addr).await, &eopts, ConnectionType::ExtensionHost, Some(start_params)).await
    })
    .await
    .unwrap_or_else(|e| panic!("ext-host connect: {e}"));
    println!("[live-ose]   ext-host handshake reply: {first}");
    steps.done("4. ext host: upgrade + auth/sign/connectionType");

    let workspace = WorkspaceData {
        id: format!("ember-live-{}", short_id()),
        name: "ws".into(),
        folders: vec![WorkspaceFolder { uri: ws_uri.clone(), name: "ws".into(), index: 0 }],
    };
    let init = InitDataParams {
        version: PINNED_VERSION.into(),
        quality: Some("stable".into()),
        commit: Some(commit.clone()),
        remote_authority: authority.clone(),
        env: env.clone(),
        workspace: Some(workspace.clone()),
        extensions: extensions.clone(),
        app_language: "en".into(),
        session_id: uuid::Uuid::new_v4().to_string(),
        machine_id: uuid::Uuid::new_v4().simple().to_string(),
        log_level: 3,
    }
    .to_json();
    let (peer, rpc_events) = within("ext host Ready → init data → Initialized", 90, exthost::initialize(econn, &init))
        .await
        .unwrap_or_else(|e| panic!("ext host bootstrap: {e}"));
    steps.done("4. ext host: Ready → init data → Initialized");

    let (diag_tx, mut diag_rx) = mpsc::unbounded_channel();
    let seen = Arc::new(Mutex::new(BTreeMap::new()));
    let responder = tokio::spawn(respond_loop(peer.clone(), rpc_events, diag_tx, Arc::clone(&seen), verbose));

    let defaults = exthost::contributed_configuration_defaults(&extensions);
    let configuration = exthost::configuration_init_data(&defaults, &[]);
    println!("[live-ose]   configuration defaults: {} keys from extension manifests", defaults.len());
    for call in exthost::bootstrap_calls(Some(&workspace), true, &configuration) {
        let name = format!("{}.{}", call.proxy, call.method);
        let pending = call.start(&peer).unwrap_or_else(|e| panic!("{name}: {e}"));
        within(&name, 30, pending.wait()).await.unwrap_or_else(|e| panic!("{name}: {e}"));
        steps.done(&format!("4. {name}"));
    }

    // Editor tabs: vscode-languageclient decides which documents to pull diagnostics for partly
    // from `window.tabGroups`. Shape: `IEditorTabGroupDto[]` / `IEditorTabDto` with
    // `TabInputKind.TextInput = 1` (extHost.protocol.ts). Best effort: a wrong shape only logs.
    let tab_model = json!([{
        "groupId": 1,
        "isActive": true,
        "viewColumn": 0,
        "tabs": [{
            "id": "ember-live-tab-1",
            "label": "data.json",
            "input": { "kind": 1, "uri": doc_uri },
            "editorId": "default",
            "isActive": true,
            "isPinned": false,
            "isPreview": false,
            "isDirty": false
        }]
    }]);
    match within(
        "ExtHostEditorTabs.$acceptEditorTabModel",
        15,
        peer.call("ExtHostEditorTabs", "$acceptEditorTabModel", vec![Arg::Json(tab_model)]),
    )
    .await
    {
        Ok(_) => steps.done("4. ExtHostEditorTabs.$acceptEditorTabModel"),
        Err(e) => eprintln!("[live-ose] warning: $acceptEditorTabModel failed (continuing): {e}"),
    }

    // Open data.json in an editor: $activateByEvent("onLanguage:json") then the delta.
    let mut bridge = DocumentBridge::new();
    for call in bridge.open_in_editor(doc_uri.clone(), DOC_TEXT, "json", "ember-live-editor-1") {
        let name = if call.method == "$activateByEvent" {
            format!("{}.{}(onLanguage:json)", call.proxy, call.method)
        } else {
            format!("{}.{}", call.proxy, call.method)
        };
        let pending = call.start(&peer).unwrap_or_else(|e| panic!("{name}: {e}"));
        within(&name, 60, pending.wait()).await.unwrap_or_else(|e| panic!("{name}: {e}"));
        steps.done(&format!("4. {name}"));
    }

    // Break it: `"ok": true` → `"ok": true,,` (insert at 3:13).
    let edit = EditorChange {
        edits: vec![TextEdit {
            range: Range { start_line_number: 3, start_column: 13, end_line_number: 3, end_column: 13 },
            text: ",,".into(),
        }],
        ..Default::default()
    };
    let call = bridge.change(&doc_uri, edit).unwrap_or_else(|e| panic!("bridge.change: {e}"));
    assert_eq!(bridge.get(&doc_uri).unwrap().text(), DOC_TEXT_AFTER_EDIT);
    let pending = call.start(&peer).unwrap_or_else(|e| panic!("$acceptModelChanged: {e}"));
    within("$acceptModelChanged", 15, pending.wait()).await.unwrap_or_else(|e| panic!("$acceptModelChanged: {e}"));
    steps.done("4. ExtHostDocuments.$acceptModelChanged (now invalid)");

    let (owner, markers) = within("MainThreadDiagnostics.$changeMany with markers for data.json", 120, async {
        loop {
            match diag_rx.recv().await {
                Some(DiagEvent::Changed { owner, entries }) => {
                    for (uri, markers) in entries {
                        let n = markers.as_ref().map_or(0, Vec::len);
                        println!("[live-ose]   $changeMany owner={owner} {} markers={n}", uri.key());
                        if uri.path.ends_with("/data.json") && n > 0 {
                            return (owner, markers.unwrap_or_default());
                        }
                    }
                }
                Some(DiagEvent::Gone(why)) => panic!("extension-host connection {why} while waiting for diagnostics"),
                None => panic!("responder task ended"),
            }
        }
    })
    .await;
    for m in &markers {
        println!(
            "[live-ose]   [{owner}] sev {} {}:{}-{}:{} {}",
            m.severity, m.start_line_number, m.start_column, m.end_line_number, m.end_column, m.message
        );
    }
    assert!(markers.iter().any(|m| m.severity == 8), "expected an Error marker: {markers:?}");
    steps.done("4. MainThreadDiagnostics.$changeMany (JSON errors)");

    // ---- 5. shutdown ----------------------------------------------------------------------------
    exthost::terminate(peer.connection());
    ipc.connection().close();
    tokio::time::sleep(Duration::from_millis(500)).await;
    responder.abort();
    assert!(server.exited().is_none(), "server died after the clients disconnected");
    steps.done("5. Terminate ext host + Disconnect both connections");
    let status = server.stop();
    println!("[live-ose]   server exit: {status:?}");
    steps.done("5. server stopped (SIGTERM to process group)");

    {
        let seen = seen.lock().unwrap();
        println!("[live-ose] ---- extension host → Ember calls ({} distinct) ----", seen.len());
        for (name, n) in seen.iter() {
            println!("[live-ose] {n:>5}  {name}");
        }
    }
    steps.summary();
    drop(server); // removes the temp dir unless EMBER_OSE_KEEP is set
}
