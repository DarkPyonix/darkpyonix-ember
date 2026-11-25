//! The editor settings of this computer, applied through its DarkPyonix manager (SPEC FR-W7a,
//! `INTENT.md` D56, #1121).
//!
//! | Method | Path | Body | Answer |
//! | ------ | ---- | ---- | ------ |
//! | PUT | `/editor/settings` | [`NodeEditorSettings`] | 200 [`ApplyReport`]; 400 for a bad body |
//! | GET | `/editor/settings` | none | 200 [`EditorSettingsState`] `{settings, applied}` |
//!
//! Both sit behind the node's token, like every route but `/health`.
//!
//! The server sends the effective settings of this computer (its own record with the node's
//! overrides on top, without the IDE choice, which is Ember's alone). The node then:
//!
//! 1. maps the editor server to the manager's runtime (`code_ose` -> `ose`, `vscode` -> `vsc`,
//!    `manual_script` -> `script`) and, when `GET /api/editor` does not list that runtime, runs
//!    `darkpyonix editor install --runtime ...` ([`Cli`]: `darkpyonix` on `PATH`, or
//!    [`ENV_BIN`]): `ose` from the pinned [`OSE_RELEASE`], `vsc` only with the VS Code Server
//!    licence accepted (`--accept-server-license-terms`), `script` with the owner's script
//!    written to `<state dir>/editor-script.sh` (0700; a changed script is installed again). Then
//!    `darkpyonix editor use <runtime>` when the runtime is new or not the manager's default.
//!    The CLI runs for at most [`CLI_TIMEOUT`]; the tail of its output is the reason on failure.
//! 2. sends `PUT /api/editor/<runtime>/presets {extensions, theme, settings}` (no `extensions`
//!    for `vsc`: VS Code's extensions are the user's). An extension that fails (422) marks the
//!    extensions field and the theme and settings are sent again without the extensions.
//! 3. answers an [`ApplyReport`]: per field whether it was applied and why not.
//!
//! The last settings and report are kept in `<state dir>/editor-settings.json` (0600) and applied
//! again when the node starts ([`spawn_reapply`]), once the manager answers. A node with no state
//! dir keeps them in memory only.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::{ApiError, Node};
use crate::client::{ClientError, NodeClient};
use crate::editor_manager::{
    tail, EditorExtension, EditorPresets, EditorStatus, ManagerClient, ManagerError, ManagerSource, OSE_RELEASE,
};
use crate::proto::ErrorCode;

/// The file in the state dir that keeps the last settings and report (mode 0600).
pub const SETTINGS_FILE: &str = "editor-settings.json";
/// The owner's script, as the manager's `script` runtime runs it (mode 0700).
pub const SCRIPT_FILE: &str = "editor-script.sh";
/// The DarkPyonix CLI to run instead of `darkpyonix` on `PATH`.
pub const ENV_BIN: &str = "DARKPYONIX_BIN";
/// The CLI's name on `PATH`.
pub const DEFAULT_BIN: &str = "darkpyonix";
/// Longest run of one CLI command (an install downloads the editor server).
pub const CLI_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How often the start-up re-apply asks whether the manager answers.
pub const REAPPLY_POLL: Duration = Duration::from_secs(5);
/// How long the start-up re-apply waits for the manager before it applies anyway (and reports
/// why it failed).
pub const REAPPLY_WAIT: Duration = Duration::from_secs(30 * 60);
/// The longest script kept.
pub const SCRIPT_MAX: usize = 64 * 1024;
/// The most extensions in one record.
pub const EXTENSIONS_MAX: usize = 200;
/// The longest theme name.
pub const THEME_MAX: usize = 256;
/// The most CLI output kept in a reason.
const OUTPUT_TAIL: usize = 600;

