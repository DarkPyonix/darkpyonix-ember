//! File watching over a WebSocket, `GET /fs/watch` (SPEC `FR-W4d`, #460).
//!
//! The server relays this socket to its clients (the 파일 tree, VCS status, external-change
//! prompts). The editor does not use it: the OSE on each node watches its own files
//! (`INTENT.md` D42).
//!
//! Protocol, JSON text frames:
//!
//! | Direction | Frame |
//! | --------- | ----- |
//! | client | `{"op":"watch","id":n,"path":"/abs","recursive":true}` |
//! | client | `{"op":"unwatch","id":n}` |
//! | node | `{"op":"watching","id":n,"path":"/canonical"}` |
//! | node | `{"op":"unwatched","id":n}` |
//! | node | `{"op":"error","id":n,"code":"...","message":"..."}` |
//! | node | `{"op":"change","id":n,"events":[{"kind":"created","path":"..."}]}` |
//! | node | `{"op":"overflow","id":n}`: events were lost, read the tree again |
//!
//! Every watched path passes [`PathPolicy::resolve_existing`]; events for a path outside the
//! roots or in a denied entry are dropped, and so are `.git` internals other than what SCM needs
//! ([`git_ignored`]). Events are collected for [`WatchLimits::debounce`] from the first one and
//! merged per path ([`merge`]).
//!
//! One native watcher per socket (and one polling watcher when a watched folder is on a network
//! share, [`crate::fs_kind`]); a path watched twice is registered once.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use notify::event::{ModifyKind, RenameMode};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::api::Node;
use crate::fs_kind::{fs_kind, FsKind};
use crate::policy::{PathPolicy, PolicyError};

/// Bounds of the watch sockets. The defaults are the SPEC's.
#[derive(Debug, Clone, Copy)]
pub struct WatchLimits {
    /// Watches one socket may hold.
    pub per_socket: usize,
    /// Watches all sockets of this node may hold together.
    pub total: usize,
    /// Events one `change` frame of one watch may carry; more is `overflow`.
    pub batch_events: usize,
    /// How long events are collected from the first one before they are sent.
    pub debounce: Duration,
    /// Raw OS events queued between the watcher thread and the socket; a full queue is
    /// `overflow` for every watch of the socket.
    pub queue: usize,
    /// Interval of the polling watcher used on network shares.
    pub poll_interval: Duration,
}

impl Default for WatchLimits {
    fn default() -> Self {
        Self {
            per_socket: 64,
            total: 1024,
            batch_events: 1000,
            debounce: Duration::from_millis(100),
            queue: 4096,
            poll_interval: Duration::from_secs(2),
        }
    }
}

/// What a client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WatchRequest {
    Watch {
        id: u64,
        path: PathBuf,
        #[serde(default)]
        recursive: bool,
    },
    Unwatch {
        id: u64,
    },
}

