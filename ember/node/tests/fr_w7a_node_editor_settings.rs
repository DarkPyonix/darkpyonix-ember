//! SPEC FR-W7a (`INTENT.md` D56, #1121): a node applies the editor settings the server sends
//! through its DarkPyonix manager, reports per field, keeps them, and applies them again at start.
//!
//! The manager is a fake: an axum server with `GET /api/editor` (the installed runtimes the test
//! sets) and `PUT /api/editor/{runtime}/presets` (which records the body, and answers 422
//! `extension_failed` for the extension `fail.ext`). The DarkPyonix CLI is a fake too: a shell
//! script given with `Node::with_darkpyonix_bin` (the same thing `DARKPYONIX_BIN` names) that
//! appends its arguments to a log and fails when an argument is `--fail-me` or the runtime is
//! `script` with a script containing `FAIL`. Not covered: a real manager and CLI, and an install
//! that downloads anything.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::editor_manager::{EditorRelays, ManagerSource, OSE_RELEASE};
use ember_node::editor_settings::{
    self, EditorServer, NodeEditorSettings, NOT_READY, NO_MANAGER, SCRIPT_FILE, SETTINGS_FILE, VSC_EXTENSIONS, VSC_LICENCE,
};
use serde_json::{json, Value};

const TOKEN: &str = "node-token";
const MANAGER_TOKEN: &str = "manager-master-token";
const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct FakeManager {
    /// The runtimes `GET /api/editor` lists as installed.
    installed: Arc<Mutex<Vec<String>>>,
    default_runtime: Arc<Mutex<String>>,
    /// `(runtime, body)` of every presets request.
    presets: Arc<Mutex<Vec<(String, Value)>>>,
    status_calls: Arc<Mutex<usize>>,
}

fn bearer_ok(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {MANAGER_TOKEN}"))
}

async fn fake_status(State(m): State<FakeManager>, h: HeaderMap) -> Response {
    if !bearer_ok(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "code": "unauthorized", "message": "bad token" } }))).into_response();
    }
    *m.status_calls.lock().unwrap() += 1;
    let runtimes: Vec<Value> = m
        .installed
        .lock()
        .unwrap()
        .iter()
        .map(|r| json!({ "runtime": r, "version": "1.139.1", "commit": "abc", "state": "stopped", "path": format!("/ide/{r}/") }))
        .collect();
    Json(json!({ "runtimes": runtimes, "default_runtime": *m.default_runtime.lock().unwrap(), "autostart": true })).into_response()
}

async fn fake_presets(State(m): State<FakeManager>, AxPath(runtime): AxPath<String>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !bearer_ok(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "code": "unauthorized", "message": "bad token" } }))).into_response();
    }
    m.presets.lock().unwrap().push((runtime.clone(), body.clone()));
    let exts = body.get("extensions").and_then(Value::as_array).cloned().unwrap_or_default();
    if runtime == "vsc" && !exts.is_empty() {
        return (StatusCode::CONFLICT, Json(json!({ "error": { "code": "conflict", "message": "vsc takes no extensions" } }))).into_response();
    }
    if exts.iter().any(|e| e["id"] == "fail.ext") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": { "code": "extension_failed", "message": "fail.ext was not installed",
                "data": { "id": "fail.ext", "output": "Extension 'fail.ext' not found." } } })),
        )
            .into_response();
    }
    Json(body).into_response()
}

async fn start_manager(installed: &[&str], default_runtime: &str) -> (String, FakeManager) {
    let m = FakeManager {
        installed: Arc::new(Mutex::new(installed.iter().map(|s| s.to_string()).collect())),
        default_runtime: Arc::new(Mutex::new(default_runtime.to_string())),
        presets: Arc::new(Mutex::new(Vec::new())),
        status_calls: Arc::new(Mutex::new(0)),
    };
    let app = Router::new()
        .route("/api/editor", get(fake_status))
        .route("/api/editor/{runtime}/presets", put(fake_presets))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, m)
}

