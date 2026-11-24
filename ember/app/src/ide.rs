//! "Open IDE": what each target does once the server has said how to reach it
//! (`GET /api/sessions/{id}/ide`).
//!
//! - **VS Code**: opened strictly in the standalone `ember-ide-webview` child process (never
//!   falling back to an external browser). With a launch that carries the server's relay route
//!   (`launch.ose.relay`, `FR-W4c` (1b), INTENT D55) the window runs in its relay mode: it starts
//!   the loopback gateway of `FR-W5b` and loads the workbench through the relay, with the server
//!   token in its environment. An older server's target with a `url` still opens that URL.
//!   This launcher process never embeds a browser engine directly.
//! - **Gateway**: the `jetbrains-gateway://` link, handed to the OS URL handler, which starts
//!   JetBrains Gateway on this machine.
//! - **Ember**: the editor screen.
//!
//! Only `http`, `https` and the JetBrains schemes are ever passed out, so a server
//! answer cannot make this app open an arbitrary file or program.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::server::IdeTarget;
use crate::ui::editor::EditorTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeKind {
    Ember,
    VsCode,
    Gateway,
}

impl IdeKind {
    pub const ALL: [IdeKind; 3] = [IdeKind::Ember, IdeKind::VsCode, IdeKind::Gateway];

    /// The server's `kind` string.
    pub fn key(self) -> &'static str {
        match self {
            IdeKind::Ember => "ember",
            IdeKind::VsCode => "vscode",
            IdeKind::Gateway => "gateway",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            IdeKind::Ember => "Ember IDE",
            IdeKind::VsCode => "VS Code",
            IdeKind::Gateway => "JetBrains Gateway",
        }
    }

    pub fn from_key(k: &str) -> Option<IdeKind> {
        IdeKind::ALL.into_iter().find(|i| i.key() == k)
    }
}

/// What choosing a target does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdeAction {
    /// Launch the standalone IDE window binary (`ember-ide-webview`) at this URL.
    OpenIdeWindow(String),
    /// Launch the standalone IDE window binary in its relay mode (`FR-W5b`, INTENT D55).
    OpenIdeRelay(IdeRelay),
    /// Hand this URL to the OS.
    OpenUrl(String),
    /// Open the Ember editor screen (its code area waits for the editor core, M8).
    EmberPlaceholder,
    /// Why it cannot be opened.
    Unavailable(String),
}

/// The VS Code target's relay launch: what `ember-ide-webview --relay-path` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeRelay {
    /// The server's relay route to the computer's OSE, `/api/computers/{id}/ide/relay/{launch}`.
    pub relay: String,
    /// The folder the workbench opens.
    pub folder: String,
    /// The server's base URL from the launch; empty means the app's own server.
    pub server_url: String,
}

/// The relay launch of a VS Code target, when the server sent one.
pub(crate) fn relay_launch(t: &IdeTarget) -> Option<IdeRelay> {
    let launch = t.launch.as_ref()?;
    let relay = launch.ose.as_ref()?.relay.clone().filter(|r| !r.trim().is_empty())?;
    Some(IdeRelay {
        relay,
        folder: launch.folder.clone(),
        server_url: launch.server_url.clone().unwrap_or_default(),
    })
}

pub fn plan(kind: IdeKind, target: Option<&IdeTarget>) -> IdeAction {
    if kind == IdeKind::Ember {
        // The caller opens the editor screen from the launch data (project, folder).
        return IdeAction::EmberPlaceholder;
    }
    let Some(t) = target else {
        return IdeAction::Unavailable(format!("the server offers no {} target", kind.label()));
    };
    if !t.available {
        return IdeAction::Unavailable(t.reason.clone().unwrap_or_else(|| format!("{} is not available", kind.label())));
    }
    if kind == IdeKind::VsCode {
        if let Some(relay) = relay_launch(t) {
            if !relay.relay.starts_with("/api/") {
                return IdeAction::Unavailable(format!("refusing relay path {:?}", relay.relay));
            }
            if relay.folder.trim().is_empty() {
                return IdeAction::Unavailable("the server gave no folder for VS Code".into());
            }
            return IdeAction::OpenIdeRelay(relay);
        }
    }
    match t.url.as_deref() {
        Some(url) if allowed_url(kind, url) => match kind {
            IdeKind::VsCode => IdeAction::OpenIdeWindow(url.to_string()),
            IdeKind::Gateway => IdeAction::OpenUrl(url.to_string()),
            IdeKind::Ember => IdeAction::EmberPlaceholder,
        },
        Some(url) => IdeAction::Unavailable(format!("refusing to open {url:?}: unexpected scheme")),
        None => IdeAction::Unavailable(format!("the server gave no URL for {}", kind.label())),
    }
}

