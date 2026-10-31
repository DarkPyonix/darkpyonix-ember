//! One letify-link session per peer pair, and Ember's connections multiplexed on it.
//!
//! The transport interface speaks of a [`Connection`] to a peer for one named service, with any
//! number of bidirectional streams. letify-link has one session per pair of nodes with any number
//! of streams. A [`PeerLink`] is the session; an [`LConn`] is one logical connection on it.
//!
//! Every letify-link stream starts with a 5 byte header: a kind and the connection id.
//!
//! - kind 1, the control stream of a connection. The opener (the dialer) follows the header with
//!   `service_len: u8, service`. The acceptor answers `status: u8, reason_len: u8, reason`
//!   (status 0: accepted). The stream then stays open for the life of the connection and carries
//!   one frame each way at the end: `'C', code: u32, reason_len: u16, reason`. End of stream
//!   without that frame means the other side dropped the connection.
//! - kind 2, a data stream of connection `id`: the application's bytes follow the header.
//!
//! Connection ids are allocated by the side that opens the connection: even on the side that
//! started the session, odd on the other, so both can open connections on one link.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use letify_link::{Error as LinkError, PathKind, Priority, Session, Stream, SubflowId, WeakSession};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;

use crate::gate::CLOSE_REVOKED;
use crate::{
    BiStream, CloseReason, Connection, ConnectionBackend, PathState, PeerId, Result, ServiceTable,
    TransportError,
};

const KIND_CTRL: u8 = 1;
const KIND_DATA: u8 = 2;
const FRAME_CLOSE: u8 = b'C';
/// How long a stream may take to send its header.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the acceptor may take to answer a new connection.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Why opening a connection on a link failed.
pub(super) enum OpenError {
    /// The peer answered and refused (no such service, or it stopped listening).
    Refused(String),
    /// The session is gone or did not answer; a new session may work.
    Dead(String),
}

/// Commands for the path manager task of an initiating link.
pub(super) enum Cmd {
    Power(letify_link::PowerState),
    Resume,
}

type KindOf = Arc<dyn Fn(SubflowId) -> Option<PathKind> + Send + Sync>;

/// A letify-link session with one peer and the connections on it.
pub(super) struct PeerLink {
    pub(super) peer: PeerId,
    /// True when this side started the session (and so owns a path manager).
    pub(super) initiator: bool,
    session: WeakSession,
    pub(super) session_id: [u8; 16],
    next: AtomicU32,
    conns: Mutex<HashMap<u32, Arc<ConnShared>>>,
    services: Arc<ServiceTable>,
    pub(super) path_tx: watch::Sender<PathState>,
    kind_of: Mutex<Option<KindOf>>,
    relay_label: String,
    /// Responder only: the first path was a relay and no direct path joined since.
    relay_only: AtomicBool,
    pub(super) local_close: AtomicBool,
    ended: AtomicBool,
    tasks: Mutex<Vec<AbortHandle>>,
    /// The demux task is kept apart: a local close must let it run to see the session end.
    demux: Mutex<Option<AbortHandle>>,
    cmds: Mutex<Option<mpsc::UnboundedSender<Cmd>>>,
}

