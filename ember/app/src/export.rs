//! Export a session as a self-contained file of its normalised transcript (FR-L9).
//!
//! The main server builds the file (`GET /sessions/{id}/export`: `format: "ember-transcript"`,
//! the session record and every stored event), so it never depends on what this client
//! happened to have loaded. The app adds `transcript`, the events reduced by the client's own
//! reducer (the representation every client renders, FR-S2), so the file is readable without
//! replaying it.

use std::path::PathBuf;

use ember_client::transcript::Transcript;
use ember_client::wire::StoredEvent;
use ember_client::Client;

use crate::config::{ensure_dir, safe_file_stem};

/// Add the reduced transcript to the server's export file and serialise it.
pub fn build(mut export: serde_json::Value) -> Result<Vec<u8>, String> {
    let events: Vec<StoredEvent> = serde_json::from_value(export.get("events").cloned().unwrap_or_default())
        .map_err(|e| format!("export has no readable events: {e}"))?;
    let mut t = Transcript::new();
    for e in &events {
        t.apply(e);
    }
    let obj = export.as_object_mut().ok_or("export is not a JSON object")?;
    obj.insert("transcript".into(), serde_json::to_value(&t).map_err(|e| e.to_string())?);
    serde_json::to_vec_pretty(&export).map_err(|e| e.to_string())
}

/// Fetch, complete and write `<dir>/<title>-<id>.json`. Returns the file's path.
pub async fn export_session(client: &Client, dir: PathBuf, id: &str, title: &str) -> Result<PathBuf, String> {
    let export = client.api().export_session(id).await.map_err(|e| e.to_string())?;
    let bytes = build(export)?;
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
    use serde_json::json;

    #[test]
    fn fr_l9_export_is_self_contained() {
        let server_file = json!({
            "format": "ember-transcript",
            "version": 1,
            "exported_at": 9,
            "session": { "id": "s1", "project": "p", "agent": "codex", "cwd": "/w", "status": "finished",
                         "title": "t", "created_at": 1, "updated_at": 2, "last_seq": 2, "pinned": true },
            "events": [
                { "session_id": "s1", "seq": 1, "at": 1, "event": { "kind": "user_message", "text": "hi" } },
                { "session_id": "s1", "seq": 2, "at": 2, "event": { "kind": "assistant_message", "text": "hello" } }
            ]
        });
        let v: serde_json::Value = serde_json::from_slice(&build(server_file).unwrap()).unwrap();
        assert_eq!(v["format"], "ember-transcript");
        assert_eq!(v["session"]["id"], "s1");
        assert_eq!(v["events"].as_array().unwrap().len(), 2);
        assert_eq!(v["transcript"]["items"][1]["text"], "hello");
        assert_eq!(v["transcript"]["last_seq"], 2);
        assert!(build(json!([])).is_err());
    }
}
