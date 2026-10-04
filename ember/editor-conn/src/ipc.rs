//! Code-OSS IPC (`src/vs/base/parts/ipc/common/ipc.ts`): the channel protocol the management
//! connection speaks.
//!
//! Wire format of one regular message: `serialize(header) ++ serialize(body)`.
//!
//! * Value encoding (`serialize`/`deserialize`, L268-327): a 1-byte `DataType` tag, then a
//!   VQL length/int (`writeInt32VQL`/`readIntVQL`, L172-209, 7 bits per byte, little-end first).
//! * Requests (`RequestType`, L40-45): header `[type, id, channelName, name]` + body `arg` for
//!   `Promise`/`EventListen`; `[type, id]` for `PromiseCancel`/`EventDispose`.
//! * Responses (`ResponseType`, L66-72): `[200]` Initialize; `[type, id]` + body `data` otherwise.
//! * Connection start (`IPCClient` constructor, L1015-1031): the client first sends its context
//!   alone (`serialize(ctx)`), then both sides' `ChannelServer`s send `[200]`. A `ChannelClient`
//!   does not send requests before it has seen the peer's `[200]`.
//!
//! The protocol is symmetric (both ends host a `ChannelServer`); Ember hosts no channels and
//! answers any server-initiated request with an error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::connection::{ConnEvent, Connection, ConnectionHandle};
use crate::{Error, RemoteError, Result};

/// `DataType` (L242-250).
mod data_type {
    pub const UNDEFINED: u8 = 0;
    pub const STRING: u8 = 1;
    pub const BUFFER: u8 = 2;
    pub const VSBUFFER: u8 = 3;
    pub const ARRAY: u8 = 4;
    pub const OBJECT: u8 = 5;
    pub const INT: u8 = 6;
}

pub mod request_type {
    pub const PROMISE: i32 = 100;
    pub const PROMISE_CANCEL: i32 = 101;
    pub const EVENT_LISTEN: i32 = 102;
    pub const EVENT_DISPOSE: i32 = 103;
}

pub mod response_type {
    pub const INITIALIZE: i32 = 200;
    pub const PROMISE_SUCCESS: i32 = 201;
    pub const PROMISE_ERROR: i32 = 202;
    pub const PROMISE_ERROR_OBJ: i32 = 203;
    pub const EVENT_FIRE: i32 = 204;
}

/// A value as the IPC serializer sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum IpcValue {
    Undefined,
    String(String),
    /// Node `Buffer` (type 2). Decoded the same as `VsBuffer`.
    Buffer(Vec<u8>),
    /// `VSBuffer` (type 3): file contents etc.
    VsBuffer(Vec<u8>),
    Array(Vec<IpcValue>),
    /// Anything else, JSON-encoded (type 5): objects, `null`, booleans, non-int32 numbers.
    Object(Value),
    /// A number with `(n | 0) === n`, i.e. an int32.
    Int(i32),
}

impl IpcValue {
    pub fn str(s: impl Into<String>) -> Self {
        Self::String(s.into())
    }

    pub fn json<T: Serialize>(v: &T) -> Result<Self> {
        Ok(Self::Object(serde_json::to_value(v)?))
    }

    /// Lossy view as JSON (buffers become `null`). Mirrors what JS code would see after
    /// `JSON.stringify` of the deserialized value.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Undefined => Value::Null,
            Self::String(s) => Value::String(s.clone()),
            Self::Buffer(_) | Self::VsBuffer(_) => Value::Null,
            Self::Array(a) => Value::Array(a.iter().map(Self::to_json).collect()),
            Self::Object(v) => v.clone(),
            Self::Int(i) => Value::from(*i),
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Buffer(b) | Self::VsBuffer(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i as i64),
            Self::Object(v) => v.as_i64(),
            _ => None,
        }
    }
}

/// `writeInt32VQL`: the value is treated as a u32 (JS `>>>`), so negatives take 5 bytes.
pub fn write_vql(out: &mut Vec<u8>, value: u32) {
    if value == 0 {
        out.push(0);
        return;
    }
    let mut v = value;
    while v != 0 {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

/// `readIntVQL`: accumulates 7-bit groups; JS bit ops truncate to 32 bits.
pub fn read_vql(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos).ok_or_else(|| Error::Malformed("truncated VQL".into()))?;
        *pos += 1;
        if shift < 64 {
            value |= ((b & 0x7f) as u64) << shift;
        }
        if b & 0x80 == 0 {
            return Ok(value as u32);
        }
        shift += 7;
        if shift > 35 {
            return Err(Error::Malformed("VQL too long".into()));
        }
    }
}

