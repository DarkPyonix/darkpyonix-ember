//! "Open IDE": what each target does once the server has said how to reach it
//! (`GET /api/sessions/{id}/ide`).
//!
//! - **VS Code**: always offered (`FR-W4c` (1b), INTENT D55). Picking it calls
//!   `POST /api/sessions/{id}/ide/vscode` ([`start_vscode`]), which starts the folder's OSE and
//!   answers its relay launch; a failure shows only as that call's error. The launch opens in an
//!   `ember-app --vscode-window` helper process ([`run_vscode_window`]): it runs the loopback
//!   gateway of `FR-W5b` with the app's own reach (HTTP with the server token, or the transport
//!   with the device key, so a `peer:` server works too) and spawns the standalone
//!   `ember-ide-webview --url <gateway> --vscode` window, which holds no server credential. The
//!   helper lives as long as that window. An older server's target with a `url` still opens that
//!   URL in the window directly. Neither this launcher process nor the helper creates a webview
//!   (NFR-L2); only `ember-ide-webview` does.
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
    /// Open a relay launch the target already carries in the `--vscode-window` helper (a server
    /// between D55 and the on-pick route sent one in the listing).
    OpenIdeRelay(IdeRelay),
    /// VS Code was picked: ask the server for its launch (`POST /api/sessions/{id}/ide/vscode`,
    /// [`start_vscode`]), then open it in the `--vscode-window` helper.
    StartVsCode,
    /// Hand this URL to the OS.
    OpenUrl(String),
    /// Open the Ember editor screen (its code area waits for the editor core, M8).
    EmberPlaceholder,
    /// Why it cannot be opened.
    Unavailable(String),
}

/// The VS Code target's relay launch: what the `ember-app --vscode-window` helper needs.
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
    if kind == IdeKind::VsCode {
        return plan_vscode(target);
    }
    let Some(t) = target else {
        return IdeAction::Unavailable(format!("the server offers no {} target", kind.label()));
    };
    if !t.available {
        return IdeAction::Unavailable(t.reason.clone().unwrap_or_else(|| format!("{} is not available", kind.label())));
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

/// VS Code is always offered (`FR-W4c` (1b), INTENT D55): with no relay launch and no `url` in
/// the listing (the current server sends `{kind: "vscode"}` only), picking it starts the launch on
/// the server, and `available`/`reason` are not read. A relay launch already in the listing, or an
/// old server's `url`, still opens as before.
fn plan_vscode(target: Option<&IdeTarget>) -> IdeAction {
    let Some(t) = target else {
        return IdeAction::StartVsCode;
    };
    if let Some(relay) = relay_launch(t) {
        return match checked_relay(relay) {
            Ok(relay) => IdeAction::OpenIdeRelay(relay),
            Err(why) => IdeAction::Unavailable(why),
        };
    }
    match t.url.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(url) if allowed_url(IdeKind::VsCode, url) => IdeAction::OpenIdeWindow(url.to_string()),
        Some(url) => IdeAction::Unavailable(format!("refusing to open {url:?}: unexpected scheme")),
        None => IdeAction::StartVsCode,
    }
}

/// A relay launch the helper may open: a relay path under `/api/` and a folder.
fn checked_relay(relay: IdeRelay) -> Result<IdeRelay, String> {
    if !relay.relay.starts_with("/api/") {
        return Err(format!("refusing relay path {:?}", relay.relay));
    }
    if relay.folder.trim().is_empty() {
        return Err("the server gave no folder for VS Code".into());
    }
    Ok(relay)
}

/// The relay launch of the on-pick route's answer, checked as [`plan`] checks one.
pub fn relay_of_launch(launch: &crate::server::VsCodeLaunch) -> Result<IdeRelay, String> {
    checked_relay(IdeRelay {
        relay: launch.ose.relay.clone(),
        folder: launch.folder.clone(),
        server_url: launch.server_url.clone().unwrap_or_default(),
    })
}

