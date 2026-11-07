//! The wire format (SPEC `FR-F3`, `FR-F6`): a frame is a 4-byte big-endian length followed by a
//! JSON envelope `{"v":1,"msg":{...}}`. A frame longer than [`MAX_FRAME`] is an error before any
//! allocation. Only Offer, Accept, Decline and Cancel exist in this unit (no chunks yet).
//!
//! An offer must fit one frame, so the number of files in one offer is bounded by 64 KiB of
//! names; a larger folder is split by the sender in a later unit.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const WIRE_VERSION: u8 = 1;
/// The largest payload of one frame.
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// A file name, or a relative path for a folder entry. Untrusted until cleaned.
    pub name: String,
    pub size: u64,
}

/// Why a receiver ended an offer; the sender shows it (`FR-F3`, `FR-F10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclineCode {
    /// The user said no.
    User,
    /// No answer within the time limit.
    Expired,
    /// The sender is not one of the user's registered devices.
    NotRegistered,
    /// The receive mode is off.
    ReceiveOff,
    /// Above the size limit.
    TooLarge,
    /// The offer is malformed (no files, wrong total).
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Offer { id: String, files: Vec<FileEntry>, total: u64 },
    Accept { id: String },
    Decline {
        id: String,
        code: DeclineCode,
        #[serde(default)]
        detail: String,
    },
    Cancel { id: String },
    /// One chunk of file data (`FR-F5`). `file` is the index in the offer's `files` list.
    Chunk {
        id: String,
        file: u32,
        offset: u64,
        data: Vec<u8>,
        #[serde(default)]
        sha256: Option<String>,
    },
    /// Receiver tells the sender to resume streaming `file` from `offset` (`FR-F5`).
    ResumeFrom { id: String, file: u32, offset: u64 },
    /// Sender or receiver pauses streaming (`FR-F4`).
    Pause { id: String },
    /// Sender or receiver resumes a paused transfer (`FR-F4`).
    Resume { id: String },
    /// Sender declares all files sent with their final SHA-256 digests (`FR-F5`).
    Done { id: String, sha256: Vec<String> },
}

impl Message {
    pub fn id(&self) -> &str {
        match self {
            Message::Offer { id, .. }
            | Message::Accept { id }
            | Message::Decline { id, .. }
            | Message::Cancel { id }
            | Message::Chunk { id, .. }
            | Message::ResumeFrom { id, .. }
            | Message::Pause { id }
            | Message::Resume { id }
            | Message::Done { id, .. } => id,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u8,
    msg: Message,
}

#[derive(Deserialize)]
struct Head {
    v: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("frame of {len} bytes is above the 65536 byte limit")]
    FrameTooLarge { len: usize },
    #[error("the frame is cut short")]
    Truncated,
    #[error("unsupported wire version {0}")]
    UnsupportedVersion(u8),
    #[error("bad message: {0}")]
    Bad(String),
    #[error("stream error: {0}")]
    Io(#[from] std::io::Error),
}

/// Encodes `msg` as one frame (length prefix included).
pub fn encode(msg: &Message) -> Result<Vec<u8>, WireError> {
    let payload = serde_json::to_vec(&Envelope { v: WIRE_VERSION, msg: msg.clone() })
        .map_err(|e| WireError::Bad(e.to_string()))?;
    if payload.len() > MAX_FRAME {
        return Err(WireError::FrameTooLarge { len: payload.len() });
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

fn decode_payload(payload: &[u8]) -> Result<Message, WireError> {
    let head: Head =
        serde_json::from_slice(payload).map_err(|e| WireError::Bad(e.to_string()))?;
    if head.v != WIRE_VERSION {
        return Err(WireError::UnsupportedVersion(head.v));
    }
    let env: Envelope =
        serde_json::from_slice(payload).map_err(|e| WireError::Bad(e.to_string()))?;
    Ok(env.msg)
}

/// Decodes exactly one whole frame (length prefix included).
pub fn decode(frame: &[u8]) -> Result<Message, WireError> {
    if frame.len() < 4 {
        return Err(WireError::Truncated);
    }
    let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if len > MAX_FRAME {
        return Err(WireError::FrameTooLarge { len });
    }
    let body = &frame[4..];
    if body.len() < len {
        return Err(WireError::Truncated);
    }
    if body.len() > len {
        return Err(WireError::Bad("bytes after the frame".into()));
    }
    decode_payload(body)
}

/// Reads one frame. `Ok(None)` is a clean end of stream between frames. The length is checked
/// before the payload buffer is allocated. Not cancel safe: do not drop a half-finished read and
/// read again.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Message>, WireError> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut head[got..]).await?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(WireError::Truncated) };
        }
        got += n;
    }
    let len = u32::from_be_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(WireError::FrameTooLarge { len });
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            WireError::Truncated
        } else {
            WireError::Io(e)
        }
    })?;
    decode_payload(&payload).map(Some)
}

pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, msg: &Message) -> Result<(), WireError> {
    let frame = encode(msg)?;
    w.write_all(&frame).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> Message {
        Message::Offer {
            id: "i1".into(),
            files: vec![FileEntry { name: "a.txt".into(), size: 3 }],
            total: 3,
        }
    }

    #[test]
    fn fr_f3_every_message_round_trips() {
        for m in [
            offer(),
            Message::Accept { id: "i1".into() },
            Message::Decline { id: "i1".into(), code: DeclineCode::TooLarge, detail: "4 GiB".into() },
            Message::Cancel { id: "i1".into() },
        ] {
            assert_eq!(decode(&encode(&m).unwrap()).unwrap(), m);
        }
    }

    #[test]
    fn fr_f3_a_frame_above_the_limit_is_an_error_not_an_allocation() {
        let mut frame = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(b"x");
        assert!(matches!(decode(&frame), Err(WireError::FrameTooLarge { .. })));
        // A hostile length with no payload at all.
        assert!(matches!(
            decode(&u32::MAX.to_be_bytes()),
            Err(WireError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn fr_f3_an_oversized_message_is_not_encoded() {
        let files = (0..5000)
            .map(|i| FileEntry { name: format!("file-number-{i}.bin"), size: 1 })
            .collect();
        let m = Message::Offer { id: "i".into(), files, total: 5000 };
        assert!(matches!(encode(&m), Err(WireError::FrameTooLarge { .. })));
    }

    #[test]
    fn fr_f3_truncated_and_garbage_input_is_an_error() {
        let frame = encode(&offer()).unwrap();
        assert!(matches!(decode(&frame[..2]), Err(WireError::Truncated)));
        assert!(matches!(decode(&frame[..frame.len() - 1]), Err(WireError::Truncated)));
        let mut garbage = 5u32.to_be_bytes().to_vec();
        garbage.extend_from_slice(b"\xff\xfe\x00{}");
        assert!(matches!(decode(&garbage), Err(WireError::Bad(_))));
        let mut empty = 0u32.to_be_bytes().to_vec();
        assert!(matches!(decode(&empty), Err(WireError::Bad(_))));
        empty.extend_from_slice(b"extra");
        assert!(decode(&empty).is_err());
    }

    #[test]
    fn fr_f3_unknown_version_and_unknown_type_are_errors() {
        let body = br#"{"v":2,"msg":{"type":"accept","id":"x"}}"#;
        let mut f = (body.len() as u32).to_be_bytes().to_vec();
        f.extend_from_slice(body);
        assert!(matches!(decode(&f), Err(WireError::UnsupportedVersion(2))));
        let body = br#"{"v":1,"msg":{"type":"chunk","id":"x"}}"#;
        let mut f = (body.len() as u32).to_be_bytes().to_vec();
        f.extend_from_slice(body);
        assert!(matches!(decode(&f), Err(WireError::Bad(_))));
    }

    #[tokio::test]
    async fn fr_f3_stream_reads_follow_the_same_rules() {
        let frame = encode(&offer()).unwrap();
        let mut two = frame.clone();
        two.extend_from_slice(&encode(&Message::Cancel { id: "i1".into() }).unwrap());
        let mut r: &[u8] = &two;
        assert_eq!(read_message(&mut r).await.unwrap(), Some(offer()));
        assert!(matches!(read_message(&mut r).await.unwrap(), Some(Message::Cancel { .. })));
        assert!(read_message(&mut r).await.unwrap().is_none());

        let mut cut: &[u8] = &frame[..frame.len() - 2];
        assert!(matches!(read_message(&mut cut).await, Err(WireError::Truncated)));
        let mut cut_head: &[u8] = &frame[..3];
        assert!(matches!(read_message(&mut cut_head).await, Err(WireError::Truncated)));
        let huge = u32::MAX.to_be_bytes();
        let mut h: &[u8] = &huge;
        assert!(matches!(read_message(&mut h).await, Err(WireError::FrameTooLarge { .. })));
    }

    #[test]
    fn fr_f3_decoder_never_panics_on_arbitrary_bytes() {
        // #527 item 7: fuzz-style arbitrary-bytes decoder check alongside the hand cases.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next_u8 = || -> u8 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xFF) as u8
        };
        for len in 0..512 {
            let buf: Vec<u8> = (0..len).map(|_| next_u8()).collect();
            let _ = decode(&buf);
        }
        for payload_len in [0u32, 1, 2, 15, 64, 256] {
            let mut frame = payload_len.to_be_bytes().to_vec();
            frame.extend((0..payload_len).map(|_| next_u8()));
            let _ = decode(&frame);
        }
    }
}