/// The reason of every field when this computer has no manager.
pub const NO_MANAGER: &str = "DarkPyonix 매니저가 설치되어 있지 않습니다";
/// The reason of the extensions field for VS Code.
pub const VSC_EXTENSIONS: &str = "VS Code의 확장은 사용자가 직접 설치합니다";
/// The reason of the editor server field for VS Code without the licence.
pub const VSC_LICENCE: &str = "VS Code Server 라이선스에 동의해야 설치합니다";
/// The reason of the preset fields when the editor server is not ready.
pub const NOT_READY: &str = "에디터 서버가 준비되지 않아 적용하지 못했습니다";

/// The editor server of a record (`editor_server`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorServer {
    /// OSE from the pinned release (the default).
    #[default]
    CodeOse,
    /// The user's own Microsoft `code serve-web` (D10).
    Vscode,
    /// The owner's script, run once by the manager, naming a `serve-web`-compatible server.
    ManualScript,
}

impl EditorServer {
    /// The manager's runtime for this editor server.
    pub fn runtime(self) -> &'static str {
        match self {
            EditorServer::CodeOse => "ose",
            EditorServer::Vscode => "vsc",
            EditorServer::ManualScript => "script",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EditorServer::CodeOse => "code_ose",
            EditorServer::Vscode => "vscode",
            EditorServer::ManualScript => "manual_script",
        }
    }
}

/// What the server sends a node: the effective record without `default_ide`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeEditorSettings {
    pub editor_server: EditorServer,
    /// The owner's script; required for `manual_script`.
    #[serde(default)]
    pub script: Option<String>,
    /// The owner accepted the VS Code Server licence; required to install `vscode`.
    #[serde(default)]
    pub vscode_license_accepted: bool,
    #[serde(default)]
    pub extensions: Vec<EditorExtension>,
    /// `workbench.colorTheme` (SPEC FR-W6).
    #[serde(default)]
    pub theme: Option<String>,
    /// Further editor defaults, passed as the presets' `settings`.
    #[serde(default)]
    pub settings: Map<String, Value>,
}

/// Checks the parts of a record both the server and the node refuse: extension ids and versions,
/// the theme's length and characters, a script for `manual_script` and its size.
pub fn validate_parts(
    editor_server: EditorServer,
    script: Option<&str>,
    extensions: &[EditorExtension],
    theme: Option<&str>,
) -> Result<(), String> {
    if extensions.len() > EXTENSIONS_MAX {
        return Err(format!("at most {EXTENSIONS_MAX} extensions"));
    }
    for e in extensions {
        crate::ose::extension_spec(&e.id, e.version.as_deref()).map_err(|m| format!("{}: {m}", e.id))?;
    }
    if let Some(t) = theme {
        if t.trim().is_empty() || t.chars().count() > THEME_MAX || t.chars().any(char::is_control) {
            return Err(format!("theme must be 1 to {THEME_MAX} characters without control characters"));
        }
    }
    if let Some(s) = script {
        if s.len() > SCRIPT_MAX {
            return Err(format!("script must be at most {SCRIPT_MAX} bytes"));
        }
    }
    if editor_server == EditorServer::ManualScript && script.is_none_or(|s| s.trim().is_empty()) {
        return Err("manual_script needs a script".into());
    }
    Ok(())
}

impl NodeEditorSettings {
    /// See [`validate_parts`].
    pub fn validate(&self) -> Result<(), String> {
        validate_parts(self.editor_server, self.script.as_deref(), &self.extensions, self.theme.as_deref())
    }
}

/// Whether one field was applied, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldReport {
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl FieldReport {
    pub fn ok() -> Self {
        FieldReport { applied: true, reason: None }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        FieldReport { applied: false, reason: Some(reason.into()) }
    }
}

/// The fields of an [`ApplyReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyFields {
    pub editor_server: FieldReport,
    pub extensions: FieldReport,
    pub theme: FieldReport,
    pub settings: FieldReport,
}

/// What one apply did: when (ms since the epoch) and per field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub applied_at: i64,
    pub fields: ApplyFields,
}

impl ApplyReport {
    /// Every field failed with `reason`.
    pub fn all_failed(reason: impl Into<String>) -> Self {
        let r = FieldReport::failed(reason);
        ApplyReport {
            applied_at: now_ms(),
            fields: ApplyFields { editor_server: r.clone(), extensions: r.clone(), theme: r.clone(), settings: r },
        }
    }