/// A fake `darkpyonix` that appends one line per call (its arguments, space separated) to `log`.
/// It fails with output on `--fail-me`, and for a `--script` file that contains `FAIL`.
fn fake_cli(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("cli.log");
    let bin = dir.join("darkpyonix");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\nfor a in \"$@\"; do\n  case \"$a\" in --fail-me) echo 'install exploded: disk full'; exit 3;; esac\ndone\nif [ \"$4\" = script ] && grep -q FAIL \"$6\" 2>/dev/null; then echo 'the script failed' >&2; exit 4; fi\nexit 0\n",
        log = log.display()
    );
    std::fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

fn cli_calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log).map(|t| t.lines().map(String::from).collect()).unwrap_or_default()
}

struct Fx {
    client: NodeClient,
    state: PathBuf,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

/// A node with a state dir, the fake CLI, and `source` as its manager.
async fn start_node(source: ManagerSource) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let state = base.join("state");
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let (bin, log) = fake_cli(&base);
    let node = node_with(&state, &root, source, &bin);
    let client = serve(node).await;
    Fx { client, state, log, _dir: dir }
}

fn node_with(state: &Path, root: &Path, source: ManagerSource, bin: &Path) -> Node {
    let mut cfg = NodeConfig::new(TOKEN, vec![root.to_path_buf()]);
    cfg.state_dir = Some(state.to_path_buf());
    Node::with_editor(cfg, Arc::new(EditorRelays::new(source))).unwrap().with_darkpyonix_bin(bin)
}

async fn serve(node: Node) -> NodeClient {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    NodeClient::new(&base, TOKEN).unwrap()
}

fn settings(v: Value) -> NodeEditorSettings {
    serde_json::from_value(v).unwrap()
}