/// Picking VS Code (`FR-W4c` (1c)): `start(session, computer)` is the on-pick route, and `open`
/// opens its launch (the `--vscode-window` helper). The route's error is the only place a VS Code
/// failure shows, as its text.
pub async fn pick_vscode<S, F>(
    session: &str,
    computer: Option<&str>,
    start: S,
    open: impl FnOnce(IdeRelay) -> Result<(), String>,
) -> Result<(), String>
where
    S: FnOnce(String, Option<String>) -> F,
    F: std::future::Future<Output = Result<crate::server::VsCodeLaunch, String>>,
{
    let launch = start(session.to_owned(), computer.map(str::to_owned)).await?;
    open(relay_of_launch(&launch)?)
}

/// The text of a failed on-pick call: the server's `error` for an answered status, else the
/// error as it reads.
fn start_error_text(e: crate::server::ServerError) -> String {
    match e {
        crate::server::ServerError::Status { message, .. } => message,
        other => other.to_string(),
    }
}

/// VS Code was picked for `session`: call `POST /api/sessions/{session}/ide/vscode` on the app's
/// server, then spawn the `--vscode-window` helper on the launch. The demo has no server.
pub async fn start_vscode(session: String, computer: Option<String>) -> Result<(), String> {
    let s = crate::services::services();
    if s.demo.is_some() {
        return Err(crate::demo::UNAVAILABLE.to_owned());
    }
    pick_vscode(
        &session,
        computer.as_deref(),
        |sid, computer| async move {
            s.server.ide_vscode(&sid, computer.as_deref()).await.map_err(start_error_text)
        },
        |relay| open_ide_relay(&relay),
    )
    .await
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

/// Environment variables that carry a server credential, which the IDE window never inherits
/// (INTENT D55: the window holds no server credential).
pub const CREDENTIAL_ENVS: [&str; 2] = ["EMBER_SERVER_TOKEN", EDITOR_TOKEN_ENV];

/// CLI flag that starts this binary as the VS Code window's helper (INTENT D55, `FR-W5b`): the
/// loopback gateway and the `ember-ide-webview` window it opens. No webview in this process.
pub const VSCODE_WINDOW_ARG: &str = "--vscode-window";

/// The `--vscode-window` helper's arguments. No credential: the helper reads the server token
/// and the device key from the app's own sources, as the `--editor` process does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsCodeWindowArgs {
    /// The server's relay route, `/api/computers/{id}/ide/relay/{launch_id}`.
    pub relay_path: String,
    /// The server the launch came from (`http(s)://...`, or the app's `peer:<id>`).
    pub server_url: String,
    /// The absolute folder the workbench opens.
    pub folder: String,
}

/// A relay route as the server gives it: under `/api/`, an `/ide/relay/` path, with no scheme,
/// query, fragment or whitespace.
fn valid_relay_path(path: &str) -> bool {
    path.starts_with("/api/")
        && path.contains("/ide/relay/")
        && !path.contains("://")
        && !path.contains(['?', '#'])
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// An absolute folder (`/...`, or `C:\...` / `C:/...` on Windows) with no control characters.
fn valid_folder(folder: &str) -> bool {
    let b = folder.as_bytes();
    let windows_abs = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    !folder.chars().any(char::is_control) && (folder.starts_with('/') || windows_abs)
}

/// Parse `--vscode-window --relay-path <p> --server-url <u> --folder <abs>` from an argument
/// list (`std::env::args()`). `None` when the flag is absent; an error names a missing or bad
/// argument.
pub fn parse_vscode_window_args<I, S>(args: I) -> Option<Result<VsCodeWindowArgs, String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let at = args.iter().position(|a| a == VSCODE_WINDOW_ARG)?;
    let (mut relay_path, mut server_url, mut folder) = (None, None, None);
    let mut it = args[at + 1..].iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--relay-path" => &mut relay_path,
            "--server-url" => &mut server_url,
            "--folder" => &mut folder,
            other => return Some(Err(format!("{VSCODE_WINDOW_ARG}: unknown argument {other:?}"))),
        };
        let Some(value) = it.next() else {
            return Some(Err(format!("{VSCODE_WINDOW_ARG}: {flag} requires a value")));
        };
        *slot = Some(value.clone());
    }
    let need = |v: Option<String>, name: &str| v.ok_or_else(|| format!("{VSCODE_WINDOW_ARG}: {name} is required"));
    Some((|| {
        let relay_path = need(relay_path, "--relay-path")?;
        let server_url = need(server_url, "--server-url")?;
        let folder = need(folder, "--folder")?;
        if !valid_relay_path(&relay_path) {
            return Err(format!("{VSCODE_WINDOW_ARG}: not a server relay path: {relay_path:?}"));
        }
        if !valid_folder(&folder) {
            return Err(format!("{VSCODE_WINDOW_ARG}: the folder must be an absolute path: {folder:?}"));
        }
        if server_url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("{VSCODE_WINDOW_ARG}: bad server address: {server_url:?}"));
        }
        Ok(VsCodeWindowArgs { relay_path, server_url, folder })
    })())
}