pub fn serialize(out: &mut Vec<u8>, v: &IpcValue) {
    match v {
        IpcValue::Undefined => out.push(data_type::UNDEFINED),
        IpcValue::String(s) => {
            out.push(data_type::STRING);
            write_vql(out, s.len() as u32);
            out.extend_from_slice(s.as_bytes());
        }
        IpcValue::Buffer(b) => {
            out.push(data_type::BUFFER);
            write_vql(out, b.len() as u32);
            out.extend_from_slice(b);
        }
        IpcValue::VsBuffer(b) => {
            out.push(data_type::VSBUFFER);
            write_vql(out, b.len() as u32);
            out.extend_from_slice(b);
        }
        IpcValue::Array(a) => {
            out.push(data_type::ARRAY);
            write_vql(out, a.len() as u32);
            for el in a {
                serialize(out, el);
            }
        }
        IpcValue::Object(o) => {
            let s = o.to_string();
            out.push(data_type::OBJECT);
            write_vql(out, s.len() as u32);
            out.extend_from_slice(s.as_bytes());
        }
        IpcValue::Int(i) => {
            out.push(data_type::INT);
            write_vql(out, *i as u32);
        }
    }
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = pos.checked_add(n).filter(|e| *e <= buf.len());
    let end = end.ok_or_else(|| Error::Malformed("truncated IPC value".into()))?;
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

pub fn deserialize(buf: &[u8], pos: &mut usize) -> Result<IpcValue> {
    let ty = *take(buf, pos, 1)?.first().unwrap();
    Ok(match ty {
        data_type::UNDEFINED => IpcValue::Undefined,
        data_type::STRING => {
            let n = read_vql(buf, pos)? as usize;
            IpcValue::String(String::from_utf8_lossy(take(buf, pos, n)?).into_owned())
        }
        data_type::BUFFER => {
            let n = read_vql(buf, pos)? as usize;
            IpcValue::Buffer(take(buf, pos, n)?.to_vec())
        }
        data_type::VSBUFFER => {
            let n = read_vql(buf, pos)? as usize;
            IpcValue::VsBuffer(take(buf, pos, n)?.to_vec())
        }
        data_type::ARRAY => {
            let n = read_vql(buf, pos)? as usize;
            let mut a = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                a.push(deserialize(buf, pos)?);
            }
            IpcValue::Array(a)
        }
        data_type::OBJECT => {
            let n = read_vql(buf, pos)? as usize;
            IpcValue::Object(serde_json::from_slice(take(buf, pos, n)?)?)
        }
        data_type::INT => IpcValue::Int(read_vql(buf, pos)? as i32),
        other => return Err(Error::Malformed(format!("unknown IPC data type {other}"))),
    })
}

/// Encode one IPC message: header then body.
pub fn encode_message(header: &IpcValue, body: &IpcValue) -> Vec<u8> {
    let mut out = Vec::new();
    serialize(&mut out, header);
    serialize(&mut out, body);
    out
}

/// Decode one IPC message into `(header, body)`. A missing body reads as `Undefined`.
pub fn decode_message(buf: &[u8]) -> Result<(IpcValue, IpcValue)> {
    let mut pos = 0;
    let header = deserialize(buf, &mut pos)?;
    let body = if pos < buf.len() { deserialize(buf, &mut pos)? } else { IpcValue::Undefined };
    Ok((header, body))
}

/// Connection-level state changes surfaced to the owner (who decides about reconnecting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcLifecycle {
    Lost(crate::connection::LostReason),
    Disconnected,
}

type Pending = oneshot::Sender<std::result::Result<IpcValue, RemoteError>>;

struct Shared {
    next_id: AtomicU32,
    pending: Mutex<HashMap<u32, Pending>>,
    listeners: Mutex<HashMap<u32, mpsc::UnboundedSender<IpcValue>>>,
}

/// An IPC channel client over a management connection.
#[derive(Clone)]
pub struct IpcClient {
    handle: ConnectionHandle,
    shared: Arc<Shared>,
    initialized: watch::Receiver<bool>,
}

/// An active event subscription (`channel.listen`). Dropping it does not unsubscribe; call
/// [`IpcClient::unlisten`].
pub struct IpcSubscription {
    pub id: u32,
    pub events: mpsc::UnboundedReceiver<IpcValue>,
}

