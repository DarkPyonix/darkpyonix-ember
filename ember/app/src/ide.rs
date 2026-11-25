//! "Open IDE": what each target does once the server has said how to reach it
//! (`GET /api/sessions/{id}/ide`).
//!
//! - **VS Code**: always offered (`FR-W4c` (1b), INTENT D55). Picking it calls
//!   `POST /api/sessions/{id}/ide/vscode` ([`start_vscode`]), which starts the folder's OSE and
//!   answers its relay launch; a failure shows only as that call's error. The launch opens in an
//!   `ember-app --vscode-window` helper process ([`run_vscode_window`]): it runs the loopback
//!   gateway of `FR-W5b` with the app's own reach (HTTP with the server token, or the transport
//!   with the device key, so a `peer:` server works too) and spawns the standalone
//!   `ember-ide-webview --vscode` window (its first page in `EMBER_IDE_WEBVIEW_URL`, not argv),
//!   which holds no server credential. The
//!   relay reaches the DarkPyonix manager, which serves the editor server under the launch's
//!   `path` and authorises it by the launch's ticket: the window's first page is
//!   `<gateway>/ide/ose/?folder=<enc>&dpx-ticket=<ticket>&egw=<gateway token>`. The helper lives
//!   as long as that window. An older server's target with a `url` still opens that
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

/// The computer's effective `default_ide` (FR-W7, FR-W7b, INTENT D56): the server's editor
/// settings, unless the node of `computer` overrides it. A project with no pick of its own launches
/// this one; `None` opens the chooser.
///
/// TODO(FR-W7, D56): the editor settings record (`default_ide` on the server, inherited and
/// overridden per node) is not built yet, so there is no default to read and this is `None`.
pub fn effective_default_ide(project: &str, computer: Option<&str>) -> Option<IdeKind> {
    let _ = (project, computer);
    None
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
#[derive(Clone, PartialEq, Eq)]
pub struct IdeRelay {
    /// The server's relay route to the computer's OSE, `/api/computers/{id}/ide/relay/{launch}`.
    pub relay: String,
    /// The editor server's base path at the DarkPyonix manager (`/ide/ose/`, `FR-W4c` (1c)).
    pub path: String,
    /// The manager's ticket for this launch (`?dpx-ticket=` on the window's first page). A
    /// credential: never in argv, never in a log.
    pub ticket: String,
    /// The folder the workbench opens.
    pub folder: String,
    /// The server's base URL from the launch; empty means the app's own server.
    pub server_url: String,
}

impl std::fmt::Debug for IdeRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdeRelay")
            .field("relay", &self.relay)
            .field("path", &self.path)
            .field("ticket", &"[REDACTED]")
            .field("folder", &self.folder)
            .field("server_url", &self.server_url)
            .finish()
    }
}