    /// True when every field was applied.
    pub fn all_applied(&self) -> bool {
        let f = &self.fields;
        f.editor_server.applied && f.extensions.applied && f.theme.applied && f.settings.applied
    }
}

/// The answer of `GET /editor/settings`, and what [`SETTINGS_FILE`] holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EditorSettingsState {
    #[serde(default)]
    pub settings: Option<NodeEditorSettings>,
    #[serde(default)]
    pub applied: Option<ApplyReport>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The DarkPyonix CLI: [`ENV_BIN`] when set and not empty, else `darkpyonix` on `PATH`. Pure, for
/// tests.
pub fn darkpyonix_bin(env: Option<OsString>) -> PathBuf {
    env.filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_BIN))
}

/// The DarkPyonix CLI, run directly (no shell) with a timeout.
#[derive(Debug, Clone)]
pub struct Cli {
    pub bin: PathBuf,
    pub timeout: Duration,
}

impl Cli {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Cli { bin: bin.into(), timeout: CLI_TIMEOUT }
    }

    /// The CLI [`darkpyonix_bin`] names from the environment.
    pub fn from_env() -> Self {
        Cli::new(darkpyonix_bin(std::env::var_os(ENV_BIN)))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs `bin args...`; answers its output, or the reason it failed with the tail of its
    /// output. The child is killed when the timeout passes.
    pub async fn run(&self, args: &[String]) -> Result<String, String> {
        let shown = format!("darkpyonix {}", args.join(" "));
        let child = tokio::process::Command::new(&self.bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("{shown}을(를) 실행할 수 없습니다: {e}"))?;
        let out = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("{shown}이(가) 실패했습니다: {e}")),
            Err(_) => return Err(format!("{shown}이(가) {}초 안에 끝나지 않았습니다", self.timeout.as_secs())),
        };
        let mut text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !err.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&err);
        }
        if out.status.success() {
            Ok(text)
        } else if text.is_empty() {
            Err(format!("{shown}이(가) 실패했습니다 ({})", out.status))
        } else {
            Err(format!("{shown}이(가) 실패했습니다 ({}): {}", out.status, tail(&text, OUTPUT_TAIL)))
        }
    }
}

fn manager_reason(e: &ManagerError) -> String {
    match e {
        ManagerError::NotInstalled(_) => NO_MANAGER.to_string(),
        ManagerError::Unreachable(m) => format!("DarkPyonix 매니저에 연결할 수 없습니다: {m}"),
        ManagerError::Refused { status, error } => format!("DarkPyonix 매니저가 거절했습니다 ({status}): {error}"),
    }
}

