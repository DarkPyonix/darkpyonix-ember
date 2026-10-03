//! A thin Chrome DevTools Protocol client over one WebSocket.
//!
//! Ember needs about a dozen CDP methods (target attach, screencast, input, navigation, browser
//! close) and must also relay raw CDP for agents. A typed client such as `chromiumoxide` generates
//! bindings for the whole protocol (a long compile and a pinned protocol revision) and owns the
//! connection through its own handler; here a JSON request/response map is all that is needed, and
//! it never lags behind the installed Chrome. Flat sessions (`Target.attachToTarget` with
//! `flatten: true`) let one connection drive any number of pages via `sessionId`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// One CDP event.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A connection to a browser's DevTools endpoint.
pub struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<CdpEvent>,
    closed: Arc<AtomicBool>,
}

/// How long one CDP call may take before it fails.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

impl Cdp {
    /// Connect to a `ws://…/devtools/browser/…` (or page) endpoint.
    pub async fn connect(ws_url: &str) -> anyhow::Result<Arc<Cdp>> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .with_context(|| format!("connecting to {ws_url}"))?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        // Frames arrive at tens per second; a slow subscriber lags rather than blocking the reader.
        let (events, _) = broadcast::channel(512);

        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = closed.clone();
        let reader_pending = pending.clone();
        let reader_events = events.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    let tx = reader_pending.lock().unwrap().remove(&id);
                    if let Some(tx) = tx {
                        let res = match v.get("error") {
                            Some(e) => Err(e.to_string()),
                            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(res);
                    }
                } else if let Some(method) = v.get("method").and_then(Value::as_str) {
                    let _ = reader_events.send(CdpEvent {
                        method: method.to_string(),
                        params: v.get("params").cloned().unwrap_or(Value::Null),
                        session_id: v.get("sessionId").and_then(Value::as_str).map(String::from),
                    });
                }
            }
            // Connection gone: fail every waiter. Dropping the senders does that.
            reader_closed.store(true, Ordering::SeqCst);
            reader_pending.lock().unwrap().clear();
        });

        Ok(Arc::new(Cdp { out, pending, next_id: AtomicU64::new(1), events, closed }))
    }

    /// Subscribe to events. The receiver closes when the connection ends.
    pub fn events(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Is the connection still open?
    pub fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && !self.out.is_closed()
    }

    /// Call `method` on the browser (`session = None`) or on an attached target.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if !self.is_open() || self.out.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("CDP connection closed");
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("{method}: {e}")),
            Ok(Err(_)) => Err(anyhow!("{method}: CDP connection closed")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("{method}: timed out"))
            }
        }
    }

    /// Fire a call without waiting for its result (screencast acks).
    pub fn send_nowait(&self, method: &str, params: Value, session: Option<&str>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let _ = self.out.send(msg.to_string());
    }
}