/// FR-W7a: a runtime the manager does not list is installed through the CLI (OSE from the pinned
/// release) and selected, then the presets go to the manager in one PUT; every field is applied,
/// and the node keeps the settings and the report (0600).
#[tokio::test]
async fn fr_w7a_node_installs_a_missing_ose_and_applies_the_presets() {
    let (url, m) = start_manager(&[], "").await;
    let f = start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await;
    let s = settings(json!({
        "editor_server": "code_ose",
        "extensions": [{ "id": "redhat.vscode-yaml", "version": "1.2.3" }, { "id": "ms-python.python" }],
        "theme": "DarkPyonix Dark",
        "settings": { "editor.fontSize": 14 }
    }));
    let report = f.client.put_editor_settings(&s).await.unwrap();
    assert!(report.all_applied(), "{report:?}");
    assert!(report.applied_at > 0);
    assert_eq!(
        cli_calls(&f.log),
        [format!("editor install --runtime ose --release {OSE_RELEASE}"), "editor use ose".to_string()]
    );
    let presets = m.presets.lock().unwrap().clone();
    assert_eq!(presets.len(), 1);
    assert_eq!(presets[0].0, "ose");
    assert_eq!(
        presets[0].1,
        json!({
            "extensions": [{ "id": "redhat.vscode-yaml", "version": "1.2.3" }, { "id": "ms-python.python" }],
            "theme": "DarkPyonix Dark",
            "settings": { "editor.fontSize": 14 }
        })
    );

    // The node keeps what it got and the report, and answers both.
    let state = f.client.editor_settings().await.unwrap();
    assert_eq!(state.settings.as_ref(), Some(&s));
    assert_eq!(state.applied.as_ref(), Some(&report));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(f.state.join(SETTINGS_FILE)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let kept: Value = serde_json::from_slice(&std::fs::read(f.state.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert_eq!(kept["settings"]["theme"], "DarkPyonix Dark");
    assert_eq!(kept["applied"]["fields"]["theme"]["applied"], true);

    // The exact report JSON: four fields, no reason on an applied one.
    let (status, body) = f.client.request_raw(axum::http::Method::GET, "/editor/settings", None).await.unwrap();
    assert_eq!(status, 200);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["applied"]["fields"], json!({
        "editor_server": { "applied": true }, "extensions": { "applied": true },
        "theme": { "applied": true }, "settings": { "applied": true }
    }));
}

/// FR-W7a: an installed runtime that is already the default is not installed or selected again;
/// one that is installed but not the default is selected.
#[tokio::test]
async fn fr_w7a_node_does_not_reinstall_an_installed_runtime() {
    let (url, m) = start_manager(&["ose", "vsc"], "ose").await;
    let f = start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await;
    let report = f.client.put_editor_settings(&settings(json!({ "editor_server": "code_ose", "theme": "T" }))).await.unwrap();
    assert!(report.all_applied(), "{report:?}");
    assert!(cli_calls(&f.log).is_empty(), "{:?}", cli_calls(&f.log));
    assert_eq!(m.presets.lock().unwrap().len(), 1);

    let report = f
        .client
        .put_editor_settings(&settings(json!({ "editor_server": "vscode", "vscode_license_accepted": true })))
        .await
        .unwrap();
    assert!(report.all_applied(), "{report:?}");
    assert_eq!(cli_calls(&f.log), ["editor use vsc"]);
}

/// FR-W7a: VS Code is installed only with the licence accepted (else the editor server field
/// fails with that reason and the presets are not sent), and its presets carry no extensions: a
/// listed extension fails its field with the reason while the theme still applies.
#[tokio::test]
async fn fr_w7a_node_vscode_needs_the_licence_and_takes_no_extensions() {
    let (url, m) = start_manager(&[], "").await;
    let f = start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await;

    let report = f.client.put_editor_settings(&settings(json!({ "editor_server": "vscode", "theme": "T" }))).await.unwrap();
    assert!(!report.fields.editor_server.applied);
    assert_eq!(report.fields.editor_server.reason.as_deref(), Some(VSC_LICENCE));
    assert_eq!(report.fields.theme.reason.as_deref(), Some(NOT_READY));
    assert!(cli_calls(&f.log).is_empty());
    assert!(m.presets.lock().unwrap().is_empty(), "presets were sent to a runtime that is not there");

    let s = settings(json!({
        "editor_server": "vscode", "vscode_license_accepted": true,
        "extensions": [{ "id": "redhat.vscode-yaml" }], "theme": "T"
    }));
    let report = f.client.put_editor_settings(&s).await.unwrap();
    assert!(report.fields.editor_server.applied, "{report:?}");
    assert_eq!(cli_calls(&f.log), ["editor install --runtime vsc --accept-server-license-terms", "editor use vsc"]);
    assert!(!report.fields.extensions.applied);
    assert_eq!(report.fields.extensions.reason.as_deref(), Some(VSC_EXTENSIONS));
    assert!(report.fields.theme.applied && report.fields.settings.applied, "{report:?}");
    let presets = m.presets.lock().unwrap().clone();
    assert_eq!(presets.last().unwrap().0, "vsc");
    assert!(presets.last().unwrap().1.get("extensions").is_none(), "{presets:?}");
}

/// FR-W7a: the owner's script is written to `<state dir>/editor-script.sh` (0700) and installed
/// with `--runtime script --script <file>`; a changed script is installed again; a CLI failure is
/// the editor server field's reason with the tail of the CLI's output.
#[tokio::test]
async fn fr_w7a_node_manual_script_is_written_and_installed() {
    let (url, m) = start_manager(&[], "").await;
    let f = start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await;
    let script = "#!/bin/sh\nexec code serve-web \"$@\"\n";
    let report = f.client.put_editor_settings(&settings(json!({ "editor_server": "manual_script", "script": script }))).await.unwrap();
    assert!(report.all_applied(), "{report:?}");
    let file = f.state.join(SCRIPT_FILE);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), script);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(
        cli_calls(&f.log),
        [format!("editor install --runtime script --script {}", file.display()), "editor use script".to_string()]
    );
    assert_eq!(m.presets.lock().unwrap().last().unwrap().0, "script");

    // Installed now: the same script is not installed again, a changed one is.
    m.installed.lock().unwrap().push("script".into());
    *m.default_runtime.lock().unwrap() = "script".into();
    let before = cli_calls(&f.log).len();
    let report = f.client.put_editor_settings(&settings(json!({ "editor_server": "manual_script", "script": script }))).await.unwrap();
    assert!(report.all_applied(), "{report:?}");
    assert_eq!(cli_calls(&f.log).len(), before);
    let report = f
        .client
        .put_editor_settings(&settings(json!({ "editor_server": "manual_script", "script": "#!/bin/sh\nFAIL\n" })))
        .await
        .unwrap();
    let reason = report.fields.editor_server.reason.clone().unwrap_or_default();
    assert!(!report.fields.editor_server.applied, "{report:?}");
    assert!(reason.contains("the script failed"), "{reason}");
    assert_eq!(report.fields.extensions.reason.as_deref(), Some(NOT_READY));
}