impl IpcClient {
    /// Take over a freshly handshaken management connection. `ctx` is the
    /// `RemoteAgentConnectionContext` (`{ remoteAuthority, clientId }`).
    pub fn start(conn: Connection, ctx: IpcValue) -> (Self, mpsc::UnboundedReceiver<IpcLifecycle>) {
        let Connection { handle, mut events } = conn;
        let shared = Arc::new(Shared {
            next_id: AtomicU32::new(0),
            pending: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
        });
        let (init_tx, initialized) = watch::channel(false);
        let (life_tx, life_rx) = mpsc::unbounded_channel();

        // IPCClient constructor: context first, then our ChannelServer's Initialize.
        let mut ctx_msg = Vec::new();
        serialize(&mut ctx_msg, &ctx);
        handle.send(ctx_msg);
        handle.send(encode_message(
            &IpcValue::Array(vec![IpcValue::Int(response_type::INITIALIZE)]),
            &IpcValue::Undefined,
        ));

        let client = Self { handle: handle.clone(), shared: Arc::clone(&shared), initialized };
        let h = handle;
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                match ev {
                    ConnEvent::Message(raw) => {
                        if let Err(e) = dispatch(&raw, &shared, &init_tx, &h) {
                            tracing::warn!("editor-conn ipc: bad message: {e}");
                        }
                    }
                    ConnEvent::Lost(r) => {
                        let _ = life_tx.send(IpcLifecycle::Lost(r));
                    }
                    ConnEvent::Disconnected => {
                        let _ = life_tx.send(IpcLifecycle::Disconnected);
                        break;
                    }
                }
            }
            // Fail everything still waiting.
            for (_, tx) in shared.pending.lock().unwrap().drain() {
                let _ = tx.send(Err(RemoteError::from_json(serde_json::json!({
                    "name": "Canceled", "message": "connection closed"
                }))));
            }
            shared.listeners.lock().unwrap().clear();
        });
        (client, life_rx)
    }

    async fn wait_initialized(&self) -> Result<()> {
        let mut rx = self.initialized.clone();
        rx.wait_for(|v| *v).await.map_err(|_| Error::Closed)?;
        Ok(())
    }

    fn alloc_id(&self) -> u32 {
        self.shared.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// `channel.call(command, arg)`.
    pub async fn call(&self, channel: &str, command: &str, arg: IpcValue) -> Result<IpcValue> {
        self.wait_initialized().await?;
        let id = self.alloc_id();
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        let header = IpcValue::Array(vec![
            IpcValue::Int(request_type::PROMISE),
            IpcValue::Int(id as i32),
            IpcValue::str(channel),
            IpcValue::str(command),
        ]);
        self.handle.send(encode_message(&header, &arg));
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(Error::Remote(e)),
            Err(_) => Err(Error::Closed),
        }
    }

    /// Send `PromiseCancel` for a request id and drop its pending reply.
    pub fn cancel(&self, id: u32) {
        self.shared.pending.lock().unwrap().remove(&id);
        let header = IpcValue::Array(vec![
            IpcValue::Int(request_type::PROMISE_CANCEL),
            IpcValue::Int(id as i32),
        ]);
        self.handle.send(encode_message(&header, &IpcValue::Undefined));
    }

    /// `channel.listen(event, arg)`.
    pub async fn listen(&self, channel: &str, event: &str, arg: IpcValue) -> Result<IpcSubscription> {
        self.wait_initialized().await?;
        let id = self.alloc_id();
        let (tx, events) = mpsc::unbounded_channel();
        self.shared.listeners.lock().unwrap().insert(id, tx);
        let header = IpcValue::Array(vec![
            IpcValue::Int(request_type::EVENT_LISTEN),
            IpcValue::Int(id as i32),
            IpcValue::str(channel),
            IpcValue::str(event),
        ]);
        self.handle.send(encode_message(&header, &arg));
        Ok(IpcSubscription { id, events })
    }

    pub fn unlisten(&self, id: u32) {
        self.shared.listeners.lock().unwrap().remove(&id);
        let header = IpcValue::Array(vec![
            IpcValue::Int(request_type::EVENT_DISPOSE),
            IpcValue::Int(id as i32),
        ]);
        self.handle.send(encode_message(&header, &IpcValue::Undefined));
    }

    pub fn connection(&self) -> &ConnectionHandle {
        &self.handle
    }
}

