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
}
