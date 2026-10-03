//! The renderer ↔ extension-host RPC protocol (`rpcProtocol.ts`), Rust side.
//!
//! Source: `src/vs/workbench/services/extensions/common/rpcProtocol.ts` at the pinned commit:
//! `MessageType` L940-953, `ArgType` L955-960, `MessageBuffer` L516-700, `MessageIO` L714-938,
//! `_receiveOneMessage` L280-356, `_receiveRequest` L358-401 (acks every request immediately),
//! `_remoteCall` L461-510.
//!
//! Message layout (all integers big-endian):
//!
//! ```text
//! type u8 | req u32 | body
//!   Request JSON  (1, 2=with cancellation): rpcId u8 | method shortString | args longString(JSON array)
//!   Request Mixed (3, 4=with cancellation): rpcId u8 | method shortString | mixedArray
//!   Acknowledged (5), Cancel (6), ReplyOKEmpty (7), ReplyErrEmpty (12): no body
//!   ReplyOKVSBuffer (8): u32 len | bytes
//!   ReplyOKJSON (9): longString(JSON)
//!   ReplyOKJSONWithBuffers (10): u32 count | longString(JSON) | count × (u32 len | bytes)
//!   ReplyErrError (11): longString(JSON of transformErrorForSerialization(err))
//! shortString = u8 len | utf8; longString = u32 len | utf8
//! mixedArray  = u8 count | count × (argType u8 | payload)
//!   argType 1 String (longString JSON), 2 VSBuffer (u32 len | bytes),
//!           3 SerializedObjectWithBuffers (u32 count | longString | count × buffer), 4 Undefined
//! ```
//!
//! Request ids are per-direction counters starting at 1. A trailing `CancellationToken` argument
//! is never serialized: it selects the "WithCancellation" message type, and the receiver appends a
//! fresh token (`_receiveRequest` L364-367).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::connection::{ConnEvent, ConnectionHandle, LostReason};
use crate::rpc_ids;
use crate::{Error, RemoteError, Result};

mod msg_type {
    pub const REQUEST_JSON_ARGS: u8 = 1;
    pub const REQUEST_JSON_ARGS_WITH_CANCELLATION: u8 = 2;
    pub const REQUEST_MIXED_ARGS: u8 = 3;
    pub const REQUEST_MIXED_ARGS_WITH_CANCELLATION: u8 = 4;
    pub const ACKNOWLEDGED: u8 = 5;
    pub const CANCEL: u8 = 6;
    pub const REPLY_OK_EMPTY: u8 = 7;
    pub const REPLY_OK_VSBUFFER: u8 = 8;
    pub const REPLY_OK_JSON: u8 = 9;
    pub const REPLY_OK_JSON_WITH_BUFFERS: u8 = 10;
    pub const REPLY_ERR_ERROR: u8 = 11;
    pub const REPLY_ERR_EMPTY: u8 = 12;
}

mod arg_type {
    pub const STRING: u8 = 1;
    pub const VSBUFFER: u8 = 2;
    pub const SERIALIZED_OBJECT_WITH_BUFFERS: u8 = 3;
    pub const UNDEFINED: u8 = 4;
}

/// One positional argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Json(Value),
    /// JS `undefined` (distinct from `null`; forces mixed encoding).
    Undefined,
    /// A `VSBuffer`.
    Buffer(Vec<u8>),
    /// `SerializableObjectWithBuffers`: JSON whose `{"$$ref$$": i}` nodes point into `buffers`
    /// (`{"$$ref$$": -1}` encodes a nested `undefined`).
    JsonWithBuffers { value: Value, buffers: Vec<Vec<u8>> },
}

impl Arg {
    pub fn json<T: serde::Serialize>(v: &T) -> Result<Self> {
        Ok(Self::Json(serde_json::to_value(v)?))
    }

    pub fn as_json(&self) -> Option<&Value> {
        match self {
            Self::Json(v) | Self::JsonWithBuffers { value: v, .. } => Some(v),
            _ => None,
        }
    }
}

/// A successful reply payload.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// `undefined`.
    Empty,
    Json(Value),
    Buffer(Vec<u8>),
    JsonWithBuffers { value: Value, buffers: Vec<Vec<u8>> },
}