fn dispatch(
    raw: &[u8],
    shared: &Shared,
    init_tx: &watch::Sender<bool>,
    handle: &ConnectionHandle,
) -> Result<()> {
    let (header, body) = decode_message(raw)?;
    let IpcValue::Array(h) = header else {
        return Err(Error::Malformed("IPC header is not an array".into()));
    };
    let ty = h.first().and_then(IpcValue::as_int).ok_or_else(|| Error::Malformed("no type".into()))? as i32;
    let id = h.get(1).and_then(IpcValue::as_int).map(|i| i as u32);
    match ty {
        response_type::INITIALIZE => {
            let _ = init_tx.send(true);
        }
        response_type::PROMISE_SUCCESS | response_type::PROMISE_ERROR | response_type::PROMISE_ERROR_OBJ => {
            let id = id.ok_or_else(|| Error::Malformed("reply without id".into()))?;
            if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                let r = if ty == response_type::PROMISE_SUCCESS {
                    Ok(body)
                } else {
                    Err(RemoteError::from_json(body.to_json()))
                };
                let _ = tx.send(r);
            }
        }
        response_type::EVENT_FIRE => {
            let id = id.ok_or_else(|| Error::Malformed("event without id".into()))?;
            if let Some(tx) = shared.listeners.lock().unwrap().get(&id) {
                let _ = tx.send(body);
            }
        }
        request_type::PROMISE => {
            // The server called a channel on *our* side; we host none.
            let id = id.unwrap_or(0);
            let header = IpcValue::Array(vec![
                IpcValue::Int(response_type::PROMISE_ERROR),
                IpcValue::Int(id as i32),
            ]);
            let channel = h.get(2).map(IpcValue::to_json).unwrap_or(Value::Null);
            let err = serde_json::json!({
                "message": format!("Unknown channel: {channel}"),
                "name": "Unknown channel",
                "stack": null,
            });
            handle.send(encode_message(&header, &IpcValue::Object(err)));
        }
        request_type::PROMISE_CANCEL | request_type::EVENT_LISTEN | request_type::EVENT_DISPOSE => {}
        other => return Err(Error::Malformed(format!("unknown IPC message type {other}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vql(v: u32) -> Vec<u8> {
        let mut o = Vec::new();
        write_vql(&mut o, v);
        o
    }

    #[test]
    fn vql_vectors() {
        assert_eq!(vql(0), vec![0]);
        assert_eq!(vql(127), vec![0x7f]);
        assert_eq!(vql(128), vec![0x80, 0x01]);
        assert_eq!(vql(300), vec![0xac, 0x02]);
        assert_eq!(vql(-1i32 as u32), vec![0xff, 0xff, 0xff, 0xff, 0x0f]);
        let mut p = 0;
        assert_eq!(read_vql(&[0xff, 0xff, 0xff, 0xff, 0x0f], &mut p).unwrap() as i32, -1);
        let mut p = 0;
        assert_eq!(read_vql(&[0xac, 0x02], &mut p).unwrap(), 300);
    }

    /// Hand-built bytes of `[100, 1, "remoteFilesystem", "stat"]` + `[ {uri} ]`, as
    /// `ChannelClient.sendRequest` would produce for `channel.call('stat', [resource])`.
    #[test]
    fn promise_request_bytes() {
        let header = IpcValue::Array(vec![
            IpcValue::Int(100),
            IpcValue::Int(1),
            IpcValue::str("remoteFilesystem"),
            IpcValue::str("stat"),
        ]);
        let body = IpcValue::Array(vec![IpcValue::Object(serde_json::json!({"a":1}))]);
        let bytes = encode_message(&header, &body);
        let mut expected = vec![4, 4, 6, 100, 6, 1, 1, 16];
        expected.extend_from_slice(b"remoteFilesystem");
        expected.extend_from_slice(&[1, 4]);
        expected.extend_from_slice(b"stat");
        expected.extend_from_slice(&[4, 1, 5, 7]);
        expected.extend_from_slice(br#"{"a":1}"#);
        assert_eq!(bytes, expected);
        assert_eq!(decode_message(&bytes).unwrap(), (header, body));
    }

    #[test]
    fn initialize_response_is_three_bytes_plus_undefined() {
        let bytes = encode_message(&IpcValue::Array(vec![IpcValue::Int(200)]), &IpcValue::Undefined);
        // [Array, len 1, Int, 200 = 0xc8 0x01] [Undefined]
        assert_eq!(bytes, vec![4, 1, 6, 0xc8, 0x01, 0]);
    }

    #[test]
    fn readdir_reply_decodes_nested_arrays() {
        // [[".git", 2], ["a.rs", 1]] as the server's serialize() emits it.
        let v = IpcValue::Array(vec![
            IpcValue::Array(vec![IpcValue::str(".git"), IpcValue::Int(2)]),
            IpcValue::Array(vec![IpcValue::str("a.rs"), IpcValue::Int(1)]),
        ]);
        let mut b = Vec::new();
        serialize(&mut b, &v);
        let mut p = 0;
        assert_eq!(deserialize(&b, &mut p).unwrap(), v);
        assert_eq!(p, b.len());
    }
}
