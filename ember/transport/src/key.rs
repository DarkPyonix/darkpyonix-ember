//! Peer identity: an Ed25519 key pair. The public key is the peer's address (`PeerId`).
//!
//! These types are ours, not the backend's, so that replacing the transport (SPEC `FR-N5`)
//! does not change any caller. The iroh backend uses the same Ed25519 key bytes, so a key file
//! written today stays valid if the backend changes.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::TransportError;

/// A peer's identity: its Ed25519 public key (32 bytes). Displayed as 64 lowercase hex chars.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId([u8; 32]);

impl PeerId {
    /// Builds a `PeerId` from raw public key bytes (SPEC `FR-N6`).
    ///
    /// The bytes must be the canonical compressed encoding of an Ed25519 point of large order:
    /// `y >= p` aliases, `x = 0` points with the sign bit set and small-order ("weak") points are
    /// refused, so one device has exactly one `PeerId` and a revocation by it cannot be dodged by
    /// re-encoding the key.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, TransportError> {
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| TransportError::InvalidKey("not a valid Ed25519 public key".into()))?;
        if !is_canonical(&bytes) {
            return Err(TransportError::InvalidKey("non-canonical Ed25519 public key".into()));
        }
        if key.is_weak() {
            return Err(TransportError::InvalidKey("weak (small-order) Ed25519 public key".into()));
        }
        Ok(Self(bytes))
    }

    /// The raw public key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Whether `signature` is this peer's Ed25519 signature over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        match VerifyingKey::from_bytes(&self.0) {
            Ok(key) => key.verify(message, &Signature::from_bytes(signature)).is_ok(),
            Err(_) => false,
        }
    }

    /// First 10 hex chars, for logs.
    pub fn fmt_short(&self) -> String {
        hex::encode(&self.0[..5])
    }
}

/// Whether `bytes` is what compressing the decoded point gives back: the 255-bit y is below
/// `p = 2^255 - 19`, and a zero x (y = 1 or y = p - 1) does not carry the sign bit.
fn is_canonical(bytes: &[u8; 32]) -> bool {
    let mut y = *bytes;
    let sign = y[31] & 0x80 != 0;
    y[31] &= 0x7f;
    // y >= p  <=>  y[31] == 0x7f, y[1..31] all 0xff, y[0] >= 0xed.
    if y[31] == 0x7f && y[1..31].iter().all(|b| *b == 0xff) && y[0] >= 0xed {
        return false;
    }
    // x = 0 exactly when y = 1 or y = p - 1; the sign bit must then be clear.
    let y_is_one = y[0] == 1 && y[1..].iter().all(|b| *b == 0);
    let y_is_p_minus_1 = y[0] == 0xec && y[1..31].iter().all(|b| *b == 0xff) && y[31] == 0x7f;
    !(sign && (y_is_one || y_is_p_minus_1))
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({})", self.fmt_short())
    }
}

impl FromStr for PeerId {
    type Err = TransportError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s.trim().to_ascii_lowercase())
            .map_err(|_| TransportError::InvalidKey("peer id is not hex".into()))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| TransportError::InvalidKey("peer id must be 32 bytes".into()))?;
        Self::from_bytes(bytes)
    }
}

impl Serialize for PeerId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PeerId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A peer's secret key (Ed25519 seed, 32 bytes). Never logged.
#[derive(Clone)]
pub struct SecretKey([u8; 32]);

impl SecretKey {
    /// Generates a fresh key from the OS random source.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("OS random source unavailable");
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Ed25519 signature over `message` (the same signature iroh's key produces for these
    /// bytes; used to prove key possession to the hub).
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        SigningKey::from_bytes(&self.0).sign(message).to_bytes()
    }

    /// The matching public identity.
    pub fn peer_id(&self) -> PeerId {
        PeerId(SigningKey::from_bytes(&self.0).verifying_key().to_bytes())
    }

    /// Loads the key stored at `path`, or generates one and writes it there with mode 0600.
    ///
    /// The file holds the 32-byte seed as 64 hex characters and a newline. If an existing file is
    /// readable by group or others, its permissions are tightened to 0600 and a warning is logged.
    pub fn load_or_generate(path: impl AsRef<Path>) -> Result<Self, TransportError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => {
                tighten_permissions(path)?;
                let bytes = hex::decode(text.trim()).map_err(|_| {
                    TransportError::InvalidKey(format!("{}: not hex", path.display()))
                })?;
                let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                    TransportError::InvalidKey(format!("{}: must hold 32 bytes", path.display()))
                })?;
                Ok(Self(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = Self::generate();
                key.write_new(path)?;
                Ok(key)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Writes the key to a new file (fails if it exists) with mode 0600.
    fn write_new(&self, path: &Path) -> Result<(), TransportError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(path)?;
        file.write_all(format!("{}\n", hex::encode(self.0)).as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey(for {})", self.peer_id().fmt_short())
    }
}

#[cfg(unix)]
fn tighten_permissions(path: &Path) -> Result<(), TransportError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{:o}", mode & 0o777),
            "transport key file was readable by others; setting mode 0600"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) -> Result<(), TransportError> {
    Ok(())
}
