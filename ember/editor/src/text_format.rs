//! The format of one document as the status bar shows it (FR-L31.7, #453): line endings, file
//! encoding and language mode, and the pure conversions behind the pickers.
//!
//! **EOL.** As Monaco: the first line break decides (`\r\n` makes the document CRLF, anything else
//! LF), so a file with mixed endings counts as the first one. A file with no line break is LF (one
//! rule on every OS; Code-OSS would take `files.eol`, whose default `auto` follows the OS).
//!
//! **Encoding.** Detected by the byte order mark only, as Code-OSS does without
//! `files.autoGuessEncoding`: `EF BB BF` is UTF-8 with BOM, `FF FE` UTF-16 LE, `FE FF` UTF-16 BE,
//! anything else UTF-8 if it decodes as UTF-8. Bytes that are none of these have no supported
//! encoding ([`detect_encoding`] answers `None`, shown as "알 수 없음") and are not opened. The ids are
//! Code-OSS's `SUPPORTED_ENCODINGS` keys (`utf8`, `utf8bom`, `utf16le`, `utf16be`), which the
//! extension host gets in `IModelAddedData.encoding` and `$acceptEncodingChanged`.

use crate::document::DocError;

/// A document's line ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    pub const ALL: [Eol; 2] = [Eol::Lf, Eol::Crlf];

    /// The sequence itself.
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
        }
    }

    /// The status bar and picker label (Code-OSS `nlsEOLLF` / `nlsEOLCRLF`).
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
        }
    }

    pub fn from_sequence(s: &str) -> Self {
        if s == "\r\n" {
            Eol::Crlf
        } else {
            Eol::Lf
        }
    }
}

/// The EOL of `text`: CRLF when the first line break is `\r\n`, else LF.
pub fn detect_eol(text: &str) -> Eol {
    Eol::from_sequence(ember_editor_conn::document::detect_eol(text))
}

/// A supported file encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

const BOM_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
const BOM_UTF16LE: &[u8] = &[0xFF, 0xFE];
const BOM_UTF16BE: &[u8] = &[0xFE, 0xFF];

impl Encoding {
    /// In the order the picker lists them (Code-OSS `SUPPORTED_ENCODINGS` order).
    pub const ALL: [Encoding; 4] = [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be];

    /// Code-OSS's encoding id.
    pub fn id(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf8",
            Encoding::Utf8Bom => "utf8bom",
            Encoding::Utf16Le => "utf16le",
            Encoding::Utf16Be => "utf16be",
        }
    }

    /// The status bar and picker label (Code-OSS `labelShort`; `labelLong` is the same for these four).
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 with BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.id() == id)
    }

    fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => &[],
            Encoding::Utf8Bom => BOM_UTF8,
            Encoding::Utf16Le => BOM_UTF16LE,
            Encoding::Utf16Be => BOM_UTF16BE,
        }
    }
}

/// The encoding a byte order mark names, if `bytes` start with one.
pub fn bom_encoding(bytes: &[u8]) -> Option<Encoding> {
    if bytes.starts_with(BOM_UTF8) {
        Some(Encoding::Utf8Bom)
    } else if bytes.starts_with(BOM_UTF16LE) {
        Some(Encoding::Utf16Le)
    } else if bytes.starts_with(BOM_UTF16BE) {
        Some(Encoding::Utf16Be)
    } else {
        None
    }
}

/// The encoding of file bytes: the BOM's, else UTF-8 if they are UTF-8, else `None` (unknown).
pub fn detect_encoding(bytes: &[u8]) -> Option<Encoding> {
    bom_encoding(bytes).or_else(|| std::str::from_utf8(bytes).ok().map(|_| Encoding::Utf8))
}

/// Decode `bytes` as `encoding`, dropping a BOM of that encoding family. Refuses bytes that are not
/// valid in it (no replacement characters: saving them would damage the file).
pub fn decode_as(bytes: &[u8], encoding: Encoding) -> Result<String, DocError> {
    match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => {
            let body = bytes.strip_prefix(BOM_UTF8).unwrap_or(bytes);
            std::str::from_utf8(body).map(str::to_owned).map_err(|_| DocError::NotUtf8)
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let body = bytes.strip_prefix(encoding.bom()).unwrap_or(bytes);
            if body.len() % 2 != 0 {
                return Err(DocError::BadEncoding(encoding.label()));
            }
            let units = body.chunks_exact(2).map(|c| match encoding {
                Encoding::Utf16Le => u16::from_le_bytes([c[0], c[1]]),
                _ => u16::from_be_bytes([c[0], c[1]]),
            });
            char::decode_utf16(units).collect::<Result<String, _>>().map_err(|_| DocError::BadEncoding(encoding.label()))
        }
    }
}

/// Decode file bytes by what they say about themselves ([`detect_encoding`]).
pub fn decode_detect(bytes: &[u8]) -> Result<(String, Encoding), DocError> {
    let encoding = detect_encoding(bytes).ok_or(DocError::NotUtf8)?;
    Ok((decode_as(bytes, encoding)?, encoding))
}

/// Decode a file that is open as `current` and changed on disk: a BOM on disk wins, else the
/// document keeps its encoding (UTF-8 with BOM becomes UTF-8 once the BOM is gone).
pub fn decode_reload(bytes: &[u8], current: Encoding) -> Result<(String, Encoding), DocError> {
    let encoding = match bom_encoding(bytes) {
        Some(e) => e,
        None if current == Encoding::Utf8Bom => Encoding::Utf8,
        None => current,
    };
    Ok((decode_as(bytes, encoding)?, encoding))
}