/// Writes `text` to `<dir>/editor-script.sh` with mode 0700 (atomically). Answers whether the
/// content changed.
pub fn write_script(dir: &Path, text: &str) -> std::io::Result<(PathBuf, bool)> {
    let path = dir.join(SCRIPT_FILE);
    if std::fs::read(&path).ok().as_deref() == Some(text.as_bytes()) {
        return Ok((path, false));
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{SCRIPT_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    write_private(&tmp, text.as_bytes(), 0o700)?;
    std::fs::rename(&tmp, &path)?;
    Ok((path, true))
}

/// Creates `path` with `mode` (on Unix) and writes `bytes` to it.
fn write_private(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Installs and selects the runtime of `s` when needed (step 1). An error is the reason.
async fn ensure_runtime(cli: &Cli, script_dir: Option<&Path>, s: &NodeEditorSettings, status: &EditorStatus) -> Result<(), String> {
    let runtime = s.editor_server.runtime();
    let installed = status.has(runtime);
    let arg = |v: &str| v.to_string();
    let install: Option<Vec<String>> = match s.editor_server {
        EditorServer::CodeOse => (!installed)
            .then(|| vec![arg("editor"), arg("install"), arg("--runtime"), arg("ose"), arg("--release"), arg(OSE_RELEASE)]),
        EditorServer::Vscode => {
            if installed {
                None
            } else if !s.vscode_license_accepted {
                return Err(VSC_LICENCE.into());
            } else {
                Some(vec![arg("editor"), arg("install"), arg("--runtime"), arg("vsc"), arg("--accept-server-license-terms")])
            }
        }
        EditorServer::ManualScript => {
            let text = s.script.as_deref().filter(|t| !t.trim().is_empty()).ok_or("실행할 스크립트가 없습니다")?;
            let dir = script_dir.ok_or("이 컴퓨터에 상태 폴더가 없어 스크립트를 저장할 수 없습니다")?;
            let (path, changed) =
                write_script(dir, text).map_err(|e| format!("스크립트를 저장할 수 없습니다: {e}"))?;
            (!installed || changed).then(|| {
                vec![arg("editor"), arg("install"), arg("--runtime"), arg("script"), arg("--script"), path.to_string_lossy().into_owned()]
            })
        }
    };
    let installs = install.is_some();
    if let Some(args) = install {
        cli.run(&args).await?;
    }
    if installs || !installed || status.default_runtime != runtime {
        cli.run(&[arg("editor"), arg("use"), arg(runtime)]).await?;
    }
    Ok(())
}

/// Sends the presets of `s` (step 2) and answers the extensions, theme and settings fields.
async fn apply_presets(client: &ManagerClient, s: &NodeEditorSettings) -> (FieldReport, FieldReport, FieldReport) {
    let runtime = s.editor_server.runtime();
    let vsc = s.editor_server == EditorServer::Vscode;
    let mut extensions = if vsc && !s.extensions.is_empty() { FieldReport::failed(VSC_EXTENSIONS) } else { FieldReport::ok() };
    let presets = EditorPresets {
        extensions: (!vsc).then(|| s.extensions.clone()),
        theme: s.theme.clone(),
        settings: Some(s.settings.clone()),
    };
    let first = client.put_presets(runtime, &presets).await;
    match first {
        Ok(_) => (extensions, FieldReport::ok(), FieldReport::ok()),
        Err(ManagerError::Refused { status: 422, error }) if presets.extensions.as_ref().is_some_and(|e| !e.is_empty()) => {
            extensions = FieldReport::failed(format!("확장을 설치하지 못했습니다: {error}"));
            // The theme and settings do not depend on the extensions: send them again alone.
            let rest = EditorPresets { extensions: None, ..presets };
            match client.put_presets(runtime, &rest).await {
                Ok(_) => (extensions, FieldReport::ok(), FieldReport::ok()),
                Err(e) => {
                    let r = FieldReport::failed(manager_reason(&e));
                    (extensions, r.clone(), r)
                }
            }
        }
        Err(e) => {
            let r = FieldReport::failed(manager_reason(&e));
            if extensions.applied {
                extensions = r.clone();
            }
            (extensions, r.clone(), r)
        }
    }
}

/// Applies `s` through the manager of `source` (steps 1 to 3). `script_dir` is where the owner's
/// script is written (the node's state dir). The server runs the same function for its own
/// machine.
pub async fn apply(source: &ManagerSource, cli: &Cli, script_dir: Option<&Path>, s: &NodeEditorSettings) -> ApplyReport {
    let client = match source.client() {
        Ok(c) => c,
        Err(e) => return ApplyReport::all_failed(manager_reason(&e)),
    };
    let status = match client.status().await {
        Ok(s) => s,
        Err(e) => return ApplyReport::all_failed(manager_reason(&e)),
    };
    if let Err(reason) = ensure_runtime(cli, script_dir, s, &status).await {
        let not_ready = FieldReport::failed(NOT_READY);
        return ApplyReport {
            applied_at: now_ms(),
            fields: ApplyFields {
                editor_server: FieldReport::failed(reason),
                extensions: not_ready.clone(),
                theme: not_ready.clone(),
                settings: not_ready,
            },
        };
    }
    let (extensions, theme, settings) = apply_presets(&client, s).await;
    ApplyReport { applied_at: now_ms(), fields: ApplyFields { editor_server: FieldReport::ok(), extensions, theme, settings } }
}

/// Reads [`SETTINGS_FILE`] in `dir`; `None` when it is missing or unreadable.
pub fn load(dir: &Path) -> Option<EditorSettingsState> {
    let text = std::fs::read(dir.join(SETTINGS_FILE)).ok()?;
    match serde_json::from_slice(&text) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", dir.join(SETTINGS_FILE).display());
            None
        }
    }
}

/// Writes [`SETTINGS_FILE`] in `dir` with mode 0600 (atomically).
pub fn save(dir: &Path, state: &EditorSettingsState) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    write_private(&tmp, &bytes, 0o600)?;
    std::fs::rename(&tmp, dir.join(SETTINGS_FILE))
}