/// The `(executable, args)` pair that starts the `--vscode-window` helper (this same binary) on
/// `relay`, against `server_url`.
pub fn vscode_window_command(exe: &Path, server_url: &str, relay: &IdeRelay) -> (PathBuf, Vec<String>) {
    (
        exe.to_path_buf(),
        vec![
            VSCODE_WINDOW_ARG.to_string(),
            "--relay-path".to_string(),
            relay.relay.clone(),
            "--server-url".to_string(),
            server_url.to_string(),
            "--folder".to_string(),
            relay.folder.clone(),
        ],
    )
}

/// The `(executable, args)` pair the helper spawns: the IDE window on the gateway's first-load URL
/// (`http://127.0.0.1:<port>/?folder=<enc>&egw=<token>`) with `--vscode`, which injects tab detach
/// and the `dragToOpenWindow` default (no proxy on the desktop, INTENT D55).
pub fn vscode_webview_command(bin: &Path, page_url: &str) -> (PathBuf, Vec<String>) {
    (bin.to_path_buf(), vec!["--url".to_string(), page_url.to_string(), "--vscode".to_string()])
}

/// Spawn the `--vscode-window` helper (`current_exe`) for `relay` on `server_url`. The window
/// binary is looked up first, so a missing `ember-ide-webview` is reported here, where the user
/// sees it, and never falls back to a browser. No credential goes to the helper: it reads them
/// itself.
pub fn open_ide_relay_with(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    server_url: &str,
    relay: &IdeRelay,
    spawn: impl FnOnce(&Path, &[String]) -> std::io::Result<()>,
) -> Result<(), String> {
    if resolve_ide_webview_bin(env_override, current_exe, path_var).is_none() {
        let msg = missing_ide_webview_error(env_override, current_exe, path_var);
        tracing::warn!(relay = %relay.relay, "{msg}");
        return Err(msg);
    }
    let Some(exe) = current_exe else {
        return Err("the path of this program is unknown, so the VS Code window cannot start".to_owned());
    };
    let (program, args) = vscode_window_command(exe, server_url, relay);
    spawn(&program, &args).map_err(|e| {
        let msg = format!("failed to start the VS Code window helper ({}): {e}", program.display());
        tracing::warn!(relay = %relay.relay, "{msg}");
        msg
    })
}

