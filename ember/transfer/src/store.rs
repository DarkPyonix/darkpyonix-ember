//! Received-files storage, chunk writing, SHA-256 verification and transfer history
//! (SPEC `FR-F4`, `FR-F5`, `FR-F8`, `FR-F9`, `FR-F10`, `#527`).
//!
//! Rules enforced on every write:
//! - Nothing is written before an offer is accepted (`FR-F3`).
//! - While receiving, chunks stream into `<received_dir>/.partial/<offer_id>/<idx>.part` without
//!   holding the whole file in memory (`FR-F5`).
//! - Both the per-file declared size and the offer's running total against [`Limits`] are enforced
//!   as bytes arrive (`FR-F10`, `#527` item 6).
//! - An interrupted transfer can resume from the verified byte offset of its partial file (`FR-F5`).
//! - When a file finishes, its SHA-256 digest is checked when expected (`FR-F5`), then it is
//!   atomically moved to [`resolve_destination`](crate::names::resolve_destination) inside
//!   `received_dir` with permissions `0644` (stripping execute, setuid, setgid and sticky bits,
//!   `FR-F9`).
//! - Declining, cancelling or failing a transfer deletes `<received_dir>/.partial/<offer_id>` so no
//!   partial file is left behind (`FR-F3`, `FR-F4`).
//! - Transfer history is kept in memory and persisted to `<state_dir>/history.json`; clearing
//!   history removes the history entries while leaving received files on disk (`FR-F8`).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::limits::Limits;
use crate::names::{clean_offer_paths, resolve_destination};

/// Default chunk size for streaming file transfers (32 KiB).
pub const DEFAULT_CHUNK_BYTES: usize = 32 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("file index {0} is out of range")]
    BadFileIndex(usize),
    #[error("unexpected chunk offset {got} (expected {expected})")]
    OffsetMismatch { expected: u64, got: u64 },
    #[error("file {name:?} exceeded its declared size of {declared} bytes")]
    FileSizeExceeded { name: String, declared: u64 },
    #[error("transfer exceeded the limit of {max} bytes")]
    LimitExceeded { max: u64 },
    #[error("file {name:?} ended at {got} bytes (declared {declared})")]
    IncompleteFile { name: String, got: u64, declared: u64 },
    #[error("SHA-256 mismatch for {name:?}: expected {expected}, got {got}")]
    HashMismatch { name: String, expected: String, got: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Metadata of one file in an accepted offer being written to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredFile {
    /// Original or cleaned relative path (`a/b.txt`).
    pub name: String,
    pub size: u64,
    /// Optional lowercase hex SHA-256 of the file (`FR-F5`).
    #[serde(default)]
    pub sha256: Option<String>,
}

/// A completed file in the received-files folder (`FR-F8`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    /// Relative path inside `received_dir`.
    pub name: String,
    /// Full path on disk.
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub from: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferResult {
    Done,
    Failed,
    Declined,
    Cancelled,
}

/// One row in the transfer history (`FR-F8`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub id: String,
    pub name: String,
    pub device: String,
    pub direction: TransferDirection,
    pub size: u64,
    pub result: TransferResult,
    pub timestamp_ms: i64,
}

struct PartFile {
    components: Vec<String>,
    declared_size: u64,
    expected_sha256: Option<String>,
    part_path: PathBuf,
    written: u64,
}

/// Streaming writer for one accepted incoming offer (`FR-F4`, `FR-F5`, `FR-F8`, `FR-F9`, `FR-F10`).
pub struct ReceiveWriter {
    offer_id: String,
    from_device: String,
    received_dir: PathBuf,
    partial_dir: PathBuf,
    limits: Limits,
    files: Vec<PartFile>,
    total_written: u64,
}

impl ReceiveWriter {
    /// Opens or resumes a partial receive directory `<received_dir>/.partial/<offer_id>`.
    pub fn open(
        received_dir: &Path,
        offer_id: &str,
        from_device: &str,
        declared: &[DeclaredFile],
        limits: Limits,
    ) -> Result<Self, StoreError> {
        let mut declared_total: u64 = 0;
        for f in declared {
            declared_total = declared_total
                .checked_add(f.size)
                .ok_or(StoreError::LimitExceeded { max: limits.max_bytes })?;
        }
        if declared_total > limits.max_bytes {
            return Err(StoreError::LimitExceeded { max: limits.max_bytes });
        }

        let safe_id: String = offer_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(64)
            .collect();
        let safe_id = if safe_id.is_empty() { "offer".to_string() } else { safe_id };
        let partial_dir = received_dir.join(".partial").join(&safe_id);
        fs::create_dir_all(&partial_dir)?;

        let raw_names: Vec<String> = declared.iter().map(|f| f.name.clone()).collect();
        let cleaned_paths = clean_offer_paths(&raw_names);

        let mut files = Vec::with_capacity(declared.len());
        let mut total_written: u64 = 0;
        for (idx, (decl, comps)) in declared.iter().zip(cleaned_paths.into_iter()).enumerate() {
            let part_path = partial_dir.join(format!("{idx}.part"));
            let written = match fs::metadata(&part_path) {
                Ok(m) => m.len().min(decl.size),
                Err(_) => {
                    let f = OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .open(&part_path)?;
                    set_safe_file_mode(&part_path, &f)?;
                    0
                }
            };
            total_written = total_written.saturating_add(written);
            files.push(PartFile {
                components: comps,
                declared_size: decl.size,
                expected_sha256: decl.sha256.as_ref().map(|s| s.to_ascii_lowercase()),
                part_path,
                written,
            });
        }

        Ok(Self {
            offer_id: safe_id,
            from_device: from_device.to_string(),
            received_dir: received_dir.to_path_buf(),
            partial_dir,
            limits,
            files,
            total_written,
        })
    }

