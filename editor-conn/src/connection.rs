//! Async driver for [`PersistentProtocol`] over any `AsyncRead + AsyncWrite` transport.
//!
//! One [`Connection`] outlives its sockets: on reconnect a new transport is attached with
//! [`ConnectionHandle::replace_transport`] and the protocol state (ids, unacked queue) carries
//! over, exactly like `PersistentProtocol.beginAcceptReconnection` upstream.
//!
//! Regular messages go to [`Connection::events`]; control (handshake) messages go to a separate
//! queue read by [`ConnectionHandle::recv_control`], so a reconnect handshake can run while the
//! IPC / RPC dispatcher keeps owning the event stream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::frame::{Frame, FrameDecoder};
use crate::protocol::{Inbound, PersistentProtocol, KEEP_ALIVE_SEND_TIME};
use crate::{Error, Result};

/// Why the current socket is considered gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostReason {
    /// EOF or I/O error on the socket.
    SocketClosed,
    /// No incoming data for `TIMEOUT_TIME` (20 s).
    Timeout,
}

/// Events for the layer above (IPC client or RPC peer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnEvent {
    /// An in-order regular message.
    Message(Vec<u8>),
    /// The peer sent `Disconnect`; do not reconnect.
    Disconnected,
    /// The socket is gone; the owner may reconnect with the same reconnection token.
    Lost(LostReason),
}

struct Inner {
    state: Mutex<PersistentProtocol>,
    writer: Mutex<Option<mpsc::UnboundedSender<Vec<Frame>>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    generation: AtomicU64,
    events_tx: mpsc::UnboundedSender<ConnEvent>,
    control_tx: mpsc::UnboundedSender<Vec<u8>>,
    control_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
}

/// Cloneable sending side of a connection.
#[derive(Clone)]
pub struct ConnectionHandle {
    inner: Arc<Inner>,
}

/// A persistent connection: the handle plus the stream of regular-message events.
pub struct Connection {
    pub handle: ConnectionHandle,
    pub events: mpsc::UnboundedReceiver<ConnEvent>,
}

impl Connection {
    /// Start driving `stream`. `initial` holds bytes already read past the HTTP upgrade.
    pub fn new<S>(stream: S, initial: Vec<u8>) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            state: Mutex::new(PersistentProtocol::new(Instant::now())),
            writer: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            events_tx,
            control_tx,
            control_rx: tokio::sync::Mutex::new(control_rx),
        });
        let handle = ConnectionHandle { inner };
        handle.attach(stream, initial);
        Connection { handle, events }
    }
}

impl ConnectionHandle {
    /// Send a regular (counted, acknowledged, replayable) message.
    pub fn send(&self, data: Vec<u8>) {
        let frames = self.inner.state.lock().unwrap().send(data, Instant::now());
        self.write(frames);
    }

    /// Send a control (handshake) message.
    pub fn send_control(&self, data: Vec<u8>) {
        let frames = self.inner.state.lock().unwrap().send_control(data);
        self.write(frames);
    }

    /// Wait for the next control message.
    pub async fn recv_control(&self) -> Result<Vec<u8>> {
        self.inner.control_rx.lock().await.recv().await.ok_or(Error::Closed)
    }

    /// Discard control messages left over from a previous handshake.
    pub async fn drain_control(&self) {
        let mut rx = self.inner.control_rx.lock().await;
        while rx.try_recv().is_ok() {}
    }

    /// Send `Disconnect` and stop all tasks.
    pub fn close(&self) {
        let frames = self.inner.state.lock().unwrap().send_disconnect();
        self.write(frames);
        // Dropping the writer's sender lets it flush what is queued (including the Disconnect)
        // and exit; the reader and ticker are aborted.
        self.inner.writer.lock().unwrap().take();
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        for t in self.inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }

