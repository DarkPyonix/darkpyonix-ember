//! ember-editor-conn: the connection layer of Ember's editor core (M8, SPEC §E).
//!
//! Ember's IDE window draws the Code-OSS workbench natively (dioxus-compose, no webview, no JS
//! engine). Everything the workbench's JavaScript used to do to talk to a Code-OSS server is done
//! here, in Rust, against the **unmodified** server and its **official** Node extension host
//! (INTENT E4: the extension host is never reimplemented).
//!
//! Layers, bottom-up (see `docs/design/EDITOR-CONNECTION.md` for the protocol facts and citations):
//!
//! | module        | Code-OSS counterpart                                              |
//! | ------------- | ----------------------------------------------------------------- |
//! | [`frame`]     | `ProtocolReader`/`ProtocolWriter` 13-byte header framing          |
//! | [`protocol`]  | `PersistentProtocol` ids/acks/replay/pause (sans-IO state)        |
//! | [`connection`]| socket driver: keep-alive, ack timer, timeout, transport swap     |
//! | [`handshake`] | `remoteAgentConnection.ts` upgrade + auth → sign → connectionType |
//! | [`ipc`]       | `base/parts/ipc/common/ipc.ts` channel client (management conn)   |
//! | [`remote_fs`] | `'remoteFilesystem'` channel (`DiskFileSystemProviderClient`)     |
//! | [`management`]| `'remoteextensionsenvironment'` / `'remoteExtensionsScanner'`     |
//! | [`rpc`]       | `rpcProtocol.ts` `MessageIO` encoder/decoder + request/reply peer |
//! | [`rpc_ids`]   | `extHost.protocol.ts` proxy-identifier numbering (pinned)         |
//! | [`exthost`]   | ext-host init (Ready/initData/Initialized) + typed minimal subset |
//! | [`document`]  | editor change events → `$acceptModelChanged`                      |
//! | [`uri`]       | `UriComponents` with `$mid` marshalling                           |
//!
//! Everything is generic over `AsyncRead + AsyncWrite`, so the same code rides a plain TCP socket,
//! a TLS stream, or an `ember-transport` stream later.
//!
//! **Version pinning.** The wire format is not a public contract. All facts here were read at
//! Code-OSS commit [`PINNED_COMMIT`] (release tag [`PINNED_VERSION`]), the release OSE is built
//! from (`build/ose/VERSION`); the ext-host RPC numbering in [`rpc_ids`] in particular must be
//! regenerated per Code-OSS release. [`PINNED_VERSION`] / [`PINNED_COMMIT`] are the single source
//! of truth for the pin in this crate:
//!
//! - a unit test fails if [`PINNED_VERSION`] differs from `build/ose/VERSION`;
//! - CI (`.github/workflows/test.yml`, job `editor-conn-pin`) regenerates [`rpc_ids::PROXY_IDS`]
//!   at the `build/ose/VERSION` tag and fails if the table or [`PINNED_COMMIT`] differ;
//! - at connect time [`handshake::verify_server`] reads the server's `GET /version` and refuses any
//!   other commit with [`Error::UnsupportedServerVersion`].

pub mod connection;
pub mod document;
pub mod exthost;
pub mod frame;
pub mod handshake;
pub mod ipc;
pub mod management;
pub mod protocol;
pub mod remote_fs;
pub mod rpc;
pub mod rpc_ids;
pub mod uri;

/// The Code-OSS release tag this crate is pinned to. Must equal `build/ose/VERSION` (checked by the
/// `pin_matches_ose_version` test and by CI).
pub const PINNED_VERSION: &str = "1.139.1";
/// The commit of [`PINNED_VERSION`] in `microsoft/vscode` (what an OSE server built from that tag
/// returns from `GET /version`). [`rpc_ids::PROXY_IDS`] was generated from this commit.
pub const PINNED_COMMIT: &str = "04c0d99f4fb0d8afe6ce4f0c58e31e183ac3e4b1";