impl Reply {
    pub fn into_json(self) -> Value {
        match self {
            Self::Empty | Self::Buffer(_) => Value::Null,
            Self::Json(v) | Self::JsonWithBuffers { value: v, .. } => v,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RpcMessage {
    Request { req: u32, rpc_id: u8, method: String, args: Vec<Arg>, cancellable: bool },
    Acknowledged { req: u32 },
    Cancel { req: u32 },
    ReplyOk { req: u32, reply: Reply },
    /// `None` = `ReplyErrEmpty`.
    ReplyErr { req: u32, error: Option<Value> },
}

// ---- encoding -------------------------------------------------------------------------------

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_short_string(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let b = s.as_bytes();
    if b.len() > 255 {
        return Err(Error::Malformed(format!("method name too long: {s}")));
    }
    out.push(b.len() as u8);
    out.extend_from_slice(b);
    Ok(())
}

fn put_long_string(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn put_buffer(out: &mut Vec<u8>, b: &[u8]) {
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

impl RpcMessage {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match self {
            Self::Request { req, rpc_id, method, args, cancellable } => {
                let simple = args.iter().all(|a| matches!(a, Arg::Json(_)));
                let ty = match (simple, cancellable) {
                    (true, false) => msg_type::REQUEST_JSON_ARGS,
                    (true, true) => msg_type::REQUEST_JSON_ARGS_WITH_CANCELLATION,
                    (false, false) => msg_type::REQUEST_MIXED_ARGS,
                    (false, true) => msg_type::REQUEST_MIXED_ARGS_WITH_CANCELLATION,
                };
                out.push(ty);
                put_u32(&mut out, *req);
                out.push(*rpc_id);
                put_short_string(&mut out, method)?;
                if simple {
                    let arr: Vec<&Value> = args.iter().filter_map(Arg::as_json).collect();
                    put_long_string(&mut out, &serde_json::to_string(&arr)?);
                } else {
                    if args.len() > 255 {
                        return Err(Error::Malformed("more than 255 mixed args".into()));
                    }
                    out.push(args.len() as u8);
                    for a in args {
                        match a {
                            Arg::Json(v) => {
                                out.push(arg_type::STRING);
                                put_long_string(&mut out, &serde_json::to_string(v)?);
                            }
                            Arg::Buffer(b) => {
                                out.push(arg_type::VSBUFFER);
                                put_buffer(&mut out, b);
                            }
                            Arg::JsonWithBuffers { value, buffers } => {
                                out.push(arg_type::SERIALIZED_OBJECT_WITH_BUFFERS);
                                put_u32(&mut out, buffers.len() as u32);
                                put_long_string(&mut out, &serde_json::to_string(value)?);
                                for b in buffers {
                                    put_buffer(&mut out, b);
                                }
                            }
                            Arg::Undefined => out.push(arg_type::UNDEFINED),
                        }
                    }
                }
            }
            Self::Acknowledged { req } => {
                out.push(msg_type::ACKNOWLEDGED);
                put_u32(&mut out, *req);
            }
            Self::Cancel { req } => {
                out.push(msg_type::CANCEL);
                put_u32(&mut out, *req);
            }
            Self::ReplyOk { req, reply } => match reply {
                Reply::Empty => {
                    out.push(msg_type::REPLY_OK_EMPTY);
                    put_u32(&mut out, *req);
                }
                Reply::Buffer(b) => {
                    out.push(msg_type::REPLY_OK_VSBUFFER);
                    put_u32(&mut out, *req);
                    put_buffer(&mut out, b);
                }
                Reply::Json(v) => {
                    out.push(msg_type::REPLY_OK_JSON);
                    put_u32(&mut out, *req);
                    put_long_string(&mut out, &serde_json::to_string(v)?);
                }
                Reply::JsonWithBuffers { value, buffers } => {
                    out.push(msg_type::REPLY_OK_JSON_WITH_BUFFERS);
                    put_u32(&mut out, *req);
                    put_u32(&mut out, buffers.len() as u32);
                    put_long_string(&mut out, &serde_json::to_string(value)?);
                    for b in buffers {
                        put_buffer(&mut out, b);
                    }
                }
            },
            Self::ReplyErr { req, error } => match error {
                Some(e) => {
                    out.push(msg_type::REPLY_ERR_ERROR);
                    put_u32(&mut out, *req);
                    put_long_string(&mut out, &serde_json::to_string(e)?);
                }
                None => {
                    out.push(msg_type::REPLY_ERR_EMPTY);
                    put_u32(&mut out, *req);
                }
            },
        }
        Ok(out)
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader { buf, pos: 0 };
        let ty = r.u8()?;
        let req = r.u32()?;
        Ok(match ty {
            msg_type::REQUEST_JSON_ARGS | msg_type::REQUEST_JSON_ARGS_WITH_CANCELLATION => {
                let rpc_id = r.u8()?;
                let method = r.short_string()?;
                let args: Vec<Value> = serde_json::from_str(&r.long_string()?)?;
                Self::Request {
                    req,
                    rpc_id,
                    method,
                    args: args.into_iter().map(Arg::Json).collect(),
                    cancellable: ty == msg_type::REQUEST_JSON_ARGS_WITH_CANCELLATION,
                }
            }
            msg_type::REQUEST_MIXED_ARGS | msg_type::REQUEST_MIXED_ARGS_WITH_CANCELLATION => {
                let rpc_id = r.u8()?;
                let method = r.short_string()?;
                let n = r.u8()? as usize;
                let mut args = Vec::with_capacity(n);
                for _ in 0..n {
                    args.push(match r.u8()? {
                        arg_type::STRING => Arg::Json(serde_json::from_str(&r.long_string()?)?),
                        arg_type::VSBUFFER => Arg::Buffer(r.buffer()?),
                        arg_type::SERIALIZED_OBJECT_WITH_BUFFERS => {
                            let count = r.u32()? as usize;
                            let value = serde_json::from_str(&r.long_string()?)?;
                            let mut buffers = Vec::with_capacity(count.min(1024));
                            for _ in 0..count {
                                buffers.push(r.buffer()?);
                            }
                            Arg::JsonWithBuffers { value, buffers }
                        }
                        arg_type::UNDEFINED => Arg::Undefined,
                        other => return Err(Error::Malformed(format!("unknown arg type {other}"))),
                    });
                }
                Self::Request {
                    req,
                    rpc_id,
                    method,
                    args,
                    cancellable: ty == msg_type::REQUEST_MIXED_ARGS_WITH_CANCELLATION,
                }
            }
            msg_type::ACKNOWLEDGED => Self::Acknowledged { req },
            msg_type::CANCEL => Self::Cancel { req },
            msg_type::REPLY_OK_EMPTY => Self::ReplyOk { req, reply: Reply::Empty },
            msg_type::REPLY_OK_VSBUFFER => Self::ReplyOk { req, reply: Reply::Buffer(r.buffer()?) },
            msg_type::REPLY_OK_JSON => {
                Self::ReplyOk { req, reply: Reply::Json(serde_json::from_str(&r.long_string()?)?) }
            }
            msg_type::REPLY_OK_JSON_WITH_BUFFERS => {
                let count = r.u32()? as usize;
                let value = serde_json::from_str(&r.long_string()?)?;
                let mut buffers = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    buffers.push(r.buffer()?);
                }
                Self::ReplyOk { req, reply: Reply::JsonWithBuffers { value, buffers } }
            }
            msg_type::REPLY_ERR_ERROR => {
                Self::ReplyErr { req, error: Some(serde_json::from_str(&r.long_string()?)?) }
            }
            msg_type::REPLY_ERR_EMPTY => Self::ReplyErr { req, error: None },
            other => return Err(Error::Malformed(format!("unknown RPC message type {other}"))),
        })
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len());
        let end = end.ok_or_else(|| Error::Malformed("truncated RPC message".into()))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn short_string(&mut self) -> Result<String> {
        let n = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn long_string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn buffer(&mut self) -> Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}

// ---- peer -----------------------------------------------------------------------------------

/// A request from the extension host to a `MainThread*` actor implemented by Ember.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingRequest {
    pub req: u32,
    pub rpc_id: u8,
    /// Resolved through [`rpc_ids`] (pinned numbering).
    pub proxy: Option<&'static str>,
    pub method: String,
    pub args: Vec<Arg>,
    pub cancellable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RpcEvent {
    Request(IncomingRequest),
    /// The extension host cancelled one of its requests.
    Cancel(u32),
    Lost(LostReason),
    Disconnected,
}

type PendingReply = oneshot::Sender<std::result::Result<Reply, RemoteError>>;

struct Shared {
    last_req: AtomicU32,
    pending: Mutex<HashMap<u32, PendingReply>>,
}

/// One side of the RPC protocol, on the extension-host connection after initialization.
#[derive(Clone)]
pub struct RpcPeer {
    handle: ConnectionHandle,
    shared: Arc<Shared>,
}

/// An outstanding call; `req` can be passed to [`RpcPeer::cancel`].
pub struct PendingCall {
    pub req: u32,
    rx: oneshot::Receiver<std::result::Result<Reply, RemoteError>>,
}

impl PendingCall {
    pub async fn wait(self) -> Result<Reply> {
        match self.rx.await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(Error::Remote(e)),
            Err(_) => Err(Error::Closed),
        }
    }
}

impl RpcPeer {
    pub fn start(
        handle: ConnectionHandle,
        mut events: mpsc::UnboundedReceiver<ConnEvent>,
    ) -> (Self, mpsc::UnboundedReceiver<RpcEvent>) {
        let shared = Arc::new(Shared { last_req: AtomicU32::new(0), pending: Mutex::new(HashMap::new()) });
        let (tx, rx) = mpsc::unbounded_channel();
        let peer = Self { handle: handle.clone(), shared: Arc::clone(&shared) };
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                match ev {
                    ConnEvent::Message(raw) => match RpcMessage::decode(&raw) {
                        Ok(msg) => on_message(msg, &shared, &handle, &tx),
                        Err(e) => tracing::warn!("editor-conn rpc: bad message: {e}"),
                    },
                    ConnEvent::Lost(r) => {
                        let _ = tx.send(RpcEvent::Lost(r));
                    }
                    ConnEvent::Disconnected => {
                        let _ = tx.send(RpcEvent::Disconnected);
                        break;
                    }
                }
            }
            for (_, p) in shared.pending.lock().unwrap().drain() {
                let _ = p.send(Err(RemoteError::from_json(serde_json::json!({
                    "name": "Canceled", "message": "extension host connection closed"
                }))));
            }
        });
        (peer, rx)
    }

