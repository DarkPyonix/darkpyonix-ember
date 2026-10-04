//! `PersistentProtocol` wire framing.
//!
//! Every message is a 13-byte header followed by `DATA_LENGTH` bytes
//! (`src/vs/base/parts/ipc/common/ipc.net.ts`, `ProtocolWriter.write` L474-478,
//! `ProtocolReader.acceptChunk` L363-370; the doc comment above `Protocol` says "9 bytes" but the
//! code and `ProtocolConstants.HeaderLength` (L290) say 13):
//!
//! ```text
//! | TYPE u8 | ID u32be | ACK u32be | DATA_LENGTH u32be | DATA ... |
//! ```

use crate::{Error, Result};

/// `ProtocolConstants.HeaderLength`.
pub const HEADER_LEN: usize = 13;

/// Refuse frames larger than this (defensive; Code-OSS has no explicit limit).
pub const MAX_FRAME_LEN: usize = 256 * 1024 * 1024;

/// `ProtocolMessageType` (ipc.net.ts L263-273). Value 4 is unused upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    None = 0,
    /// Counted, acknowledged, replayed on reconnect.
    Regular = 1,
    /// Handshake messages; not counted, not acknowledged.
    Control = 2,
    Ack = 3,
    Disconnect = 5,
    ReplayRequest = 6,
    Pause = 7,
    Resume = 8,
    KeepAlive = 9,
}

impl MessageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::None,
            1 => Self::Regular,
            2 => Self::Control,
            3 => Self::Ack,
            5 => Self::Disconnect,
            6 => Self::ReplayRequest,
            7 => Self::Pause,
            8 => Self::Resume,
            9 => Self::KeepAlive,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub ty: MessageType,
    /// Message id; non-zero only for `Regular`.
    pub id: u32,
    /// Highest regular id the sender has received from us.
    pub ack: u32,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn new(ty: MessageType, id: u32, ack: u32, data: Vec<u8>) -> Self {
        Self { ty, id, ack, data }
    }

    pub fn empty(ty: MessageType, ack: u32) -> Self {
        Self::new(ty, 0, ack, Vec::new())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        self.encode_into(&mut out);
        out
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.ty as u8);
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&self.ack.to_be_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.data);
    }
}

/// Incremental decoder: feed arbitrary chunks, pull whole frames.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Bytes buffered but not yet consumed as a frame.
    pub fn buffered(&self) -> &[u8] {
        &self.buf
    }

    /// Take everything not yet decoded (`PersistentProtocol.readEntireBuffer`).
    pub fn take_buffered(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Returns the next complete frame, `Ok(None)` if more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let ty_raw = self.buf[0];
        let id = u32::from_be_bytes(self.buf[1..5].try_into().unwrap());
        let ack = u32::from_be_bytes(self.buf[5..9].try_into().unwrap());
        let len = u32::from_be_bytes(self.buf[9..13].try_into().unwrap()) as usize;
        if len > MAX_FRAME_LEN {
            return Err(Error::Malformed(format!("frame length {len} exceeds limit")));
        }
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let data = self.buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buf.drain(..HEADER_LEN + len);
        // Upstream fires unknown types too and then ignores them in `_receiveMessage`; we map
        // them to `None` so the state machine ignores them the same way.
        let ty = MessageType::from_u8(ty_raw).unwrap_or(MessageType::None);
        Ok(Some(Frame { ty, id, ack, data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_header_exactly() {
        let f = Frame::new(MessageType::Regular, 1, 2, b"hi".to_vec());
        assert_eq!(
            f.encode(),
            vec![1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 2, b'h', b'i'],
        );
    }

    #[test]
    fn keepalive_is_header_only() {
        let f = Frame::empty(MessageType::KeepAlive, 7);
        assert_eq!(f.encode(), vec![9, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0]);
    }

    #[test]
    fn decodes_across_split_chunks() {
        let a = Frame::new(MessageType::Control, 0, 0, br#"{"type":"ok"}"#.to_vec()).encode();
        let b = Frame::empty(MessageType::Ack, 3).encode();
        let mut all = a.clone();
        all.extend_from_slice(&b);

        let mut d = FrameDecoder::new();
        for byte in &all[..5] {
            d.push(std::slice::from_ref(byte));
            assert!(d.next_frame().unwrap().is_none());
        }
        d.push(&all[5..]);
        let f1 = d.next_frame().unwrap().unwrap();
        assert_eq!(f1.ty, MessageType::Control);
        assert_eq!(f1.data, br#"{"type":"ok"}"#);
        let f2 = d.next_frame().unwrap().unwrap();
        assert_eq!(f2.ty, MessageType::Ack);
        assert_eq!(f2.ack, 3);
        assert!(d.next_frame().unwrap().is_none());
        assert!(d.buffered().is_empty());
    }

    #[test]
    fn unknown_type_maps_to_none() {
        let mut raw = Frame::empty(MessageType::Ack, 0).encode();
        raw[0] = 4;
        let mut d = FrameDecoder::new();
        d.push(&raw);
        assert_eq!(d.next_frame().unwrap().unwrap().ty, MessageType::None);
    }
}
