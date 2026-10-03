//! Export a session as a self-contained file of its normalised transcript (FR-L9).
//!
//! The file holds the session record, every stored event (fetched fresh from the main server,
//! so it does not depend on what this client happened to have loaded) and the transcript
//! reduced from them, the same representation every client renders (FR-S2).

use std::path::PathBuf;

use serde::Serialize;

use ember_client::transcript::Transcript;
use ember_client::wire::{SessionRecord, StoredEvent};
use ember_client::Client;

use crate::config::{ensure_dir, safe_file_stem};

pub const EXPORT_FORMAT: u32 = 1;

#[derive(Debug, Serialize)]
pub struct ExportFile<'a> {
    pub format: u32,
    pub exported_at: i64,
    pub session: &'a SessionRecord,
    pub events: &'a [StoredEvent],
    pub transcript: &'a Transcript,
}

pub fn build(session: &SessionRecord, events: &[StoredEvent], exported_at: i64) -> serde_json::Result<Vec<u8>> {
    let mut t = Transcript::new();
    for e in events {
        t.apply(e);
    }
    serde_json::to_vec_pretty(&ExportFile { format: EXPORT_FORMAT, exported_at, session, events, transcript: &t })
}

/// Fetch, reduce and write `<dir>/<title>-<id>.json`. Returns the file's path.
pub async fn export_session(client: &Client, dir: PathBuf, id: &str, title: &str) -> Result<PathBuf, String> {
    let detail = client.api().session(id).await.map_err(|e| e.to_string())?;
    let events = client.api().events(id, 0).await.map_err(|e| e.to_string())?;
    let bytes = build(&detail.session, &events, crate::model::now_ms()).map_err(|e| e.to_string())?;
    let short: String = id.chars().take(8).collect();
    let path = dir.join(format!("{}-{short}.json", safe_file_stem(title)));
    tokio::task::spawn_blocking(move || -> std::io::Result<PathBuf> {
        ensure_dir(&dir)?;
        std::fs::write(&path, bytes)?;
        Ok(path)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::wire::{AgentEvent, SessionStatus};

    #[test]
    fn fr_l9_export_is_self_contained() {
        let rec = SessionRecord {
            id: "s1".into(),
            project: "p".into(),
            agent: "codex".into(),
            cwd: "/w".into(),
            model: None,
            native_id: None,
            status: SessionStatus::Finished,
            title: "t".into(),
            created_at: 1,
            updated_at: 2,
            last_seq: 2,
        };
        let events = vec![
            StoredEvent { session_id: "s1".into(), seq: 1, at: 1, event: AgentEvent::UserMessage { text: "hi".into() } },
            StoredEvent { session_id: "s1".into(), seq: 2, at: 2, event: AgentEvent::AssistantMessage { text: "hello".into() } },
        ];
        let v: serde_json::Value = serde_json::from_slice(&build(&rec, &events, 9).unwrap()).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["session"]["id"], "s1");
        assert_eq!(v["events"].as_array().unwrap().len(), 2);
        assert_eq!(v["transcript"]["items"][1]["text"], "hello");
        assert_eq!(v["transcript"]["last_seq"], 2);
    }
}
