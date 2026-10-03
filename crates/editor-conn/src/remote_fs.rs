//! The `'remoteFilesystem'` IPC channel.
//!
//! Client side upstream: `RemoteFileSystemProviderClient`
//! (`src/vs/workbench/services/remote/common/remoteFileSystemProviderClient.ts`, channel name
//! L16) which is a `DiskFileSystemProviderClient`
//! (`src/vs/platform/files/common/diskFileSystemProviderClient.ts`: `stat` L80, `readdir` L88,
//! `readFile` L96, `writeFile` L161, `mkdir` L193, `delete` L197, `rename` L201, watching
//! L231-259). Server side: `RemoteAgentFileSystemProviderChannel`
//! (`src/vs/server/node/remoteFileSystemProviderServer.ts`) over
//! `src/vs/platform/files/node/diskFileSystemProviderServer.ts` (command switch L40-65).
//!
//! Every command's `arg` is an array of positional arguments. URIs are `vscode-remote://…`
//! (the server's URI transformer maps them to `file:`).

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::ipc::{IpcClient, IpcSubscription, IpcValue};
use crate::uri::UriComponents;
use crate::{Error, Result};

pub const CHANNEL: &str = "remoteFilesystem";

/// `FileType` bit flags (platform/files/common/files.ts L449).
pub mod file_type {
    pub const UNKNOWN: u32 = 0;
    pub const FILE: u32 = 1;
    pub const DIRECTORY: u32 = 2;
    pub const SYMBOLIC_LINK: u32 = 64;
}

/// `IStat` (files.ts L498-524).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Stat {
    #[serde(rename = "type")]
    pub file_type: u32,
    /// Milliseconds since the epoch.
    pub mtime: f64,
    pub ctime: f64,
    pub size: u64,
    #[serde(default)]
    pub permissions: Option<u32>,
}

impl Stat {
    pub fn is_dir(&self) -> bool {
        self.file_type & file_type::DIRECTORY != 0
    }
    pub fn is_file(&self) -> bool {
        self.file_type & file_type::FILE != 0
    }
}

/// `IFileWriteOptions` (files.ts L378).
#[derive(Debug, Clone, Serialize)]
pub struct WriteOptions {
    pub create: bool,
    pub overwrite: bool,
    pub unlock: bool,
    pub atomic: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { create: true, overwrite: true, unlock: false, atomic: false }
    }
}

/// `IFileDeleteOptions`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOptions {
    pub recursive: bool,
    pub use_trash: bool,
    pub atomic: bool,
}

/// `IWatchOptions` (files.ts L526-570).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchOptions {
    pub recursive: bool,
    pub excludes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub includes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<i64>,
}

/// `FileChangeType` (files.ts L977).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeType {
    Updated,
    Added,
    Deleted,
}

/// `IFileChange` (files.ts L986).
#[derive(Debug, Clone, PartialEq)]
pub struct FileChange {
    pub kind: FileChangeType,
    pub resource: UriComponents,
    pub correlation_id: Option<i64>,
}

#[derive(Deserialize)]
struct RawFileChange {
    #[serde(rename = "type")]
    ty: u8,
    resource: UriComponents,
    #[serde(rename = "cId", default)]
    c_id: Option<i64>,
}

/// One delivery of the `fileChange` event: changes, or a watcher error string.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchEvent {
    Changes(Vec<FileChange>),
    Error(String),
}

/// A client for one remote filesystem session (one `sessionId` partitions watch events).
pub struct RemoteFs {
    ipc: IpcClient,
    session_id: String,
}

fn uri_arg(uri: &UriComponents) -> Result<IpcValue> {
    IpcValue::json(uri)
}

impl RemoteFs {
    pub fn new(ipc: IpcClient) -> Self {
        Self { ipc, session_id: uuid::Uuid::new_v4().to_string() }
    }

    async fn call(&self, cmd: &str, args: Vec<IpcValue>) -> Result<IpcValue> {
        self.ipc.call(CHANNEL, cmd, IpcValue::Array(args)).await
    }

    pub async fn stat(&self, uri: &UriComponents) -> Result<Stat> {
        let v = self.call("stat", vec![uri_arg(uri)?]).await?;
        Ok(serde_json::from_value(v.to_json())?)
    }

    pub async fn read_file(&self, uri: &UriComponents) -> Result<Vec<u8>> {
        let v = self.call("readFile", vec![uri_arg(uri)?, IpcValue::Undefined]).await?;
        match v {
            IpcValue::VsBuffer(b) | IpcValue::Buffer(b) => Ok(b),
            other => Err(Error::Malformed(format!("readFile returned {other:?}"))),
        }
    }

    pub async fn write_file(&self, uri: &UriComponents, content: Vec<u8>, opts: &WriteOptions) -> Result<()> {
        self.call("writeFile", vec![uri_arg(uri)?, IpcValue::VsBuffer(content), IpcValue::json(opts)?])
            .await?;
        Ok(())
    }

