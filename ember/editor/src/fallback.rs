//! What to do when a connect fails (M8 criterion "version check at connect; fall back to the VS
//! Code target when unknown").
//!
//! The connection layer refuses any server whose `GET /version` commit is not
//! `ember_editor_conn::PINNED_COMMIT`, whose `/version` is missing or empty, or which answers
//! "version mismatch" during the handshake. All three surface as
//! `ember_editor_conn::Error::UnsupportedServerVersion`. That is not retryable and not a failure of
//! the computer: the wire numbering is simply unknown, so the UI offers the VS Code target
//! ("Open in VS Code") instead of the Ember editor. Other errors (network, auth) are not a reason
//! to switch targets and are reported as they are.

use tokio::sync::mpsc;

use crate::engine::SessionUpdate;
use crate::session::EditorSession;
use crate::Error;

/// Why the Ember editor cannot be used with this server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// Use the VS Code target. `server_commit` is `None` when the server's commit is unknown.
    OpenInVsCode { server_commit: Option<String> },
}

impl Fallback {
    /// One line for the UI, saying what was found and what happens instead.
    pub fn message(&self) -> String {
        match self {
            Fallback::OpenInVsCode { server_commit: Some(c) } => format!(
                "This computer's editor server is Code-OSS {c}, but Ember's editor supports {} ({}). Opening in VS Code instead.",
                ember_editor_conn::PINNED_COMMIT,
                ember_editor_conn::PINNED_VERSION
            ),
            Fallback::OpenInVsCode { server_commit: None } => format!(
                "This computer's editor server version is unknown; Ember's editor supports Code-OSS {}. Opening in VS Code instead.",
                ember_editor_conn::PINNED_VERSION
            ),
        }
    }
}

/// Percent-encodes everything but RFC 3986 unreserved characters (and `/` when `keep_slash` is
/// `true`), matching `ember_server::api::ide::percent_encode`.
fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && b == b'/');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Constructs the VS Code Web fallback URL (`<base>/?folder=<workspace_path>`), or returns
/// `vscode_base_url` as-is when it already contains `?folder=`.
pub fn vscode_fallback_url(vscode_base_url: &str, workspace_path: &str) -> String {
    if vscode_base_url.contains("?folder=") || vscode_base_url.contains("&folder=") {
        vscode_base_url.to_owned()
    } else {
        format!("{}/?folder={}", vscode_base_url.trim_end_matches('/'), percent_encode(workspace_path, true))
    }
}

/// The resolved fallback action when `EditorSession::connect_or_fallback` switches from the Ember
/// editor to the VS Code target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackTarget {
    pub fallback: Fallback,
    /// The VS Code target URL (`<ide_url>/?folder=<workspace_path>`) when a VS Code base URL is configured.
    pub vscode_url: Option<String>,
}

impl FallbackTarget {
    pub fn from_error(err: &Error, vscode_base_url: Option<&str>, workspace_path: &str) -> Option<Self> {
        let fallback = fallback_for(err)?;
        let vscode_url = vscode_base_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(|base| vscode_fallback_url(base, workspace_path));
        Some(Self { fallback, vscode_url })
    }
}

/// Outcome of connecting to a server with connect-time version check and VS Code fallback.
pub enum ConnectOutcome {
    Connected {
        session: EditorSession,
        updates: mpsc::UnboundedReceiver<SessionUpdate>,
    },
    Fallback(FallbackTarget),
}

impl std::fmt::Debug for ConnectOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected { session, .. } => {
                f.debug_struct("Connected").field("authority", &session.authority()).finish()
            }
            Self::Fallback(target) => f.debug_tuple("Fallback").field(target).finish(),
        }
    }
}

/// `Some` when `err` means "fall back to the VS Code target", `None` for every other error.
pub fn fallback_for(err: &Error) -> Option<Fallback> {
    match err {
        Error::Conn(ember_editor_conn::Error::UnsupportedServerVersion { server }) => {
            Some(Fallback::OpenInVsCode { server_commit: server.clone().filter(|c| !c.is_empty()) })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_editor_conn::Error as ConnError;

    #[test]
    fn unknown_commit_falls_back() {
        let e = Error::Conn(ConnError::UnsupportedServerVersion { server: None });
        assert_eq!(fallback_for(&e), Some(Fallback::OpenInVsCode { server_commit: None }));
    }

    #[test]
    fn other_commit_falls_back_and_is_named() {
        let e = Error::Conn(ConnError::UnsupportedServerVersion { server: Some("deadbeef".into()) });
        let f = fallback_for(&e).unwrap();
        assert_eq!(f, Fallback::OpenInVsCode { server_commit: Some("deadbeef".into()) });
        assert!(f.message().contains("deadbeef"));
        assert!(f.message().contains(ember_editor_conn::PINNED_VERSION));
    }

    #[test]
    fn empty_commit_counts_as_unknown() {
        let e = Error::Conn(ConnError::UnsupportedServerVersion { server: Some(String::new()) });
        assert_eq!(fallback_for(&e), Some(Fallback::OpenInVsCode { server_commit: None }));
    }

    #[test]
    fn other_errors_do_not_fall_back() {
        assert_eq!(fallback_for(&Error::Closed), None);
        assert_eq!(fallback_for(&Error::Refused("x".into())), None);
        assert_eq!(fallback_for(&Error::Conn(ConnError::Timeout("handshake"))), None);
        assert_eq!(fallback_for(&Error::Conn(ConnError::Refused("auth mismatch".into()))), None);
    }

    #[test]
    fn fallback_target_constructs_vscode_url() {
        let e = Error::Conn(ConnError::UnsupportedServerVersion { server: Some("deadbeef".into()) });
        let t = FallbackTarget::from_error(&e, Some("http://127.0.0.1:8890/"), "/Users/me/my app").unwrap();
        assert_eq!(
            t,
            FallbackTarget {
                fallback: Fallback::OpenInVsCode { server_commit: Some("deadbeef".into()) },
                vscode_url: Some("http://127.0.0.1:8890/?folder=/Users/me/my%20app".into()),
            }
        );
        let already = FallbackTarget::from_error(
            &e,
            Some("http://127.0.0.1:8890/?folder=/Users/me/my%20app"),
            "/Users/me/my app",
        )
        .unwrap();
        assert_eq!(already.vscode_url.as_deref(), Some("http://127.0.0.1:8890/?folder=/Users/me/my%20app"));
    }
}
