//! ember-editor-conn — the connection layer of Ember's editor core (M8, SPEC §E).
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
//! Code-OSS commit [`PINNED_CODE_OSS_COMMIT`] (version [`PINNED_CODE_OSS_VERSION`]); the
//! ext-host RPC numbering in [`rpc_ids`] in particular must be regenerated per Code-OSS release.

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

/// The Code-OSS commit whose source this crate was written against.
pub const PINNED_CODE_OSS_COMMIT: &str = "0036dcb6c18a603d2168c00fa578be600987ac7b";
/// `package.json` version at [`PINNED_CODE_OSS_COMMIT`].
pub const PINNED_CODE_OSS_VERSION: &str = "1.141.0";

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