    /// Start a call to an `ExtHost*` actor by name.
    pub fn start_call(&self, proxy: &str, method: &str, args: Vec<Arg>, cancellable: bool) -> Result<PendingCall> {
        let rpc_id = rpc_ids::id_of(proxy).ok_or_else(|| Error::UnknownProxy(proxy.to_owned()))?;
        let req = self.shared.last_req.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(req, tx);
        let msg = RpcMessage::Request { req, rpc_id, method: method.to_owned(), args, cancellable };
        self.handle.send(msg.encode()?);
        Ok(PendingCall { req, rx })
    }

    pub async fn call(&self, proxy: &str, method: &str, args: Vec<Arg>) -> Result<Reply> {
        self.start_call(proxy, method, args, false)?.wait().await
    }

    /// Fire a call whose reply we do not need (`void` methods upstream still get a reply).
    pub fn fire(&self, proxy: &str, method: &str, args: Vec<Arg>) -> Result<()> {
        let pending = self.start_call(proxy, method, args, false)?;
        self.shared.pending.lock().unwrap().remove(&pending.req);
        Ok(())
    }

    /// Cancel a cancellable call (`MessageType.Cancel`); the pending reply is dropped.
    pub fn cancel(&self, req: u32) {
        self.shared.pending.lock().unwrap().remove(&req);
        if let Ok(b) = (RpcMessage::Cancel { req }).encode() {
            self.handle.send(b);
        }
    }

