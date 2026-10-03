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
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    Ok(())
}

/// A temp file unique to this write, so concurrent saves (the debounced writer and an explicit
/// flush) never rename each other's file away.
fn tmp_path(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_saves_do_not_race_on_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let snap = CacheSnapshot { format: CACHE_FORMAT, ..Default::default() };
        let saves: Vec<_> = (0..32).map(|_| save(&path, &snap)).collect();
        for r in futures_util::future::join_all(saves).await {
            r.unwrap();
        }
        assert!(load(&path).await.is_some());
        let leftovers = std::fs::read_dir(dir.path()).unwrap().filter(|e| {
            e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".tmp")
        });
        assert_eq!(leftovers.count(), 0);
    }

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