/// FR-W7a: an extension the manager cannot install (422 `extension_failed`) fails the extensions
/// field with the manager's message and the CLI output; the theme and settings are sent again
/// without the extensions and apply.
#[tokio::test]
async fn fr_w7a_node_reports_a_failed_extension_with_its_reason() {
    let (url, m) = start_manager(&["ose"], "ose").await;
    let f = start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await;
    let s = settings(json!({ "editor_server": "code_ose", "extensions": [{ "id": "fail.ext" }], "theme": "T" }));
    let report = f.client.put_editor_settings(&s).await.unwrap();
    assert!(report.fields.editor_server.applied);
    assert!(!report.fields.extensions.applied);
    let reason = report.fields.extensions.reason.clone().unwrap();
    assert!(reason.contains("fail.ext was not installed") && reason.contains("not found"), "{reason}");
    assert!(report.fields.theme.applied && report.fields.settings.applied, "{report:?}");
    let presets = m.presets.lock().unwrap().clone();
    assert_eq!(presets.len(), 2);
    assert!(presets[1].1.get("extensions").is_none());
    assert_eq!(presets[1].1["theme"], "T");
}

/// FR-W7a: with no manager every field fails with the reason that says so, and an unreachable
/// manager gives every field its reason too.
#[tokio::test]
async fn fr_w7a_node_without_a_manager_reports_every_field() {
    let f = start_node(ManagerSource::none()).await;
    let report = f.client.put_editor_settings(&settings(json!({ "editor_server": "code_ose" }))).await.unwrap();
    for field in [&report.fields.editor_server, &report.fields.extensions, &report.fields.theme, &report.fields.settings] {
        assert!(!field.applied);
        assert_eq!(field.reason.as_deref(), Some(NO_MANAGER));
    }
    assert!(cli_calls(&f.log).is_empty());

    let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone_url = format!("http://{}", gone.local_addr().unwrap());
    drop(gone);
    let f = start_node(ManagerSource::fixed(gone_url, MANAGER_TOKEN)).await;
    let report = f.client.put_editor_settings(&settings(json!({ "editor_server": "code_ose" }))).await.unwrap();
    assert!(!report.fields.theme.applied);
    assert!(report.fields.theme.reason.as_deref().unwrap().contains("연결할 수 없습니다"), "{report:?}");
}

