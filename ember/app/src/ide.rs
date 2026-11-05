//! "Open IDE" (FR-L7): what each target does once the server has said how to reach it
//! (`GET /api/sessions/{id}/ide`).
//!
//! - **VS Code**: the wrapped VS Code Web URL, opened in the **system browser**. This process
//!   never hosts it (NFR-L2): the URL is handed to the OS URL handler.
//! - **Gateway**: the `jetbrains-gateway://` link, handed to the OS URL handler, which starts
//!   JetBrains Gateway on this machine.
//! - **Ember**: the editor screen (FR-L31); its code area waits for the editor core (M8).
//!
//! Only `http`, `https` and the JetBrains schemes are ever passed to the OS, so a server
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
    /// Hand this URL to the OS.
    OpenUrl(String),
    /// Open the Ember editor screen (its code area waits for the editor core, M8).
    EmberPlaceholder,
    /// Why it cannot be opened.
    Unavailable(String),
}

pub fn plan(kind: IdeKind, target: Option<&IdeTarget>) -> IdeAction {
    if kind == IdeKind::Ember {
        // The caller opens the editor screen (FR-L31) from the launch data (project, folder).
        return IdeAction::EmberPlaceholder;
    }
    let Some(t) = target else {
        return IdeAction::Unavailable(format!("the server offers no {} target", kind.label()));
    };
    if !t.available {
        return IdeAction::Unavailable(t.reason.clone().unwrap_or_else(|| format!("{} is not available", kind.label())));
    }
    match t.url.as_deref() {
        Some(url) if allowed_url(kind, url) => IdeAction::OpenUrl(url.to_string()),
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

/// CLI flag that launches this binary directly into the editor, which runs as its own HTML-mode window (FR-L31).
/// TODO(dioxus-compose#152): once the `window` element on a shared host lands, open the editor
/// window on the same host instead of spawning a second process.
pub const EDITOR_ARG: &str = "--editor";

/// Parse `--editor <project> [folder]` from an argument list (`std::env::args()`).
pub fn parse_editor_args<I, S>(args: I) -> Option<EditorTarget>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parse_editor_args_with_token(args, std::env::var("EMBER_EDITOR_TOKEN").unwrap_or_default())
}

fn parse_editor_args_with_token<I, S>(args: I, token: String) -> Option<EditorTarget>
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
            return Some(EditorTarget { project, folder, endpoint, token });
        }
    }
    None
}

/// The `(executable, args)` pair that spawns a separate Ember editor window (FR-L31).
/// TODO(dioxus-compose#152): replace with an in-process `window` node once #152 lands.
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
    Command::new(bin).args(args).env("EMBER_EDITOR_TOKEN", &target.token).spawn()
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
            IdeAction::OpenUrl("http://h:8890/?folder=%2Fw".into())
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
        let target = parse_editor_args_with_token(std::iter::once("ember".to_owned()).chain(args), "secret".into()).unwrap();
        assert_eq!(target.endpoint, "localhost:8000");
        assert_eq!(target.token, "secret");
    }
}
