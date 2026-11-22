//! "Open IDE": what each target does once the server has said how to reach it
//! (`GET /api/sessions/{id}/ide`).
//!
//! - **VS Code**: the wrapped VS Code Web URL, opened in the standalone
//!   `ember-ide-webview` child process (falling back to the OS URL handler if the
//!   helper binary is not installed). This launcher process never embeds a browser
//!   engine directly.
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
    /// Hand this URL to the OS.
    OpenUrl(String),
    /// Open the Ember editor screen (its code area waits for the editor core, M8).
    EmberPlaceholder,
    /// Why it cannot be opened.
    Unavailable(String),
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

/// Spawn `bin --url <url>` when present, or invoke `fallback` when the binary is absent.
pub fn open_ide_window_with(
    bin: Option<&Path>,
    url: &str,
    fallback: impl FnOnce(&str) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match bin {
        Some(exe) => {
            let (program, args) = ide_window_command(exe, url);
            Command::new(program).args(args).spawn().map(|_| ())
        }
        None => {
            tracing::warn!(
                url = %url,
                "ember-ide-webview binary not found; falling back to system browser"
            );
            fallback(url)
        }
    }
}

/// Open `url` in the standalone `ember-ide-webview` binary, falling back to `open_external`
/// if the helper binary is not installed.
pub fn open_ide_window(url: &str) -> std::io::Result<()> {
    let env_override = std::env::var(IDE_WEBVIEW_BIN_ENV).ok();
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    let bin = resolve_ide_webview_bin(
        env_override.as_deref(),
        current_exe.as_deref(),
        path_var.as_deref(),
    );
    open_ide_window_with(bin.as_deref(), url, open_external)
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
        var(EDITOR_VSCODE_ENV),
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
/// Environment variable carrying the project's VS Code target URL, which the editor window offers
/// when the server is a Code-OSS commit it does not support (SPEC FR-E10, #32).
pub const EDITOR_VSCODE_ENV: &str = "EMBER_EDITOR_VSCODE";

fn parse_editor_args_with_env<I, S>(
    args: I,
    token: String,
    server_url: String,
    computer: String,
    relay: String,
    vscode_url: String,
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
            return Some(EditorTarget { project, folder, endpoint, token, server_url, computer, relay, vscode_url });
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
    Command::new(bin)
        .args(args)
        .env(EDITOR_TOKEN_ENV, &target.token)
        .env(EDITOR_SERVER_ENV, &target.server_url)
        .env(EDITOR_COMPUTER_ENV, &target.computer)
        .env(EDITOR_RELAY_ENV, &target.relay)
        .env(EDITOR_VSCODE_ENV, &target.vscode_url)
        .spawn()
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
    fn fr_l7_open_ide_window_spawns_helper_or_falls_back() {
        let mut fallback_called_with = None;
        open_ide_window_with(None, "http://127.0.0.1:8890/?folder=%2Fw", |u| {
            fallback_called_with = Some(u.to_owned());
            Ok(())
        })
        .expect("fallback succeeds");
        assert_eq!(
            fallback_called_with.as_deref(),
            Some("http://127.0.0.1:8890/?folder=%2Fw")
        );

        #[cfg(unix)]
        {
            let mut fallback_hit = false;
            open_ide_window_with(
                Some(Path::new("/usr/bin/true")),
                "http://127.0.0.1:8890/?folder=%2Fw",
                |_| {
                    fallback_hit = true;
                    Ok(())
                },
            )
            .expect("spawn helper succeeds");
            assert!(!fallback_hit, "fallback must not run when helper binary exists");
        }
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
            String::new(),
        )
        .unwrap();
        assert_eq!(target.relay, relay);
        assert_eq!(target.endpoint, "127.0.0.1:9000");
        assert_eq!(EDITOR_RELAY_ENV, "EMBER_EDITOR_RELAY");
    }

    /// SPEC FR-E10 (#32): the VS Code target the fallback offers travels in the environment.
    #[test]
    fn fr_e10_editor_args_carry_the_vscode_url_in_the_environment() {
        let (_, args) = editor_command(Path::new("ember"), "project", "/work", "127.0.0.1:9000");
        let target = parse_editor_args_with_env(
            std::iter::once("ember".to_owned()).chain(args),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            "http://node:8890/?folder=%2Fwork".into(),
        )
        .unwrap();
        assert_eq!(target.vscode_url, "http://node:8890/?folder=%2Fwork");
        assert_eq!(EDITOR_VSCODE_ENV, "EMBER_EDITOR_VSCODE");
    }
}