/// The node's editor settings: the last ones it got, the last report, and the CLI it runs.
pub struct EditorSettingsHost {
    state_dir: Option<PathBuf>,
    cli: Cli,
    current: Mutex<EditorSettingsState>,
    /// One apply at a time.
    apply_lock: tokio::sync::Mutex<()>,
}

impl EditorSettingsHost {
    /// The host of `state_dir` (its [`SETTINGS_FILE`] read now), running `cli`.
    pub fn new(state_dir: Option<PathBuf>, cli: Cli) -> Self {
        let current = state_dir.as_deref().and_then(load).unwrap_or_default();
        EditorSettingsHost { state_dir, cli, current: Mutex::new(current), apply_lock: tokio::sync::Mutex::new(()) }
    }

    pub fn state_dir(&self) -> Option<&Path> {
        self.state_dir.as_deref()
    }

    pub fn cli(&self) -> &Cli {
        &self.cli
    }

    /// The last settings and report.
    pub fn state(&self) -> EditorSettingsState {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn persist(&self) {
        let Some(dir) = &self.state_dir else { return };
        let state = self.state();
        if let Err(e) = save(dir, &state) {
            tracing::warn!("could not write {}: {e}", dir.join(SETTINGS_FILE).display());
        }
    }

    /// Keeps `s` (so a restart applies it again even if this apply is cut short), applies it and
    /// keeps the report.
    pub async fn apply(&self, source: &ManagerSource, s: NodeEditorSettings) -> ApplyReport {
        let _one = self.apply_lock.lock().await;
        self.current.lock().unwrap_or_else(|e| e.into_inner()).settings = Some(s.clone());
        self.persist();
        self.run(source, &s).await
    }

    /// Applies the settings kept now again, if there are any (the start-up re-apply).
    pub async fn reapply(&self, source: &ManagerSource) -> Option<ApplyReport> {
        let _one = self.apply_lock.lock().await;
        let s = self.state().settings?;
        Some(self.run(source, &s).await)
    }

    async fn run(&self, source: &ManagerSource, s: &NodeEditorSettings) -> ApplyReport {
        let report = apply(source, &self.cli, self.state_dir.as_deref(), s).await;
        {
            let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
            // A newer PUT cannot have changed the settings meanwhile: it waits for the lock.
            cur.applied = Some(report.clone());
        }
        self.persist();
        report
    }
}

/// Applies the kept settings again in the background, once the manager answers `GET /api/editor`
/// (polled every [`REAPPLY_POLL`], at most [`REAPPLY_WAIT`]; after that it applies anyway, which
/// reports why it failed). `None` when nothing is kept.
pub fn spawn_reapply(node: &Node) -> Option<tokio::task::JoinHandle<()>> {
    spawn_reapply_with(node, REAPPLY_POLL, REAPPLY_WAIT)
}

/// [`spawn_reapply`] with other timings (tests).
pub fn spawn_reapply_with(node: &Node, poll: Duration, wait: Duration) -> Option<tokio::task::JoinHandle<()>> {
    node.editor_settings().state().settings.as_ref()?;
    let node = node.clone();
    Some(tokio::spawn(async move {
        let source = node.editor().source().clone();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let ready = match source.client() {
                Ok(c) => c.status().await.is_ok(),
                Err(_) => false,
            };
            if ready || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(poll).await;
        }
        if let Some(report) = node.editor_settings().reapply(&source).await {
            tracing::info!(all_applied = report.all_applied(), "editor settings applied again at start");
        }
    }))
}

