//! File watch subscriptions of the app's screens (SPEC `FR-W4d`, #460).
//!
//! A view (the 버전 관리 page or sheet) asks for a [`WatchKey`] (computer, folder, recursive)
//! and gets a change counter that goes up when files below the folder change. The server
//! relays at most 64 watch sockets in total, so the app opens ONE socket per computer and
//! shares it; two views of the same key share one node watch (reference counted).
//!
//! [`Core`] is pure: it takes the driver's inputs with an explicit `now` and answers with
//! [`Cmd`]s, so the rules are unit tested without a network. [`subscribe`] and the driver
//! below own the sockets on `services().rt`, like `owner_claim::drive`.
//!
//! Rules:
//! - Events are debounced per view: at most one refresh per [`DEBOUNCE`], fired at the end of
//!   the window (trailing edge), so a burst of events gives one refresh.
//! - `overflow` and a reconnect force a refresh, because events may have been lost.
//! - A refused watch (`forbidden_path`, `not_found`, ...) turns that subscription off; a refused
//!   socket (401, 403 `scope_denied`, 404 unknown computer or a server without the route) turns
//!   every subscription of the computer off. One log line each; the view keeps its manual
//!   refresh. Other socket failures reconnect with the backoff of the owner claim.
//! - While a view's tag is held (a state-changing git call of that view is in flight) the view
//!   does not refresh: the call reloads by itself. A release after a failed call fires the
//!   pending refresh.
//! - When the last view of a computer goes, its socket is closed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ember_client::api::{Api, ApiError};
use ember_client::fs_watch::FsWatchMessage;
use tokio::sync::mpsc;

use crate::owner_claim::Backoff;

/// At most one refresh per view in this window.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// What a subscription watches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WatchKey {
    pub computer: String,
    pub folder: PathBuf,
    pub recursive: bool,
}

pub type ViewId = u64;