/// The relay launch of a VS Code target, when the server sent one.
pub(crate) fn relay_launch(t: &IdeTarget) -> Option<IdeRelay> {
    let launch = t.launch.as_ref()?;
    let ose = launch.ose.as_ref()?;
    let relay = ose.relay.clone().filter(|r| !r.trim().is_empty())?;
    Some(IdeRelay {
        relay,
        path: ose.path.clone(),
        ticket: ose.ticket.clone(),
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

/// A relay launch the helper may open: a relay path under `/api/`, the manager's path, a ticket
/// and a folder.
fn checked_relay(relay: IdeRelay) -> Result<IdeRelay, String> {
    if !relay.relay.starts_with("/api/") {
        return Err(format!("refusing relay path {:?}", relay.relay));
    }
    if !valid_ose_path(&relay.path) {
        return Err(format!("refusing editor server path {:?}", relay.path));
    }
    if !valid_ticket(&relay.ticket) {
        return Err("the server gave no usable editor ticket for VS Code".into());
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
        path: launch.ose.path.clone(),
        ticket: launch.ose.ticket.clone(),
        folder: launch.folder.clone(),
        server_url: launch.server_url.clone().unwrap_or_default(),
    })
}

/// The editor server's base path at the manager as the server gives it (`/ide/ose/`): absolute,
/// with no scheme, query, fragment, `..`, whitespace or control character.
pub fn valid_ose_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains("://")
        && !path.contains(['?', '#', '%'])
        && !path.split('/').any(|seg| seg == "..")
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The manager's ticket: one cookie value (`ember_editor_conn::handshake::Ticket`).
pub fn valid_ticket(ticket: &str) -> bool {
    ember_editor_conn::handshake::Ticket::is_cookie_value(ticket)
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

/// Environment variables that carry a server credential or an editor ticket, which the IDE
/// window never inherits (INTENT D55: the window holds no server credential; it gets the ticket
/// only in its first page's URL).
pub const CREDENTIAL_ENVS: [&str; 4] =
    ["EMBER_SERVER_TOKEN", EDITOR_TICKET_ENV, EDITOR_VSCODE_TICKET_ENV, VSCODE_WINDOW_TICKET_ENV];

/// Environment variable carrying the manager's ticket to the `--vscode-window` helper (never argv).
pub const VSCODE_WINDOW_TICKET_ENV: &str = "EMBER_VSCODE_WINDOW_TICKET";

/// The query that carries the manager's ticket on the window's first page; the manager answers
/// it with a 302 to the same URL without it and `Set-Cookie: dpx-editor=...` (INTENT D55).
pub const TICKET_QUERY: &str = "dpx-ticket";

/// CLI flag that starts this binary as the VS Code window's helper (INTENT D55, `FR-W5b`): the
/// loopback gateway and the `ember-ide-webview` window it opens. No webview in this process.
pub const VSCODE_WINDOW_ARG: &str = "--vscode-window";

/// The `--vscode-window` helper's arguments. No credential in argv: the helper reads the server
/// token and the device key from the app's own sources, as the `--editor` process does, and the
/// manager's ticket from [`VSCODE_WINDOW_TICKET_ENV`].
#[derive(Clone, PartialEq, Eq)]
pub struct VsCodeWindowArgs {
    /// The server's relay route, `/api/computers/{id}/ide/relay/{launch_id}`.
    pub relay_path: String,
    /// The server the launch came from (`http(s)://...`, or the app's `peer:<id>`).
    pub server_url: String,
    /// The absolute folder the workbench opens.
    pub folder: String,
    /// The editor server's base path at the manager (`/ide/ose/`).
    pub ose_path: String,
    /// The manager's ticket, from the environment.
    pub ticket: String,
}

impl std::fmt::Debug for VsCodeWindowArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VsCodeWindowArgs")
            .field("relay_path", &self.relay_path)
            .field("server_url", &self.server_url)
            .field("folder", &self.folder)
            .field("ose_path", &self.ose_path)
            .field("ticket", &"[REDACTED]")
            .finish()
    }
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

/// Parse `--vscode-window --relay-path <p> --server-url <u> --folder <abs> --ose-path <path>`
/// from an argument list (`std::env::args()`), with the ticket from [`VSCODE_WINDOW_TICKET_ENV`].
/// `None` when the flag is absent; an error names a missing or bad argument.
pub fn parse_vscode_window_args<I, S>(args: I) -> Option<Result<VsCodeWindowArgs, String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parse_vscode_window_args_with_ticket(args, std::env::var(VSCODE_WINDOW_TICKET_ENV).unwrap_or_default())
}

fn parse_vscode_window_args_with_ticket<I, S>(args: I, ticket: String) -> Option<Result<VsCodeWindowArgs, String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let at = args.iter().position(|a| a == VSCODE_WINDOW_ARG)?;
    let (mut relay_path, mut server_url, mut folder, mut ose_path) = (None, None, None, None);
    let mut it = args[at + 1..].iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--relay-path" => &mut relay_path,
            "--server-url" => &mut server_url,
            "--folder" => &mut folder,
            "--ose-path" => &mut ose_path,
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
        let ose_path = need(ose_path, "--ose-path")?;
        if !valid_relay_path(&relay_path) {
            return Err(format!("{VSCODE_WINDOW_ARG}: not a server relay path: {relay_path:?}"));
        }
        if !valid_folder(&folder) {
            return Err(format!("{VSCODE_WINDOW_ARG}: the folder must be an absolute path: {folder:?}"));
        }
        if server_url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("{VSCODE_WINDOW_ARG}: bad server address: {server_url:?}"));
        }
        if !valid_ose_path(&ose_path) {
            return Err(format!("{VSCODE_WINDOW_ARG}: not an editor server path: {ose_path:?}"));
        }
        if !valid_ticket(&ticket) {
            return Err(format!("{VSCODE_WINDOW_ARG}: {VSCODE_WINDOW_TICKET_ENV} holds no usable editor ticket"));
        }
        Ok(VsCodeWindowArgs { relay_path, server_url, folder, ose_path, ticket })
    })())
}

/// The `(executable, args)` pair that starts the `--vscode-window` helper (this same binary) on
/// `relay`, against `server_url`. The ticket is not here: it goes in the child's environment
/// ([`VSCODE_WINDOW_TICKET_ENV`], [`vscode_window_env`]).
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
            "--ose-path".to_string(),
            relay.path.clone(),
        ],
    )
}

/// The environment the `--vscode-window` helper is started with: the manager's ticket.
pub fn vscode_window_env(relay: &IdeRelay) -> Vec<(&'static str, String)> {
    vec![(VSCODE_WINDOW_TICKET_ENV, relay.ticket.clone())]
}

