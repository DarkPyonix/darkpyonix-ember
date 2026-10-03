//! Project mount for Claude Code's file tools — **design stub, not implemented**.
//!
//! Status: Claude Code's Read, Edit, Write, Glob and Grep run inside the `claude` process (Grep
//! via its embedded ripgrep) against **this server's** disk. Until this module is implemented,
//! a Claude Code session on another computer runs only its Bash tool there (through
//! `ember-exec`); its file tools are local-only, and the project directory must exist on the
//! server at the same absolute path (the adapter refuses to start otherwise). Codex sessions are
//! not affected: their file changes go through the node's exec-server.
//!
//! # Design (`docs/design/INTERCEPTION.md`, option (a))
//!
//! Mount each of the node's project roots on the server **at the same absolute path**, so paths
//! the agent sees are the node's (FR-X2), backed by the ember node file API:
//!
//! | Server OS | Mechanism | Notes |
//! |-|-|-|
//! | Linux (e.g. Raspberry Pi) | FUSE (`fuser` crate) in-process | `allow_other` not needed: the agent runs as the same user |
//! | macOS | loopback NFSv3 server in ember server + `mount_nfs -o soft,intr,timeo=…,locallocks 127.0.0.1:/… <path>` | no kext; `mount_nfs` ships with macOS; paths like `/home/x` need `synthetic.conf` |
//!
//! Operation mapping (node API, `node/src/api.rs`):
//!
//! - lookup / getattr → `POST /v1/fs/stat`; readdir → `POST /v1/fs/list`
//! - read → `POST /v1/fs/read` (range + whole-file SHA-256, the FR-S7 observation key)
//! - write / create / truncate → `POST /v1/fs/write` with `expect` (hash precondition), buffered
//!   per open file and flushed on close/fsync
//! - **missing on the node, needed first**: rename, remove, mkdir, symlink/readlink, chmod/
//!   setattr, a batched stat+readdir, and a change feed to invalidate the attribute cache.
//!
//! Failure handling: a hung mount must never wedge the agent in uninterruptible I/O — soft mounts
//! with short timeouts (NFS) or `EIO` after a deadline (FUSE); unmount and remount on every
//! computer switch, before the agent is restarted.
//!
//! Latency budget: one round trip per uncached syscall; measure on the Pi before committing
//! (INTERCEPTION.md recommendation 5). Prefetching the tree by content hash is the fallback.

use std::path::{Path, PathBuf};

/// Mounts a node's project roots on this server. Not implemented: see the module docs.
pub trait ProjectMount: Send + Sync {
    /// Mount `roots` of the node at the same absolute paths here.
    fn mount(&self, node_url: &str, token: &str, roots: &[PathBuf]) -> anyhow::Result<()>;
    /// Unmount everything this mount owns (on a switch away, or shutdown).
    fn unmount(&self) -> anyhow::Result<()>;
    /// Whether `path` is currently served from the node.
    fn covers(&self, path: &Path) -> bool;
}

/// The only implementation today: nothing is mounted, so file tools stay local.
pub struct NoMount;

impl ProjectMount for NoMount {
    fn mount(&self, _node_url: &str, _token: &str, _roots: &[PathBuf]) -> anyhow::Result<()> {
        anyhow::bail!("project mount is not implemented; Claude Code file tools are local-only")
    }

    fn unmount(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn covers(&self, _path: &Path) -> bool {
        false
    }
}