fn allowed_url(kind: IdeKind, url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    match kind {
        IdeKind::VsCode => lower.starts_with("http://") || lower.starts_with("https://"),
        IdeKind::Gateway => lower.starts_with("jetbrains-gateway://") || lower.starts_with("jetbrains://"),
        IdeKind::Ember => false,
    }
}

/// The argv that hands `url` to this OS's URL handler (default browser, Gateway, …).
pub fn open_command(url: &str) -> Vec<String> {
    let argv: Vec<&str> = if cfg!(target_os = "macos") {
        vec!["open", url]
    } else if cfg!(target_os = "windows") {
        // Not `cmd /c start`: cmd would split a Gateway link at its `&`s.
        vec!["rundll32", "url.dll,FileProtocolHandler", url]
    } else {
        vec!["xdg-open", url]
    };
    argv.into_iter().map(String::from).collect()
}

/// Open `url` with the OS handler. Spawns and returns; does not wait for the browser.
pub fn open_external(url: &str) -> std::io::Result<()> {
    let argv = open_command(url);
    Command::new(&argv[0]).args(&argv[1..]).spawn().map(|_| ())
}

/// Environment variable overriding the path to the standalone IDE window binary.
pub const IDE_WEBVIEW_BIN_ENV: &str = "EMBER_IDE_WEBVIEW_BIN";

/// Executable file name of the standalone IDE window binary.
pub const IDE_WEBVIEW_BIN_NAME: &str = if cfg!(windows) {
    "ember-ide-webview.exe"
} else {
    "ember-ide-webview"
};