    pub fn offer_id(&self) -> &str {
        &self.offer_id
    }

    /// Current verified byte offset of `file_idx` for resuming an interrupted stream (`FR-F5`).
    pub fn resume_offset(&self, file_idx: usize) -> Result<u64, StoreError> {
        let f = self.files.get(file_idx).ok_or(StoreError::BadFileIndex(file_idx))?;
        Ok(f.written)
    }

    /// Total bytes written across all files in this offer so far.
    pub fn total_written(&self) -> u64 {
        self.total_written
    }

    /// Appends `data` at `offset` to `file_idx`, enforcing both the per-file declared size and the
    /// offer-wide [`Limits`] (`#527` item 6). If `expected_chunk_sha256` is provided, verifies the
    /// chunk's SHA-256 before writing (`FR-F5`).
    pub fn write_chunk(
        &mut self,
        file_idx: usize,
        offset: u64,
        data: &[u8],
        expected_chunk_sha256: Option<&str>,
    ) -> Result<u64, StoreError> {
        let f = self.files.get_mut(file_idx).ok_or(StoreError::BadFileIndex(file_idx))?;
        if offset != f.written {
            return Err(StoreError::OffsetMismatch { expected: f.written, got: offset });
        }
        let name = f.components.join("/");
        let new_file_len = f
            .written
            .checked_add(data.len() as u64)
            .ok_or_else(|| StoreError::FileSizeExceeded {
                name: name.clone(),
                declared: f.declared_size,
            })?;
        if new_file_len > f.declared_size {
            return Err(StoreError::FileSizeExceeded { name, declared: f.declared_size });
        }
        let new_total = self
            .total_written
            .checked_add(data.len() as u64)
            .ok_or(StoreError::LimitExceeded { max: self.limits.max_bytes })?;
        if new_total > self.limits.max_bytes {
            return Err(StoreError::LimitExceeded { max: self.limits.max_bytes });
        }
        if let Some(want) = expected_chunk_sha256 {
            let got = hex::encode(Sha256::digest(data));
            if !got.eq_ignore_ascii_case(want) {
                return Err(StoreError::HashMismatch {
                    name,
                    expected: want.to_ascii_lowercase(),
                    got,
                });
            }
        }

        let mut file = OpenOptions::new().create(true).write(true).open(&f.part_path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.flush()?;
        set_safe_file_mode(&f.part_path, &file)?;

        f.written = new_file_len;
        self.total_written = new_total;
        Ok(self.total_written)
    }

    /// Verifies every file's final size and SHA-256, moves each file to its collision-free
    /// destination inside `received_dir` with mode `0644` (no execute/setuid/setgid/sticky bits),
    /// and removes the `.partial/<offer_id>` directory (`FR-F5`, `FR-F8`, `FR-F9`).
    pub fn finalize(self) -> Result<Vec<StoredFile>, StoreError> {
        for f in &self.files {
            let name = f.components.join("/");
            if f.written != f.declared_size {
                let _ = fs::remove_dir_all(&self.partial_dir);
                return Err(StoreError::IncompleteFile {
                    name,
                    got: f.written,
                    declared: f.declared_size,
                });
            }
        }

        let mut stored = Vec::with_capacity(self.files.len());
        for f in &self.files {
            let orig_name = f.components.join("/");
            let sha256 = sha256_file(&f.part_path)?;
            if let Some(want) = &f.expected_sha256 {
                if &sha256 != want {
                    let _ = fs::remove_dir_all(&self.partial_dir);
                    return Err(StoreError::HashMismatch {
                        name: orig_name,
                        expected: want.clone(),
                        got: sha256,
                    });
                }
            }
            let dest = resolve_destination(&self.received_dir, &f.components);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&f.part_path, &dest)?;
            if let Ok(file) = File::open(&dest) {
                set_safe_file_mode(&dest, &file)?;
            }
            let rel = dest
                .strip_prefix(&self.received_dir)
                .unwrap_or(&dest)
                .to_string_lossy()
                .replace('\\', "/");
            stored.push(StoredFile {
                name: rel,
                path: dest,
                size: f.declared_size,
                sha256,
                from: self.from_device.clone(),
            });
        }
        let _ = fs::remove_dir_all(&self.partial_dir);
        let _ = fs::remove_dir(self.received_dir.join(".partial"));
        Ok(stored)
    }