    /// Answer an [`IncomingRequest`]. `Err(Some(v))` sends `ReplyErrError` with `v`
    /// (shape of `transformErrorForSerialization`: `{ $isError: true, name, message, stack }`).
    pub fn respond(&self, req: u32, result: std::result::Result<Reply, Option<Value>>) {
        let msg = match result {
            Ok(reply) => RpcMessage::ReplyOk { req, reply },
            Err(error) => RpcMessage::ReplyErr { req, error },
        };
        match msg.encode() {
            Ok(b) => self.handle.send(b),
            Err(e) => tracing::warn!("editor-conn rpc: cannot encode reply: {e}"),
        }
    }
}

/// A serialized JS error as `transformErrorForSerialization` produces it.
pub fn js_error(name: &str, message: &str) -> Value {
    serde_json::json!({ "$isError": true, "name": name, "message": message, "stack": null })
}

fn on_message(msg: RpcMessage, shared: &Shared, handle: &ConnectionHandle, tx: &mpsc::UnboundedSender<RpcEvent>) {
    match msg {
        RpcMessage::Request { req, rpc_id, method, args, cancellable } => {
            // `_receiveRequest` acknowledges before running the handler.
            if let Ok(b) = (RpcMessage::Acknowledged { req }).encode() {
                handle.send(b);
            }
            let _ = tx.send(RpcEvent::Request(IncomingRequest {
                req,
                rpc_id,
                proxy: rpc_ids::name_of(rpc_id),
                method,
                args,
                cancellable,
            }));
        }
        RpcMessage::Acknowledged { .. } => {}
        RpcMessage::Cancel { req } => {
            let _ = tx.send(RpcEvent::Cancel(req));
        }
        RpcMessage::ReplyOk { req, reply } => {
            if let Some(p) = shared.pending.lock().unwrap().remove(&req) {
                let _ = p.send(Ok(reply));
            }
        }
        RpcMessage::ReplyErr { req, error } => {
            if let Some(p) = shared.pending.lock().unwrap().remove(&req) {
                let _ = p.send(Err(RemoteError::from_json(error.unwrap_or(Value::Null))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Hand-built bytes for `ExtHostDocuments.$acceptModelSaved(uri)` as `_requestJSONArgs` emits.
    #[test]
    fn json_request_bytes() {
        let msg = RpcMessage::Request {
            req: 1,
            rpc_id: 95,
            method: "$acceptModelSaved".into(),
            args: vec![Arg::Json(json!({"$mid":1,"scheme":"file","path":"/a"}))],
            cancellable: false,
        };
        let b = msg.encode().unwrap();
        let args = r#"[{"$mid":1,"path":"/a","scheme":"file"}]"#;
        let mut expected = vec![1, 0, 0, 0, 1, 95, 17];
        expected.extend_from_slice(b"$acceptModelSaved");
        expected.extend_from_slice(&(args.len() as u32).to_be_bytes());
        expected.extend_from_slice(args.as_bytes());
        // serde_json orders object keys alphabetically without the preserve_order feature.
        assert_eq!(b, expected);
        assert_eq!(RpcMessage::decode(&b).unwrap(), msg);
    }

    #[test]
    fn undefined_arg_forces_mixed_encoding() {
        let msg = RpcMessage::Request {
            req: 7,
            rpc_id: 104,
            method: "$provideHover".into(),
            args: vec![Arg::Json(json!(3)), Arg::Undefined],
            cancellable: true,
        };
        let b = msg.encode().unwrap();
        assert_eq!(b[0], 4); // RequestMixedArgsWithCancellation
        let mut expected = vec![4, 0, 0, 0, 7, 104, 13];
        expected.extend_from_slice(b"$provideHover");
        expected.extend_from_slice(&[2, 1, 0, 0, 0, 1, b'3', 4]);
        assert_eq!(b, expected);
        assert_eq!(RpcMessage::decode(&b).unwrap(), msg);
    }

    #[test]
    fn replies_round_trip() {
        for m in [
            RpcMessage::Acknowledged { req: 9 },
            RpcMessage::Cancel { req: 9 },
            RpcMessage::ReplyOk { req: 9, reply: Reply::Empty },
            RpcMessage::ReplyOk { req: 9, reply: Reply::Json(json!({"contents":[]})) },
            RpcMessage::ReplyOk { req: 9, reply: Reply::Buffer(vec![1, 2, 3]) },
            RpcMessage::ReplyOk {
                req: 9,
                reply: Reply::JsonWithBuffers { value: json!({"$$ref$$":0}), buffers: vec![vec![7]] },
            },
            RpcMessage::ReplyErr { req: 9, error: Some(js_error("Error", "boom")) },
            RpcMessage::ReplyErr { req: 9, error: None },
        ] {
            let b = m.encode().unwrap();
            assert_eq!(RpcMessage::decode(&b).unwrap(), m);
        }
    }

    #[test]
    fn ack_is_five_bytes() {
        assert_eq!(RpcMessage::Acknowledged { req: 258 }.encode().unwrap(), vec![5, 0, 0, 1, 2]);
    }

    /// A captured-shape request from the ext host: `MainThreadDiagnostics.$changeMany(owner, entries)`.
    #[test]
    fn decodes_incoming_diagnostics_request() {
        let args = r#"["rust-analyzer",[[{"$mid":1,"scheme":"vscode-remote","authority":"h","path":"/w/a.rs"},[{"severity":8,"message":"E","startLineNumber":1,"startColumn":1,"endLineNumber":1,"endColumn":2}]]]]"#;
        let method = "$changeMany";
        let mut b = vec![1, 0, 0, 0, 3, 16, method.len() as u8];
        b.extend_from_slice(method.as_bytes());
        b.extend_from_slice(&(args.len() as u32).to_be_bytes());
        b.extend_from_slice(args.as_bytes());
        match RpcMessage::decode(&b).unwrap() {
            RpcMessage::Request { req, rpc_id, method, args, cancellable } => {
                assert_eq!((req, rpc_id, method.as_str(), cancellable), (3, 16, "$changeMany", false));
                assert_eq!(rpc_ids::name_of(rpc_id), Some("MainThreadDiagnostics"));
                assert_eq!(args.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }
}