impl PeerLink {
    pub(super) fn new(
        peer: PeerId,
        initiator: bool,
        session: &Session,
        services: Arc<ServiceTable>,
        relay_label: String,
        relay_only: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            peer,
            initiator,
            session: session.downgrade(),
            session_id: session.session_id(),
            next: AtomicU32::new(0),
            conns: Mutex::new(HashMap::new()),
            services,
            path_tx: watch::channel(PathState::Unknown).0,
            kind_of: Mutex::new(None),
            relay_label,
            relay_only: AtomicBool::new(relay_only),
            local_close: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
            demux: Mutex::new(None),
            cmds: Mutex::new(None),
        })
    }

    pub(super) fn set_kind_of(&self, f: KindOf) {
        *self.kind_of.lock().unwrap() = Some(f);
    }

    pub(super) fn set_cmds(&self, tx: mpsc::UnboundedSender<Cmd>) {
        *self.cmds.lock().unwrap() = Some(tx);
    }

    pub(super) fn add_task(&self, handle: AbortHandle) {
        self.tasks.lock().unwrap().push(handle);
    }

    pub(super) fn set_demux(&self, handle: AbortHandle) {
        *self.demux.lock().unwrap() = Some(handle);
    }

    /// Stops every task of the link at once, dropping its session handles, and ends its
    /// connections (the transport was dropped without a graceful close, or the session stopped
    /// answering).
    pub(super) fn abort_all(&self) {
        self.ended.store(true, Ordering::SeqCst);
        if let Some(h) = self.demux.lock().unwrap().take() {
            h.abort();
        }
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        *self.cmds.lock().unwrap() = None;
        let conns: Vec<Arc<ConnShared>> = self.conns.lock().unwrap().drain().map(|(_, c)| c).collect();
        for c in conns {
            c.set_closed(CloseReason::Lost("the session was abandoned".into()));
        }
        self.path_tx.send_replace(PathState::Unknown);
    }

    pub(super) fn session(&self) -> Option<Session> {
        self.session.upgrade()
    }

    pub(super) fn is_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// A direct path joined a session that began over a relay.
    pub(super) fn note_direct_join(&self) {
        self.relay_only.store(false, Ordering::SeqCst);
    }

    /// The route now, with a fresh round trip sample.
    pub(super) fn compute_state(&self) -> PathState {
        let Some(session) = self.session.upgrade() else {
            return PathState::Unknown;
        };
        if session.is_closed() || self.is_ended() {
            return PathState::Unknown;
        }
        let stats = session.subflow_stats();
        if stats.is_empty() {
            return PathState::Unknown;
        }
        let rtt = stats.iter().filter_map(|s| s.srtt.or(s.rtt)).min().unwrap_or(Duration::ZERO);
        let kind_of = self.kind_of.lock().unwrap().clone();
        let relayed = match kind_of {
            Some(f) => stats.iter().all(|s| f(s.id) == Some(PathKind::Relay)),
            None => self.relay_only.load(Ordering::SeqCst),
        };
        if relayed {
            PathState::Relayed { relay: self.relay_label.clone(), rtt }
        } else {
            PathState::Direct { rtt, remote: None }
        }
    }

    /// Recomputes the route and notifies watchers when it changed.
    pub(super) fn update_state(&self) {
        let state = self.compute_state();
        self.path_tx.send_if_modified(|cur| {
            if cur.same_route(&state) {
                false
            } else {
                *cur = state;
                true
            }
        });
    }

    /// Tells the initiating link's path manager about a power change, or the session directly.
    pub(super) fn set_power(&self, state: letify_link::PowerState) {
        if let Some(tx) = self.cmds.lock().unwrap().as_ref() {
            let _ = tx.send(Cmd::Power(state));
        } else if let Some(s) = self.session() {
            s.set_power_state(state);
        }
    }

    /// Re-establish paths now. Only the side with a path manager can redial.
    pub(super) fn resume_now(&self) {
        if let Some(tx) = self.cmds.lock().unwrap().as_ref() {
            let _ = tx.send(Cmd::Resume);
        }
    }

    /// Closes the session from this side (it will not be redialed).
    pub(super) async fn close_local(&self, reason: Option<&str>) {
        self.local_close.store(true, Ordering::SeqCst);
        let tasks: Vec<AbortHandle> = self.tasks.lock().unwrap().drain(..).collect();
        // Stop the path manager first so it does not redial.
        *self.cmds.lock().unwrap() = None;
        for t in tasks {
            t.abort();
        }
        if let Some(session) = self.session() {
            match reason {
                Some(r) => session.close_with_reason(r).await,
                None => session.close().await,
            }
        }
    }

    /// The session ended: every connection on it ends too.
    pub(super) fn teardown(&self, session: &Session, err: LinkError) {
        self.ended.store(true, Ordering::SeqCst);
        let reason = if self.local_close.load(Ordering::SeqCst) {
            CloseReason::Local
        } else if let Some(r) = session.close_reason() {
            let code = if r == "revoked" { CLOSE_REVOKED } else { 0 };
            CloseReason::Remote { code, reason: r }
        } else {
            match err {
                LinkError::Closed => CloseReason::Remote { code: 0, reason: "session closed by peer".into() },
                other => CloseReason::Lost(other.to_string()),
            }
        };
        let conns: Vec<Arc<ConnShared>> = self.conns.lock().unwrap().drain().map(|(_, c)| c).collect();
        for c in conns {
            c.set_closed(reason.clone());
        }
        self.path_tx.send_replace(PathState::Unknown);
        let tasks: Vec<AbortHandle> = self.tasks.lock().unwrap().drain(..).collect();
        for t in tasks {
            t.abort();
        }
        *self.cmds.lock().unwrap() = None;
    }

    fn new_conn(self: &Arc<Self>, id: u32) -> Option<(Arc<ConnShared>, Arc<LConn>)> {
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(ConnShared {
            id,
            close: watch::channel(None).0,
            incoming_tx: in_tx,
            ctrl_w: Mutex::new(None),
            link: Arc::downgrade(self),
        });
        {
            let mut conns = self.conns.lock().unwrap();
            if self.is_ended() || conns.contains_key(&id) {
                return None;
            }
            conns.insert(id, shared.clone());
        }
        let conn = Arc::new(LConn { shared: shared.clone(), incoming: tokio::sync::Mutex::new(in_rx), link: self.clone() });
        Some((shared, conn))
    }

    /// Opens a connection for `service` to the peer on this link.
    pub(super) async fn open_conn(self: &Arc<Self>, service: &str) -> std::result::Result<Connection, OpenError> {
        let dead = |why: &str| OpenError::Dead(why.to_string());
        if service.is_empty() || service.len() > 255 {
            return Err(OpenError::Refused("service names are 1 to 255 bytes".into()));
        }
        let session = self.session().ok_or_else(|| dead("the session is gone"))?;
        let counter = self.next.fetch_add(1, Ordering::SeqCst);
        let id = (counter << 1) | u32::from(!self.initiator);
        let (shared, lconn) = self.new_conn(id).ok_or_else(|| dead("the session has ended"))?;
        let result = async {
            let mut stream = session.open_stream().await.map_err(|e| OpenError::Dead(e.to_string()))?;
            let mut buf = vec![KIND_CTRL];
            buf.extend_from_slice(&id.to_be_bytes());
            buf.push(service.len() as u8);
            buf.extend_from_slice(service.as_bytes());
            stream.write_all(&buf).await.map_err(|e| OpenError::Dead(e.to_string()))?;
            let mut reply = [0u8; 2];
            tokio::time::timeout(OPEN_TIMEOUT, stream.read_exact(&mut reply))
                .await
                .map_err(|_| OpenError::Dead("the peer did not answer the connection request".into()))?
                .map_err(|e| OpenError::Dead(e.to_string()))?;
            if reply[0] != 0 {
                let mut why = vec![0u8; reply[1] as usize];
                let _ = stream.read_exact(&mut why).await;
                return Err(OpenError::Refused(String::from_utf8_lossy(&why).into_owned()));
            }
            Ok(stream)
        }
        .await;
        let stream = match result {
            Ok(s) => s,
            Err(e) => {
                self.conns.lock().unwrap().remove(&id);
                return Err(e);
            }
        };
        let (r, w) = tokio::io::split(stream);
        *shared.ctrl_w.lock().unwrap() = Some(w);
        tokio::spawn(ctrl_reader(r, shared));
        Ok(Connection::new(self.peer, service, lconn))
    }

    /// The control stream of a connection the peer opened.
    async fn accept_conn(self: &Arc<Self>, id: u32, mut stream: Stream) {
        // The peer allocates ids of the other parity than ours.
        if (id & 1 == 1) == !self.initiator {
            return;
        }
        let mut len = [0u8; 1];
        match tokio::time::timeout(HEADER_TIMEOUT, stream.read_exact(&mut len)).await {
            Ok(Ok(_)) => {}
            _ => return,
        }
        let mut name = vec![0u8; len[0] as usize];
        match tokio::time::timeout(HEADER_TIMEOUT, stream.read_exact(&mut name)).await {
            Ok(Ok(_)) => {}
            _ => return,
        }
        let Ok(service) = String::from_utf8(name) else { return };
        let refuse = |why: &'static str| {
            let mut out = vec![1u8, why.len() as u8];
            out.extend_from_slice(why.as_bytes());
            out
        };
        let Some(tx) = self.services.sender(&service) else {
            let _ = stream.write_all(&refuse("service not available")).await;
            let _ = stream.shutdown().await;
            return;
        };
        let Some((shared, lconn)) = self.new_conn(id) else {
            let _ = stream.write_all(&refuse("connection id in use")).await;
            let _ = stream.shutdown().await;
            return;
        };
        if stream.write_all(&[0u8, 0u8]).await.is_err() {
            self.conns.lock().unwrap().remove(&id);
            return;
        }
        let (r, w) = tokio::io::split(stream);
        *shared.ctrl_w.lock().unwrap() = Some(w);
        tokio::spawn(ctrl_reader(r, shared.clone()));
        let conn = Connection::new(self.peer, &service, lconn);
        if tx.send(conn).await.is_err() {
            // The listener went away between the check and the delivery.
            shared.close_local(1, "service stopped listening");
        }
    }
}