pub(crate) fn routes() -> Router<Node> {
    Router::new().route("/editor/settings", get(get_settings).put(put_settings))
}

async fn get_settings(State(n): State<Node>) -> Json<EditorSettingsState> {
    Json(n.editor_settings().state())
}

async fn put_settings(State(n): State<Node>, body: Bytes) -> Response {
    let s: NodeEditorSettings = match serde_json::from_slice(&body) {
        Ok(s) => s,
        Err(e) => {
            return ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, format!("bad request body: {e}"))
                .into_response()
        }
    };
    if let Err(m) = s.validate() {
        return ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, m).into_response();
    }
    let source = n.editor().source().clone();
    Json(n.editor_settings().apply(&source, s).await).into_response()
}

impl NodeClient {
    /// `GET /editor/settings`: the node's last settings and report.
    pub async fn editor_settings(&self) -> Result<EditorSettingsState, ClientError> {
        self.get("/editor/settings").await
    }

    /// `PUT /editor/settings`: applies `s` on the node and answers its report. An install can take
    /// minutes: give the client a deadline to match.
    pub async fn put_editor_settings(&self, s: &NodeEditorSettings) -> Result<ApplyReport, ClientError> {
        self.put("/editor/settings", s).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_w7a_runtimes_map_from_the_editor_server() {
        assert_eq!(EditorServer::CodeOse.runtime(), "ose");
        assert_eq!(EditorServer::Vscode.runtime(), "vsc");
        assert_eq!(EditorServer::ManualScript.runtime(), "script");
        assert_eq!(serde_json::to_value(EditorServer::ManualScript).unwrap(), "manual_script");
        assert!(serde_json::from_value::<EditorServer>(serde_json::json!("emacs")).is_err());
    }

    #[test]
    fn fr_w7a_cli_is_the_env_binary_else_darkpyonix_on_path() {
        assert_eq!(darkpyonix_bin(None), PathBuf::from("darkpyonix"));
        assert_eq!(darkpyonix_bin(Some(OsString::new())), PathBuf::from("darkpyonix"));
        assert_eq!(darkpyonix_bin(Some("/opt/dpx/bin/darkpyonix".into())), PathBuf::from("/opt/dpx/bin/darkpyonix"));
    }

    #[test]
    fn fr_w7a_validate_refuses_bad_ids_and_a_missing_script() {
        let ext = |id: &str| EditorExtension { id: id.into(), version: None };
        assert!(validate_parts(EditorServer::CodeOse, None, &[ext("redhat.vscode-yaml")], Some("DarkPyonix Dark")).is_ok());
        assert!(validate_parts(EditorServer::CodeOse, None, &[ext("../x")], None).is_err());
        assert!(validate_parts(EditorServer::ManualScript, None, &[], None).is_err());
        assert!(validate_parts(EditorServer::ManualScript, Some("  "), &[], None).is_err());
        assert!(validate_parts(EditorServer::ManualScript, Some("exec code serve-web"), &[], None).is_ok());
        assert!(validate_parts(EditorServer::CodeOse, None, &[], Some("")).is_err());
        assert!(validate_parts(EditorServer::CodeOse, None, &[], Some("a\nb")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn fr_w7a_state_file_is_0600_and_round_trips() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = EditorSettingsState {
            settings: Some(NodeEditorSettings { theme: Some("T".into()), ..Default::default() }),
            applied: Some(ApplyReport::all_failed("x")),
        };
        save(dir.path(), &state).unwrap();
        let mode = std::fs::metadata(dir.path().join(SETTINGS_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load(dir.path()), Some(state));
        let (path, changed) = write_script(dir.path(), "echo hi\n").unwrap();
        assert!(changed);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(!write_script(dir.path(), "echo hi\n").unwrap().1, "the same script is not new");
    }
}
