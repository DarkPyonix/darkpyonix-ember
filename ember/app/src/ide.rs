//! "Open IDE" (FR-L7): what each target does once the server has said how to reach it
//! (`GET /api/sessions/{id}/ide`).
//!
//! - **VS Code**: the wrapped VS Code Web URL, opened in the **system browser**. This process
//!   never hosts it (NFR-L2): the URL is handed to the OS URL handler.
//! - **Gateway**: the `jetbrains-gateway://` link, handed to the OS URL handler, which starts
//!   JetBrains Gateway on this machine.
//! - **Ember**: the editor core arrives in M8; until then it is a placeholder.
//!
//! Only `http`, `https` and the JetBrains schemes are ever passed to the OS, so a server
//! answer cannot make this app open an arbitrary file or program.

use std::process::Command;

use crate::server::IdeTarget;

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
    /// The Ember editor (M8) is not built yet.
    EmberPlaceholder,
    /// Why it cannot be opened.
    Unavailable(String),
}

pub fn plan(kind: IdeKind, target: Option<&IdeTarget>) -> IdeAction {
    if kind == IdeKind::Ember {
        // TODO(M8, editor core): open the Ember IDE window with `target.launch`
        // (session, project, computer, folder, server_url).
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
}