/// The window's first page on the gateway at `addr`:
/// `http://127.0.0.1:<port><path>?folder=<enc>&dpx-ticket=<ticket>&egw=<gateway token>`. The
/// gateway answers `egw` with a 302 that keeps the other query parameters, and the manager answers
/// `dpx-ticket` with a 302 and its own cookie (INTENT D55), so both leave the address bar.
pub fn vscode_page_url(addr: std::net::SocketAddr, gateway_token: &str, args: &VsCodeWindowArgs) -> String {
    let path = format!("{}/", args.ose_path.trim_end_matches('/'));
    ember_gateway::page_url_at(
        addr,
        gateway_token,
        &path,
        &[("folder", args.folder.as_str()), (TICKET_QUERY, args.ticket.as_str())],
    )
}

/// The variable `ember-ide-webview` reads its page URL from instead of `--url` (its
/// `cli::URL_ENV`; this crate does not link the window crate, so the name is repeated here).
pub const IDE_WEBVIEW_URL_ENV: &str = "EMBER_IDE_WEBVIEW_URL";

/// What the helper spawns: the IDE window on its first page ([`vscode_page_url`]) with
/// `--vscode`, which injects tab detach and the `dragToOpenWindow` default (no proxy on the
/// desktop, INTENT D55). The page URL carries the manager's ticket and the gateway's token, so it
/// goes in [`IDE_WEBVIEW_URL_ENV`], never argv (any process on the machine can read argv,
/// #1064); the window reads it once and removes it from its environment.
pub fn vscode_webview_command(bin: &Path, page_url: &str) -> (PathBuf, Vec<String>, Vec<(&'static str, String)>) {
    (
        bin.to_path_buf(),
        vec!["--vscode".to_string()],
        vec![(IDE_WEBVIEW_URL_ENV, page_url.to_string())],
    )
}

/// Spawn the `--vscode-window` helper (`current_exe`) for `relay` on `server_url`. The window
/// binary is looked up first, so a missing `ember-ide-webview` is reported here, where the user
/// sees it, and never falls back to a browser. No server credential goes to the helper (it reads
/// them itself), and the manager's ticket goes in its environment, never argv.
pub fn open_ide_relay_with(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    server_url: &str,
    relay: &IdeRelay,
    spawn: impl FnOnce(&Path, &[String], &[(&'static str, String)]) -> std::io::Result<()>,
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
    spawn(&program, &args, &vscode_window_env(relay)).map_err(|e| {
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
        |program, args, envs| {
            let mut cmd = Command::new(program);
            cmd.args(args);
            for (name, value) in envs {
                cmd.env(name, value);
            }
            cmd.spawn().map(|_| ())
        },
    )
}

/// The `--vscode-window` helper (INTENT D55, `FR-W5b`): runs in this process after the app's
/// services are prepared (the same server token and device key as the `--editor` process).
/// Starts the gateway on the app's runtime with the app's reach for `args.server_url`, spawns
/// `ember-ide-webview --vscode` with the gateway page in `EMBER_IDE_WEBVIEW_URL` and waits for it; when the window exits, the
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
    let (program, argv, envs) = vscode_webview_command(&bin, &vscode_page_url(gateway.addr(), gateway.token(), args));
    let mut cmd = Command::new(&program);
    cmd.args(&argv);
    for name in CREDENTIAL_ENVS {
        cmd.env_remove(name);
    }
    for (name, value) in envs {
        cmd.env(name, value);
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
        var(EDITOR_TICKET_ENV),
        var(EDITOR_SERVER_ENV),
        var(EDITOR_COMPUTER_ENV),
        var(EDITOR_RELAY_ENV),
        var(EDITOR_OSE_PATH_ENV),
        VsCodeEnv {
            url: var(EDITOR_VSCODE_ENV),
            relay: var(EDITOR_VSCODE_RELAY_ENV),
            path: var(EDITOR_VSCODE_PATH_ENV),
            ticket: var(EDITOR_VSCODE_TICKET_ENV),
            folder: var(EDITOR_VSCODE_FOLDER_ENV),
            session: var(EDITOR_VSCODE_SESSION_ENV),
        },
    )
}

/// Environment variable carrying the DarkPyonix manager's editor ticket of the Ember target's
/// launch (`ose.ticket`, `FR-W4c` (1), INTENT D55), never argv. The editor sends it as
/// `Cookie: dpx-editor=<ticket>`; there is no connection token any more.
pub const EDITOR_TICKET_ENV: &str = "EMBER_EDITOR_TICKET";
/// Environment variable carrying the editor server's base path at the manager (`ose.path`,
/// `/ide/ose/`). Empty means the root (the demo's direct OSE).
pub const EDITOR_OSE_PATH_ENV: &str = "EMBER_EDITOR_OSE_PATH";
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
/// Environment variable carrying the manager's path of the VS Code target's relay launch
/// (`launch.ose.path`), beside [`EDITOR_VSCODE_RELAY_ENV`].
pub const EDITOR_VSCODE_PATH_ENV: &str = "EMBER_EDITOR_VSCODE_PATH";
/// Environment variable carrying the manager's ticket of the VS Code target's relay launch
/// (`launch.ose.ticket`), beside [`EDITOR_VSCODE_RELAY_ENV`]. Never argv.
pub const EDITOR_VSCODE_TICKET_ENV: &str = "EMBER_EDITOR_VSCODE_TICKET";
/// Environment variable carrying the folder the VS Code target's relay launch opens
/// (`launch.folder`), beside [`EDITOR_VSCODE_RELAY_ENV`].
pub const EDITOR_VSCODE_FOLDER_ENV: &str = "EMBER_EDITOR_VSCODE_FOLDER";
/// Environment variable carrying the session whose VS Code launch the editor window's fallback
/// asks for (`POST /api/sessions/{id}/ide/vscode`, FR-W4c (1c)) when the listing offered VS Code
/// with no launch, as the current server always does.
pub const EDITOR_VSCODE_SESSION_ENV: &str = "EMBER_EDITOR_VSCODE_SESSION";

/// The VS Code target the editor window's fallback opens, as it travels in the environment.
#[derive(Clone, Default, PartialEq, Eq)]
struct VsCodeEnv {
    url: String,
    relay: String,
    path: String,
    ticket: String,
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
    /// a relay path under `/api/` with the manager's path, a ticket and a folder, dialed on the
    /// editor's own server; else an http(s) URL. `None` when the target offers neither, and the window then shows no button.
    pub fn for_editor(target: &EditorTarget) -> Option<VsCodeFallback> {
        let relay = target.vscode_relay.trim();
        if !relay.is_empty() {
            return checked_relay(IdeRelay {
                relay: relay.to_owned(),
                path: target.vscode_path.clone(),
                ticket: target.vscode_ticket.clone(),
                folder: target.vscode_folder.clone(),
                server_url: target.server_url.clone(),
            })
            .ok()
            .map(VsCodeFallback::Relay);
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
    ticket: String,
    server_url: String,
    computer: String,
    relay: String,
    ose_path: String,
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
                ose_path,
                ticket,
                server_url,
                computer,
                relay,
                vscode_url: vscode.url,
                vscode_relay: vscode.relay,
                vscode_path: vscode.path,
                vscode_ticket: vscode.ticket,
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

/// Spawn an editor with the manager's ticket (and every other launch detail but the project,
/// folder and demo endpoint) in the child environment, never in argv.
pub fn spawn_editor_target(exe: &Path, target: &EditorTarget) -> std::io::Result<std::process::Child> {
    let (bin, args) = editor_command(exe, &target.project, &target.folder, &target.endpoint);
    // The owner handoff (FR-S12, #1042): a one-time token, recorded with the child's PID.
    let handoff = crate::owner_handoff::new_token();
    let child = Command::new(bin)
        .args(args)
        .env(crate::owner_handoff::HANDOFF_ENV, &handoff)
        .env(EDITOR_TICKET_ENV, &target.ticket)
        .env(EDITOR_SERVER_ENV, &target.server_url)
        .env(EDITOR_COMPUTER_ENV, &target.computer)
        .env(EDITOR_RELAY_ENV, &target.relay)
        .env(EDITOR_OSE_PATH_ENV, &target.ose_path)
        .env(EDITOR_VSCODE_ENV, &target.vscode_url)
        .env(EDITOR_VSCODE_RELAY_ENV, &target.vscode_relay)
        .env(EDITOR_VSCODE_PATH_ENV, &target.vscode_path)
        .env(EDITOR_VSCODE_TICKET_ENV, &target.vscode_ticket)
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
            "ose": { "relay": "/api/computers/node-a/ide/relay/v1", "path": "/ide/ose/", "ticket": "tk-v1", "ose_commit": "abc" }
        }))
        .unwrap()
    }

    fn v1_relay() -> IdeRelay {
        IdeRelay {
            relay: "/api/computers/node-a/ide/relay/v1".into(),
            path: "/ide/ose/".into(),
            ticket: "tk-v1".into(),
            folder: "/work/repo".into(),
            server_url: "http://127.0.0.1:8740".into(),
        }
    }

    type Spawned = Option<(PathBuf, Vec<String>, Vec<(&'static str, String)>)>;

    /// SPEC FR-L7, FR-W4c (1c), INTENT D55: picking VS Code calls the on-pick route for the
    /// session, then spawns `ember-app --vscode-window` on its launch, with no credential in argv;
    /// the route's error is what the user sees, and nothing is spawned then.
    #[tokio::test]
    async fn fr_l7_vscode_pick_calls_the_on_pick_route_then_spawns_the_helper() {
        let mut asked: Option<(String, Option<String>)> = None;
        let mut spawned: Spawned = None;
        pick_vscode(
            "s1",
            None,
            |sid, computer| {
                asked = Some((sid, computer));
                async { Ok(vscode_launch()) }
            },
            |relay| {
                assert_eq!(relay, v1_relay());
                open_ide_relay_with(
                    Some("/custom/ember-ide-webview"),
                    Some(Path::new("/opt/ember/ember-app")),
                    None,
                    &relay.server_url,
                    &relay,
                    |prog, args, envs| {
                        spawned = Some((prog.to_path_buf(), args.to_vec(), envs.to_vec()));
                        Ok(())
                    },
                )
            },
        )
        .await
        .expect("picked");
        assert_eq!(asked, Some(("s1".to_owned(), None)));
        let (prog, args, envs) = spawned.expect("the helper was spawned");
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
                "--ose-path",
                "/ide/ose/",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("ember-ide-webview")), "the shell does not start the window itself");
        assert!(!args.iter().any(|a| a.contains("tk-v1")), "the ticket is never in argv");
        assert_eq!(envs, vec![(VSCODE_WINDOW_TICKET_ENV, "tk-v1".to_owned())], "the ticket goes in the environment");

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
        let mut no_ticket = vscode_launch();
        no_ticket.ose.ticket = String::new();
        assert!(relay_of_launch(&no_ticket).is_err());
        let mut bad_ticket = vscode_launch();
        bad_ticket.ose.ticket = "a;b".into();
        assert!(relay_of_launch(&bad_ticket).is_err());
        let mut bad_path = vscode_launch();
        bad_path.ose.path = "http://evil/".into();
        assert!(relay_of_launch(&bad_path).is_err());
        assert!(!format!("{:?}", v1_relay()).contains("tk-v1"), "the ticket never reaches a log");

        // A missing window binary is reported by the shell, before any helper starts.
        let mut called = false;
        let missing = open_ide_relay_with(
            None,
            Some(Path::new("/nonexistent/ember-dir/ember-app")),
            None,
            "http://127.0.0.1:8740",
            &relay_of_launch(&vscode_launch()).unwrap(),
            |_, _, _| {
                called = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(!called && missing.contains(IDE_WEBVIEW_BIN_NAME), "{missing}");
    }

    /// SPEC FR-L7, INTENT D55: `ember-app --vscode-window` takes the relay path, the server, an
    /// absolute folder and the manager's path in argv, and the ticket from the environment only;
    /// the command and environment the shell builds parse back.
    #[test]
    fn fr_l7_helper_args_parse() {
        assert_eq!(parse_vscode_window_args(["ember-app"]), None);
        assert_eq!(parse_vscode_window_args(["ember-app", "--editor", "p", "/w"]), None);
        let relay = relay_of_launch(&vscode_launch()).unwrap();
        let (_, args) = vscode_window_command(Path::new("ember-app"), "peer:abcd", &relay);
        let env = vscode_window_env(&relay);
        assert_eq!(env[0].0, VSCODE_WINDOW_TICKET_ENV);
        let parsed = parse_vscode_window_args_with_ticket(std::iter::once("ember-app".to_owned()).chain(args), env[0].1.clone())
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed,
            VsCodeWindowArgs {
                relay_path: "/api/computers/node-a/ide/relay/v1".into(),
                server_url: "peer:abcd".into(),
                folder: "/work/repo".into(),
                ose_path: "/ide/ose/".into(),
                ticket: "tk-v1".into(),
            }
        );
        assert!(!format!("{parsed:?}").contains("tk-v1"));
        let good = [
            "--relay-path", "/api/computers/c/ide/relay/l", "--server-url", "http://127.0.0.1:8740", "--folder", "/w",
            "--ose-path", "/ide/ose/",
        ];
        let parse_with = |args: &[&str], ticket: &str| {
            parse_vscode_window_args_with_ticket(["ember-app", "--vscode-window"].iter().chain(args.iter()).copied(), ticket.to_owned())
        };
        let parse = |args: &[&str]| parse_with(args, "tk");
        assert!(parse(&good).unwrap().is_ok());
        assert!(parse(&good[..6]).unwrap().unwrap_err().contains("--ose-path"));
        assert!(parse(&good[..4]).unwrap().unwrap_err().contains("--folder"));
        assert!(parse(&good[2..]).unwrap().unwrap_err().contains("--relay-path"));
        assert!(parse(&good[..5]).unwrap().unwrap_err().contains("requires a value"));
        // No ticket in the environment, or one that is not a cookie value: refused.
        assert!(parse_with(&good, "").unwrap().unwrap_err().contains(VSCODE_WINDOW_TICKET_ENV));
        assert!(parse_with(&good, "a b").unwrap().is_err());
        let mut ticket_arg = good.to_vec();
        ticket_arg.extend(["--ticket", "tk"]);
        assert!(parse(&ticket_arg).unwrap().unwrap_err().contains("unknown argument"), "no ticket in argv");
        let mut token = good.to_vec();
        token.extend(["--token", "secret"]);
        assert!(parse(&token).unwrap().unwrap_err().contains("unknown argument"), "no credential in argv");
        for (i, bad) in [
            (1, "/other/path"),
            (1, "/api/computers/c/ide/relay/l?x=1"),
            (1, "http://evil/api/computers/c/ide/relay/l"),
            (3, "http://a b"),
            (5, "relative/dir"),
            (7, "http://evil/"),
            (7, "/ide/../x/"),
            (7, "/ide/ose/?x=1"),
        ] {
            let mut args = good.to_vec();
            args[i] = bad;
            assert!(parse(&args).unwrap().is_err(), "{bad:?} must be refused");
        }
        let mut win = good.to_vec();
        win[5] = "C:\\work";
        assert!(parse(&win).unwrap().is_ok());
    }

    fn window_args(folder: &str) -> VsCodeWindowArgs {
        VsCodeWindowArgs {
            relay_path: "/api/computers/node-a/ide/relay/v1".into(),
            server_url: "http://127.0.0.1:8740".into(),
            folder: folder.into(),
            ose_path: "/ide/ose/".into(),
            ticket: "tk-v1".into(),
        }
    }

    /// SPEC FR-L7, FR-W4c (1c), FR-W5b, INTENT D55: the window's first page is the manager's path
    /// on the gateway, with the folder, the manager's ticket and the gateway's token in the query.
    #[test]
    fn fr_l7_vscode_window_url_carries_the_manager_path_and_ticket() {
        let addr: std::net::SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let page = vscode_page_url(addr, "0123abcd", &window_args("/work/ember app"));
        assert_eq!(page, "http://127.0.0.1:41234/ide/ose/?folder=%2Fwork%2Fember%20app&dpx-ticket=tk-v1&egw=0123abcd");
        // A path without its trailing slash is the same page.
        let bare = VsCodeWindowArgs { ose_path: "/ide/ose".into(), ..window_args("/w") };
        assert!(vscode_page_url(addr, "t", &bare).starts_with("http://127.0.0.1:41234/ide/ose/?folder=%2Fw&"));
        // The gateway's 302 for `egw` keeps the manager's ticket and the folder.
        let target = page.strip_prefix("http://127.0.0.1:41234").unwrap();
        assert_eq!(
            ember_gateway::access(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes(), "0123abcd"),
            ember_gateway::Access::Redirect("/ide/ose/?folder=%2Fwork%2Fember%20app&dpx-ticket=tk-v1".into())
        );
        // The window title still comes from `?folder=` (`<folder> - Ember IDE`).
        assert!(page.contains("?folder=%2Fwork%2Fember%20app&"));
    }

    /// SPEC FR-L7, FR-W5b, INTENT D55: the helper opens `ember-ide-webview` on the window's first
    /// page with `--vscode`, and strips the server credentials and tickets from the window's
    /// environment.
    #[test]
    fn fr_l7_helper_spawns_the_window_on_the_gateway_url_with_the_vscode_flag() {
        let addr: std::net::SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let page = vscode_page_url(addr, "0123abcd", &window_args("/work/ember app"));
        let (prog, args, envs) = vscode_webview_command(Path::new("/opt/ember/ember-ide-webview"), &page);
        assert_eq!(prog, PathBuf::from("/opt/ember/ember-ide-webview"));
        assert_eq!(args, vec!["--vscode"]);
        assert_eq!(envs, vec![(IDE_WEBVIEW_URL_ENV, page.clone())]);
        assert!(!args.iter().any(|a| a.contains("/ide/relay/") || a.contains("--relay-path")));
        for name in ["EMBER_SERVER_TOKEN", EDITOR_TICKET_ENV, EDITOR_VSCODE_TICKET_ENV, VSCODE_WINDOW_TICKET_ENV] {
            assert!(CREDENTIAL_ENVS.contains(&name), "{name}");
        }
        // The helper itself never builds a webview: this crate links no browser engine.
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("ember-gateway") && !manifest.contains("wry") && !manifest.contains("tao ="));
    }

    /// SPEC FR-L7, INTENT D55 (#1064): the window's first page carries the manager's ticket
    /// (`dpx-ticket`) and the gateway's token (`egw`), so the helper hands it to `ember-ide-webview`
    /// in `EMBER_IDE_WEBVIEW_URL` and keeps argv free of both, and of `--url`.
    #[test]
    fn fr_l7_helper_spawns_the_window_with_the_url_in_the_environment() {
        let addr: std::net::SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let page = vscode_page_url(addr, "0123abcd", &window_args("/work/repo"));
        assert!(page.contains("dpx-ticket=tk-v1") && page.contains("egw=0123abcd"));
        let (_, args, envs) = vscode_webview_command(Path::new("/opt/ember/ember-ide-webview"), &page);
        for a in &args {
            assert!(!a.contains("dpx-ticket") && !a.contains("egw") && !a.contains("tk-v1") && !a.contains("0123abcd"), "{a}");
        }
        assert!(!args.iter().any(|a| a == "--url"));
        assert_eq!(IDE_WEBVIEW_URL_ENV, "EMBER_IDE_WEBVIEW_URL");
        assert_eq!(envs.iter().find(|(n, _)| *n == IDE_WEBVIEW_URL_ENV).map(|(_, v)| v.as_str()), Some(page.as_str()));
        // The spawn sets them: `run_vscode_window` applies every pair to the child.
        let src = include_str!("ide.rs");
        let run = src.split("pub fn run_vscode_window(").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(run.contains("let (program, argv, envs) = vscode_webview_command(") && run.contains("cmd.env(name, value)"));
        // The window crate reads the same name.
        let cli = include_str!("../../ide-webview/src/cli.rs");
        assert!(cli.contains("pub const URL_ENV: &str = \"EMBER_IDE_WEBVIEW_URL\";"));
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
                "ose": { "relay": "/api/computers/node-a/ide/relay/l1", "path": "/ide/ose/", "ticket": "tk-l1" }
            }
        }))
        .unwrap();
        let relay = IdeRelay {
            relay: "/api/computers/node-a/ide/relay/l1".into(),
            path: "/ide/ose/".into(),
            ticket: "tk-l1".into(),
            folder: "/work/repo".into(),
            server_url: "http://127.0.0.1:8740".into(),
        };
        assert_eq!(plan(IdeKind::VsCode, Some(&t)), IdeAction::OpenIdeRelay(relay.clone()));

        let mut captured: Spawned = None;
        open_ide_relay_with(
            Some("/custom/ember-ide-webview"),
            Some(Path::new("/opt/ember/ember-app")),
            None,
            "http://127.0.0.1:8740",
            &relay,
            |prog, args, envs| {
                captured = Some((prog.to_path_buf(), args.to_vec(), envs.to_vec()));
                Ok(())
            },
        )
        .expect("spawn");
        let (prog, args, _) = captured.unwrap();
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
        // A relay launch without the manager's ticket (a server older than D55's manager) is not opened.
        let mut no_ticket = t.clone();
        no_ticket.launch.as_mut().unwrap().ose.as_mut().unwrap().ticket = String::new();
        assert!(matches!(plan(IdeKind::VsCode, Some(&no_ticket)), IdeAction::Unavailable(_)));
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
            String::new(),
            VsCodeEnv::default(),
        )
        .unwrap();
        assert_eq!(target.endpoint, "localhost:8000");
        assert_eq!(target.ticket, "secret");
        assert_eq!(target.server_url, "http://srv:1");
        assert_eq!(target.computer, "node-a");
        assert_eq!(target.relay, "");
    }

    #[test]
    fn fr_n9_editor_args_carry_the_relay_path_in_the_environment() {
        let relay = "/api/computers/local/ide/relay/abc";
        let (_, args) = editor_command(Path::new("ember"), "project", "/work", "");
        assert!(!args.iter().any(|arg| arg.contains("/ide/relay/")), "the relay path is not in argv");
        let target = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args),
            "tk".into(),
            "peer:abc".into(),
            "local".into(),
            relay.into(),
            "/ide/ose/".into(),
            VsCodeEnv::default(),
        )
        .unwrap();
        assert_eq!(target.relay, relay);
        assert_eq!(target.endpoint, "");
        assert_eq!(EDITOR_RELAY_ENV, "EMBER_EDITOR_RELAY");
    }

    /// SPEC FR-W4c (1), FR-N9.1, INTENT D55: the editor process gets the manager's path and ticket
    /// of the Ember target's launch in its environment, never in argv, and no connection token.
    #[test]
    fn fr_w4c_editor_process_gets_the_manager_path_and_ticket_in_the_environment() {
        let (_, args) = editor_command(Path::new("ember"), "project", "/work", "");
        let target = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args.clone()),
            "tk-ember".into(),
            "http://srv:1".into(),
            "node-a".into(),
            "/api/computers/node-a/ide/relay/e1".into(),
            "/ide/ose/".into(),
            VsCodeEnv::default(),
        )
        .unwrap();
        assert_eq!((target.ose_path.as_str(), target.ticket.as_str()), ("/ide/ose/", "tk-ember"));
        assert!(!args.iter().any(|a| a.contains("tk-ember") || a.contains("/ide/ose/")));
        assert!(!format!("{target:?}").contains("tk-ember"), "the ticket never reaches a log");
        assert_eq!((EDITOR_TICKET_ENV, EDITOR_OSE_PATH_ENV), ("EMBER_EDITOR_TICKET", "EMBER_EDITOR_OSE_PATH"));
        // The spawn puts both in the child's environment (the source is the contract here: the
        // child is a real process).
        let source = include_str!("ide.rs");
        let spawn = &source[source.find("pub fn spawn_editor_target").unwrap()..];
        let spawn = &spawn[..spawn.find("\n}\n").unwrap()];
        for line in [".env(EDITOR_TICKET_ENV, &target.ticket)", ".env(EDITOR_OSE_PATH_ENV, &target.ose_path)",
            ".env(EDITOR_VSCODE_PATH_ENV, &target.vscode_path)", ".env(EDITOR_VSCODE_TICKET_ENV, &target.vscode_ticket)"] {
            assert!(spawn.contains(line), "{line}");
        }
        assert!(!spawn.contains("EMBER_EDITOR_TOKEN"));
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
            String::new(),
            VsCodeEnv {
                url: String::new(),
                relay: "/api/computers/node-a/ide/relay/v1".into(),
                path: "/ide/ose/".into(),
                ticket: "tk-v1".into(),
                folder: "/work/repo".into(),
                session: String::new(),
            },
        )
        .unwrap();
        assert_eq!(target.vscode_relay, "/api/computers/node-a/ide/relay/v1");
        assert_eq!((target.vscode_path.as_str(), target.vscode_ticket.as_str()), ("/ide/ose/", "tk-v1"));
        assert!(!args.iter().any(|a| a.contains("tk-v1")), "the ticket is not in argv");
        assert_eq!((EDITOR_VSCODE_PATH_ENV, EDITOR_VSCODE_TICKET_ENV), ("EMBER_EDITOR_VSCODE_PATH", "EMBER_EDITOR_VSCODE_TICKET"));
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
            ticket: "editor-secret".into(),
            ose_path: "/ide/ose/".into(),
            server_url: "http://127.0.0.1:8740".into(),
            computer: "node-a".into(),
            relay: "/api/computers/node-a/ide/relay/e1".into(),
            vscode_relay: "/api/computers/node-a/ide/relay/v1".into(),
            vscode_path: "/ide/ose/".into(),
            vscode_ticket: "tk-v1".into(),
            vscode_folder: "/work/repo".into(),
            ..Default::default()
        };
        let relay = v1_relay();
        let fallback = VsCodeFallback::for_editor(&target);
        assert_eq!(fallback, Some(VsCodeFallback::Relay(relay.clone())));

        let mut captured: Spawned = None;
        open_ide_relay_with(
            Some("/custom/ember-ide-webview"),
            Some(Path::new("/opt/ember/ember-app")),
            None,
            &relay.server_url,
            &relay,
            |prog, args, envs| {
                captured = Some((prog.to_path_buf(), args.to_vec(), envs.to_vec()));
                Ok(())
            },
        )
        .expect("spawn");
        let (prog, args, envs) = captured.unwrap();
        assert_eq!(envs, vec![(VSCODE_WINDOW_TICKET_ENV, "tk-v1".to_owned())]);
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
                "--ose-path",
                "/ide/ose/",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("secret") || a.contains("tk-v1")), "no credential is ever in argv");

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
        let no_ticket = EditorTarget { vscode_ticket: String::new(), ..target.clone() };
        assert_eq!(VsCodeFallback::for_editor(&no_ticket), None);

        // An old server's URL opens in URL mode; a non-http(s) one offers nothing.
        let old = EditorTarget { vscode_url: "http://node:8890/?folder=%2Fwork".into(), ..Default::default() };
        assert_eq!(VsCodeFallback::for_editor(&old), Some(VsCodeFallback::Url("http://node:8890/?folder=%2Fwork".into())));
        let file = EditorTarget { vscode_url: "file:///etc/passwd".into(), ..Default::default() };
        assert_eq!(VsCodeFallback::for_editor(&file), None);
        assert_eq!(VsCodeFallback::for_editor(&EditorTarget::default()), None);
    }
}