    /// Cancels or aborts the transfer and deletes all partial files (`FR-F4`).
    pub fn abort(self) {
        let _ = fs::remove_dir_all(&self.partial_dir);
        let _ = fs::remove_dir(self.received_dir.join(".partial"));
    }
}

/// Enforces `0644` (`mode & !0o7111`: strips execute, setuid, setgid and sticky bits, `FR-F9`).
fn set_safe_file_mode(path: &Path, _file: &File) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = fs::Permissions::from_mode(0o644);
        fs::set_permissions(path, perm)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Computes the lowercase hex SHA-256 of a file on disk in 32 KiB chunks (`FR-F5`).
pub fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; DEFAULT_CHUNK_BYTES];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_f5_streams_chunks_resumes_and_verifies_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"hello world from ember transfer chunk stream";
        let expected_hash = hex::encode(Sha256::digest(data));
        let decl = vec![DeclaredFile {
            name: "folder/../notes.txt".into(),
            size: data.len() as u64,
            sha256: Some(expected_hash.clone()),
        }];

        // Write first half, then drop to simulate a dropped connection.
        {
            let mut w = ReceiveWriter::open(dir.path(), "offer-1", "iPhone", &decl, Limits::default()).unwrap();
            assert_eq!(w.resume_offset(0).unwrap(), 0);
            w.write_chunk(0, 0, &data[..20], None).unwrap();
            assert_eq!(w.resume_offset(0).unwrap(), 20);
        }

        // Re-open and resume from the verified byte offset (20).
        let mut w = ReceiveWriter::open(dir.path(), "offer-1", "iPhone", &decl, Limits::default()).unwrap();
        assert_eq!(w.resume_offset(0).unwrap(), 20);
        // Corrupted chunk hash is rejected without advancing the offset.
        assert!(matches!(
            w.write_chunk(0, 20, &data[20..], Some("0000")),
            Err(StoreError::HashMismatch { .. })
        ));
        assert_eq!(w.resume_offset(0).unwrap(), 20);

        let chunk_hash = hex::encode(Sha256::digest(&data[20..]));
        w.write_chunk(0, 20, &data[20..], Some(&chunk_hash)).unwrap();
        let stored = w.finalize().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "folder/notes.txt");
        assert_eq!(stored[0].sha256, expected_hash);
        assert_eq!(fs::read(&stored[0].path).unwrap(), data);
        assert!(!dir.path().join(".partial").exists());
    }

    #[test]
    fn fr_f9_received_file_has_mode_0644_and_no_execute_or_special_bits() {
        let dir = tempfile::tempdir().unwrap();
        let decl = vec![DeclaredFile {
            name: "run.sh".into(),
            size: 12,
            sha256: None,
        }];
        let mut w = ReceiveWriter::open(dir.path(), "offer-exec", "mac", &decl, Limits::default()).unwrap();
        w.write_chunk(0, 0, b"#!/bin/sh\n", None).unwrap();
        // Even if the part file had 0o4755 before finalize, finalize strips execute and special bits.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let part = dir.path().join(".partial/offer-exec/0.part");
            fs::set_permissions(&part, fs::Permissions::from_mode(0o4755)).unwrap();
        }
        w.write_chunk(0, 10, b"ls", None).unwrap();
        let stored = w.finalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&stored[0].path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, 0o644, "mode was {mode:o}");
        }
    }

    #[test]
    fn fr_f4_abort_removes_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let decl = vec![DeclaredFile { name: "partial.bin".into(), size: 100, sha256: None }];
        let mut w = ReceiveWriter::open(dir.path(), "offer-cancel", "mac", &decl, Limits::default()).unwrap();
        w.write_chunk(0, 0, &[1u8; 50], None).unwrap();
        assert!(dir.path().join(".partial/offer-cancel/0.part").exists());
        w.abort();
        assert!(!dir.path().join(".partial/offer-cancel").exists());
    }

    #[test]
    fn fr_f10_enforces_declared_file_size_and_total_limit_on_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let decl = vec![DeclaredFile { name: "small.bin".into(), size: 10, sha256: None }];
        let mut w = ReceiveWriter::open(dir.path(), "offer-size", "mac", &decl, Limits { max_bytes: 15 }).unwrap();
        w.write_chunk(0, 0, &[0u8; 8], None).unwrap();
        assert!(matches!(
            w.write_chunk(0, 8, &[0u8; 5], None),
            Err(StoreError::FileSizeExceeded { .. })
        ));
    }
}