    /// `[name, FileType]` pairs.
    pub async fn readdir(&self, uri: &UriComponents) -> Result<Vec<(String, u32)>> {
        let v = self.call("readdir", vec![uri_arg(uri)?]).await?;
        let entries = match v {
            IpcValue::Array(entries) => entries,
            other => return Ok(serde_json::from_value(other.to_json())?),
        };
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            match e {
                IpcValue::Array(pair) if pair.len() == 2 => {
                    let name = match &pair[0] {
                        IpcValue::String(s) => s.clone(),
                        other => return Err(Error::Malformed(format!("readdir name {other:?}"))),
                    };
                    let ty = pair[1].as_int().unwrap_or(0) as u32;
                    out.push((name, ty));
                }
                other => return Err(Error::Malformed(format!("readdir entry {other:?}"))),
            }
        }
        Ok(out)
    }

    pub async fn mkdir(&self, uri: &UriComponents) -> Result<()> {
        self.call("mkdir", vec![uri_arg(uri)?]).await.map(|_| ())
    }

    pub async fn delete(&self, uri: &UriComponents, opts: &DeleteOptions) -> Result<()> {
        self.call("delete", vec![uri_arg(uri)?, IpcValue::json(opts)?]).await.map(|_| ())
    }

    pub async fn rename(&self, from: &UriComponents, to: &UriComponents, overwrite: bool) -> Result<()> {
        let opts = serde_json::json!({ "overwrite": overwrite });
        self.call("rename", vec![uri_arg(from)?, uri_arg(to)?, IpcValue::Object(opts)])
            .await
            .map(|_| ())
    }

    /// Subscribe to this session's `fileChange` event. Call once, before [`Self::watch`].
    pub async fn subscribe_changes(&self) -> Result<mpsc::UnboundedReceiver<WatchEvent>> {
        let IpcSubscription { mut events, .. } = self
            .ipc
            .listen(CHANNEL, "fileChange", IpcValue::Array(vec![IpcValue::str(&self.session_id)]))
            .await?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(v) = events.recv().await {
                let ev = match parse_watch_event(v) {
                    Ok(ev) => ev,
                    Err(e) => {
                        tracing::warn!("editor-conn fs: bad fileChange payload: {e}");
                        continue;
                    }
                };
                if tx.send(ev).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    /// Start watching; returns the request id to pass to [`Self::unwatch`].
    pub async fn watch(&self, uri: &UriComponents, opts: &WatchOptions) -> Result<String> {
        let req = uuid::Uuid::new_v4().to_string();
        self.call(
            "watch",
            vec![IpcValue::str(&self.session_id), IpcValue::str(&req), uri_arg(uri)?, IpcValue::json(opts)?],
        )
        .await?;
        Ok(req)
    }

    pub async fn unwatch(&self, req: &str) -> Result<()> {
        self.call("unwatch", vec![IpcValue::str(&self.session_id), IpcValue::str(req)])
            .await
            .map(|_| ())
    }
}

/// The `fileChange` payload is `IFileChange[] | string` (diskFileSystemProviderClient.ts L239).
pub fn parse_watch_event(v: IpcValue) -> Result<WatchEvent> {
    match v {
        IpcValue::String(s) => Ok(WatchEvent::Error(s)),
        other => {
            let raw: Vec<RawFileChange> = serde_json::from_value(other.to_json())?;
            Ok(WatchEvent::Changes(
                raw.into_iter()
                    .map(|r| FileChange {
                        kind: match r.ty {
                            1 => FileChangeType::Added,
                            2 => FileChangeType::Deleted,
                            _ => FileChangeType::Updated,
                        },
                        resource: r.resource,
                        correlation_id: r.c_id,
                    })
                    .collect(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_object() {
        let s: Stat = serde_json::from_value(serde_json::json!({
            "type": 2, "mtime": 1727900000000u64, "ctime": 1727800000000u64, "size": 4096
        }))
        .unwrap();
        assert!(s.is_dir());
        assert_eq!(s.permissions, None);
    }

    #[test]
    fn parses_file_change_array_and_error() {
        let v = IpcValue::Array(vec![IpcValue::Object(serde_json::json!({
            "type": 1,
            "resource": {"$mid":1,"scheme":"vscode-remote","authority":"h","path":"/w/new.rs"}
        }))]);
        let ev = parse_watch_event(v).unwrap();
        assert_eq!(
            ev,
            WatchEvent::Changes(vec![FileChange {
                kind: FileChangeType::Added,
                resource: UriComponents::remote("h", "/w/new.rs"),
                correlation_id: None,
            }])
        );
        assert_eq!(
            parse_watch_event(IpcValue::str("EMFILE")).unwrap(),
            WatchEvent::Error("EMFILE".into())
        );
    }

    #[test]
    fn write_file_args_shape() {
        let uri = UriComponents::remote("h", "/w/a.txt");
        let args = IpcValue::Array(vec![
            IpcValue::json(&uri).unwrap(),
            IpcValue::VsBuffer(b"hi".to_vec()),
            IpcValue::json(&WriteOptions::default()).unwrap(),
        ]);
        let json = args.to_json();
        assert_eq!(json[2], serde_json::json!({"create":true,"overwrite":true,"unlock":false,"atomic":false}));
    }
}