/// What the node sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WatchReply {
    Watching { id: u64, path: PathBuf },
    Unwatched { id: u64 },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        code: String,
        message: String,
    },
    Change { id: u64, events: Vec<WatchEvent> },
    Overflow { id: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Changed,
    Removed,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchEvent {
    pub kind: ChangeKind,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PathBuf>,
}

/// The files inside a `.git` directory that still produce events: what `git status` and the
/// branch display read. Everything under `refs/` is kept too.
pub const GIT_KEPT: [&str; 4] = ["HEAD", "index", "packed-refs", "MERGE_HEAD"];

/// Whether `path` is a `.git` internal that is dropped. A path with no `.git` component, the
/// `.git` entry itself, [`GIT_KEPT`] files directly in it and anything below `.git/refs` are kept.
pub fn git_ignored(path: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    let Some(at) = parts.iter().rposition(|p| *p == ".git") else { return false };
    let rest = &parts[at + 1..];
    match rest {
        [] => false,
        [first, ..] if *first == "refs" => false,
        [only] => !GIT_KEPT.iter().any(|k| *only == *k),
        _ => true,
    }
}

/// One path's pending change merged with a newer one: created then changed stays created,
/// removed then created is changed (the file is there again, with new content), otherwise the
/// newer kind wins.
pub fn merge(old: ChangeKind, new: ChangeKind) -> ChangeKind {
    match (old, new) {
        (ChangeKind::Created, ChangeKind::Changed) => ChangeKind::Created,
        (ChangeKind::Removed, ChangeKind::Created) => ChangeKind::Changed,
        (_, new) => new,
    }
}

/// The events of one watch collected for one batch, in first-seen order, one entry per path.
#[derive(Debug, Default)]
pub struct Batch {
    order: Vec<WatchEvent>,
    index: HashMap<PathBuf, usize>,
}

impl Batch {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Add a created, changed or removed event of `path`, merged with a pending one.
    pub fn push(&mut self, kind: ChangeKind, path: PathBuf) {
        if let Some(&i) = self.index.get(&path) {
            let e = &mut self.order[i];
            if e.kind != ChangeKind::Renamed {
                e.kind = merge(e.kind, kind);
                return;
            }
        }
        self.index.insert(path.clone(), self.order.len());
        self.order.push(WatchEvent { kind, path, to: None });
    }

    /// Add a paired rename. A pending `removed` of `from` and `created` of `to` (the halves some
    /// platforms report before the pair) are folded into it.
    pub fn rename(&mut self, from: PathBuf, to: PathBuf) {
        let half = |b: &Batch, p: &Path, k: ChangeKind| b.index.get(p).is_some_and(|&i| b.order[i].kind == k);
        let drop_from = half(self, &from, ChangeKind::Removed);
        let drop_to = half(self, &to, ChangeKind::Created);
        if drop_from || drop_to {
            self.order.retain(|e| {
                !((drop_from && e.path == from && e.kind == ChangeKind::Removed)
                    || (drop_to && e.path == to && e.kind == ChangeKind::Created))
            });
            self.reindex();
        }
        self.order.push(WatchEvent { kind: ChangeKind::Renamed, path: from, to: Some(to) });
    }

    fn reindex(&mut self) {
        self.index = self.order.iter().enumerate().filter(|(_, e)| e.kind != ChangeKind::Renamed).map(|(i, e)| (e.path.clone(), i)).collect();
    }

    pub fn take(&mut self) -> Vec<WatchEvent> {
        self.index.clear();
        std::mem::take(&mut self.order)
    }
}

/// One OS change, already mapped from a `notify` event.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Raw {
    One(ChangeKind, PathBuf),
    Rename(PathBuf, PathBuf),
}

/// Map a `notify` event. Access events are dropped. A rename the platform reports per path
/// (FSEvents: `Modify(Name(Any))`) is `created` when the path exists and `removed` when not.
fn map_event(ev: &Event) -> Vec<Raw> {
    let each = |k: ChangeKind| ev.paths.iter().map(|p| Raw::One(k, p.clone())).collect();
    match &ev.kind {
        EventKind::Access(_) => Vec::new(),
        EventKind::Create(_) => each(ChangeKind::Created),
        EventKind::Remove(_) => each(ChangeKind::Removed),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
            vec![Raw::Rename(ev.paths[0].clone(), ev.paths[1].clone())]
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => each(ChangeKind::Removed),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => each(ChangeKind::Created),
        EventKind::Modify(ModifyKind::Name(_)) => ev
            .paths
            .iter()
            .map(|p| {
                let k = if std::fs::symlink_metadata(p).is_ok() { ChangeKind::Created } else { ChangeKind::Removed };
                Raw::One(k, p.clone())
            })
            .collect(),
        _ => each(ChangeKind::Changed),
    }
}

/// Whether an event path may be reported: inside a root, not denied, not a dropped `.git`
/// internal. Event paths are not canonicalized (a removed file has none); they are spelled below
/// the canonical watched path, and the watcher does not follow symlinks.
fn reportable(policy: &PathPolicy, path: &Path) -> bool {
    policy.roots().iter().any(|r| path.starts_with(r)) && !policy.is_denied(path) && !git_ignored(path)
}

struct WatchEntry {
    root: PathBuf,
    recursive: bool,
    poll: bool,
}

impl WatchEntry {
    fn covers(&self, path: &Path) -> bool {
        path == self.root || (if self.recursive { path.starts_with(&self.root) } else { path.parent() == Some(&self.root) })
    }
}

/// OS registrations of one socket: a path is registered once, recursive when any watch of it is.
#[derive(Default)]
struct Registrations {
    native: Option<RecommendedWatcher>,
    poll: Option<PollWatcher>,
    /// (poll, path) -> (registered recursively, watches using it)
    paths: HashMap<(bool, PathBuf), (bool, usize)>,
}

type RawTx = mpsc::Sender<notify::Result<Event>>;

