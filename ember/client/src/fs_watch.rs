//! File watching of a computer through the server (SPEC `FR-W4d`, #460).
//!
//! [`crate::api::Api::watch_fs`] opens `GET /api/computers/{id}/fs/watch`, over plain HTTP with
//! the server token or over the transport (the device key is the credential, and only a `full`
//! device passes). The frames are the node's, relayed unchanged:
//! [`FsWatch::watch`] and [`FsWatch::unwatch`] send requests, [`FsWatch::next`] reads the
//! answers and the change batches. An [`FsWatchMessage::Overflow`] means events were lost: read
//! the watched tree again.

use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

use crate::api::{ApiError, ApiResult, PushSocket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsChangeKind {
    Created,
    Changed,
    Removed,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsChange {
    pub kind: FsChangeKind,
    pub path: PathBuf,
    /// The new path of a `renamed` change.
    #[serde(default)]
    pub to: Option<PathBuf>,
}

/// What the node sends on the watch socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FsWatchMessage {
    /// The watch is live; event paths are below `path` (the canonical spelling).
    Watching { id: u64, path: PathBuf },
    Unwatched { id: u64 },
    /// A refused request (`forbidden_path`, `not_found`, `too_many_watches`, ...).
    Error {
        #[serde(default)]
        id: Option<u64>,
        code: String,
        message: String,
    },
    Change { id: u64, events: Vec<FsChange> },
    /// Events of watch `id` were lost: read its tree again.
    Overflow { id: u64 },
}

/// An open watch socket. Dropping it ends every watch on it.
pub struct FsWatch {
    socket: PushSocket,
    next_id: u64,
}

impl FsWatch {
    pub(crate) fn new(socket: PushSocket) -> Self {
        Self { socket, next_id: 1 }
    }

    async fn send(&mut self, v: serde_json::Value) -> ApiResult<()> {
        self.socket.send(Message::Text(v.to_string().into())).await.map_err(|e| ApiError::Push(e.to_string()))
    }

    /// Ask for a watch of `path` (absolute, on the computer). The answer, `Watching` or `Error`
    /// with the returned id, comes through [`FsWatch::next`].
    pub async fn watch(&mut self, path: impl AsRef<Path>, recursive: bool) -> ApiResult<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({ "op": "watch", "id": id, "path": path.as_ref(), "recursive": recursive })).await?;
        Ok(id)
    }

    /// End watch `id`; the node answers `Unwatched` (or `Error` `unknown_watch`).
    pub async fn unwatch(&mut self, id: u64) -> ApiResult<()> {
        self.send(serde_json::json!({ "op": "unwatch", "id": id })).await
    }

    /// The next message; `None` once the socket ended (closed by either side, or the device was
    /// revoked).
    pub async fn next(&mut self) -> ApiResult<Option<FsWatchMessage>> {
        loop {
            match self.socket.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Text(t))) => {
                    return serde_json::from_str(t.as_str()).map(Some).map_err(|e| ApiError::Decode(e.to_string()))
                }
                Some(Ok(_)) => continue,
                Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => return Ok(None),
                Some(Err(e)) => return Err(ApiError::Push(e.to_string())),
            }
        }
    }

    /// Close the socket.
    pub async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_w4d_client_decodes_the_node_frames() {
        let m: FsWatchMessage = serde_json::from_str(
            r#"{"op":"change","id":2,"events":[{"kind":"renamed","path":"/r/a","to":"/r/b"},{"kind":"created","path":"/r/c"}]}"#,
        )
        .unwrap();
        assert_eq!(
            m,
            FsWatchMessage::Change {
                id: 2,
                events: vec![
                    FsChange { kind: FsChangeKind::Renamed, path: "/r/a".into(), to: Some("/r/b".into()) },
                    FsChange { kind: FsChangeKind::Created, path: "/r/c".into(), to: None },
                ],
            }
        );
        let m: FsWatchMessage = serde_json::from_str(r#"{"op":"error","id":1,"code":"forbidden_path","message":"x"}"#).unwrap();
        assert!(matches!(m, FsWatchMessage::Error { id: Some(1), ref code, .. } if code == "forbidden_path"));
        let m: FsWatchMessage = serde_json::from_str(r#"{"op":"overflow","id":4}"#).unwrap();
        assert_eq!(m, FsWatchMessage::Overflow { id: 4 });
    }
}