/// Resolve the standalone IDE window binary in priority order:
/// 1. `EMBER_IDE_WEBVIEW_BIN` environment override,
/// 2. sibling executable next to the running `ember-app` binary,
/// 3. lookup on `PATH`.
pub fn resolve_ide_webview_bin(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(p) = env_override.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(sibling) = current_exe
        .and_then(Path::parent)
        .map(|dir| dir.join(IDE_WEBVIEW_BIN_NAME))
        .filter(|p| p.is_file())
    {
        return Some(sibling);
    }
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            let candidate = dir.join(IDE_WEBVIEW_BIN_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The `(executable, args)` pair that launches the standalone IDE window binary.
pub fn ide_window_command(bin: &Path, url: &str) -> (PathBuf, Vec<String>) {
    (
        bin.to_path_buf(),
        vec!["--url".to_string(), url.to_string()],
    )
}

/// The environment variable that carries the server token to the IDE window's relay mode, never
/// argv (`ember/ide-webview/src/cli.rs` `SERVER_TOKEN_ENV`).
pub const IDE_SERVER_TOKEN_ENV: &str = "EMBER_SERVER_TOKEN";

/// The `(executable, args)` pair that launches the IDE window in its relay mode.
pub fn ide_relay_command(bin: &Path, server_url: &str, relay: &IdeRelay) -> (PathBuf, Vec<String>) {
    (
        bin.to_path_buf(),
        vec![
            "--relay-path".to_string(),
            relay.relay.clone(),
            "--server-url".to_string(),
            server_url.to_string(),
            "--folder".to_string(),
            relay.folder.clone(),
        ],
    )
}

/// Resolve and spawn `ember-ide-webview --relay-path <relay> --server-url <url> --folder <dir>`,
/// with `token` handed to `spawn` for the child's environment. Never falls back to a browser.
pub fn open_ide_relay_with(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    server_url: &str,
    relay: &IdeRelay,
    token: Option<&str>,
    spawn: impl FnOnce(&Path, &[String], Option<&str>) -> std::io::Result<()>,
) -> Result<(), String> {
    let Some(exe) = resolve_ide_webview_bin(env_override, current_exe, path_var) else {
        let msg = missing_ide_webview_error(env_override, current_exe, path_var);
        tracing::warn!(relay = %relay.relay, "{msg}");
        return Err(msg);
    };
    let (program, args) = ide_relay_command(&exe, server_url, relay);
    spawn(&program, &args, token).map_err(|e| {
        let msg = format!("failed to launch {} ({}): {e}", IDE_WEBVIEW_BIN_NAME, program.display());
        tracing::warn!(relay = %relay.relay, "{msg}");
        msg
    })
}

/// Open the VS Code target's relay launch in `ember-ide-webview`'s relay mode. The server is the
/// launch's, else the app's own. The window dials the relay over `ws(s)` with the server token
/// under the terminal relay's rules (`FR-S11`, `FR-L31.19`); a server reached over the device
/// link (`peer:<id>`) is not supported by the window yet.
pub fn open_ide_relay(relay: &IdeRelay) -> Result<(), String> {
    let server_url = if relay.server_url.trim().is_empty() {
        crate::services::try_services().map(|s| s.config.server_url.clone()).unwrap_or_default()
    } else {
        relay.server_url.clone()
    };
    if crate::terminal_stream::is_peer_base(&server_url) {
        return Err("기기 연결로 접속한 서버에서는 아직 VS Code 창을 열 수 없습니다".to_owned());
    }
    crate::terminal_stream::relay_allowed(&server_url)?;
    let token = crate::terminal_stream::server_token(&server_url);
    let env_override = std::env::var(IDE_WEBVIEW_BIN_ENV).ok();
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    open_ide_relay_with(
        env_override.as_deref(),
        current_exe.as_deref(),
        path_var.as_deref(),
        &server_url,
        relay,
        token.as_deref(),
        |program, args, token| {
            let mut cmd = Command::new(program);
            cmd.args(args);
            match token {
                Some(t) => cmd.env(IDE_SERVER_TOKEN_ENV, t),
                None => cmd.env_remove(IDE_SERVER_TOKEN_ENV),
            };
            cmd.spawn().map(|_| ())
        },
    )
}

/// Format a clear error naming the missing standalone IDE window binary and every
/// location that was checked.
pub fn missing_ide_webview_error(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> String {
    let mut searched = Vec::new();
    match env_override.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => searched.push(format!("{IDE_WEBVIEW_BIN_ENV}={p}")),
        None => searched.push(format!("{IDE_WEBVIEW_BIN_ENV} (unset)")),
    }
    if let Some(dir) = current_exe.and_then(Path::parent) {
        searched.push(format!("sibling {}", dir.join(IDE_WEBVIEW_BIN_NAME).display()));
    }
    if path_var.is_some_and(|p| !p.is_empty()) {
        searched.push("PATH".to_owned());
    } else {
        searched.push("PATH (unset)".to_owned());
    }
    format!(
        "{IDE_WEBVIEW_BIN_NAME} binary was not found (checked: {}). Install or build {IDE_WEBVIEW_BIN_NAME}, or set {IDE_WEBVIEW_BIN_ENV}.",
        searched.join(", ")
    )
}

/// Resolve and spawn `ember-ide-webview --url <url>`.
///
/// Never falls back to an external browser: if the helper binary is missing or
/// fails to start, returns an error naming `ember-ide-webview` and where it was
/// looked for.
pub fn open_ide_window_with(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    url: &str,
    spawn: impl FnOnce(&Path, &[String]) -> std::io::Result<()>,
) -> Result<(), String> {
    let Some(exe) = resolve_ide_webview_bin(env_override, current_exe, path_var) else {
        let msg = missing_ide_webview_error(env_override, current_exe, path_var);
        tracing::warn!(url = %url, "{msg}");
        return Err(msg);
    };
    let (program, args) = ide_window_command(&exe, url);
    spawn(&program, &args).map_err(|e| {
        let msg = format!(
            "failed to launch {} ({}): {e}",
            IDE_WEBVIEW_BIN_NAME,
            program.display()
        );
        tracing::warn!(url = %url, "{msg}");
        msg
    })
}

/// Open `url` in the standalone `ember-ide-webview` binary. Never falls back to
/// an external browser.
pub fn open_ide_window(url: &str) -> Result<(), String> {
    let env_override = std::env::var(IDE_WEBVIEW_BIN_ENV).ok();
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    open_ide_window_with(
        env_override.as_deref(),
        current_exe.as_deref(),
        path_var.as_deref(),
        url,
        |program, args| Command::new(program).args(args).spawn().map(|_| ()),
    )
}

/// CLI flag that launches this binary directly into the editor, which runs as its own HTML-mode window (FR-L31).
/// TODO(dioxus-compose#161): open the editor window on the shell's host instead of spawning a
/// second process. The `Window` element (dioxus-compose#152) is in the pin; an HTML page under a
/// Compose root is what is missing.
pub const EDITOR_ARG: &str = "--editor";

/// Parse `--editor <project> [folder]` from an argument list (`std::env::args()`).
pub fn parse_editor_args<I, S>(args: I) -> Option<EditorTarget>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    parse_editor_args_with_env(
        args,
        var(EDITOR_TOKEN_ENV),
        var(EDITOR_SERVER_ENV),
        var(EDITOR_COMPUTER_ENV),
        var(EDITOR_RELAY_ENV),
        VsCodeEnv { url: var(EDITOR_VSCODE_ENV), relay: var(EDITOR_VSCODE_RELAY_ENV), folder: var(EDITOR_VSCODE_FOLDER_ENV) },
    )
}

/// Environment variable carrying the editor connection token (never argv).
pub const EDITOR_TOKEN_ENV: &str = "EMBER_EDITOR_TOKEN";
/// Environment variable carrying the ember server's base URL.
pub const EDITOR_SERVER_ENV: &str = "EMBER_EDITOR_SERVER";
/// Environment variable carrying the computer id for the terminal relay.
pub const EDITOR_COMPUTER_ENV: &str = "EMBER_EDITOR_COMPUTER";
/// Environment variable carrying the server's relay route to the OSE (FR-N9.1), which the
/// editor dials instead of the endpoint when it is set.
pub const EDITOR_RELAY_ENV: &str = "EMBER_EDITOR_RELAY";
/// Environment variable carrying an old server's VS Code target URL (a target with `url` and no
/// relay launch), which the editor window's fallback opens in URL mode (SPEC FR-E10, #32). Empty
/// when the target has a relay launch.
pub const EDITOR_VSCODE_ENV: &str = "EMBER_EDITOR_VSCODE";
/// Environment variable carrying the VS Code target's relay path (`launch.ose.relay`,
/// `/api/computers/{id}/ide/relay/{launch}`, FR-W4c, INTENT D55), which the editor window's
/// fallback opens with `ember-ide-webview --relay-path` against the editor's own server
/// (`EMBER_EDITOR_SERVER`) and server token (SPEC FR-E10).
pub const EDITOR_VSCODE_RELAY_ENV: &str = "EMBER_EDITOR_VSCODE_RELAY";
/// Environment variable carrying the folder the VS Code target's relay launch opens
/// (`launch.folder`), beside [`EDITOR_VSCODE_RELAY_ENV`].
pub const EDITOR_VSCODE_FOLDER_ENV: &str = "EMBER_EDITOR_VSCODE_FOLDER";

/// The VS Code target the editor window's fallback opens, as it travels in the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VsCodeEnv {
    url: String,
    relay: String,
    folder: String,
}