impl Registrations {
    fn handler(tx: &RawTx, overflow: &Arc<AtomicBool>) -> impl FnMut(notify::Result<Event>) + Send + 'static {
        let (tx, overflow) = (tx.clone(), overflow.clone());
        move |res| {
            if tx.try_send(res).is_err() {
                overflow.store(true, Ordering::SeqCst);
            }
        }
    }

    fn add(&mut self, e: &WatchEntry, tx: &RawTx, overflow: &Arc<AtomicBool>, limits: &WatchLimits) -> notify::Result<()> {
        let key = (e.poll, e.root.clone());
        let (registered_recursive, users) = self.paths.get(&key).copied().unwrap_or((false, 0));
        let need = users == 0 || (e.recursive && !registered_recursive);
        if need {
            let mode = if e.recursive || registered_recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
            let config = Config::default().with_follow_symlinks(false);
            if e.poll {
                if self.poll.is_none() {
                    self.poll = Some(PollWatcher::new(Self::handler(tx, overflow), config.with_poll_interval(limits.poll_interval))?);
                }
                let w = self.poll.as_mut().expect("just set");
                if users > 0 {
                    let _ = w.unwatch(&e.root);
                }
                w.watch(&e.root, mode)?;
            } else {
                if self.native.is_none() {
                    self.native = Some(RecommendedWatcher::new(Self::handler(tx, overflow), config)?);
                }
                let w = self.native.as_mut().expect("just set");
                if users > 0 {
                    let _ = w.unwatch(&e.root);
                }
                w.watch(&e.root, mode)?;
            }
        }
        self.paths.insert(key, (registered_recursive || e.recursive, users + 1));
        Ok(())
    }

    fn remove(&mut self, e: &WatchEntry) {
        let key = (e.poll, e.root.clone());
        let Some((rec, users)) = self.paths.get(&key).copied() else { return };
        if users > 1 {
            self.paths.insert(key, (rec, users - 1));
            return;
        }
        self.paths.remove(&key);
        let _ = if e.poll {
            self.poll.as_mut().map(|w| w.unwatch(&e.root))
        } else {
            self.native.as_mut().map(|w| w.unwatch(&e.root))
        };
    }
}

/// Gives the node-wide watch count back when a socket ends.
struct TotalGuard {
    total: Arc<AtomicUsize>,
    held: usize,
}