/// FR-W7a: a bad body is 400 and reaches neither the CLI nor the manager; the routes sit behind
/// the node's token.
#[tokio::test]
async fn fr_w7a_node_refuses_a_bad_body() {
    let (url, m) = start_manager(&[], "").await;
    let f = start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await;
    for bad in [
        json!({ "editor_server": "emacs" }),
        json!({ "editor_server": "code_ose", "extensions": [{ "id": "../../etc" }] }),
        json!({ "editor_server": "code_ose", "extensions": [{ "id": "a.b", "version": "--force" }] }),
        json!({ "editor_server": "manual_script" }),
        json!({ "editor_server": "code_ose", "theme": "" }),
        json!([1, 2]),
    ] {
        let (status, body) = f.client.request_raw(axum::http::Method::PUT, "/editor/settings", Some(&bad)).await.unwrap();
        assert_eq!(status, 400, "{bad}: {}", String::from_utf8_lossy(&body));
    }
    assert!(cli_calls(&f.log).is_empty());
    assert!(m.presets.lock().unwrap().is_empty());
    assert_eq!(*m.status_calls.lock().unwrap(), 0);

    let base = f.client.base_url().unwrap().to_string();
    let wrong = NodeClient::new(&base, "wrong").unwrap();
    let e = wrong.editor_settings().await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
    let e = wrong.put_editor_settings(&NodeEditorSettings::default()).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
}

/// FR-W7a: a node started with settings in its state dir applies them again once the manager
/// answers, and keeps the new report.
#[tokio::test]
async fn fr_w7a_node_applies_the_kept_settings_again_at_start() {
    let (url, m) = start_manager(&["ose"], "ose").await;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let state = base.join("state");
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let (bin, _log) = fake_cli(&base);
    let kept = editor_settings::EditorSettingsState {
        settings: Some(settings(json!({ "editor_server": "code_ose", "theme": "Kept Theme" }))),
        applied: None,
    };
    editor_settings::save(&state, &kept).unwrap();

    // Nothing kept: nothing to re-apply.
    let empty = base.join("empty-state");
    let bare = node_with(&empty, &root, ManagerSource::fixed(url.clone(), MANAGER_TOKEN), &bin);
    assert!(editor_settings::spawn_reapply(&bare).is_none());

    let node = node_with(&state, &root, ManagerSource::fixed(url, MANAGER_TOKEN), &bin);
    assert_eq!(node.editor_settings().state().settings.unwrap().editor_server, EditorServer::CodeOse);
    let task = editor_settings::spawn_reapply_with(&node, Duration::from_millis(20), WAIT).expect("settings are kept");
    tokio::time::timeout(WAIT, task).await.expect("the re-apply did not finish").unwrap();
    let presets = m.presets.lock().unwrap().clone();
    assert_eq!(presets.len(), 1);
    assert_eq!(presets[0].1["theme"], "Kept Theme");
    let report = node.editor_settings().state().applied.expect("a report after the re-apply");
    assert!(report.all_applied(), "{report:?}");
    assert_eq!(editor_settings::load(&state).unwrap().applied, Some(report));
}

/// FR-W7a: the start-up re-apply waits for a manager that does not answer yet.
#[tokio::test]
async fn fr_w7a_node_reapply_waits_for_the_manager() {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let state = base.join("state");
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let (bin, _log) = fake_cli(&base);
    editor_settings::save(
        &state,
        &editor_settings::EditorSettingsState { settings: Some(settings(json!({ "editor_server": "code_ose" }))), applied: None },
    )
    .unwrap();
    let node = node_with(&state, &root, ManagerSource::fixed(format!("http://{addr}"), MANAGER_TOKEN), &bin);
    let task = editor_settings::spawn_reapply_with(&node, Duration::from_millis(50), WAIT).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the re-apply did not wait for the manager");

    // The manager comes up on the same port.
    let m = FakeManager {
        installed: Arc::new(Mutex::new(vec!["ose".into()])),
        default_runtime: Arc::new(Mutex::new("ose".into())),
        presets: Arc::new(Mutex::new(Vec::new())),
        status_calls: Arc::new(Mutex::new(0)),
    };
    let app = Router::new()
        .route("/api/editor", get(fake_status))
        .route("/api/editor/{runtime}/presets", put(fake_presets))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let deadline = Instant::now() + WAIT;
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "the re-apply never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(m.presets.lock().unwrap().len(), 1);
}