/// Open the VS Code target's relay launch in the `--vscode-window` helper. The server is the
/// launch's, else the app's own. Over HTTP the server token goes only where the terminal relay
/// may send it (`FR-S11`, `FR-L31.19`), checked here so the refusal shows; a server reached over
/// the transport (`peer:<id>`) is dialed with the device key by the helper.
pub fn open_ide_relay(relay: &IdeRelay) -> Result<(), String> {
    let services = crate::services::try_services();
    let server_url = if relay.server_url.trim().is_empty() {
        services.map(|s| s.config.server_url.clone()).unwrap_or_default()
    } else {
        relay.server_url.clone()
    };
    let over_peer = services.is_some_and(|s| s.client.api().peer().is_some());
    if !over_peer {
        crate::terminal_stream::relay_allowed(&server_url)?;
    }
    let env_override = std::env::var(IDE_WEBVIEW_BIN_ENV).ok();
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    open_ide_relay_with(
        env_override.as_deref(),
        current_exe.as_deref(),
        path_var.as_deref(),
        &server_url,
        relay,
        |program, args| {
            let mut cmd = Command::new(program);
            cmd.args(args);
            cmd.spawn().map(|_| ())
        },
    )
}

/// The `--vscode-window` helper (INTENT D55, `FR-W5b`): runs in this process after the app's
/// services are prepared (the same server token and device key as the `--editor` process).
/// Starts the gateway on the app's runtime with the app's reach for `args.server_url`, spawns
/// `ember-ide-webview --url <gateway> --vscode` and waits for it; when the window exits, the
/// gateway stops with this process. Creates no webview itself (NFR-L2).
pub fn run_vscode_window(args: &VsCodeWindowArgs) -> Result<(), String> {
    let s = crate::services::services();
    let reach = crate::editor_reach::reach_for(&args.server_url);
    let dial = crate::editor_reach::ReachDial::new(reach, args.relay_path.clone());
    let gateway = s
        .rt
        .block_on(ember_gateway::Gateway::start_with(dial))
        .map_err(|e| format!("failed to start the IDE gateway: {e}"))?;
    tracing::info!(origin = %gateway.origin(), relay = %args.relay_path, "IDE gateway started");
    let env_override = std::env::var(IDE_WEBVIEW_BIN_ENV).ok();
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    let Some(bin) = resolve_ide_webview_bin(env_override.as_deref(), current_exe.as_deref(), path_var.as_deref()) else {
        return Err(missing_ide_webview_error(env_override.as_deref(), current_exe.as_deref(), path_var.as_deref()));
    };
    let (program, argv) = vscode_webview_command(&bin, &gateway.page_url(&args.folder));
    let mut cmd = Command::new(&program);
    cmd.args(&argv);
    for name in CREDENTIAL_ENVS {
        cmd.env_remove(name);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to launch {} ({}): {e}", IDE_WEBVIEW_BIN_NAME, program.display()))?;
    drop(gateway);
    if status.success() {
        Ok(())
    } else {
        Err(format!("{IDE_WEBVIEW_BIN_NAME} exited with {status}"))
    }
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
        VsCodeEnv {
            url: var(EDITOR_VSCODE_ENV),
            relay: var(EDITOR_VSCODE_RELAY_ENV),
            folder: var(EDITOR_VSCODE_FOLDER_ENV),
            session: var(EDITOR_VSCODE_SESSION_ENV),
        },
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
/// `/api/computers/{id}/ide/relay/{launch}`, FR-W4c, INTENT D55) when the listing carried one,
/// which the editor window's fallback opens in the `--vscode-window` helper against the editor's
/// own server (`EMBER_EDITOR_SERVER`) (SPEC FR-E10).
pub const EDITOR_VSCODE_RELAY_ENV: &str = "EMBER_EDITOR_VSCODE_RELAY";
/// Environment variable carrying the folder the VS Code target's relay launch opens
/// (`launch.folder`), beside [`EDITOR_VSCODE_RELAY_ENV`].
pub const EDITOR_VSCODE_FOLDER_ENV: &str = "EMBER_EDITOR_VSCODE_FOLDER";
/// Environment variable carrying the session whose VS Code launch the editor window's fallback
/// asks for (`POST /api/sessions/{id}/ide/vscode`, FR-W4c (1c)) when the listing offered VS Code
/// with no launch, as the current server always does.
pub const EDITOR_VSCODE_SESSION_ENV: &str = "EMBER_EDITOR_VSCODE_SESSION";

/// The VS Code target the editor window's fallback opens, as it travels in the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VsCodeEnv {
    url: String,
    relay: String,
    folder: String,
    session: String,
}

/// What the Ember editor window's `VS Code에서 열기` opens when the server is a Code-OSS commit
/// the editor does not support (SPEC FR-E10, #32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsCodeFallback {
    /// The VS Code target's relay launch (FR-W4c, INTENT D55): the `--vscode-window` helper.
    Relay(IdeRelay),
    /// An old server's VS Code target URL: the IDE window's URL mode.
    Url(String),
    /// VS Code is offered with no launch (FR-W4c (1b)): the button asks the server for the
    /// session's launch ([`start_vscode`]) and opens it in the helper.
    Start { session: String },
}