/// What the Ember editor window's `VS Code에서 열기` opens when the server is a Code-OSS commit
/// the editor does not support (SPEC FR-E10, #32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsCodeFallback {
    /// The VS Code target's relay launch (FR-W4c, INTENT D55): the IDE window's relay mode.
    Relay(IdeRelay),
    /// An old server's VS Code target URL: the IDE window's URL mode.
    Url(String),
}

impl VsCodeFallback {
    /// The VS Code target of an IDE launch, when it is available: its relay launch, else an old
    /// server's `url`.
    pub fn of_target(target: Option<&IdeTarget>) -> Option<VsCodeFallback> {
        let t = target.filter(|t| t.available)?;
        if let Some(relay) = relay_launch(t) {
            return Some(VsCodeFallback::Relay(relay));
        }
        t.url.clone().filter(|u| !u.trim().is_empty()).map(VsCodeFallback::Url)
    }

    /// The fallback of an editor window's target, checked as [`plan`] checks the VS Code target:
    /// a relay path under `/api/` with a folder, dialed on the editor's own server; else an
    /// http(s) URL. `None` when the target offers neither, and the window then shows no button.
    pub fn for_editor(target: &EditorTarget) -> Option<VsCodeFallback> {
        let relay = target.vscode_relay.trim();
        if !relay.is_empty() {
            if !relay.starts_with("/api/") || target.vscode_folder.trim().is_empty() {
                return None;
            }
            return Some(VsCodeFallback::Relay(IdeRelay {
                relay: relay.to_owned(),
                folder: target.vscode_folder.clone(),
                server_url: target.server_url.clone(),
            }));
        }
        let url = target.vscode_url.trim();
        allowed_url(IdeKind::VsCode, url).then(|| VsCodeFallback::Url(url.to_owned()))
    }

    /// Spawn the IDE window for this fallback: relay mode with the server token in its
    /// environment ([`open_ide_relay`]), or URL mode ([`open_ide_window`]).
    pub fn open(&self) -> Result<(), String> {
        match self {
            VsCodeFallback::Relay(relay) => open_ide_relay(relay),
            VsCodeFallback::Url(url) => open_ide_window(url),
        }
    }
}

fn parse_editor_args_with_env<I, S>(
    args: I,
    token: String,
    server_url: String,
    computer: String,
    relay: String,
    vscode: VsCodeEnv,
) -> Option<EditorTarget>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg.as_ref() == EDITOR_ARG {
            let project = it.next()?.as_ref().to_string();
            let folder = it
                .next()
                .map(|s| s.as_ref().to_string())
                .unwrap_or_default();
            let endpoint = it.next().map(|s| s.as_ref().to_string()).unwrap_or_default();
            return Some(EditorTarget {
                project,
                folder,
                endpoint,
                token,
                server_url,
                computer,
                relay,
                vscode_url: vscode.url,
                vscode_relay: vscode.relay,
                vscode_folder: vscode.folder,
            });
        }
    }
    None
}

/// The `(executable, args)` pair that spawns a separate Ember editor window (FR-L31).
/// TODO(dioxus-compose#161): replace with an in-process `Window` node (dioxus-compose#152, in
/// the pin) once an HTML page can sit under the shell's Compose root.
pub fn editor_command(exe: &Path, project: &str, folder: &str, endpoint: &str) -> (PathBuf, Vec<String>) {
    (
        exe.to_path_buf(),
        vec![
            EDITOR_ARG.to_string(),
            project.to_string(),
            folder.to_string(),
            endpoint.to_string(),
        ],
    )
}