/// Runs for the life of a session: routes every stream the peer opens.
pub(super) async fn run_demux(link: Arc<PeerLink>, session: Session) {
    let end = loop {
        match session.accept_stream().await {
            Ok(stream) => {
                tokio::spawn(handle_stream(link.clone(), stream));
            }
            Err(e) => break e,
        }
    };
    link.teardown(&session, end);
}

/// Keeps the route of a link up to date for [`Connection::watch_path`].
pub(super) async fn run_path_watch(link: Arc<PeerLink>, session: Session) {
    let mut changes = session.subflow_changes();
    drop(session);
    link.update_state();
    while changes.changed().await.is_ok() {
        if link.is_ended() {
            break;
        }
        link.update_state();
    }
}

async fn handle_stream(link: Arc<PeerLink>, mut stream: Stream) {
    let mut head = [0u8; 5];
    match tokio::time::timeout(HEADER_TIMEOUT, stream.read_exact(&mut head)).await {
        Ok(Ok(_)) => {}
        _ => return,
    }
    let id = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
    match head[0] {
        KIND_CTRL => link.accept_conn(id, stream).await,
        KIND_DATA => {
            let shared = link.conns.lock().unwrap().get(&id).cloned();
            if let Some(shared) = shared {
                if !shared.is_closed() {
                    let _ = shared.incoming_tx.send(shared.wrap(stream));
                }
            }
        }
        _ => {}
    }
}

