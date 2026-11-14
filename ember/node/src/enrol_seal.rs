//! The two cryptographic pieces of the enrol flow (SPEC `FR-C2d`, `FR-C2j`, INTENT D39, issues
//! #540 and #541, all [provisional]). Written, not compiled.
//!
//! **Sealed delivery (#540).** The node makes a one-time X25519 key pair for each enrol run and
//! sends the public half in `node_public_info.enrol_key`. The server never sends the node token
//! in clear: it makes its own one-time X25519 key, derives a ChaCha20-Poly1305 key from the
//! shared secret with HKDF-SHA256, and sends the sealed token ([`Sealed`]). Whoever only reads
//! the traffic, even with the poll secret in hand, holds neither private key and cannot read the
//! token. The node's private key lives in memory only and is gone when the run ends.
//!
//! **Server proof (#541).** A server that has an identity key (its Ed25519 transport key) signs
//! the enrol id and the node's `enrol_key` ([`sign_proof`]) and returns the signature in the
//! redeem answer. The node checks it against `--server-fingerprint` before it polls for any
//! credential ([`verify_proof`]). The signature covers the node's own fresh key, so an answer
//! recorded from another run does not verify, and a machine in the middle that swaps the key
//! cannot produce a valid proof for the swapped key without the server's private key.
//!
//! Both sides use this one module (the server depends on `ember-node`), so the two ends cannot
//! disagree about the construction.
//!
//! | Piece | Value |
//! | ----- | ----- |
//! | Key agreement | X25519, one-time key on each side; an all-zero shared secret is refused |
//! | Key derivation | HKDF-SHA256, salt [`SEAL_CONTEXT`], info `enrol_id 0x00 node_key server_key` |
//! | Cipher | ChaCha20-Poly1305, 12 zero bytes of nonce (each key seals one message) |
//! | Associated data | [`SEAL_CONTEXT`], a newline, the enrol id |
//! | Proof | Ed25519 over [`PROOF_CONTEXT`], a newline, the enrol id, a newline, `enrol_key` |

use std::fmt;

use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// The version of the sealed form ([`Sealed::v`]).
pub const SEAL_VERSION: u8 = 1;
/// Domain separation of the seal: the HKDF salt and the start of the associated data.
pub const SEAL_CONTEXT: &str = "ember-enrol-seal-v1";
/// Domain separation of the server proof: the start of the signed message.
pub const PROOF_CONTEXT: &str = "ember-enrol-server-proof-v1";
/// The longest enrol id either function accepts (the server's ids are 36-character UUIDs).
pub const MAX_ENROL_ID_CHARS: usize = 128;
/// The longest sealed text accepted, in hex characters (a token is a few hundred bytes at most).
pub const MAX_SEALED_HEX_CHARS: usize = 4096;

/// Why a seal or an open failed. No variant carries key material or the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    #[error("the enrol key is not 64 lowercase hex characters")]
    BadKey,
    #[error("the enrol key is a weak point")]
    WeakKey,
    #[error("the enrol id is not valid")]
    BadEnrolId,
    #[error("the sealed credential has an unknown version")]
    Version,
    #[error("the sealed credential is malformed")]
    Malformed,
    #[error("the sealed credential cannot be opened with this node's key")]
    Open,
    #[error("the credential could not be sealed")]
    Seal,
}

/// A node token sealed to a node's enrol key. Holds no secret: it is what crosses the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sealed {
    /// [`SEAL_VERSION`].
    pub v: u8,
    /// The server's one-time X25519 public key, 64 lowercase hex characters.
    pub epk: String,
    /// The ciphertext and its 16-byte tag, lowercase hex.
    pub ct: String,
}

/// Whether `s` has the shape of an enrol key: exactly 64 lowercase hex characters.
pub fn is_key_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn key_bytes(hex_key: &str) -> Result<[u8; 32], SealError> {
    if !is_key_hex(hex_key) {
        return Err(SealError::BadKey);
    }
    let bytes = hex::decode(hex_key).map_err(|_| SealError::BadKey)?;
    bytes.try_into().map_err(|_| SealError::BadKey)
}