    /// Attach a fresh transport for a reconnect. Regular sends are queued until
    /// [`Self::finish_reconnect`].
    pub fn replace_transport<S>(&self, stream: S, initial: Vec<u8>)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        self.inner.state.lock().unwrap().begin_reconnect(Instant::now());
        self.attach(stream, initial);
    }

    /// The reconnect handshake succeeded: re-ack and replay unacknowledged messages.
    pub fn finish_reconnect(&self) {
        let frames = self.inner.state.lock().unwrap().end_reconnect(Instant::now());
        self.write(frames);
    }

    pub fn is_reconnecting(&self) -> bool {
        self.inner.state.lock().unwrap().is_reconnecting()
    }

    fn write(&self, frames: Vec<Frame>) {
        if frames.is_empty() {
            return;
        }
        if let Some(tx) = self.inner.writer.lock().unwrap().as_ref() {
            let _ = tx.send(frames);
        }
    }

    fn attach<S>(&self, stream: S, initial: Vec<u8>)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let gen = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        for t in self.inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }

        let (mut rd, mut wr) = tokio::io::split(stream);
        let (wtx, mut wrx) = mpsc::unbounded_channel::<Vec<Frame>>();
        // Replacing the sender ends the previous writer once it has drained its queue.
        *self.inner.writer.lock().unwrap() = Some(wtx);

        // Writer: encode and write batches of frames. Detached: it ends when its sender is
        // dropped (transport replaced or connection closed), after flushing what was queued.
        let inner_w = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            while let Some(frames) = wrx.recv().await {
                let mut buf = Vec::new();
                for f in &frames {
                    f.encode_into(&mut buf);
                }
                if wr.write_all(&buf).await.is_err() || wr.flush().await.is_err() {
                    if let Some(inner) = inner_w.upgrade() {
                        if inner.generation.load(Ordering::SeqCst) == gen {
                            let _ = inner.events_tx.send(ConnEvent::Lost(LostReason::SocketClosed));
                        }
                    }
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });

        // Reader: decode frames, run them through the state machine.
        let inner_r = Arc::clone(&self.inner);
        let this = self.clone();
        let reader = tokio::spawn(async move {
            let mut dec = FrameDecoder::new();
            dec.push(&initial);
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                loop {
                    let frame = match dec.next_frame() {
                        Ok(Some(f)) => f,
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!("editor-conn: dropping socket: {e}");
                            if inner_r.generation.load(Ordering::SeqCst) == gen {
                                let _ = inner_r.events_tx.send(ConnEvent::Lost(LostReason::SocketClosed));
                            }
                            return;
                        }
                    };
                    let (inbound, out) = inner_r.state.lock().unwrap().receive(frame, Instant::now());
                    this.write(out);
                    match inbound {
                        Some(Inbound::Regular(d)) => {
                            let _ = inner_r.events_tx.send(ConnEvent::Message(d));
                        }
                        Some(Inbound::Control(d)) => {
                            let _ = inner_r.control_tx.send(d);
                        }
                        Some(Inbound::Disconnect) => {
                            let _ = inner_r.events_tx.send(ConnEvent::Disconnected);
                        }
                        None => {}
                    }
                }
                match rd.read(&mut chunk).await {
                    Ok(0) | Err(_) => {
                        if inner_r.generation.load(Ordering::SeqCst) == gen {
                            let _ = inner_r.events_tx.send(ConnEvent::Lost(LostReason::SocketClosed));
                        }
                        return;
                    }
                    Ok(n) => dec.push(&chunk[..n]),
                }
            }
        });

        // Ticker: delayed acks, keep-alives, timeout detection. Holds only a weak reference so
        // a dropped connection does not keep it alive.
        let weak_t = Arc::downgrade(&self.inner);
        let ticker = tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(500));
            let mut last_keep_alive = Instant::now();
            loop {
                iv.tick().await;
                let Some(inner_t) = weak_t.upgrade() else { return };
                if inner_t.generation.load(Ordering::SeqCst) != gen {
                    return;
                }
                let this_t = ConnectionHandle { inner: Arc::clone(&inner_t) };
                let now = Instant::now();
                let (mut out, dead) = inner_t.state.lock().unwrap().tick(now);
                if now.duration_since(last_keep_alive) >= KEEP_ALIVE_SEND_TIME {
                    last_keep_alive = now;
                    let mut st = inner_t.state.lock().unwrap();
                    if !st.is_reconnecting() {
                        out.extend(st.keep_alive());
                    }
                }
                this_t.write(out);
                if dead {
                    let _ = inner_t.events_tx.send(ConnEvent::Lost(LostReason::Timeout));
                }
            }
        });

        self.inner.tasks.lock().unwrap().extend([reader, ticker]);
    }
}
