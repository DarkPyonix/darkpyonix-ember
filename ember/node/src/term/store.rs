//! Session metadata on disk: `<state dir>/terms/<id>.json` (one [`TermRecord`] per session),
//! `<id>.snap` (a screen snapshot written on a graceful node shutdown) and the keepers' unix
//! sockets `<id prefix>.sock`. The directory is created `0700`.

use std::io;
use std::path::{Path, PathBuf};

use super::session::TermRecord;

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(state_dir: &Path) -> io::Result<Self> {
        let dir = state_dir.join("terms");
        std::fs::create_dir_all(&dir)?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { dir })
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn snapshot_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.snap"))
    }

    /// Short, because unix socket paths are limited to ~104 bytes.
    pub fn socket_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.sock", &id[..12.min(id.len())]))
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }

    pub fn save(&self, rec: &TermRecord) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(rec).map_err(io::Error::other)?;
        self.write_atomic(&self.record_path(&rec.id), &json)
    }

    /// Every readable record; unreadable files are skipped with a warning.
    pub fn load_all(&self) -> Vec<TermRecord> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<TermRecord>(&b).map_err(|e| e.to_string())) {
                Ok(r) => out.push(r),
                Err(e) => tracing::warn!(path = %p.display(), "skipping unreadable terminal record: {e}"),
            }
        }
        out.sort_by_key(|r| r.created_ms);
        out
    }

    pub fn remove(&self, id: &str) {
        let _ = std::fs::remove_file(self.record_path(id));
        let _ = std::fs::remove_file(self.snapshot_path(id));
    }

    pub fn save_snapshot(&self, id: &str, data: &[u8]) -> io::Result<()> {
        self.write_atomic(&self.snapshot_path(id), data)
    }

    /// Read and delete a shutdown snapshot.
    pub fn take_snapshot(&self, id: &str) -> Option<Vec<u8>> {
        let p = self.snapshot_path(id);
        let data = std::fs::read(&p).ok();
        let _ = std::fs::remove_file(&p);
        data
    }
}