fn id_ok(enrol_id: &str) -> bool {
    !enrol_id.is_empty()
        && enrol_id.len() <= MAX_ENROL_ID_CHARS
        && enrol_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 32 bytes from the OS random source. `generate_key` of the cipher is the one random-bytes call
/// this crate already links (the `getrandom` feature of `chacha20poly1305`).
fn random_32() -> Zeroizing<[u8; 32]> {
    let key = ChaCha20Poly1305::generate_key(&mut OsRng);
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(key.as_slice());
    out
}

/// Whether `hex_key` can be sealed to: the right shape, and not one of the few weak points whose
/// shared secret is all zero whatever the other key is. The server checks this at redeem, so a
/// key that would fail at delivery (when the token can no longer be handed over twice) is refused
/// before the code is spent.
pub fn key_ok(hex_key: &str) -> bool {
    let Ok(bytes) = key_bytes(hex_key) else { return false };
    // Any fixed scalar shows a weak point: the result is all zero for every scalar.
    let probe = StaticSecret::from([0x42u8; 32]);
    probe.diffie_hellman(&PublicKey::from(bytes)).was_contributory()
}

/// The symmetric key of one seal.
fn derive(shared: &[u8; 32], enrol_id: &str, node_key: &[u8; 32], server_key: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, SealError> {
    let mut info = Vec::with_capacity(enrol_id.len() + 1 + 64);
    info.extend_from_slice(enrol_id.as_bytes());
    info.push(0);
    info.extend_from_slice(node_key);
    info.extend_from_slice(server_key);
    let mut okm = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(SEAL_CONTEXT.as_bytes()), shared)
        .expand(&info, okm.as_mut_slice())
        .map_err(|_| SealError::Seal)?;
    Ok(okm)
}

fn aad(enrol_id: &str) -> Vec<u8> {
    format!("{SEAL_CONTEXT}\n{enrol_id}").into_bytes()
}

/// Seals `token` for the node that sent `node_key_hex` in the redeem of `enrol_id`. Each call
/// makes a fresh server key, so two seals of one token differ.
pub fn seal(enrol_id: &str, node_key_hex: &str, token: &str) -> Result<Sealed, SealError> {
    if !id_ok(enrol_id) {
        return Err(SealError::BadEnrolId);
    }
    let node_key = key_bytes(node_key_hex)?;
    let secret = StaticSecret::from(*random_32());
    let public = PublicKey::from(&secret);
    let shared = secret.diffie_hellman(&PublicKey::from(node_key));
    if !shared.was_contributory() {
        return Err(SealError::WeakKey);
    }
    let key = derive(shared.as_bytes(), enrol_id, &node_key, public.as_bytes())?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_slice()));
    let ct = cipher
        .encrypt(Nonce::from_slice(&[0u8; 12]), Payload { msg: token.as_bytes(), aad: &aad(enrol_id) })
        .map_err(|_| SealError::Seal)?;
    Ok(Sealed { v: SEAL_VERSION, epk: hex::encode(public.as_bytes()), ct: hex::encode(ct) })
}

/// A node's one-time enrol key pair. The private half never leaves this value: it is not
/// serialised, not printed, and wiped when the value is dropped.
pub struct NodeKey {
    secret: StaticSecret,
    public: [u8; 32],
}

impl NodeKey {
    /// A fresh key from the OS random source.
    pub fn generate() -> NodeKey {
        NodeKey::from_bytes(*random_32())
    }

    /// The key pair of a given private scalar. For tests that need a known key; a real run uses
    /// [`NodeKey::generate`].
    pub fn from_bytes(secret: [u8; 32]) -> NodeKey {
        let secret = StaticSecret::from(secret);
        let public = PublicKey::from(&secret).to_bytes();
        NodeKey { secret, public }
    }

    /// The public half as it goes into `node_public_info.enrol_key`: 64 lowercase hex characters.
    pub fn public_hex(&self) -> String {
        hex::encode(self.public)
    }

    /// Opens what [`seal`] made for this key and `enrol_id`. A sealed value for another key,
    /// another enrol id, or one changed on the way is [`SealError::Open`].
    pub fn open(&self, enrol_id: &str, sealed: &Sealed) -> Result<Zeroizing<String>, SealError> {
        if !id_ok(enrol_id) {
            return Err(SealError::BadEnrolId);
        }
        if sealed.v != SEAL_VERSION {
            return Err(SealError::Version);
        }
        let server_key = key_bytes(&sealed.epk).map_err(|_| SealError::Malformed)?;
        let ct_shape = !sealed.ct.is_empty()
            && sealed.ct.len() <= MAX_SEALED_HEX_CHARS
            && sealed.ct.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !ct_shape {
            return Err(SealError::Malformed);
        }
        let ct = hex::decode(&sealed.ct).map_err(|_| SealError::Malformed)?;
        let shared = self.secret.diffie_hellman(&PublicKey::from(server_key));
        if !shared.was_contributory() {
            return Err(SealError::Malformed);
        }
        let key = derive(shared.as_bytes(), enrol_id, &self.public, &server_key)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_slice()));
        let plain = Zeroizing::new(
            cipher
                .decrypt(Nonce::from_slice(&[0u8; 12]), Payload { msg: &ct, aad: &aad(enrol_id) })
                .map_err(|_| SealError::Open)?,
        );
        let text = std::str::from_utf8(&plain).map_err(|_| SealError::Open)?;
        Ok(Zeroizing::new(text.to_string()))
    }
}