/// Spawn `exe --editor <project> <folder>` and return without waiting for the window to close.
pub fn spawn_editor_with(exe: &Path, project: &str, folder: &str) -> std::io::Result<std::process::Child> {
    spawn_editor_target(exe, &EditorTarget { project: project.into(), folder: folder.into(), ..Default::default() })
}

/// Spawn an editor with the token in the child environment, never in argv.
pub fn spawn_editor_target(exe: &Path, target: &EditorTarget) -> std::io::Result<std::process::Child> {
    let (bin, args) = editor_command(exe, &target.project, &target.folder, &target.endpoint);
    // The owner handoff (FR-S12, #1042): a one-time token, recorded with the child's PID.
    let handoff = crate::owner_handoff::new_token();
    let child = Command::new(bin)
        .args(args)
        .env(crate::owner_handoff::HANDOFF_ENV, &handoff)
        .env(EDITOR_TOKEN_ENV, &target.token)
        .env(EDITOR_SERVER_ENV, &target.server_url)
        .env(EDITOR_COMPUTER_ENV, &target.computer)
        .env(EDITOR_RELAY_ENV, &target.relay)
        .env(EDITOR_VSCODE_ENV, &target.vscode_url)
        .env(EDITOR_VSCODE_RELAY_ENV, &target.vscode_relay)
        .env(EDITOR_VSCODE_FOLDER_ENV, &target.vscode_folder)
        .spawn()?;
    let mut reg = crate::owner_handoff::registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.record(handoff, u64::from(child.id()), std::time::Instant::now());
    Ok(child)
}

/// Spawn a new Ember process in `--editor <project> <folder>` mode (FR-L31).
pub fn spawn_editor_process(project: &str, folder: &str) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    spawn_editor_with(&exe, project, folder).map(|_| ())
}

