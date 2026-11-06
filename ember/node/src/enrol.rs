//! What a node keeps after it enrolled with an ember server (SPEC `FR-C2h`): `enrol.json` in the
//! state dir (the server-minted node token and where it came from), and the server's peer id
//! appended to `allowed-peers`.
//!
//! This module is storage only (N1 of the node enrol design): reading and validating the file,
//! and writing it together with `allowed-peers` so that both change or neither does. The
//! `ember-node enrol` command that fetches the credential is not built yet (N2).

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// `<state dir>/enrol.json`.
pub const ENROL_FILE: &str = "enrol.json";

/// The file is a few hundred bytes; anything larger is not ours.
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_TOKEN_CHARS: usize = 512;

/// The contents of `enrol.json`. `node_token` is a secret: `Debug` redacts it.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Enrolment {
    /// The server's base URL as the node used it.
    pub server: String,
    /// The bearer token the server uses against this node.
    pub node_token: String,
    /// The server's transport peer id (64 lowercase hex), when the server delivered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_peer_id: Option<String>,
    /// The computer name the owner chose.
    pub name: String,
    pub computer_id: String,
    pub enrolled_ms: i64,
}

impl fmt::Debug for Enrolment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Enrolment")
            .field("server", &self.server)
            .field("node_token", &"[redacted]")
            .field("server_peer_id", &self.server_peer_id)
            .field("name", &self.name)
            .field("computer_id", &self.computer_id)
            .field("enrolled_ms", &self.enrolled_ms)
            .finish()
    }
}

fn is_peer_hex(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Shape checks that never echo the value (the token is a secret).
fn validate(e: &Enrolment) -> anyhow::Result<()> {
    anyhow::ensure!(!e.node_token.is_empty(), "the node token is empty");
    anyhow::ensure!(e.node_token.chars().count() <= MAX_TOKEN_CHARS, "the node token is longer than {MAX_TOKEN_CHARS} characters");
    anyhow::ensure!(
        e.node_token.chars().all(|c| c.is_ascii_graphic()),
        "the node token must be printable ASCII without spaces"
    );
    if let Some(id) = &e.server_peer_id {
        anyhow::ensure!(is_peer_hex(id), "server_peer_id must be 64 lowercase hex characters");
    }
    Ok(())
}

/// Reads `<state dir>/enrol.json`. A missing file is `Ok(None)`. An oversized, malformed or
/// invalid file is an error that names the file and never quotes its contents. A file readable
/// by group or others only logs a warning (existing shared setups must keep working).
pub fn read_enrolment(state_dir: &Path) -> anyhow::Result<Option<Enrolment>> {
    let path = state_dir.join(ENROL_FILE);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot open {}", path.display()))),
    };
    let meta = file.metadata().with_context(|| format!("cannot stat {}", path.display()))?;
    anyhow::ensure!(meta.len() <= MAX_FILE_BYTES, "{} is larger than {} bytes", path.display(), MAX_FILE_BYTES);
    if meta.permissions().mode() & 0o077 != 0 {
        tracing::warn!(path = %path.display(), "enrol file is accessible to group or others; run chmod 600 on it");
    }
    let mut data = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut data).with_context(|| format!("cannot read {}", path.display()))?;
    anyhow::ensure!(data.len() as u64 <= MAX_FILE_BYTES, "{} is larger than {} bytes", path.display(), MAX_FILE_BYTES);
    // The serde error is dropped on purpose: for a wrong type it can quote the value.
    let e: Enrolment = serde_json::from_slice(&data).map_err(|err| {
        anyhow::anyhow!("{} is malformed (not a valid enrolment file; line {}, column {})", path.display(), err.line(), err.column())
    })?;
    validate(&e).map_err(|err| anyhow::anyhow!("{} is invalid: {err}", path.display()))?;
    Ok(Some(e))
}

/// The new text of an `allowed-peers` file after adding `id`: `existing` byte for byte, then the
/// id on its own line. `Ok(None)` when `id` is already listed. An `existing` text that does not
/// parse is an error: what cannot be read is never overwritten.
pub fn append_allowed_peer(existing: &str, id: &str) -> anyhow::Result<Option<String>> {
    anyhow::ensure!(is_peer_hex(id), "a peer id is 64 lowercase hex characters");
    let new: ember_transport::PeerId = id.parse().map_err(|e| anyhow::anyhow!("peer id: {e}"))?;
    let listed = crate::transport::parse_peer_list(existing)?;
    if listed.contains(&new) {
        return Ok(None);
    }
    let mut text = String::with_capacity(existing.len() + 66);
    text.push_str(existing);
    if !existing.is_empty() && !existing.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(id);
    text.push('\n');
    Ok(Some(text))
}