impl fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The public half only.
        write!(f, "NodeKey({})", &self.public_hex()[..10])
    }
}

/// The bytes a server signs for `enrol_id` and the node key of that redeem. `None` when either
/// has the wrong shape (the newline separators are unambiguous only for these shapes).
pub fn proof_message(enrol_id: &str, node_key_hex: &str) -> Option<Vec<u8>> {
    if !id_ok(enrol_id) || !is_key_hex(node_key_hex) {
        return None;
    }
    Some(format!("{PROOF_CONTEXT}\n{enrol_id}\n{node_key_hex}").into_bytes())
}

/// The server's proof for one redeem: its Ed25519 signature over [`proof_message`], 128 lowercase
/// hex characters. `None` when the enrol id or the node key has the wrong shape.
pub fn sign_proof(server_key: &ember_transport::SecretKey, enrol_id: &str, node_key_hex: &str) -> Option<String> {
    let message = proof_message(enrol_id, node_key_hex)?;
    Some(hex::encode(server_key.sign(&message)))
}

/// Whether `proof_hex` is the signature of the server `server_peer_id` (64 lowercase hex) over
/// this enrol id and this node key. False for any malformed input.
pub fn verify_proof(server_peer_id: &str, enrol_id: &str, node_key_hex: &str, proof_hex: &str) -> bool {
    if !is_key_hex(server_peer_id) || proof_hex.len() != 128 {
        return false;
    }
    let Ok(peer) = server_peer_id.parse::<ember_transport::PeerId>() else { return false };
    let Some(message) = proof_message(enrol_id, node_key_hex) else { return false };
    let Ok(sig) = hex::decode(proof_hex) else { return false };
    let Ok(sig) = <[u8; 64]>::try_from(sig) else { return false };
    peer.verify(&message, &sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b6f2c1e-enrol-1";
    const TOKEN: &str = "dpn_0123456789abcdef-token";

    #[test]
    fn fr_c2d_seal_the_node_opens_what_the_server_sealed() {
        let node = NodeKey::generate();
        let sealed = seal(ID, &node.public_hex(), TOKEN).unwrap();
        assert_eq!(sealed.v, SEAL_VERSION);
        assert!(is_key_hex(&sealed.epk));
        assert_eq!(node.open(ID, &sealed).unwrap().as_str(), TOKEN);
    }

    #[test]
    fn fr_c2d_seal_the_sealed_form_does_not_contain_the_token() {
        let node = NodeKey::generate();
        let sealed = seal(ID, &node.public_hex(), TOKEN).unwrap();
        let wire = serde_json::to_string(&sealed).unwrap();
        assert!(!wire.contains(TOKEN) && !wire.contains(&hex::encode(TOKEN)), "{wire}");
        // 16 bytes of tag after the token's bytes.
        assert_eq!(sealed.ct.len(), (TOKEN.len() + 16) * 2);
    }

    #[test]
    fn fr_c2d_seal_another_key_cannot_open_it() {
        let node = NodeKey::generate();
        let other = NodeKey::generate();
        let sealed = seal(ID, &node.public_hex(), TOKEN).unwrap();
        assert_eq!(other.open(ID, &sealed).unwrap_err(), SealError::Open);
    }

    #[test]
    fn fr_c2d_seal_is_bound_to_the_enrol_id() {
        let node = NodeKey::generate();
        let sealed = seal(ID, &node.public_hex(), TOKEN).unwrap();
        assert_eq!(node.open("another-enrol", &sealed).unwrap_err(), SealError::Open);
    }

    #[test]
    fn fr_c2d_seal_a_changed_ciphertext_or_key_is_refused() {
        let node = NodeKey::generate();
        let sealed = seal(ID, &node.public_hex(), TOKEN).unwrap();
        let mut flipped = sealed.clone();
        let last = if flipped.ct.ends_with('0') { "1" } else { "0" };
        flipped.ct.replace_range(flipped.ct.len() - 1.., last);
        assert_eq!(node.open(ID, &flipped).unwrap_err(), SealError::Open);
        let mut other_epk = sealed.clone();
        other_epk.epk = NodeKey::generate().public_hex();
        assert_eq!(node.open(ID, &other_epk).unwrap_err(), SealError::Open);
        let mut version = sealed.clone();
        version.v = 2;
        assert_eq!(node.open(ID, &version).unwrap_err(), SealError::Version);
        for bad in ["", "zz", "ABCD"] {
            let mut m = sealed.clone();
            m.ct = bad.into();
            assert_eq!(node.open(ID, &m).unwrap_err(), SealError::Malformed, "{bad:?}");
        }
        let mut m = sealed;
        m.epk = "nothex".into();
        assert_eq!(node.open(ID, &m).unwrap_err(), SealError::Malformed);
    }

    #[test]
    fn fr_c2d_seal_two_seals_of_one_token_differ() {
        let node = NodeKey::generate();
        let a = seal(ID, &node.public_hex(), TOKEN).unwrap();
        let b = seal(ID, &node.public_hex(), TOKEN).unwrap();
        assert_ne!(a.epk, b.epk);
        assert_ne!(a.ct, b.ct);
    }

    #[test]
    fn fr_c2d_seal_a_known_key_gives_a_stable_public_half() {
        let a = NodeKey::from_bytes([7u8; 32]);
        let b = NodeKey::from_bytes([7u8; 32]);
        assert_eq!(a.public_hex(), b.public_hex());
        assert!(key_ok(&a.public_hex()));
        assert_ne!(a.public_hex(), NodeKey::from_bytes([8u8; 32]).public_hex());
        assert!(!format!("{a:?}").contains(&hex::encode([7u8; 32])));
    }

    #[test]
    fn fr_c2d_seal_refuses_weak_and_malformed_keys() {
        // The all-zero point and the point 1 have small order: the shared secret is all zero.
        let zero = "00".repeat(32);
        let one = format!("01{}", "00".repeat(31));
        for weak in [&zero, &one] {
            assert!(!key_ok(weak), "{weak}");
            assert_eq!(seal(ID, weak, TOKEN).unwrap_err(), SealError::WeakKey, "{weak}");
        }
        for bad in ["", "ab", &"g".repeat(64), &"AB".repeat(32), &"a".repeat(63), &"a".repeat(65)] {
            assert!(!key_ok(bad), "{bad}");
            assert_eq!(seal(ID, bad, TOKEN).unwrap_err(), SealError::BadKey, "{bad}");
        }
        let node = NodeKey::generate();
        for bad_id in ["", "has space", "new\nline", &"i".repeat(MAX_ENROL_ID_CHARS + 1)] {
            assert_eq!(seal(bad_id, &node.public_hex(), TOKEN).unwrap_err(), SealError::BadEnrolId, "{bad_id:?}");
        }
    }

    #[test]
    fn fr_c2j_proof_verifies_for_the_signing_server_only() {
        let server = ember_transport::SecretKey::generate();
        let sid = server.peer_id().to_string();
        let node = NodeKey::generate().public_hex();
        let proof = sign_proof(&server, ID, &node).unwrap();
        assert_eq!(proof.len(), 128);
        assert!(verify_proof(&sid, ID, &node, &proof));
        // Another server's id, another enrol id, another node key: all refused.
        let rogue = ember_transport::SecretKey::generate();
        assert!(!verify_proof(&rogue.peer_id().to_string(), ID, &node, &proof));
        assert!(!verify_proof(&sid, "another-enrol", &node, &proof));
        assert!(!verify_proof(&sid, ID, &NodeKey::generate().public_hex(), &proof));
        // A rogue server that claims the real id cannot make the proof.
        let forged = sign_proof(&rogue, ID, &node).unwrap();
        assert!(!verify_proof(&sid, ID, &node, &forged));
    }

    #[test]
    fn fr_c2j_proof_refuses_malformed_input() {
        let server = ember_transport::SecretKey::generate();
        let sid = server.peer_id().to_string();
        let node = NodeKey::generate().public_hex();
        let proof = sign_proof(&server, ID, &node).unwrap();
        assert!(!verify_proof(&sid.to_uppercase(), ID, &node, &proof));
        assert!(!verify_proof("", ID, &node, &proof));
        assert!(!verify_proof(&sid, ID, &node, ""));
        assert!(!verify_proof(&sid, ID, &node, &proof[..126]));
        assert!(!verify_proof(&sid, ID, &node, &"zz".repeat(64)));
        assert!(!verify_proof(&sid, "bad id", &node, &proof));
        assert!(sign_proof(&server, "bad id", &node).is_none());
        assert!(sign_proof(&server, ID, "nothex").is_none());
        assert!(proof_message(ID, &node).unwrap().starts_with(PROOF_CONTEXT.as_bytes()));
    }
}