impl TotalGuard {
    fn take(&mut self, cap: usize) -> bool {
        let ok = self
            .total
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < cap).then_some(n + 1))
            .is_ok();
        if ok {
            self.held += 1;
        }
        ok
    }

    fn give(&mut self) {
        if self.held > 0 {
            self.held -= 1;
            self.total.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for TotalGuard {
    fn drop(&mut self) {
        self.total.fetch_sub(self.held, Ordering::SeqCst);
    }
}

/// `GET /fs/watch` (behind the node's bearer token).
pub(crate) async fn ws(State(node): State<Node>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(64 * 1024).on_upgrade(move |socket| session(node, socket))
}

fn error(id: Option<u64>, code: &str, message: impl Into<String>) -> WatchReply {
    WatchReply::Error { id, code: code.into(), message: message.into() }
}

fn policy_error(id: u64, e: PolicyError) -> WatchReply {
    match &e {
        PolicyError::NotAbsolute(_) => error(Some(id), "bad_request", e.to_string()),
        PolicyError::Outside(_) => error(Some(id), "forbidden_path", e.to_string()),
        PolicyError::Io(_, io) if io.kind() == std::io::ErrorKind::NotFound => error(Some(id), "not_found", e.to_string()),
        PolicyError::Io(..) => error(Some(id), "bad_request", e.to_string()),
    }
}

async fn reply(ws: &mut WebSocket, r: &WatchReply) -> bool {
    let text = serde_json::to_string(r).expect("serialisable");
    ws.send(Message::Text(text.into())).await.is_ok()
}

async fn session(node: Node, mut ws: WebSocket) {
    let limits = *node.watch_limits();
    let policy = node.policy().clone();
    let mut total = TotalGuard { total: node.watch_total().clone(), held: 0 };
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(limits.queue.max(1));
    let queue_full = Arc::new(AtomicBool::new(false));
    let mut regs = Registrations::default();
    let mut watches: HashMap<u64, WatchEntry> = HashMap::new();
    let mut pending: HashMap<u64, Batch> = HashMap::new();
    let mut overflowed: HashSet<u64> = HashSet::new();
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        let flush_at = deadline;
        tokio::select! {
            msg = ws.recv() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Binary(_))) => {
                        if !reply(&mut ws, &error(None, "bad_request", "text frames only")).await { break; }
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let answer = match serde_json::from_str::<WatchRequest>(text.as_str()) {
                    Err(e) => error(None, "bad_request", format!("bad frame: {e}")),
                    Ok(WatchRequest::Watch { id, path, recursive }) => {
                        if watches.contains_key(&id) {
                            error(Some(id), "duplicate_id", format!("watch {id} exists"))
                        } else if watches.len() >= limits.per_socket {
                            error(Some(id), "too_many_watches", format!("at most {} watches per socket", limits.per_socket))
                        } else {
                            match policy.resolve_existing(&path) {
                                Err(e) => policy_error(id, e),
                                Ok(root) => {
                                    if !total.take(limits.total) {
                                        error(Some(id), "too_many_watches", format!("at most {} watches on this computer", limits.total))
                                    } else {
                                        let poll = fs_kind(&root) == FsKind::Network;
                                        let entry = WatchEntry { root: root.clone(), recursive, poll };
                                        match regs.add(&entry, &tx, &queue_full, &limits) {
                                            Ok(()) => {
                                                watches.insert(id, entry);
                                                WatchReply::Watching { id, path: root }
                                            }
                                            Err(e) => {
                                                total.give();
                                                error(Some(id), "watch_failed", e.to_string())
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(WatchRequest::Unwatch { id }) => match watches.remove(&id) {
                        None => error(Some(id), "unknown_watch", format!("no watch {id}")),
                        Some(entry) => {
                            regs.remove(&entry);
                            total.give();
                            pending.remove(&id);
                            overflowed.remove(&id);
                            WatchReply::Unwatched { id }
                        }
                    },
                };
                if !reply(&mut ws, &answer).await { break; }
            }
            Some(res) = rx.recv() => {
                absorb(res, &policy, &watches, &mut pending, &mut overflowed, limits.batch_events);
                if deadline.is_none() && (!pending.is_empty() || !overflowed.is_empty()) {
                    deadline = Some(tokio::time::Instant::now() + limits.debounce);
                }
            }
            _ = async { tokio::time::sleep_until(flush_at.expect("guarded")).await }, if flush_at.is_some() => {
                deadline = None;
                if queue_full.swap(false, Ordering::SeqCst) {
                    overflowed.extend(watches.keys().copied());
                }
                if !flush(&mut ws, &watches, &mut pending, &mut overflowed).await { break; }
            }
        }
        // A full queue with no event behind it still needs a flush.
        if deadline.is_none() && queue_full.load(Ordering::SeqCst) {
            deadline = Some(tokio::time::Instant::now() + limits.debounce);
        }
    }
    // Dropping `regs` stops the OS watchers; `total` gives the watches back.
    drop(regs);
}

fn absorb(
    res: notify::Result<Event>,
    policy: &PathPolicy,
    watches: &HashMap<u64, WatchEntry>,
    pending: &mut HashMap<u64, Batch>,
    overflowed: &mut HashSet<u64>,
    cap: usize,
) {
    let affected = |paths: &[PathBuf]| -> Vec<u64> {
        if paths.is_empty() {
            watches.keys().copied().collect()
        } else {
            watches.iter().filter(|(_, w)| paths.iter().any(|p| p.starts_with(&w.root) || w.root.starts_with(p))).map(|(id, _)| *id).collect()
        }
    };
    let ev = match res {
        Ok(ev) => ev,
        Err(e) => {
            overflowed.extend(affected(&e.paths));
            return;
        }
    };
    if ev.need_rescan() {
        overflowed.extend(affected(&ev.paths));
        return;
    }
    for raw in map_event(&ev) {
        for (id, w) in watches {
            if overflowed.contains(id) {
                continue;
            }
            let batch = pending.entry(*id).or_default();
            match &raw {
                Raw::One(kind, p) => {
                    if w.covers(p) && reportable(policy, p) {
                        batch.push(*kind, p.clone());
                    }
                }
                Raw::Rename(from, to) => {
                    let f = w.covers(from) && reportable(policy, from);
                    let t = w.covers(to) && reportable(policy, to);
                    match (f, t) {
                        (true, true) => batch.rename(from.clone(), to.clone()),
                        (true, false) => batch.push(ChangeKind::Removed, from.clone()),
                        (false, true) => batch.push(ChangeKind::Created, to.clone()),
                        (false, false) => {}
                    }
                }
            }
            if batch.len() > cap {
                pending.remove(id);
                overflowed.insert(*id);
            }
        }
    }
    pending.retain(|_, b| !b.is_empty());
}

async fn flush(
    ws: &mut WebSocket,
    watches: &HashMap<u64, WatchEntry>,
    pending: &mut HashMap<u64, Batch>,
    overflowed: &mut HashSet<u64>,
) -> bool {
    let mut ids: Vec<u64> = overflowed.drain().filter(|id| watches.contains_key(id)).collect();
    ids.sort_unstable();
    for id in ids {
        pending.remove(&id);
        if !reply(ws, &WatchReply::Overflow { id }).await {
            return false;
        }
    }
    let mut ids: Vec<u64> = pending.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let Some(mut batch) = pending.remove(&id) else { continue };
        if batch.is_empty() || !watches.contains_key(&id) {
            continue;
        }
        if !reply(ws, &WatchReply::Change { id, events: batch.take() }).await {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_w4d_git_ignore_keeps_what_scm_needs() {
        for kept in [
            "/r/src/main.rs",
            "/r/.git",
            "/r/.git/HEAD",
            "/r/.git/index",
            "/r/.git/packed-refs",
            "/r/.git/MERGE_HEAD",
            "/r/.git/refs/heads/main",
            "/r/.git/refs/remotes/origin/feat/x",
            "/r/.gitignore",
            "/r/.github/workflows/test.yml",
        ] {
            assert!(!git_ignored(Path::new(kept)), "{kept} must be kept");
        }
    }

    #[test]
    fn fr_w4d_git_ignore_drops_objects_logs_and_locks() {
        for dropped in [
            "/r/.git/objects/ab/cdef",
            "/r/.git/objects/pack/pack-1.pack",
            "/r/.git/logs/HEAD",
            "/r/.git/index.lock",
            "/r/.git/HEAD.lock",
            "/r/.git/FETCH_HEAD",
            "/r/.git/config",
            "/r/.git/hooks/pre-commit",
            "/r/.git/modules/sub/HEAD",
            "/r/vendor/dep/.git/objects/x",
        ] {
            assert!(git_ignored(Path::new(dropped)), "{dropped} must be dropped");
        }
    }

    #[test]
    fn fr_w4d_merge_rules() {
        use ChangeKind::*;
        assert_eq!(merge(Created, Changed), Created);
        assert_eq!(merge(Removed, Created), Changed);
        assert_eq!(merge(Changed, Changed), Changed);
        assert_eq!(merge(Created, Removed), Removed);
        assert_eq!(merge(Changed, Removed), Removed);
    }

    #[test]
    fn fr_w4d_merge_batch_lists_a_path_once_and_folds_rename_halves() {
        let mut b = Batch::default();
        b.push(ChangeKind::Created, "/r/a".into());
        b.push(ChangeKind::Changed, "/r/a".into());
        b.push(ChangeKind::Changed, "/r/a".into());
        b.push(ChangeKind::Removed, "/r/x".into());
        b.push(ChangeKind::Created, "/r/y".into());
        b.rename("/r/x".into(), "/r/y".into());
        let out = b.take();
        assert_eq!(
            out,
            vec![
                WatchEvent { kind: ChangeKind::Created, path: "/r/a".into(), to: None },
                WatchEvent { kind: ChangeKind::Renamed, path: "/r/x".into(), to: Some("/r/y".into()) },
            ]
        );
        assert!(b.is_empty());
    }

    #[test]
    fn fr_w4d_wire_frames() {
        let r: WatchRequest = serde_json::from_str(r#"{"op":"watch","id":3,"path":"/r","recursive":true}"#).unwrap();
        assert_eq!(r, WatchRequest::Watch { id: 3, path: "/r".into(), recursive: true });
        let r: WatchRequest = serde_json::from_str(r#"{"op":"unwatch","id":3}"#).unwrap();
        assert_eq!(r, WatchRequest::Unwatch { id: 3 });
        let c = WatchReply::Change {
            id: 3,
            events: vec![WatchEvent { kind: ChangeKind::Renamed, path: "/r/a".into(), to: Some("/r/b".into()) }],
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::json!({"op":"change","id":3,"events":[{"kind":"renamed","path":"/r/a","to":"/r/b"}]})
        );
        assert_eq!(serde_json::to_value(WatchReply::Overflow { id: 3 }).unwrap(), serde_json::json!({"op":"overflow","id":3}));
    }

    #[test]
    fn fr_w4d_a_non_recursive_watch_covers_direct_children_only() {
        let w = WatchEntry { root: "/r".into(), recursive: false, poll: false };
        assert!(w.covers(Path::new("/r")));
        assert!(w.covers(Path::new("/r/a")));
        assert!(!w.covers(Path::new("/r/a/b")));
        let w = WatchEntry { root: "/r".into(), recursive: true, poll: false };
        assert!(w.covers(Path::new("/r/a/b")));
        assert!(!w.covers(Path::new("/rx/a")));
    }
}