/// Files made for one write, before any of them is renamed into place.
struct Staged {
    enrol_tmp: PathBuf,
    enrol_dst: PathBuf,
    /// The `allowed-peers` temp and its destination, when that file changes.
    peers: Option<(PathBuf, PathBuf)>,
    /// Where the previous `enrol.json` is kept during the swap.
    backup: PathBuf,
}

impl Staged {
    fn remove_temps(&self) {
        let _ = fs::remove_file(&self.enrol_tmp);
        if let Some((tmp, _)) = &self.peers {
            let _ = fs::remove_file(tmp);
        }
    }
}

/// 16 hex characters, unique per call: a v4 UUID (uuid is already a dependency).
fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

fn temp_name(dir: &Path, name: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{name}.{suffix}.tmp"))
}

/// Creates `<dir>/<name>.<suffix>.tmp` with `create_new` (O_EXCL: an existing file or a planted
/// symlink at that name is an error, never followed), writes `bytes`, sets `mode`, syncs.
fn stage_file(dir: &Path, name: &str, suffix: &str, bytes: &[u8], mode: u32) -> io::Result<PathBuf> {
    let path = temp_name(dir, name, suffix);
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    let result = (|| {
        f.write_all(bytes)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.sync_all()
    })();
    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            drop(f);
            let _ = fs::remove_file(&path);
            Err(e)
        }
    }
}

/// [`stage_file`] with a fresh suffix, tried up to 5 times when the name is taken.
fn stage_retry(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> anyhow::Result<PathBuf> {
    let mut last = None;
    for _ in 0..5 {
        match stage_file(dir, name, &suffix(), bytes, mode) {
            Ok(p) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot stage {name} in {}", dir.display()))),
        }
    }
    let e = last.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists));
    Err(anyhow::Error::new(e).context(format!("cannot find a free temporary name for {name} in {}", dir.display())))
}

/// Renames the staged files into place, `enrol.json` first. The previous `enrol.json` is kept as
/// the backup during the swap. If a rename fails, the previous state is put back (with the real
/// `fs::rename`, not the injected one) and the error is returned; on success the backup is deleted.
fn commit(stage: &Staged, rename: &dyn Fn(&Path, &Path) -> io::Result<()>) -> anyhow::Result<()> {
    let had_previous = fs::symlink_metadata(&stage.enrol_dst).is_ok();
    if had_previous {
        rename(&stage.enrol_dst, &stage.backup).context("cannot move the previous enrol.json aside")?;
    }
    let restore = |cause: anyhow::Error| -> anyhow::Error {
        let restored = if had_previous {
            fs::rename(&stage.backup, &stage.enrol_dst)
        } else {
            match fs::remove_file(&stage.enrol_dst) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        };
        match restored {
            Ok(()) => cause,
            // Keep the backup: it is the only copy of the previous file.
            Err(e) => cause.context(format!(
                "and the previous enrol.json could not be restored ({e}); it is kept as {}",
                stage.backup.display()
            )),
        }
    };
    if let Err(e) = rename(&stage.enrol_tmp, &stage.enrol_dst) {
        return Err(restore(anyhow::Error::new(e).context("cannot move enrol.json into place")));
    }
    if let Some((tmp, dst)) = &stage.peers {
        if let Err(e) = rename(tmp, dst) {
            return Err(restore(anyhow::Error::new(e).context("cannot update allowed-peers")));
        }
    }
    if had_previous {
        let _ = fs::remove_file(&stage.backup);
    }
    Ok(())
}

/// Writes `enrol.json` and, with `allow_peer`, adds that peer id to `allowed-peers`, so that both
/// change or neither does (SPEC `FR-C2h`).
///
/// An existing `enrol.json` is an error unless `replace`. Both files are staged first (mode 0600,
/// `create_new`, synced), then `enrol.json` is renamed into place, then `allowed-peers`. If the
/// second rename fails, the previous `enrol.json` is restored (or the new one removed). An
/// `enrol.json` without the peer is harmless (the HTTP path works); a peer without a token is
/// not, hence the order. `allowed-peers` keeps its text byte for byte and its mode.
pub fn write_enrolment(state_dir: &Path, e: &Enrolment, allow_peer: Option<&str>, replace: bool) -> anyhow::Result<()> {
    write_with(state_dir, e, allow_peer, replace, &|from, to| fs::rename(from, to))
}