/// Refuse any server commit other than [`PINNED_COMMIT`]. A server without a commit (a dev build
/// answers `GET /version` with an empty body) is refused too: its proxy numbering is unknown.
pub fn check_server_commit(server_commit: &str) -> Result<()> {
    let server_commit = server_commit.trim();
    if server_commit == PINNED_COMMIT {
        Ok(())
    } else {
        Err(Error::UnsupportedServerVersion { server: Some(server_commit.to_owned()) })
    }
}

/// Errors from any layer of the connection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("malformed frame or message: {0}")]
    Malformed(String),
    #[error("handshake failed: {0}")]
    Handshake(String),
    /// The server sent `{ "type": "error", "reason": ... }` during the handshake.
    #[error("server refused connection: {0}")]
    Refused(String),
    #[error("connection closed")]
    Closed,
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// A remote call (IPC or RPC) failed on the other side.
    #[error("remote error: {0}")]
    Remote(RemoteError),
    #[error("unknown proxy or method: {0}")]
    UnknownProxy(String),
    /// The server is not the Code-OSS commit this crate's tables were generated for, so the RPC
    /// numbering and signatures cannot be trusted. Not retryable; fall back to "Open in VS Code".
    /// `server` is the commit from `GET /version`, or `None` when the server itself refused the
    /// handshake with "version mismatch".
    #[error(
        "unsupported server version: server commit {server:?}, ember-editor-conn supports {pinned_commit} (Code-OSS {pinned_version})",
        pinned_commit = PINNED_COMMIT,
        pinned_version = PINNED_VERSION
    )]
    UnsupportedServerVersion { server: Option<String> },
}

/// An error returned by the other side of an IPC/RPC call.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteError {
    /// `name` of a serialized JS `Error` (e.g. `"EntryNotFound (FileSystemError)"`), if any.
    pub name: Option<String>,
    pub message: String,
    /// The raw payload, for errors that are not `Error` instances.
    pub raw: serde_json::Value,
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(n) => write!(f, "{n}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl RemoteError {
    pub fn from_json(raw: serde_json::Value) -> Self {
        let name = raw.get("name").and_then(|v| v.as_str()).map(str::to_owned);
        let message = raw
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| raw.to_string());
        Self { name, message, raw }
    }

    /// `FileSystemProviderErrorCode` (e.g. `EntryNotFound`) when this is a filesystem error.
    /// Code-OSS names these `"<code> (FileSystemError)"` (platform/files/common/files.ts L858).
    pub fn fs_code(&self) -> Option<&str> {
        self.name.as_deref().and_then(|n| n.strip_suffix(" (FileSystemError)"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin has one source of truth: `build/ose/VERSION` names the tag OSE is built from, and this
    /// crate must be pinned to the same tag.
    #[test]
    fn pin_matches_ose_version() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/ose/VERSION");
        let version = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            version.trim(),
            PINNED_VERSION,
            "build/ose/VERSION and ember_editor_conn::PINNED_VERSION differ: regenerate src/rpc_ids.rs with \
             scripts/gen_rpc_ids.sh at the new tag and update PINNED_VERSION / PINNED_COMMIT"
        );
    }

    #[test]
    fn pinned_commit_is_a_full_sha() {
        assert_eq!(PINNED_COMMIT.len(), 40);
        assert!(PINNED_COMMIT.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn server_commit_check() {
        assert!(check_server_commit(PINNED_COMMIT).is_ok());
        assert!(check_server_commit(&format!("{PINNED_COMMIT}\n")).is_ok());
        let err = check_server_commit("0036dcb6c18a603d2168c00fa578be600987ac7b").unwrap_err();
        assert!(matches!(err, Error::UnsupportedServerVersion { server: Some(ref c) } if c.starts_with("0036dcb6")));
        assert!(matches!(check_server_commit(""), Err(Error::UnsupportedServerVersion { .. })));
    }
}