/// Run `spawner(project, folder)` and format the user-facing notice string for the result.
pub fn spawn_editor_notice(
    project: &str,
    folder: &str,
    spawner: impl FnOnce(&str, &str) -> std::io::Result<()>,
) -> String {
    match spawner(project, folder) {
        Ok(()) => "Ember 에디터: 새 창에서 여는 중입니다".to_string(),
        Err(e) => {
            tracing::warn!(project = %project, "could not spawn editor window: {e}");
            format!("Ember 에디터: 열지 못했습니다: {e}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: &str, available: bool, url: Option<&str>) -> IdeTarget {
        IdeTarget {
            kind: kind.into(),
            available,
            url: url.map(String::from),
            command: None,
            launch: None,
            reason: (!available).then(|| "no ide_url configured".into()),
        }
    }

    #[test]
    fn fr_l7_targets() {
        assert_eq!(
            plan(IdeKind::VsCode, Some(&target("vscode", true, Some("http://h:8890/?folder=%2Fw")))),
            IdeAction::OpenIdeWindow("http://h:8890/?folder=%2Fw".into())
        );
        assert_eq!(
            plan(IdeKind::Gateway, Some(&target("gateway", true, Some("jetbrains-gateway://connect#type=ssh")))),
            IdeAction::OpenUrl("jetbrains-gateway://connect#type=ssh".into())
        );
        assert_eq!(plan(IdeKind::Ember, None), IdeAction::EmberPlaceholder);
        assert_eq!(
            plan(IdeKind::VsCode, Some(&target("vscode", false, None))),
            IdeAction::Unavailable("no ide_url configured".into())
        );
        // A server cannot make the client open a local file or program.
        assert!(matches!(
            plan(IdeKind::VsCode, Some(&target("vscode", true, Some("file:///etc/passwd")))),
            IdeAction::Unavailable(_)
        ));
        assert!(matches!(
            plan(IdeKind::Gateway, Some(&target("gateway", true, Some("http://x")))),
            IdeAction::Unavailable(_)
        ));
        assert_eq!(IdeKind::from_key("gateway"), Some(IdeKind::Gateway));
        assert_eq!(open_command("u").last().map(String::as_str), Some("u"));
    }

    /// SPEC FR-L7 (VS Code target), FR-W4c (1b), INTENT D55 (#1064): a VS Code target whose
    /// launch carries the relay route (and no `url`) opens the IDE window in its relay mode, with
    /// the server token in the child's environment and never in argv.
    #[test]
    fn fr_l7_vscode_target_with_relay_spawns_the_window_in_relay_mode() {
        let t: IdeTarget = serde_json::from_value(serde_json::json!({
            "kind": "vscode", "available": true,
            "launch": {
                "session": "s", "project": "p", "computer": "node-a", "computer_name": "studio",
                "folder": "/work/repo", "server_url": "http://127.0.0.1:8740",
                "ose": { "relay": "/api/computers/node-a/ide/relay/l1" }
            }
        }))
        .unwrap();
        let relay = IdeRelay {
            relay: "/api/computers/node-a/ide/relay/l1".into(),
            folder: "/work/repo".into(),
            server_url: "http://127.0.0.1:8740".into(),
        };
        assert_eq!(plan(IdeKind::VsCode, Some(&t)), IdeAction::OpenIdeRelay(relay.clone()));

        let mut captured: Option<(PathBuf, Vec<String>, Option<String>)> = None;
        open_ide_relay_with(
            Some("/custom/ember-ide-webview"),
            None,
            None,
            "http://127.0.0.1:8740",
            &relay,
            Some("srv-secret"),
            |prog, args, token| {
                captured = Some((prog.to_path_buf(), args.to_vec(), token.map(str::to_owned)));
                Ok(())
            },
        )
        .expect("spawn");
        let (prog, args, token) = captured.unwrap();
        assert_eq!(prog, PathBuf::from("/custom/ember-ide-webview"));
        assert_eq!(
            args,
            vec![
                "--relay-path",
                "/api/computers/node-a/ide/relay/l1",
                "--server-url",
                "http://127.0.0.1:8740",
                "--folder",
                "/work/repo",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("srv-secret")), "the token is never in argv");
        assert!(!args.iter().any(|a| a == "--url"));
        assert_eq!(token.as_deref(), Some("srv-secret"), "the token goes to the child's environment");
        assert_eq!(IDE_SERVER_TOKEN_ENV, "EMBER_SERVER_TOKEN");

        // An older server's target with a url keeps the url path; a relay target with no folder
        // or a relay path outside /api is not opened.
        let mut old = t.clone();
        old.launch = None;
        old.url = Some("http://h:8890/?folder=%2Fw".into());
        assert_eq!(plan(IdeKind::VsCode, Some(&old)), IdeAction::OpenIdeWindow("http://h:8890/?folder=%2Fw".into()));
        let mut no_folder = t.clone();
        no_folder.launch.as_mut().unwrap().folder = String::new();
        assert!(matches!(plan(IdeKind::VsCode, Some(&no_folder)), IdeAction::Unavailable(_)));
        let mut bad = t.clone();
        bad.launch.as_mut().unwrap().ose.as_mut().unwrap().relay = Some("/other/x".into());
        assert!(matches!(plan(IdeKind::VsCode, Some(&bad)), IdeAction::Unavailable(_)));
        // The Ember target's relay is not the VS Code window's.
        let mut ember = t.clone();
        ember.kind = "ember".into();
        assert_eq!(plan(IdeKind::Ember, Some(&ember)), IdeAction::EmberPlaceholder);
    }

    #[test]
    fn fr_l7_ide_webview_binary_resolution_and_command() {
        let base = std::env::temp_dir().join(format!(
            "ember-ide-webview-resolve-{}",
            std::process::id()
        ));
        let sibling_dir = base.join("sibling");
        let path_dir = base.join("path_bin");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&sibling_dir).unwrap();
        std::fs::create_dir_all(&path_dir).unwrap();

        let sibling_bin = sibling_dir.join(IDE_WEBVIEW_BIN_NAME);
        let path_bin = path_dir.join(IDE_WEBVIEW_BIN_NAME);
        std::fs::write(&sibling_bin, b"stub").unwrap();
        std::fs::write(&path_bin, b"stub").unwrap();

        let fake_exe = sibling_dir.join("ember-app");
        let path_os = std::env::join_paths([&path_dir]).unwrap();

        // 1. Environment override wins over sibling and PATH.
        assert_eq!(
            resolve_ide_webview_bin(
                Some("/custom/ember-ide-webview"),
                Some(&fake_exe),
                Some(path_os.as_os_str()),
            ),
            Some(PathBuf::from("/custom/ember-ide-webview"))
        );

        // 2. Sibling next to current_exe wins over PATH.
        assert_eq!(
            resolve_ide_webview_bin(None, Some(&fake_exe), Some(path_os.as_os_str())),
            Some(sibling_bin.clone())
        );

        // 3. PATH lookup finds the binary when no sibling exists.
        let other_exe = base.join("other/ember-app");
        assert_eq!(
            resolve_ide_webview_bin(None, Some(&other_exe), Some(path_os.as_os_str())),
            Some(path_bin)
        );

        // 4. Returns None when nothing matches.
        assert_eq!(resolve_ide_webview_bin(None, Some(&other_exe), None), None);

        // Command construction passes --url <url>.
        let (bin, args) = ide_window_command(&sibling_bin, "http://127.0.0.1:8890/?folder=%2Fw");
        assert_eq!(bin, sibling_bin);
        assert_eq!(args, vec!["--url", "http://127.0.0.1:8890/?folder=%2Fw"]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fr_l7_open_ide_window_errors_without_browser_fallback_when_missing() {
        let missing_exe = Path::new("/nonexistent/ember-dir/ember-app");
        let mut spawn_called = false;
        let err = open_ide_window_with(
            None,
            Some(missing_exe),
            None,
            "http://127.0.0.1:8890/?folder=%2Fw",
            |_, _| {
                spawn_called = true;
                Ok(())
            },
        )
        .expect_err("missing helper binary must return an error instead of falling back");
        assert!(
            !spawn_called,
            "spawner or external browser opener must never be invoked when binary is missing"
        );
        assert!(
            err.contains(IDE_WEBVIEW_BIN_NAME),
            "error should name {IDE_WEBVIEW_BIN_NAME}, got: {err}"
        );
        assert!(
            err.contains(IDE_WEBVIEW_BIN_ENV),
            "error should mention {IDE_WEBVIEW_BIN_ENV}, got: {err}"
        );
        assert!(
            err.contains("ember-dir"),
            "error should include searched sibling directory, got: {err}"
        );
    }

    #[test]
    fn fr_l7_open_ide_window_spawns_helper_and_reports_spawn_error() {
        let mut captured: Option<(PathBuf, Vec<String>)> = None;
        open_ide_window_with(
            Some("/custom/ember-ide-webview"),
            None,
            None,
            "http://127.0.0.1:8890/?folder=%2Fw",
            |prog, args| {
                captured = Some((prog.to_path_buf(), args.to_vec()));
                Ok(())
            },
        )
        .expect("spawn helper succeeds");
        assert_eq!(
            captured,
            Some((
                PathBuf::from("/custom/ember-ide-webview"),
                vec![
                    "--url".to_owned(),
                    "http://127.0.0.1:8890/?folder=%2Fw".to_owned(),
                ]
            ))
        );

        let err = open_ide_window_with(
            Some("/custom/ember-ide-webview"),
            None,
            None,
            "http://127.0.0.1:8890/?folder=%2Fw",
            |_, _| Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")),
        )
        .expect_err("spawn failure must return an error");
        assert!(err.contains(IDE_WEBVIEW_BIN_NAME));
        assert!(err.contains("/custom/ember-ide-webview"));
    }

    #[test]
    fn fr_l31_editor_args_parse_and_spawn_separate_window_process() {
        assert_eq!(parse_editor_args(["ember"]), None);
        assert_eq!(parse_editor_args(["ember", "--editor"]), None);
        assert_eq!(
            parse_editor_args(["ember", "--editor", "demo-project", "/work/demo"]),
            Some(EditorTarget {
                project: "demo-project".into(),
                folder: "/work/demo".into(),
                ..Default::default()
            })
        );
        assert_eq!(
            parse_editor_args(["ember", "--editor", "demo-project"]),
            Some(EditorTarget {
                project: "demo-project".into(),
                folder: String::new(),
                ..Default::default()
            })
        );

        let (exe, args) = editor_command(Path::new("/usr/local/bin/ember"), "ember-core", "/repo", "");
        assert_eq!(exe, PathBuf::from("/usr/local/bin/ember"));
        assert_eq!(args, vec!["--editor", "ember-core", "/repo", ""]);
        assert_eq!(
            parse_editor_args(std::iter::once("ember".to_string()).chain(args)),
            Some(EditorTarget {
                project: "ember-core".into(),
                folder: "/repo".into(),
                ..Default::default()
            })
        );

        // Verify the spawn path executes a child process cleanly with the `--editor` arguments.
        #[cfg(unix)]
        {
            let mut child = spawn_editor_with(Path::new("/usr/bin/true"), "ember-core", "/repo")
                .expect("spawn editor process");
            let status = child.wait().expect("wait for child");
            assert!(status.success());
        }
    }

    #[test]
    fn fr_l31_editor_connection_arguments_round_trip() {
        let (_, args) = editor_command(Path::new("ember"), "project", "/work", "localhost:8000");
        assert!(!args.iter().any(|arg| arg == "secret"));
        assert!(!args.iter().any(|arg| arg == "http://srv:1" || arg == "node-a"));
        let target = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args),
            "secret".into(),
            "http://srv:1".into(),
            "node-a".into(),
            String::new(),
            VsCodeEnv::default(),
        )
        .unwrap();
        assert_eq!(target.endpoint, "localhost:8000");
        assert_eq!(target.token, "secret");
        assert_eq!(target.server_url, "http://srv:1");
        assert_eq!(target.computer, "node-a");
        assert_eq!(target.relay, "");
    }

    #[test]
    fn fr_n9_editor_args_carry_the_relay_path_in_the_environment() {
        let relay = "/api/computers/local/ide/relay/abc";
        let (_, args) = editor_command(Path::new("ember"), "project", "/work", "127.0.0.1:9000");
        assert!(!args.iter().any(|arg| arg.contains("/ide/relay/")), "the relay path is not in argv");
        let target = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args),
            "placeholder".into(),
            "peer:abc".into(),
            "local".into(),
            relay.into(),
            VsCodeEnv::default(),
        )
        .unwrap();
        assert_eq!(target.relay, relay);
        assert_eq!(target.endpoint, "127.0.0.1:9000");
        assert_eq!(EDITOR_RELAY_ENV, "EMBER_EDITOR_RELAY");
    }

    /// SPEC FR-E10 (#32), FR-W4c: the VS Code target the fallback offers travels in the
    /// environment, its relay launch (path and folder) or an old server's URL, never in argv.
    #[test]
    fn fr_e10_editor_args_carry_the_vscode_relay_in_the_environment() {
        let (_, args) = editor_command(Path::new("ember"), "project", "/work", "127.0.0.1:9000");
        assert!(!args.iter().any(|arg| arg.contains("/ide/relay/")), "the relay path is not in argv");
        let target = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args.clone()),
            String::new(),
            "http://srv:1".into(),
            String::new(),
            String::new(),
            VsCodeEnv {
                url: String::new(),
                relay: "/api/computers/node-a/ide/relay/v1".into(),
                folder: "/work/repo".into(),
            },
        )
        .unwrap();
        assert_eq!(target.vscode_relay, "/api/computers/node-a/ide/relay/v1");
        assert_eq!(target.vscode_folder, "/work/repo");
        assert_eq!(target.vscode_url, "");
        assert_eq!(EDITOR_VSCODE_RELAY_ENV, "EMBER_EDITOR_VSCODE_RELAY");
        assert_eq!(EDITOR_VSCODE_FOLDER_ENV, "EMBER_EDITOR_VSCODE_FOLDER");

        // An old server's VS Code target (`url`, no relay launch) still travels as the URL.
        let old = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            VsCodeEnv { url: "http://node:8890/?folder=%2Fwork".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(old.vscode_url, "http://node:8890/?folder=%2Fwork");
        assert_eq!(EDITOR_VSCODE_ENV, "EMBER_EDITOR_VSCODE");
    }

    /// SPEC FR-E10 (#32), FR-L7, INTENT D55: the fallback of an editor window whose VS Code target
    /// is a relay launch spawns `ember-ide-webview` in relay mode on the editor's own server, with
    /// the server token in the child's environment and never in argv; a bad relay path offers
    /// nothing, and an old server's URL still opens in URL mode.
    #[test]
    fn fr_e10_fallback_opens_vscode_through_the_relay() {
        let target = EditorTarget {
            project: "p".into(),
            folder: "/work/repo".into(),
            server_url: "http://127.0.0.1:8740".into(),
            computer: "node-a".into(),
            relay: "/api/computers/node-a/ide/relay/e1".into(),
            vscode_relay: "/api/computers/node-a/ide/relay/v1".into(),
            vscode_folder: "/work/repo".into(),
            ..Default::default()
        };
        let relay = IdeRelay {
            relay: "/api/computers/node-a/ide/relay/v1".into(),
            folder: "/work/repo".into(),
            server_url: "http://127.0.0.1:8740".into(),
        };
        let fallback = VsCodeFallback::for_editor(&target);
        assert_eq!(fallback, Some(VsCodeFallback::Relay(relay.clone())));

        let mut captured: Option<(PathBuf, Vec<String>, Option<String>)> = None;
        open_ide_relay_with(
            Some("/custom/ember-ide-webview"),
            None,
            None,
            &relay.server_url,
            &relay,
            Some("srv-secret"),
            |prog, args, token| {
                captured = Some((prog.to_path_buf(), args.to_vec(), token.map(str::to_owned)));
                Ok(())
            },
        )
        .expect("spawn");
        let (prog, args, token) = captured.unwrap();
        assert_eq!(prog, PathBuf::from("/custom/ember-ide-webview"));
        assert_eq!(
            args,
            vec![
                "--relay-path",
                "/api/computers/node-a/ide/relay/v1",
                "--server-url",
                "http://127.0.0.1:8740",
                "--folder",
                "/work/repo",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("srv-secret")), "the token is never in argv");
        assert_eq!(token.as_deref(), Some("srv-secret"));

        // A relay path outside `/api/`, or one with no folder, offers no button.
        let bad = EditorTarget { vscode_relay: "http://evil/relay".into(), ..target.clone() };
        assert_eq!(VsCodeFallback::for_editor(&bad), None);
        let no_folder = EditorTarget { vscode_folder: String::new(), ..target.clone() };
        assert_eq!(VsCodeFallback::for_editor(&no_folder), None);

        // An old server's URL opens in URL mode; a non-http(s) one offers nothing.
        let old = EditorTarget { vscode_url: "http://node:8890/?folder=%2Fwork".into(), ..Default::default() };
        assert_eq!(VsCodeFallback::for_editor(&old), Some(VsCodeFallback::Url("http://node:8890/?folder=%2Fwork".into())));
        let file = EditorTarget { vscode_url: "file:///etc/passwd".into(), ..Default::default() };
        assert_eq!(VsCodeFallback::for_editor(&file), None);
        assert_eq!(VsCodeFallback::for_editor(&EditorTarget::default()), None);
    }
}