impl VsCodeFallback {
    /// The VS Code target of an IDE launch for `session`: its relay launch, else an old server's
    /// `url`, else the on-pick launch. VS Code is always offered (FR-W4c (1b)), so `available` is
    /// not read; with no target at all there is nothing to offer.
    pub fn of_target(target: Option<&IdeTarget>, session: &str) -> Option<VsCodeFallback> {
        let t = target?;
        if let Some(relay) = relay_launch(t) {
            return Some(VsCodeFallback::Relay(relay));
        }
        if let Some(url) = t.url.clone().filter(|u| !u.trim().is_empty()) {
            return Some(VsCodeFallback::Url(url));
        }
        (!session.trim().is_empty()).then(|| VsCodeFallback::Start { session: session.to_owned() })
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
        if !url.is_empty() {
            return allowed_url(IdeKind::VsCode, url).then(|| VsCodeFallback::Url(url.to_owned()));
        }
        let session = target.vscode_session.trim();
        (!session.is_empty()).then(|| VsCodeFallback::Start { session: session.to_owned() })
    }

    /// Open the IDE window for this fallback: the `--vscode-window` helper on a relay launch
    /// ([`open_ide_relay`]), URL mode ([`open_ide_window`]), or the on-pick route first
    /// ([`start_vscode`], on the app's runtime; its error is posted as the notice, the only place
    /// a VS Code failure shows).
    pub fn open(&self) -> Result<(), String> {
        match self {
            VsCodeFallback::Relay(relay) => open_ide_relay(relay),
            VsCodeFallback::Url(url) => open_ide_window(url),
            VsCodeFallback::Start { session } => {
                let session = session.clone();
                let s = crate::services::try_services().ok_or_else(|| "the app is not ready".to_owned())?;
                s.rt.spawn(async move {
                    if let Err(e) = start_vscode(session, None).await {
                        tracing::warn!("could not open the VS Code target: {e}");
                        crate::ui::cr::notice::post(format!("VS Code: 열지 못했습니다: {e}"));
                    }
                });
                Ok(())
            }
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
                vscode_session: vscode.session,
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
        .env(EDITOR_VSCODE_SESSION_ENV, &target.vscode_session)
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
        // VS Code is always offered (FR-W4c (1b)): with no launch and no url, picking it starts
        // the launch on the server, whatever `available` says.
        assert_eq!(plan(IdeKind::VsCode, Some(&target("vscode", false, None))), IdeAction::StartVsCode);
        assert_eq!(plan(IdeKind::VsCode, None), IdeAction::StartVsCode);
        assert_eq!(
            plan(IdeKind::Gateway, Some(&target("gateway", false, None))),
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

    /// SPEC FR-L7, FR-W4c (1b), INTENT D55 (#1064): the current server lists VS Code as
    /// `{kind: "vscode"}` alone. It parses as available, picking it starts the launch, and its
    /// chooser row is an ordinary row: no badge, no unavailable line.
    #[test]
    fn fr_l7_vscode_is_always_offered() {
        let t: IdeTarget = serde_json::from_value(serde_json::json!({ "kind": "vscode" })).unwrap();
        assert!(t.available, "a vscode target without `available` is available");
        assert_eq!((t.url.as_deref(), t.launch.as_ref(), t.reason.as_deref()), (None, None, None));
        assert_eq!(plan(IdeKind::VsCode, Some(&t)), IdeAction::StartVsCode);

        let launch: crate::server::IdeLaunch = serde_json::from_value(serde_json::json!({
            "session": "s", "project": "p", "folder": "/work/repo",
            "computer": { "name": "local", "local": true },
            "targets": [
                { "kind": "ember", "available": false, "reason": "no OSE" },
                { "kind": "vscode" },
                { "kind": "gateway", "available": false, "reason": "no ssh_host" }
            ]
        }))
        .unwrap();
        assert!(launch.target("vscode").unwrap().available);
        let choices = crate::ui::session::ide_choices(None);
        let rows = crate::ui::cr::session::ide_pop_rows(&choices, None);
        let rows = crate::ui::cr::session::with_ember_notice(rows, &choices, Some(&Ok(launch)));
        let (row, _) = rows
            .iter()
            .zip(&choices)
            .find(|(_, c)| c.kind == IdeKind::VsCode)
            .expect("VS Code is in the chooser");
        assert!(row.badge.is_none(), "no badge on VS Code: {:?}", row.badge);
        assert_eq!(row.description.as_deref(), Some(crate::ui::session::ide_description(IdeKind::VsCode)));
        assert!(!row.danger && !row.spinner);
    }

    fn vscode_launch() -> crate::server::VsCodeLaunch {
        serde_json::from_value(serde_json::json!({
            "session": "s1", "project": "p", "computer": "node-a", "computer_name": "studio",
            "folder": "/work/repo", "server_url": "http://127.0.0.1:8740",
            "ose": { "relay": "/api/computers/node-a/ide/relay/v1", "ose_commit": "abc" }
        }))
        .unwrap()
    }

    /// SPEC FR-L7, FR-W4c (1c), INTENT D55: picking VS Code calls the on-pick route for the
    /// session, then spawns `ember-app --vscode-window` on its launch, with no credential in argv;
    /// the route's error is what the user sees, and nothing is spawned then.
    #[tokio::test]
    async fn fr_l7_vscode_pick_calls_the_on_pick_route_then_spawns_the_helper() {
        let mut asked: Option<(String, Option<String>)> = None;
        let mut spawned: Option<(PathBuf, Vec<String>)> = None;
        pick_vscode(
            "s1",
            None,
            |sid, computer| {
                asked = Some((sid, computer));
                async { Ok(vscode_launch()) }
            },
            |relay| {
                assert_eq!(
                    relay,
                    IdeRelay {
                        relay: "/api/computers/node-a/ide/relay/v1".into(),
                        folder: "/work/repo".into(),
                        server_url: "http://127.0.0.1:8740".into(),
                    }
                );
                open_ide_relay_with(
                    Some("/custom/ember-ide-webview"),
                    Some(Path::new("/opt/ember/ember-app")),
                    None,
                    &relay.server_url,
                    &relay,
                    |prog, args| {
                        spawned = Some((prog.to_path_buf(), args.to_vec()));
                        Ok(())
                    },
                )
            },
        )
        .await
        .expect("picked");
        assert_eq!(asked, Some(("s1".to_owned(), None)));
        let (prog, args) = spawned.expect("the helper was spawned");
        assert_eq!(prog, PathBuf::from("/opt/ember/ember-app"), "the helper is this same program");
        assert_eq!(
            args,
            vec![
                "--vscode-window",
                "--relay-path",
                "/api/computers/node-a/ide/relay/v1",
                "--server-url",
                "http://127.0.0.1:8740",
                "--folder",
                "/work/repo",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("ember-ide-webview")), "the shell does not start the window itself");

        // The route's error is shown as it is, and nothing opens.
        let mut opened = false;
        let err = pick_vscode(
            "s1",
            Some("node-a"),
            |_, computer| async move {
                assert_eq!(computer.as_deref(), Some("node-a"));
                Err("node unreachable: connection refused".to_owned())
            },
            |_| {
                opened = true;
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err, "node unreachable: connection refused");
        assert!(!opened);
        assert_eq!(
            start_error_text(crate::server::ServerError::Status { status: 502, message: "no OSE binary".into() }),
            "no OSE binary"
        );

        // A launch whose relay is outside `/api/` or has no folder is not opened.
        let mut bad = vscode_launch();
        bad.ose.relay = "http://evil/relay".into();
        assert!(relay_of_launch(&bad).is_err());
        let mut no_folder = vscode_launch();
        no_folder.folder = String::new();
        assert!(relay_of_launch(&no_folder).is_err());

        // A missing window binary is reported by the shell, before any helper starts.
        let mut called = false;
        let missing = open_ide_relay_with(
            None,
            Some(Path::new("/nonexistent/ember-dir/ember-app")),
            None,
            "http://127.0.0.1:8740",
            &relay_of_launch(&vscode_launch()).unwrap(),
            |_, _| {
                called = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(!called && missing.contains(IDE_WEBVIEW_BIN_NAME), "{missing}");
    }

    /// SPEC FR-L7, INTENT D55: `ember-app --vscode-window` takes the relay path, the server and an
    /// absolute folder, and no credential; the command the shell builds parses back.
    #[test]
    fn fr_l7_helper_args_parse() {
        assert_eq!(parse_vscode_window_args(["ember-app"]), None);
        assert_eq!(parse_vscode_window_args(["ember-app", "--editor", "p", "/w"]), None);
        let relay = relay_of_launch(&vscode_launch()).unwrap();
        let (_, args) = vscode_window_command(Path::new("ember-app"), "peer:abcd", &relay);
        let parsed = parse_vscode_window_args(std::iter::once("ember-app".to_owned()).chain(args)).unwrap().unwrap();
        assert_eq!(
            parsed,
            VsCodeWindowArgs {
                relay_path: "/api/computers/node-a/ide/relay/v1".into(),
                server_url: "peer:abcd".into(),
                folder: "/work/repo".into(),
            }
        );
        let good = ["--relay-path", "/api/computers/c/ide/relay/l", "--server-url", "http://127.0.0.1:8740", "--folder", "/w"];
        let parse = |args: &[&str]| parse_vscode_window_args(["ember-app", "--vscode-window"].iter().chain(args.iter()).copied());
        assert!(parse(&good).unwrap().is_ok());
        assert!(parse(&good[..4]).unwrap().unwrap_err().contains("--folder"));
        assert!(parse(&good[2..]).unwrap().unwrap_err().contains("--relay-path"));
        assert!(parse(&good[..5]).unwrap().unwrap_err().contains("requires a value"));
        let mut token = good.to_vec();
        token.extend(["--token", "secret"]);
        assert!(parse(&token).unwrap().unwrap_err().contains("unknown argument"), "no credential in argv");
        for (i, bad) in [
            (1, "/other/path"),
            (1, "/api/computers/c/ide/relay/l?x=1"),
            (1, "http://evil/api/computers/c/ide/relay/l"),
            (3, "http://a b"),
            (5, "relative/dir"),
        ] {
            let mut args = good.to_vec();
            args[i] = bad;
            assert!(parse(&args).unwrap().is_err(), "{bad:?} must be refused");
        }
        let mut win = good.to_vec();
        win[5] = "C:\\work";
        assert!(parse(&win).unwrap().is_ok());
    }

    /// SPEC FR-L7, FR-W5b, INTENT D55: the helper opens `ember-ide-webview` on the gateway's
    /// first-load URL with `--vscode`, and strips the server credentials from the window's
    /// environment.
    #[test]
    fn fr_l7_helper_spawns_the_window_on_the_gateway_url_with_the_vscode_flag() {
        let addr: std::net::SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let page = ember_gateway::page_url(addr, "0123abcd", "/work/ember app");
        assert_eq!(page, "http://127.0.0.1:41234/?folder=%2Fwork%2Fember%20app&egw=0123abcd");
        let (prog, args) = vscode_webview_command(Path::new("/opt/ember/ember-ide-webview"), &page);
        assert_eq!(prog, PathBuf::from("/opt/ember/ember-ide-webview"));
        assert_eq!(args, vec!["--url", page.as_str(), "--vscode"]);
        assert!(!args.iter().any(|a| a.contains("/ide/relay/") || a.contains("--relay-path")));
        assert!(CREDENTIAL_ENVS.contains(&"EMBER_SERVER_TOKEN") && CREDENTIAL_ENVS.contains(&EDITOR_TOKEN_ENV));
        // The helper itself never builds a webview: this crate links no browser engine.
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("ember-gateway") && !manifest.contains("wry") && !manifest.contains("tao ="));
    }

    /// SPEC FR-L7 (VS Code target): a relay launch already in the listing (a server between D55
    /// and the on-pick route) opens in the helper too; an old server's `url` keeps the URL mode.
    #[test]
    fn fr_l7_vscode_target_with_relay_spawns_the_helper() {
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

        let mut captured: Option<(PathBuf, Vec<String>)> = None;
        open_ide_relay_with(
            Some("/custom/ember-ide-webview"),
            Some(Path::new("/opt/ember/ember-app")),
            None,
            "http://127.0.0.1:8740",
            &relay,
            |prog, args| {
                captured = Some((prog.to_path_buf(), args.to_vec()));
                Ok(())
            },
        )
        .expect("spawn");
        let (prog, args) = captured.unwrap();
        assert_eq!(prog, PathBuf::from("/opt/ember/ember-app"));
        assert_eq!(args[0], VSCODE_WINDOW_ARG);
        assert!(args.windows(2).any(|w| w == ["--relay-path", "/api/computers/node-a/ide/relay/l1"]));
        assert!(!args.iter().any(|a| a == "--url"));

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
                session: String::new(),
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
    /// is a relay launch spawns the `--vscode-window` helper on the editor's own server, with no
    /// credential in argv; a target offered with no launch asks for the session's launch; a bad
    /// relay path offers nothing, and an old server's URL still opens in URL mode.
    #[test]
    fn fr_e10_fallback_opens_vscode_through_the_relay() {
        let target = EditorTarget {
            project: "p".into(),
            folder: "/work/repo".into(),
            token: "editor-secret".into(),
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

        let mut captured: Option<(PathBuf, Vec<String>)> = None;
        open_ide_relay_with(
            Some("/custom/ember-ide-webview"),
            Some(Path::new("/opt/ember/ember-app")),
            None,
            &relay.server_url,
            &relay,
            |prog, args| {
                captured = Some((prog.to_path_buf(), args.to_vec()));
                Ok(())
            },
        )
        .expect("spawn");
        let (prog, args) = captured.unwrap();
        assert_eq!(prog, PathBuf::from("/opt/ember/ember-app"));
        assert_eq!(
            args,
            vec![
                "--vscode-window",
                "--relay-path",
                "/api/computers/node-a/ide/relay/v1",
                "--server-url",
                "http://127.0.0.1:8740",
                "--folder",
                "/work/repo",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("secret")), "no credential is ever in argv");

        // The current server offers VS Code with no launch: the button asks for the session's.
        let start = EditorTarget { vscode_session: "s1".into(), ..Default::default() };
        assert_eq!(VsCodeFallback::for_editor(&start), Some(VsCodeFallback::Start { session: "s1".into() }));
        let (_, editor_args) = editor_command(Path::new("ember"), "p", "/w", "");
        let parsed = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(editor_args),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            VsCodeEnv { session: "s1".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(parsed.vscode_session, "s1");
        assert_eq!(EDITOR_VSCODE_SESSION_ENV, "EMBER_EDITOR_VSCODE_SESSION");

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