/// The state of one connection that the link's tables and tasks share.
pub(super) struct ConnShared {
    id: u32,
    close: watch::Sender<Option<CloseReason>>,
    incoming_tx: mpsc::UnboundedSender<BiStream>,
    ctrl_w: Mutex<Option<WriteHalf<Stream>>>,
    link: Weak<PeerLink>,
}

impl ConnShared {
    fn is_closed(&self) -> bool {
        self.close.borrow().is_some()
    }

    fn reason(&self) -> Option<CloseReason> {
        self.close.borrow().clone()
    }

    /// Records why the connection ended. True when this call ended it.
    fn set_closed(&self, reason: CloseReason) -> bool {
        self.close.send_if_modified(|state| {
            if state.is_none() {
                *state = Some(reason);
                true
            } else {
                false
            }
        })
    }

    fn forget(&self) {
        if let Some(link) = self.link.upgrade() {
            link.conns.lock().unwrap().remove(&self.id);
        }
    }

    /// Closes the connection from this side and tells the peer, without waiting.
    pub(super) fn close_local(&self, code: u32, reason: &str) {
        if !self.set_closed(CloseReason::Local) {
            return;
        }
        let w = self.ctrl_w.lock().unwrap().take();
        if let Some(mut w) = w {
            let mut frame = vec![FRAME_CLOSE];
            frame.extend_from_slice(&code.to_be_bytes());
            let reason = &reason.as_bytes()[..reason.len().min(u16::MAX as usize)];
            frame.extend_from_slice(&(reason.len() as u16).to_be_bytes());
            frame.extend_from_slice(reason);
            // Queue the frame at once, so that it goes out before a session close that follows
            // (a revoke). A stream has room for a few bytes unless its window is exhausted; if
            // so the rest is written by a task.
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            let mut off = 0;
            while off < frame.len() {
                match Pin::new(&mut w).poll_write(&mut cx, &frame[off..]) {
                    Poll::Ready(Ok(n)) if n > 0 => off += n,
                    _ => break,
                }
            }
            if off == frame.len() {
                let _ = Pin::new(&mut w).poll_shutdown(&mut cx);
            } else if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = w.write_all(&frame[off..]).await;
                    let _ = w.shutdown().await;
                });
            }
        }
        self.forget();
    }

    /// Wraps a letify stream for the application, ending it when the connection closes.
    fn wrap(&self, stream: Stream) -> BiStream {
        let (r, w) = tokio::io::split(stream);
        BiStream::new(Guarded::new(w, self.close.subscribe()), Guarded::new(r, self.close.subscribe()))
    }
}