/// The bytes of `text` in `encoding`, BOM included where the encoding has one.
pub fn encode_as(text: &str, encoding: Encoding) -> Vec<u8> {
    let mut out = encoding.bom().to_vec();
    match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => out.extend_from_slice(text.as_bytes()),
        Encoding::Utf16Le => text.encode_utf16().for_each(|u| out.extend_from_slice(&u.to_le_bytes())),
        Encoding::Utf16Be => text.encode_utf16().for_each(|u| out.extend_from_slice(&u.to_be_bytes())),
    }
    out
}

/// What "인코딩 변경" does with the chosen encoding (Code-OSS `EncodingMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncodingMode {
    /// 인코딩으로 다시 열기: read the file again in the encoding (unsaved edits are discarded).
    Reopen,
    /// 인코딩으로 저장: write the buffer in the encoding.
    Save,
}

/// What the status bar shows for one open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentFormat {
    pub eol: Eol,
    pub encoding: Encoding,
    pub language_id: String,
    /// The first `aliases` entry of the language's `contributes.languages`, else the id.
    pub language_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_7_eol_is_the_first_line_break() {
        assert_eq!(detect_eol("a\nb"), Eol::Lf);
        assert_eq!(detect_eol("a\r\nb"), Eol::Crlf);
        assert_eq!(detect_eol("a\r\nb\nc"), Eol::Crlf, "mixed counts as the first");
        assert_eq!(detect_eol("a\nb\r\nc"), Eol::Lf, "mixed counts as the first");
        assert_eq!(detect_eol("no break"), Eol::Lf);
        assert_eq!(detect_eol(""), Eol::Lf);
        assert_eq!((Eol::Lf.label(), Eol::Crlf.label()), ("LF", "CRLF"));
        assert_eq!(Eol::Crlf.as_str(), "\r\n");
    }

    #[test]
    fn fr_l31_7_encoding_is_detected_by_bom_then_utf8() {
        assert_eq!(detect_encoding(b"plain"), Some(Encoding::Utf8));
        assert_eq!(detect_encoding("한글".as_bytes()), Some(Encoding::Utf8));
        assert_eq!(detect_encoding(b"\xef\xbb\xbfhi"), Some(Encoding::Utf8Bom));
        assert_eq!(detect_encoding(b"\xff\xfeh\x00"), Some(Encoding::Utf16Le));
        assert_eq!(detect_encoding(b"\xfe\xff\x00h"), Some(Encoding::Utf16Be));
        assert_eq!(detect_encoding(b"\xc0\xc1latin-1?"), None, "unknown");
        assert_eq!(detect_encoding(b""), Some(Encoding::Utf8));
    }

    #[test]
    fn fr_l31_7_encoding_labels_and_ids_follow_code_oss() {
        let labels: Vec<_> = Encoding::ALL.iter().map(|e| e.label()).collect();
        assert_eq!(labels, ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"]);
        let ids: Vec<_> = Encoding::ALL.iter().map(|e| e.id()).collect();
        assert_eq!(ids, ["utf8", "utf8bom", "utf16le", "utf16be"]);
        assert_eq!(Encoding::from_id("utf16be"), Some(Encoding::Utf16Be));
        assert_eq!(Encoding::from_id("latin1"), None);
    }

    #[test]
    fn fr_l31_7_decode_and_encode_round_trip() {
        for e in Encoding::ALL {
            let bytes = encode_as("a😀\r\n한", e);
            assert_eq!(decode_as(&bytes, e).unwrap(), "a😀\r\n한", "{e:?}");
            assert_eq!(decode_detect(&bytes).unwrap().1, if e == Encoding::Utf8 { Encoding::Utf8 } else { e });
        }
        assert_eq!(encode_as("hi", Encoding::Utf16Le), b"\xff\xfeh\x00i\x00".to_vec());
        assert_eq!(encode_as("hi", Encoding::Utf16Be), b"\xfe\xff\x00h\x00i".to_vec());
        assert_eq!(encode_as("hi", Encoding::Utf8Bom), b"\xef\xbb\xbfhi".to_vec());
        assert_eq!(decode_detect(b"\xff\xfeh\x00i\x00").unwrap(), ("hi".to_string(), Encoding::Utf16Le));
    }

    #[test]
    fn fr_l31_7_bytes_that_do_not_fit_the_encoding_are_refused() {
        assert_eq!(decode_as(b"\xff\xfeh\x00", Encoding::Utf8), Err(DocError::NotUtf8));
        assert!(matches!(decode_as(b"abc", Encoding::Utf16Le), Err(DocError::BadEncoding(_))), "odd length");
        // A lone high surrogate.
        assert!(matches!(decode_as(b"\x00\xd8", Encoding::Utf16Le), Err(DocError::BadEncoding(_))));
        assert_eq!(decode_detect(b"\xc0"), Err(DocError::NotUtf8));
        // UTF-16 without a BOM decodes when chosen explicitly (reopen with encoding).
        assert_eq!(decode_as(b"h\x00i\x00", Encoding::Utf16Le).unwrap(), "hi");
    }

    #[test]
    fn fr_l31_7_reload_keeps_the_encoding_unless_the_bom_says_otherwise() {
        assert_eq!(decode_reload(b"h\x00i\x00", Encoding::Utf16Le).unwrap(), ("hi".into(), Encoding::Utf16Le));
        assert_eq!(decode_reload(b"hi", Encoding::Utf8Bom).unwrap(), ("hi".into(), Encoding::Utf8));
        assert_eq!(decode_reload(b"\xef\xbb\xbfhi", Encoding::Utf8).unwrap(), ("hi".into(), Encoding::Utf8Bom));
    }
}