/// What the driver does for the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Open the socket of `computer` after `delay`.
    Open { computer: String, delay: Duration },
    Watch { computer: String, key: WatchKey },
    Unwatch { computer: String, id: u64 },
    Close { computer: String },
    /// One log line (an error that turned a subscription off).
    Log(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubState {
    /// Asked for (or waiting for the socket).
    Pending,
    Live,
    /// Refused: no refresh from the watch, manual refresh only.
    Off,
}

#[derive(Debug)]
struct Sub {
    refs: usize,
    id: Option<u64>,
    canonical: Option<PathBuf>,
    state: SubState,
}

#[derive(Debug)]
struct View {
    key: WatchKey,
    tag: String,
    dirty_since: Option<Instant>,
    last_fire: Option<Instant>,
    count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SockState {
    Connecting,
    Open,
    /// Refused for good while any view of the computer remains.
    Off,
}

#[derive(Debug)]
struct Sock {
    state: SockState,
    attempt: u32,
    was_open: bool,
}

/// Whether a socket error is final (no reconnect): the token (401), the device scope (403) or
/// an unknown computer or a server without the route (404).
pub fn is_final(e: &ApiError) -> bool {
    matches!(e.status(), Some(401 | 403 | 404))
}

/// Whether `path` is the folder or below it. Recursive or not, the node only reports paths
/// below the watched folder; anything else is not this view's.
pub fn is_below(path: &Path, folder: &Path) -> bool {
    path.starts_with(folder)
}

/// The pure state of every subscription.
#[derive(Debug)]
pub struct Core {
    subs: BTreeMap<WatchKey, Sub>,
    views: BTreeMap<ViewId, View>,
    socks: BTreeMap<String, Sock>,
    holds: HashMap<String, usize>,
    backoff: Backoff,
}

impl Default for Core {
    fn default() -> Self {
        Core::new(Backoff::default())
    }
}

impl Core {
    pub fn new(backoff: Backoff) -> Self {
        Core { subs: BTreeMap::new(), views: BTreeMap::new(), socks: BTreeMap::new(), holds: HashMap::new(), backoff }
    }

    /// Register view `view` for `key`; `tag` groups views for [`Core::hold`].
    pub fn subscribe(&mut self, view: ViewId, key: WatchKey, tag: &str) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        let computer = key.computer.clone();
        let sock_state = match self.socks.get(&computer) {
            Some(s) => s.state,
            None => {
                self.socks.insert(computer.clone(), Sock { state: SockState::Connecting, attempt: 0, was_open: false });
                cmds.push(Cmd::Open { computer: computer.clone(), delay: Duration::ZERO });
                SockState::Connecting
            }
        };
        let fresh = !self.subs.contains_key(&key);
        let sub = self.subs.entry(key.clone()).or_insert(Sub { refs: 0, id: None, canonical: None, state: SubState::Pending });
        sub.refs += 1;
        if fresh {
            match sock_state {
                SockState::Off => sub.state = SubState::Off,
                SockState::Open => cmds.push(Cmd::Watch { computer, key: key.clone() }),
                SockState::Connecting => {}
            }
        }
        self.views.insert(view, View { key, tag: tag.to_string(), dirty_since: None, last_fire: None, count: 0 });
        cmds
    }

    /// Drop view `view`; the last view of a key unwatches it, the last of a computer closes the
    /// socket.
    pub fn unsubscribe(&mut self, view: ViewId) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        let Some(v) = self.views.remove(&view) else { return cmds };
        let computer = v.key.computer.clone();
        if let Some(sub) = self.subs.get_mut(&v.key) {
            sub.refs = sub.refs.saturating_sub(1);
            if sub.refs == 0 {
                let id = sub.id;
                self.subs.remove(&v.key);
                let open = self.socks.get(&computer).is_some_and(|s| s.state == SockState::Open);
                if let (Some(id), true) = (id, open) {
                    cmds.push(Cmd::Unwatch { computer: computer.clone(), id });
                }
            }
        }
        if !self.subs.keys().any(|k| k.computer == computer) && self.socks.remove(&computer).is_some() {
            cmds.push(Cmd::Close { computer });
        }
        cmds
    }

    /// The socket sent the watch request of `key` as `id`.
    pub fn on_watch_sent(&mut self, key: &WatchKey, id: u64) -> Vec<Cmd> {
        match self.subs.get_mut(key) {
            Some(sub) => {
                sub.id = Some(id);
                Vec::new()
            }
            // Unsubscribed while the request was on its way.
            None => vec![Cmd::Unwatch { computer: key.computer.clone(), id }],
        }
    }

    /// The socket of `computer` is open: (re)send every watch; after a lost socket or failed
    /// tries, refresh every view of it.
    pub fn on_connected(&mut self, computer: &str, now: Instant) -> Vec<Cmd> {
        let Some(sock) = self.socks.get_mut(computer) else { return Vec::new() };
        let reconnect = sock.was_open || sock.attempt > 0;
        sock.state = SockState::Open;
        sock.was_open = true;
        sock.attempt = 0;
        let mut cmds = Vec::new();
        for (key, sub) in self.subs.iter_mut().filter(|(k, _)| k.computer == computer) {
            if sub.state == SubState::Off {
                continue;
            }
            sub.id = None;
            sub.canonical = None;
            sub.state = SubState::Pending;
            cmds.push(Cmd::Watch { computer: computer.to_string(), key: key.clone() });
        }
        if reconnect {
            self.mark_computer(computer, now);
        }
        cmds
    }

    /// The socket of `computer` ended (`error` is `None` for a plain close by the other side).
    pub fn on_ended(&mut self, computer: &str, error: Option<&ApiError>) -> Vec<Cmd> {
        let Some(sock) = self.socks.get_mut(computer) else { return Vec::new() };
        if let Some(e) = error.filter(|e| is_final(e)) {
            sock.state = SockState::Off;
            for (_, sub) in self.subs.iter_mut().filter(|(k, _)| k.computer == computer) {
                sub.state = SubState::Off;
                sub.id = None;
            }
            return vec![Cmd::Log(format!("file watch of computer {computer} is off, manual refresh only: {e}"))];
        }
        sock.state = SockState::Connecting;
        let delay = Duration::from_millis(self.backoff.delay_ms(sock.attempt));
        sock.attempt = sock.attempt.saturating_add(1);
        for (_, sub) in self.subs.iter_mut().filter(|(k, _)| k.computer == computer) {
            if sub.state != SubState::Off {
                sub.state = SubState::Pending;
            }
            sub.id = None;
        }
        vec![Cmd::Open { computer: computer.to_string(), delay }]
    }

    /// A frame of the socket of `computer`.
    pub fn on_message(&mut self, computer: &str, msg: FsWatchMessage, now: Instant) -> Vec<Cmd> {
        let find = |subs: &BTreeMap<WatchKey, Sub>, id: u64| {
            subs.iter().find(|(k, s)| k.computer == computer && s.id == Some(id)).map(|(k, _)| k.clone())
        };
        match msg {
            FsWatchMessage::Watching { id, path } => {
                if let Some(key) = find(&self.subs, id) {
                    let sub = self.subs.get_mut(&key).expect("found");
                    sub.canonical = Some(path);
                    sub.state = SubState::Live;
                }
                Vec::new()
            }
            FsWatchMessage::Error { id, code, message } => {
                let key = id.and_then(|id| find(&self.subs, id));
                match key {
                    Some(key) => {
                        let sub = self.subs.get_mut(&key).expect("found");
                        sub.state = SubState::Off;
                        sub.id = None;
                        vec![Cmd::Log(format!(
                            "file watch of {} on {computer} is off, manual refresh only: {code} ({message})",
                            key.folder.display()
                        ))]
                    }
                    None => vec![Cmd::Log(format!("file watch on {computer}: {code} ({message})"))],
                }
            }
            FsWatchMessage::Change { id, events } => {
                if let Some(key) = find(&self.subs, id) {
                    let canonical = self.subs[&key].canonical.clone();
                    let hit = |p: &Path, folder: &Path| is_below(p, folder) || canonical.as_deref().is_some_and(|c| is_below(p, c));
                    for v in self.views.values_mut().filter(|v| v.key == key) {
                        let relevant = events
                            .iter()
                            .any(|e| hit(&e.path, &v.key.folder) || e.to.as_deref().is_some_and(|t| hit(t, &v.key.folder)));
                        if relevant && v.dirty_since.is_none() {
                            v.dirty_since = Some(now);
                        }
                    }
                }
                Vec::new()
            }
            FsWatchMessage::Overflow { id } => {
                if let Some(key) = find(&self.subs, id) {
                    for v in self.views.values_mut().filter(|v| v.key == key) {
                        v.dirty_since.get_or_insert(now);
                    }
                }
                Vec::new()
            }
            FsWatchMessage::Unwatched { .. } => Vec::new(),
        }
    }

    fn mark_computer(&mut self, computer: &str, now: Instant) {
        let subs = &self.subs;
        for v in self.views.values_mut().filter(|v| v.key.computer == computer) {
            if subs.get(&v.key).is_some_and(|s| s.state != SubState::Off) {
                v.dirty_since.get_or_insert(now);
            }
        }
    }

    /// A state-changing call of the views tagged `tag` starts.
    pub fn hold(&mut self, tag: &str) {
        *self.holds.entry(tag.to_string()).or_default() += 1;
    }

    /// The call ended. `refreshed`: it reloaded the view itself, so pending events are dropped;
    /// otherwise they fire at the next [`Core::poll`].
    pub fn release(&mut self, tag: &str, refreshed: bool, now: Instant) {
        let left = match self.holds.get_mut(tag) {
            Some(n) => {
                *n = n.saturating_sub(1);
                *n
            }
            None => return,
        };
        if left == 0 {
            self.holds.remove(tag);
        }
        if refreshed {
            for v in self.views.values_mut().filter(|v| v.tag == tag) {
                v.dirty_since = None;
                v.last_fire = Some(now);
            }
        }
    }

    /// Fire the views that are due: each gets its new counter. Also the next deadline.
    pub fn poll(&mut self, now: Instant) -> (Vec<(ViewId, u64)>, Option<Instant>) {
        let mut fired = Vec::new();
        let mut next: Option<Instant> = None;
        for (id, v) in self.views.iter_mut() {
            let Some(since) = v.dirty_since else { continue };
            if self.holds.get(&v.tag).is_some_and(|n| *n > 0) {
                continue;
            }
            let mut at = since + DEBOUNCE;
            if let Some(last) = v.last_fire {
                at = at.max(last + DEBOUNCE);
            }
            if now >= at {
                v.count += 1;
                v.last_fire = Some(now);
                v.dirty_since = None;
                fired.push((*id, v.count));
            } else {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        (fired, next)
    }

    /// The state of view `view`'s subscription.
    pub fn state(&self, view: ViewId) -> Option<SubState> {
        self.views.get(&view).and_then(|v| self.subs.get(&v.key)).map(|s| s.state)
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty() && self.socks.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------------------------

/// Called on the runtime with the view's new counter.
pub type Bump = Box<dyn Fn(u64) + Send + 'static>;

enum Input {
    Subscribe { view: ViewId, key: WatchKey, tag: String, bump: Bump },
    Unsubscribe(ViewId),
    Hold(String),
    Release { tag: String, refreshed: bool },
    Sent { computer: String, generation: u64, key: WatchKey, id: u64 },
    Connected { computer: String, generation: u64 },
    Message { computer: String, generation: u64, msg: FsWatchMessage },
    Ended { computer: String, generation: u64, error: Option<ApiError> },
}

enum SockCmd {
    Watch(WatchKey),
    Unwatch(u64),
    Close,
}

struct Hub {
    tx: mpsc::UnboundedSender<Input>,
    next_view: AtomicU64,
}

fn hub() -> Option<&'static Hub> {
    static HUB: OnceLock<Option<Hub>> = OnceLock::new();
    HUB.get_or_init(|| {
        let s = crate::services::try_services()?;
        if s.demo.is_some() {
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        s.rt.spawn(drive(s.client.api().clone(), tx.clone(), rx));
        Some(Hub { tx, next_view: AtomicU64::new(1) })
    })
    .as_ref()
}

/// A view's interest. Dropping it unsubscribes.
pub struct Subscription {
    view: ViewId,
    tx: mpsc::UnboundedSender<Input>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.tx.send(Input::Unsubscribe(self.view));
    }
}

/// Subscribe to `key`. `bump` runs on the runtime with the view's counter after each debounced
/// change. `None` in demo mode or before the services exist: the view keeps its manual refresh.
pub fn subscribe(key: WatchKey, tag: &str, bump: Bump) -> Option<Subscription> {
    let hub = hub()?;
    let view = hub.next_view.fetch_add(1, Ordering::Relaxed);
    hub.tx.send(Input::Subscribe { view, key, tag: tag.to_string(), bump }).ok()?;
    Some(Subscription { view, tx: hub.tx.clone() })
}

/// A state-changing call of the views tagged `tag` starts (see [`Core::hold`]).
pub fn hold(tag: &str) {
    if let Some(hub) = hub() {
        let _ = hub.tx.send(Input::Hold(tag.to_string()));
    }
}

/// The call ended; `refreshed` when it reloaded the view itself.
pub fn release(tag: &str, refreshed: bool) {
    if let Some(hub) = hub() {
        let _ = hub.tx.send(Input::Release { tag: tag.to_string(), refreshed });
    }
}

async fn drive(api: Api, tx: mpsc::UnboundedSender<Input>, mut rx: mpsc::UnboundedReceiver<Input>) {
    let mut core = Core::default();
    let mut bumps: HashMap<ViewId, Bump> = HashMap::new();
    let mut socks: HashMap<String, (u64, mpsc::UnboundedSender<SockCmd>)> = HashMap::new();
    let mut generation: u64 = 0;
    let mut deadline: Option<Instant> = None;
    loop {
        let input = match deadline {
            Some(at) => match tokio::time::timeout_at(at.into(), rx.recv()).await {
                Ok(i) => i,
                Err(_) => {
                    let (fired, next) = core.poll(Instant::now());
                    for (view, n) in fired {
                        if let Some(b) = bumps.get(&view) {
                            b(n);
                        }
                    }
                    deadline = next;
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let Some(input) = input else { return };
        let now = Instant::now();
        let current = |socks: &HashMap<String, (u64, mpsc::UnboundedSender<SockCmd>)>, c: &str, g: u64| socks.get(c).is_some_and(|(sg, _)| *sg == g);
        let cmds = match input {
            Input::Subscribe { view, key, tag, bump } => {
                bumps.insert(view, bump);
                core.subscribe(view, key, &tag)
            }
            Input::Unsubscribe(view) => {
                bumps.remove(&view);
                core.unsubscribe(view)
            }
            Input::Hold(tag) => {
                core.hold(&tag);
                Vec::new()
            }
            Input::Release { tag, refreshed } => {
                core.release(&tag, refreshed, now);
                Vec::new()
            }
            Input::Sent { computer, generation: g, key, id } if current(&socks, &computer, g) => core.on_watch_sent(&key, id),
            Input::Connected { computer, generation: g } if current(&socks, &computer, g) => core.on_connected(&computer, now),
            Input::Message { computer, generation: g, msg } if current(&socks, &computer, g) => core.on_message(&computer, msg, now),
            Input::Ended { computer, generation: g, error } if current(&socks, &computer, g) => {
                socks.remove(&computer);
                core.on_ended(&computer, error.as_ref())
            }
            // A frame of a socket that was closed or replaced.
            _ => Vec::new(),
        };
        for cmd in cmds {
            match cmd {
                Cmd::Open { computer, delay } => {
                    generation += 1;
                    let (stx, srx) = mpsc::unbounded_channel();
                    if let Some((_, old)) = socks.insert(computer.clone(), (generation, stx)) {
                        let _ = old.send(SockCmd::Close);
                    }
                    tokio::spawn(socket_task(api.clone(), computer, generation, delay, srx, tx.clone()));
                }
                Cmd::Watch { computer, key } => {
                    if let Some((_, s)) = socks.get(&computer) {
                        let _ = s.send(SockCmd::Watch(key));
                    }
                }
                Cmd::Unwatch { computer, id } => {
                    if let Some((_, s)) = socks.get(&computer) {
                        let _ = s.send(SockCmd::Unwatch(id));
                    }
                }
                Cmd::Close { computer } => {
                    if let Some((_, s)) = socks.remove(&computer) {
                        let _ = s.send(SockCmd::Close);
                    }
                }
                Cmd::Log(line) => tracing::info!("{line}"),
            }
        }
        let (fired, next) = core.poll(now);
        for (view, n) in fired {
            if let Some(b) = bumps.get(&view) {
                b(n);
            }
        }
        deadline = next;
    }
}

async fn socket_task(
    api: Api,
    computer: String,
    generation: u64,
    delay: Duration,
    mut cmds: mpsc::UnboundedReceiver<SockCmd>,
    out: mpsc::UnboundedSender<Input>,
) {
    if !delay.is_zero() {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                // Closed (or dropped) while waiting: no socket. A watch or unwatch asked for
                // meanwhile is dropped: `Core::on_connected` sends every watch again.
                c = cmds.recv() => if matches!(c, None | Some(SockCmd::Close)) { return },
            }
        }
    }
    let mut fs = match api.watch_fs(&computer).await {
        Ok(fs) => fs,
        Err(e) => {
            let _ = out.send(Input::Ended { computer, generation, error: Some(e) });
            return;
        }
    };
    let _ = out.send(Input::Connected { computer: computer.clone(), generation });
    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                None | Some(SockCmd::Close) => {
                    fs.close().await;
                    return;
                }
                Some(SockCmd::Watch(key)) => match fs.watch(&key.folder, key.recursive).await {
                    Ok(id) => {
                        let _ = out.send(Input::Sent { computer: computer.clone(), generation, key, id });
                    }
                    Err(e) => {
                        let _ = out.send(Input::Ended { computer, generation, error: Some(e) });
                        return;
                    }
                },
                Some(SockCmd::Unwatch(id)) => {
                    if let Err(e) = fs.unwatch(id).await {
                        let _ = out.send(Input::Ended { computer, generation, error: Some(e) });
                        return;
                    }
                }
            },
            msg = fs.next() => match msg {
                Ok(Some(msg)) => {
                    let _ = out.send(Input::Message { computer: computer.clone(), generation, msg });
                }
                // A frame this build does not know: skip it.
                Err(ApiError::Decode(e)) => tracing::debug!("file watch frame skipped: {e}"),
                Ok(None) => {
                    let _ = out.send(Input::Ended { computer, generation, error: None });
                    return;
                }
                Err(e) => {
                    let _ = out.send(Input::Ended { computer, generation, error: Some(e) });
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::fs_watch::{FsChange, FsChangeKind};

    fn key(c: &str, f: &str) -> WatchKey {
        WatchKey { computer: c.into(), folder: f.into(), recursive: true }
    }

    fn change(id: u64, path: &str) -> FsWatchMessage {
        FsWatchMessage::Change { id, events: vec![FsChange { kind: FsChangeKind::Changed, path: path.into(), to: None }] }
    }

    /// A core with view 1 on `/r` of `c1`, its socket open and the watch live as id 1.
    fn live(now: Instant) -> Core {
        let mut c = Core::default();
        c.subscribe(1, key("c1", "/r"), "p");
        c.on_connected("c1", now);
        c.on_watch_sent(&key("c1", "/r"), 1);
        c.on_message("c1", FsWatchMessage::Watching { id: 1, path: "/r".into() }, now);
        c
    }

    #[test]
    fn fr_w4d_app_views_of_one_folder_share_one_watch_and_one_socket() {
        let mut c = Core::default();
        assert_eq!(c.subscribe(1, key("c1", "/r"), "p"), vec![Cmd::Open { computer: "c1".into(), delay: Duration::ZERO }]);
        assert!(c.subscribe(2, key("c1", "/r"), "p").is_empty(), "same key: no second socket, no second watch");
        let now = Instant::now();
        assert_eq!(c.on_connected("c1", now), vec![Cmd::Watch { computer: "c1".into(), key: key("c1", "/r") }]);
        c.on_watch_sent(&key("c1", "/r"), 7);
        // Another folder of the same computer: the same socket.
        assert_eq!(c.subscribe(3, key("c1", "/s"), "q"), vec![Cmd::Watch { computer: "c1".into(), key: key("c1", "/s") }]);
        c.on_watch_sent(&key("c1", "/s"), 8);
        assert!(c.unsubscribe(1).is_empty(), "view 2 still uses the watch");
        assert_eq!(c.unsubscribe(2), vec![Cmd::Unwatch { computer: "c1".into(), id: 7 }]);
        assert_eq!(c.unsubscribe(3), vec![Cmd::Unwatch { computer: "c1".into(), id: 8 }, Cmd::Close { computer: "c1".into() }]);
        assert!(c.is_empty());
    }

    #[test]
    fn fr_w4d_app_a_watch_answered_after_unsubscribe_is_unwatched() {
        let mut c = Core::default();
        c.subscribe(1, key("c1", "/r"), "p");
        c.on_connected("c1", Instant::now());
        c.unsubscribe(1);
        assert_eq!(c.on_watch_sent(&key("c1", "/r"), 3), vec![Cmd::Unwatch { computer: "c1".into(), id: 3 }]);
    }

    #[test]
    fn fr_w4d_app_many_events_give_one_refresh() {
        let t0 = Instant::now();
        let mut c = live(t0);
        for i in 0..50 {
            c.on_message("c1", change(1, "/r/a.txt"), t0 + Duration::from_millis(i * 5));
        }
        let (fired, next) = c.poll(t0 + Duration::from_millis(300));
        assert!(fired.is_empty());
        assert_eq!(next, Some(t0 + DEBOUNCE));
        let (fired, next) = c.poll(t0 + DEBOUNCE);
        assert_eq!(fired, vec![(1, 1)]);
        assert_eq!(next, None);
        // A new event right after the refresh waits a whole window from that refresh.
        let t1 = t0 + DEBOUNCE + Duration::from_millis(10);
        c.on_message("c1", change(1, "/r/b.txt"), t1);
        assert!(c.poll(t1 + Duration::from_millis(400)).0.is_empty());
        assert_eq!(c.poll(t1 + DEBOUNCE).0, vec![(1, 2)]);
    }

    #[test]
    fn fr_w4d_app_overflow_forces_a_refresh() {
        let t0 = Instant::now();
        let mut c = live(t0);
        c.on_message("c1", FsWatchMessage::Overflow { id: 1 }, t0);
        assert_eq!(c.poll(t0 + DEBOUNCE).0, vec![(1, 1)]);
    }

    #[test]
    fn fr_w4d_app_paths_outside_the_folder_do_not_bump() {
        let t0 = Instant::now();
        let mut c = live(t0);
        c.on_message("c1", change(1, "/other/a.txt"), t0);
        c.on_message("c1", change(1, "/rx/a.txt"), t0);
        assert!(c.poll(t0 + DEBOUNCE * 2).0.is_empty());
        // The canonical spelling of the `watching` answer counts too.
        let mut c = Core::default();
        c.subscribe(1, key("c1", "/tmp/r"), "p");
        c.on_connected("c1", t0);
        c.on_watch_sent(&key("c1", "/tmp/r"), 1);
        c.on_message("c1", FsWatchMessage::Watching { id: 1, path: "/private/tmp/r".into() }, t0);
        c.on_message("c1", change(1, "/private/tmp/r/x"), t0);
        assert_eq!(c.poll(t0 + DEBOUNCE).0, vec![(1, 1)]);
    }

    #[test]
    fn fr_w4d_app_a_refused_watch_turns_off_and_logs_once() {
        let t0 = Instant::now();
        let mut c = Core::default();
        c.subscribe(1, key("c1", "/r"), "p");
        c.on_connected("c1", t0);
        c.on_watch_sent(&key("c1", "/r"), 1);
        let cmds = c.on_message("c1", FsWatchMessage::Error { id: Some(1), code: "forbidden_path".into(), message: "no".into() }, t0);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], Cmd::Log(l) if l.contains("forbidden_path")));
        assert_eq!(c.state(1), Some(SubState::Off));
        assert!(c.poll(t0 + DEBOUNCE).0.is_empty());
        // A reconnect does not ask for the refused watch again.
        assert_eq!(c.on_ended("c1", None).len(), 1);
        assert!(c.on_connected("c1", t0).is_empty());
    }

    #[test]
    fn fr_w4d_app_a_refused_socket_turns_every_subscription_off() {
        for status in [401_u16, 403, 404] {
            let mut c = Core::default();
            c.subscribe(1, key("c1", "/r"), "p");
            let e = ApiError::Status { status, message: "x".into(), code: None };
            let cmds = c.on_ended("c1", Some(&e));
            assert!(matches!(cmds.as_slice(), [Cmd::Log(_)]), "{status}: no reconnect");
            assert_eq!(c.state(1), Some(SubState::Off));
            // A second view of the computer stays off without a socket.
            assert!(c.subscribe(2, key("c1", "/s"), "q").is_empty());
            assert_eq!(c.state(2), Some(SubState::Off));
            // When every view went, a later one tries again.
            c.unsubscribe(1);
            assert_eq!(c.unsubscribe(2), vec![Cmd::Close { computer: "c1".into() }]);
            assert_eq!(c.subscribe(3, key("c1", "/r"), "p"), vec![Cmd::Open { computer: "c1".into(), delay: Duration::ZERO }]);
        }
    }

    #[test]
    fn fr_w4d_app_reconnect_backs_off_rewatches_and_refreshes() {
        let t0 = Instant::now();
        let mut c = live(t0);
        let busy = ApiError::Status { status: 503, message: "full".into(), code: None };
        assert_eq!(c.on_ended("c1", Some(&busy)), vec![Cmd::Open { computer: "c1".into(), delay: Duration::from_millis(1_000) }]);
        assert_eq!(c.state(1), Some(SubState::Pending));
        assert_eq!(c.on_ended("c1", None), vec![Cmd::Open { computer: "c1".into(), delay: Duration::from_millis(2_000) }]);
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(c.on_connected("c1", t1), vec![Cmd::Watch { computer: "c1".into(), key: key("c1", "/r") }]);
        assert_eq!(c.poll(t1 + DEBOUNCE).0, vec![(1, 1)], "events may have been lost");
        // The backoff starts again after a good connection.
        assert_eq!(c.on_ended("c1", None), vec![Cmd::Open { computer: "c1".into(), delay: Duration::from_millis(1_000) }]);
    }

    #[test]
    fn fr_w4d_app_no_refresh_while_a_git_call_runs() {
        let t0 = Instant::now();
        let mut c = live(t0);
        c.hold("p");
        c.on_message("c1", change(1, "/r/.git/index"), t0);
        assert!(c.poll(t0 + DEBOUNCE * 3).0.is_empty(), "held");
        // The call reloaded by itself: the pending events are dropped.
        c.release("p", true, t0 + DEBOUNCE * 3);
        assert!(c.poll(t0 + DEBOUNCE * 10).0.is_empty());
        // A failed call did not reload: the pending refresh fires.
        c.hold("p");
        let t1 = t0 + DEBOUNCE * 11;
        c.on_message("c1", change(1, "/r/a"), t1);
        c.release("p", false, t1);
        assert_eq!(c.poll(t1 + DEBOUNCE).0, vec![(1, 1)]);
        // Holding another tag does not stop this view.
        c.hold("other");
        let t2 = t1 + DEBOUNCE * 3;
        c.on_message("c1", change(1, "/r/a"), t2);
        assert_eq!(c.poll(t2 + DEBOUNCE).0, vec![(1, 2)]);
    }
}
