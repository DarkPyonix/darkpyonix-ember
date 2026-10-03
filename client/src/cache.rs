//! Snapshot cache for offline cold start (FR-L1).
//!
//! One small JSON file at a path the host app chooses (its data/cache directory differs per
//! platform). Writes go to a sibling temp file and are renamed into place, so a crash mid-write
//! leaves the previous snapshot intact.

use std::path::{Path, PathBuf};

use crate::state::{CacheSnapshot, CACHE_FORMAT};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache encode: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Read the snapshot at `path`. A missing, unreadable, corrupt or other-format file yields
/// `None`: the cache is an optimisation, never a reason to fail start-up.
pub async fn load(path: &Path) -> Option<CacheSnapshot> {
    let bytes = tokio::fs::read(path).await.ok()?;
    match serde_json::from_slice::<CacheSnapshot>(&bytes) {
        Ok(s) if s.format == CACHE_FORMAT => Some(s),
        Ok(s) => {
            tracing::info!(format = s.format, "ignoring cache in another format");
            None
        }
        Err(e) => {
            tracing::warn!("ignoring unreadable cache {}: {e}", path.display());
            None
        }
    }
}

pub async fn save(path: &Path, snap: &CacheSnapshot) -> Result<(), CacheError> {
    let bytes = serde_json::to_vec(snap)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = tmp_path(path);
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_and_tolerate_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/cache.json");
        assert!(load(&path).await.is_none());
        let snap = CacheSnapshot { format: CACHE_FORMAT, baselined: true, ..Default::default() };
        save(&path, &snap).await.unwrap();
        assert_eq!(load(&path).await, Some(snap));
        tokio::fs::write(&path, b"{not json").await.unwrap();
        assert!(load(&path).await.is_none());
        tokio::fs::write(&path, br#"{"format":99,"baselined":true,"sessions":[],"last_seen":{},"computers":[]}"#)
            .await
            .unwrap();
        assert!(load(&path).await.is_none());
    }
}