/// Reads the control stream until the peer closes the connection or drops it.
async fn ctrl_reader(mut r: ReadHalf<Stream>, shared: Arc<ConnShared>) {
    let mut tag = [0u8; 1];
    let reason = match r.read_exact(&mut tag).await {
        Ok(_) if tag[0] == FRAME_CLOSE => {
            let mut head = [0u8; 6];
            match r.read_exact(&mut head).await {
                Ok(_) => {
                    let code = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
                    let mut why = vec![0u8; u16::from_be_bytes([head[4], head[5]]) as usize];
                    let _ = r.read_exact(&mut why).await;
                    CloseReason::Remote { code, reason: String::from_utf8_lossy(&why).into_owned() }
                }
                Err(e) => CloseReason::Lost(e.to_string()),
            }
        }
        Ok(_) => CloseReason::Lost("protocol error on the control stream".into()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            // The whole session may have been closed by the peer (a revoke): then its reason
            // is the better answer than "dropped".
            let from_session = shared.link.upgrade().and_then(|l| l.session()).and_then(|s| s.close_reason());
            match from_session {
                Some(r) => CloseReason::Remote { code: if r == "revoked" { CLOSE_REVOKED } else { 0 }, reason: r },
                None => CloseReason::Remote { code: 0, reason: "connection dropped".into() },
            }
        }
        Err(e) => CloseReason::Lost(e.to_string()),
    };
    shared.set_closed(reason);
    // The peer ended its half; finish ours so it sees the end of the stream too.
    let w = shared.ctrl_w.lock().unwrap().take();
    if let Some(mut w) = w {
        let _ = w.shutdown().await;
    }
    shared.forget();
}

/// An Ember connection on a link.
pub(super) struct LConn {
    shared: Arc<ConnShared>,
    incoming: tokio::sync::Mutex<mpsc::UnboundedReceiver<BiStream>>,
    link: Arc<PeerLink>,
}

impl Drop for LConn {
    fn drop(&mut self) {
        // Last handle gone: the connection ends, as with QUIC.
        self.shared.close_local(0, "connection dropped");
    }
}

impl LConn {
    async fn open(&self, priority: Priority) -> Result<BiStream> {
        if let Some(r) = self.shared.reason() {
            return Err(TransportError::Closed(r));
        }
        let session = self
            .link
            .session()
            .ok_or_else(|| TransportError::Closed(CloseReason::Lost("session gone".into())))?;
        let mut stream = session
            .open_stream_with(priority)
            .await
            .map_err(|e| TransportError::Stream(e.to_string()))?;
        let mut head = [0u8; 5];
        head[0] = KIND_DATA;
        head[1..].copy_from_slice(&self.shared.id.to_be_bytes());
        stream.write_all(&head).await.map_err(|e| TransportError::Stream(e.to_string()))?;
        Ok(self.shared.wrap(stream))
    }
}

#[async_trait]
impl ConnectionBackend for LConn {
    async fn open_bi(&self) -> Result<BiStream> {
        self.open(Priority::Interactive).await
    }

    async fn open_bi_bulk(&self) -> Result<BiStream> {
        self.open(Priority::Bulk).await
    }

    async fn accept_bi(&self) -> Result<BiStream> {
        let mut close = self.shared.close.subscribe();
        let mut incoming = self.incoming.lock().await;
        loop {
            if let Some(r) = self.shared.reason() {
                return Err(TransportError::Closed(r));
            }
            tokio::select! {
                s = incoming.recv() => {
                    return s.ok_or_else(|| TransportError::Closed(CloseReason::Lost("session gone".into())));
                }
                _ = close.changed() => {}
            }
        }
    }

    fn path_state(&self) -> PathState {
        if self.shared.is_closed() {
            return PathState::Unknown;
        }
        self.link.compute_state()
    }

    fn watch_path(&self) -> watch::Receiver<PathState> {
        self.link.path_tx.subscribe()
    }

    fn close(&self, code: u32, reason: &str) {
        self.shared.close_local(code, reason);
    }

    async fn closed(&self) -> CloseReason {
        let mut rx = self.shared.close.subscribe();
        match rx.wait_for(|state| state.is_some()).await {
            Ok(state) => (*state).clone().unwrap_or(CloseReason::Local),
            Err(_) => CloseReason::Local,
        }
    }
}

/// A stream half that ends (reads EOF, writes fail) once its connection closed.
struct Guarded<T> {
    inner: T,
    closed: Mutex<Pin<Box<dyn Future<Output = ()> + Send>>>,
    done: bool,
}

impl<T> Guarded<T> {
    fn new(inner: T, rx: watch::Receiver<Option<CloseReason>>) -> Self {
        let fut = async move {
            let mut rx = rx;
            let _ = rx.wait_for(|state| state.is_some()).await;
        };
        Self { inner, closed: Mutex::new(Box::pin(fut)), done: false }
    }

    /// Polls the close signal (registering the waker); true once the connection closed.
    fn is_done(&mut self, cx: &mut Context<'_>) -> bool {
        if !self.done {
            let fut = self.closed.get_mut().unwrap_or_else(|e| e.into_inner());
            if fut.as_mut().poll(cx).is_ready() {
                self.done = true;
            }
        }
        self.done
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Guarded<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.is_done(cx) {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Guarded<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        if self.is_done(cx) {
            return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