fn write_with(
    state_dir: &Path,
    e: &Enrolment,
    allow_peer: Option<&str>,
    replace: bool,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> anyhow::Result<()> {
    validate(e)?;
    let enrol_dst = state_dir.join(ENROL_FILE);
    if !replace && fs::symlink_metadata(&enrol_dst).is_ok() {
        anyhow::bail!("{} already exists; pass --replace to replace it", enrol_dst.display());
    }

    // The new allowed-peers text, decided before anything is created.
    let peers_dst = crate::transport::allowed_peers_file(state_dir);
    let mut peers_new: Option<(String, u32)> = None;
    if let Some(id) = allow_peer {
        let (existing, mode) = match fs::read_to_string(&peers_dst) {
            Ok(text) => {
                let mode = fs::metadata(&peers_dst).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
                (text, mode)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => (String::new(), 0o600),
            Err(err) => return Err(anyhow::Error::new(err).context(format!("cannot read {}", peers_dst.display()))),
        };
        if let Some(text) = append_allowed_peer(&existing, id)? {
            peers_new = Some((text, mode));
        }
    }

    if !state_dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_dir)
            .with_context(|| format!("cannot create {}", state_dir.display()))?;
    }

    let json = serde_json::to_vec_pretty(e).context("cannot encode enrol.json")?;
    let enrol_tmp = stage_retry(state_dir, ENROL_FILE, &json, 0o600)?;
    let peers = match peers_new {
        None => None,
        Some((text, mode)) => match stage_retry(state_dir, crate::transport::ALLOWED_PEERS_FILE, text.as_bytes(), mode) {
            Ok(tmp) => Some((tmp, peers_dst)),
            Err(err) => {
                let _ = fs::remove_file(&enrol_tmp);
                return Err(err);
            }
        },
    };
    let stage = Staged { enrol_tmp, enrol_dst, peers, backup: temp_name(state_dir, ENROL_FILE, &format!("bak.{}", suffix())) };
    let result = commit(&stage, rename);
    stage.remove_temps();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn peer() -> String {
        ember_transport::SecretKey::generate().peer_id().to_string()
    }

    fn sample() -> Enrolment {
        Enrolment {
            server: "https://ember.example".into(),
            node_token: "tok_SECRET-value.123".into(),
            server_peer_id: None,
            name: "build box".into(),
            computer_id: "c1".into(),
            enrolled_ms: 1_700_000_000_000,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn fr_c2_node_read_missing_file_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_enrolment(d.path()).unwrap().is_none());
    }

    #[test]
    fn fr_c2_node_read_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let mut e = sample();
        e.server_peer_id = Some(peer());
        write_enrolment(d.path(), &e, None, false).unwrap();
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(e));
        let e2 = sample();
        write_enrolment(d.path(), &e2, None, true).unwrap();
        let text = fs::read_to_string(d.path().join(ENROL_FILE)).unwrap();
        assert!(!text.contains("server_peer_id"), "an absent peer id is not serialized");
    }

    #[test]
    fn fr_c2_node_read_malformed_names_the_file_and_never_quotes_it() {
        let d = tempfile::tempdir().unwrap();
        let body = r#"{"server": 12, "node_token": "tok_SECRET-value.123", "name": "x" "#;
        fs::write(d.path().join(ENROL_FILE), body).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("enrol.json") && err.contains("malformed"), "{err}");
        assert!(!err.contains("tok_SECRET"), "{err}");
        // A well-formed document of the wrong type must not quote the value either.
        let body = r#"{"server":"s","node_token":"tok_SECRET-value.123","name":"n","computer_id":"c","enrolled_ms":"tok_SECRET-value.123"}"#;
        fs::write(d.path().join(ENROL_FILE), body).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("malformed") && !err.contains("tok_SECRET"), "{err}");
    }

    #[test]
    fn fr_c2_node_read_oversized_file_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(ENROL_FILE), vec![b' '; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("larger than"), "{err}");
    }

    #[test]
    fn fr_c2_node_read_rejects_bad_tokens() {
        let d = tempfile::tempdir().unwrap();
        for bad in ["", "has space", "tab\there", "caf\u{e9}", "a".repeat(MAX_TOKEN_CHARS + 1).as_str()] {
            let mut e = sample();
            e.node_token = bad.to_string();
            fs::write(d.path().join(ENROL_FILE), serde_json::to_vec(&e).unwrap()).unwrap();
            let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
            assert!(err.contains("token") && err.contains("enrol.json"), "{err}");
            if !bad.is_empty() {
                assert!(!err.contains(bad), "the token is never echoed");
            }
        }
        let mut ok = sample();
        ok.node_token = "a".repeat(MAX_TOKEN_CHARS);
        fs::write(d.path().join(ENROL_FILE), serde_json::to_vec(&ok).unwrap()).unwrap();
        assert!(read_enrolment(d.path()).unwrap().is_some());
    }

    #[test]
    fn fr_c2_node_read_rejects_bad_server_peer_id() {
        let d = tempfile::tempdir().unwrap();
        for bad in ["abc", "A".repeat(64).as_str(), "g".repeat(64).as_str(), "a".repeat(63).as_str()] {
            let mut e = sample();
            e.server_peer_id = Some(bad.to_string());
            fs::write(d.path().join(ENROL_FILE), serde_json::to_vec(&e).unwrap()).unwrap();
            let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
            assert!(err.contains("server_peer_id"), "{err}");
        }
    }

    #[test]
    fn fr_c2_node_debug_redacts_the_token() {
        let shown = format!("{:?} {:#?}", sample(), sample());
        assert!(!shown.contains("tok_SECRET"), "{shown}");
        assert!(shown.contains("redacted") && shown.contains("build box"));
    }

    #[test]
    fn fr_c2_node_append_keeps_text_and_adds_its_own_line() {
        let a = peer();
        let b = peer();
        let existing = format!("# my servers\n{a}  # home\n\n");
        let out = append_allowed_peer(&existing, &b).unwrap().unwrap();
        assert_eq!(out, format!("{existing}{b}\n"));
        assert!(out.starts_with(&existing), "existing text is kept byte for byte");
    }

    #[test]
    fn fr_c2_node_append_adds_a_newline_when_the_text_has_none() {
        let a = peer();
        let b = peer();
        let out = append_allowed_peer(&a, &b).unwrap().unwrap();
        assert_eq!(out, format!("{a}\n{b}\n"));
        assert_eq!(append_allowed_peer("", &b).unwrap().unwrap(), format!("{b}\n"));
    }

    #[test]
    fn fr_c2_node_append_does_not_duplicate() {
        let a = peer();
        assert!(append_allowed_peer(&format!("{a}\n"), &a).unwrap().is_none());
        assert!(append_allowed_peer(&format!("x\n{a}"), &a).is_err(), "unparsable text is an error before anything else");
        let b = peer();
        assert!(append_allowed_peer(&format!("{a}, {b} # both\n"), &b).unwrap().is_none());
    }

    #[test]
    fn fr_c2_node_append_refuses_unparsable_existing_text_and_bad_ids() {
        let a = peer();
        assert!(append_allowed_peer("not-a-key\n", &a).is_err());
        for bad in ["", "abc", a.to_uppercase().as_str(), format!("{a}0").as_str()] {
            assert!(append_allowed_peer("", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn fr_c2_node_write_makes_enrol_json_0600_and_no_allowed_peers_without_a_peer() {
        let d = tempfile::tempdir().unwrap();
        write_enrolment(d.path(), &sample(), None, false).unwrap();
        assert_eq!(mode(&d.path().join(ENROL_FILE)), 0o600);
        assert!(!crate::transport::allowed_peers_file(d.path()).exists());
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()], "no temp files remain");
    }

    #[test]
    fn fr_c2_node_write_creates_a_missing_state_dir_0700() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("a").join("node");
        write_enrolment(&dir, &sample(), None, false).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert!(dir.join(ENROL_FILE).exists());
    }

    #[test]
    fn fr_c2_node_write_adds_exactly_the_one_peer_and_keeps_the_old_text_and_mode() {
        let d = tempfile::tempdir().unwrap();
        let old = peer();
        let new = peer();
        let path = crate::transport::allowed_peers_file(d.path());
        fs::write(&path, format!("# keep\n{old}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        write_enrolment(d.path(), &sample(), Some(&new), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("# keep\n{old}\n{new}\n"));
        assert_eq!(mode(&path), 0o640, "the mode of an existing file is kept");
        assert_eq!(names(d.path()), vec!["allowed-peers".to_string(), ENROL_FILE.to_string()]);
    }

    #[test]
    fn fr_c2_node_write_creates_allowed_peers_0600_with_only_the_new_id() {
        let d = tempfile::tempdir().unwrap();
        let new = peer();
        write_enrolment(d.path(), &sample(), Some(&new), false).unwrap();
        let path = crate::transport::allowed_peers_file(d.path());
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{new}\n"));
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn fr_c2_node_write_leaves_allowed_peers_alone_when_the_peer_is_listed() {
        let d = tempfile::tempdir().unwrap();
        let a = peer();
        let path = crate::transport::allowed_peers_file(d.path());
        fs::write(&path, format!("{a}\n")).unwrap();
        write_enrolment(d.path(), &sample(), Some(&a), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{a}\n"));
        assert_eq!(names(d.path()).len(), 2);
    }

    #[test]
    fn fr_c2_node_write_refuses_an_existing_file_without_replace_and_replaces_with_it() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(ENROL_FILE);
        fs::write(&path, b"old bytes").unwrap();
        let err = write_enrolment(d.path(), &sample(), None, false).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), b"old bytes");
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()]);
        write_enrolment(d.path(), &sample(), None, true).unwrap();
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(sample()));
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()], "the backup is deleted");
    }

    #[test]
    fn fr_c2_node_write_refuses_an_invalid_token_and_a_bad_peer_without_leftovers() {
        let d = tempfile::tempdir().unwrap();
        let mut e = sample();
        e.node_token = "has space".into();
        assert!(write_enrolment(d.path(), &e, None, false).is_err());
        assert!(write_enrolment(d.path(), &sample(), Some("nope"), false).is_err());
        assert!(names(d.path()).is_empty());
    }

    #[test]
    fn fr_c2_node_write_never_follows_a_planted_name_create_new() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("victim");
        fs::write(&target, b"victim").unwrap();
        std::os::unix::fs::symlink(&target, temp_name(d.path(), "enrol.json", "0123456789abcdef")).unwrap();
        let err = stage_file(d.path(), "enrol.json", "0123456789abcdef", b"new", 0o600).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"victim", "the symlink target is untouched");
        // A fresh suffix works, and a second call with the same one fails.
        stage_file(d.path(), "enrol.json", "fedcba9876543210", b"new", 0o600).unwrap();
        assert!(stage_file(d.path(), "enrol.json", "fedcba9876543210", b"again", 0o600).is_err());
    }

    #[test]
    fn fr_c2_node_write_rolls_back_when_the_second_rename_fails() {
        let d = tempfile::tempdir().unwrap();
        let old = peer();
        let new = peer();
        let enrol = d.path().join(ENROL_FILE);
        let peers = crate::transport::allowed_peers_file(d.path());
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        fs::write(&peers, format!("{old}\n")).unwrap();
        let before = names(d.path());
        let calls = Cell::new(0);
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            calls.set(calls.get() + 1);
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        let err = write_with(d.path(), &sample(), Some(&new), true, &failing).unwrap_err();
        assert!(format!("{err:#}").contains("allowed-peers"), "{err:#}");
        assert_eq!(calls.get(), 3, "backup, enrol.json, allowed-peers");
        assert_eq!(fs::read(&enrol).unwrap(), b"previous enrol bytes");
        assert_eq!(fs::read_to_string(&peers).unwrap(), format!("{old}\n"));
        assert_eq!(names(d.path()), before, "no temp or backup files remain");
    }

    #[test]
    fn fr_c2_node_write_rollback_removes_the_new_file_when_there_was_none() {
        let d = tempfile::tempdir().unwrap();
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        assert!(write_with(d.path(), &sample(), Some(&peer()), false, &failing).is_err());
        assert!(names(d.path()).is_empty(), "{:?}", names(d.path()));
    }

    #[test]
    fn fr_c2_node_write_rollback_when_the_first_rename_fails_keeps_the_previous_file() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            if from.to_string_lossy().ends_with(".tmp") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        assert!(write_with(d.path(), &sample(), None, true, &failing).is_err());
        assert_eq!(fs::read(&enrol).unwrap(), b"previous enrol bytes");
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()]);
    }
}
