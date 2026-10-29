use ember_transport::{PeerId, SecretKey};

#[test]
fn key_file_is_created_0600_and_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/transport.key");
    let k1 = SecretKey::load_or_generate(&path).unwrap();
    let k2 = SecretKey::load_or_generate(&path).unwrap();
    assert_eq!(k1.peer_id(), k2.peer_id());
    assert_eq!(k1.to_bytes(), k2.to_bytes());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Loosened permissions are tightened on load.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        SecretKey::load_or_generate(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn corrupt_key_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.key");
    std::fs::write(&path, "not hex").unwrap();
    assert!(SecretKey::load_or_generate(&path).is_err());
}

#[test]
fn peer_id_display_parse_and_serde() {
    let id = SecretKey::generate().peer_id();
    let text = id.to_string();
    assert_eq!(text.len(), 64);
    assert_eq!(text.parse::<PeerId>().unwrap(), id);
    assert!("zz".parse::<PeerId>().is_err());
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<PeerId>(&json).unwrap(), id);
}

#[test]
fn distinct_keys_distinct_ids() {
    assert_ne!(SecretKey::generate().peer_id(), SecretKey::generate().peer_id());
}

#[test]
fn sign_and_verify() {
    let key = SecretKey::generate();
    let msg = b"darkpyonix-hub/v2/link\nl_00\nabcd";
    let sig = key.sign(msg);
    assert!(key.peer_id().verify(msg, &sig));
    assert!(!key.peer_id().verify(b"other", &sig));
    assert!(!SecretKey::generate().peer_id().verify(msg, &sig));
    // Deterministic (RFC 8032).
    assert_eq!(sig, key.sign(msg));
}

// ---- FR-N6: canonical, non-weak device identities (#235) ----

fn unhex(s: &str) -> [u8; 32] {
    hex::decode(s).unwrap().try_into().unwrap()
}

/// Every small-order point, in both sign-bit encodings where they differ.
const SMALL_ORDER: [&str; 8] = [
    "0100000000000000000000000000000000000000000000000000000000000000",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "0000000000000000000000000000000000000000000000000000000000000080",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
];

/// `bytes` with `p = 2^255 - 19` added to the 255-bit y, the same point encoded non-canonically.
fn add_p(mut bytes: [u8; 32]) -> Option<[u8; 32]> {
    let sign = bytes[31] & 0x80;
    bytes[31] &= 0x7f;
    let mut p = [0xffu8; 32];
    p[0] = 0xed;
    p[31] = 0x7f;
    let mut carry = 0u16;
    for i in 0..32 {
        let s = bytes[i] as u16 + p[i] as u16 + carry;
        bytes[i] = s as u8;
        carry = s >> 8;
    }
    if bytes[31] & 0x80 != 0 || carry != 0 {
        return None; // does not fit in 255 bits
    }
    bytes[31] |= sign;
    Some(bytes)
}

/// A valid key whose y is small enough to have a non-canonical `y + p` alias, with that alias.
fn key_with_alias() -> ([u8; 32], [u8; 32]) {
    for y in 2u8..19 {
        for sign in [0u8, 0x80] {
            let mut b = [0u8; 32];
            b[0] = y;
            b[31] = sign;
            if let Ok(id) = PeerId::from_bytes(b) {
                if let Some(alias) = add_p(b) {
                    return (*id.as_bytes(), alias);
                }
            }
        }
    }
    panic!("no small-y valid key found");
}

#[test]
fn fr_n6_small_order_keys_are_rejected_everywhere() {
    for h in SMALL_ORDER {
        let b = unhex(h);
        assert!(PeerId::from_bytes(b).is_err(), "from_bytes accepted {h}");
        assert!(h.parse::<PeerId>().is_err(), "FromStr accepted {h}");
        assert!(serde_json::from_str::<PeerId>(&format!("\"{h}\"")).is_err(), "Deserialize accepted {h}");
    }
}

#[test]
fn fr_n6_x_zero_with_sign_bit_set_is_rejected() {
    // y = 1 gives x = 0; the sign bit on a zero x is "negative zero".
    let mut b = [0u8; 32];
    b[0] = 1;
    b[31] = 0x80;
    assert!(PeerId::from_bytes(b).is_err());
    assert!(hex::encode(b).parse::<PeerId>().is_err());
    assert!(serde_json::from_str::<PeerId>(&format!("\"{}\"", hex::encode(b))).is_err());
}

#[test]
fn fr_n6_non_canonical_y_alias_of_a_valid_key_is_rejected() {
    let (canonical, alias) = key_with_alias();
    assert_ne!(canonical, alias);
    assert!(PeerId::from_bytes(canonical).is_ok());
    assert!(PeerId::from_bytes(alias).is_err());
    assert!(hex::encode(alias).parse::<PeerId>().is_err());
    assert!(serde_json::from_str::<PeerId>(&format!("\"{}\"", hex::encode(alias))).is_err());
}

#[test]
fn fr_n6_hex_is_case_insensitive_and_stored_lowercase() {
    let id = SecretKey::generate().peer_id();
    let upper = id.to_string().to_uppercase();
    let parsed: PeerId = upper.parse().unwrap();
    assert_eq!(parsed, id);
    assert_eq!(parsed.to_string(), id.to_string());
    assert_eq!(serde_json::to_string(&parsed).unwrap(), format!("\"{}\"", id));
    let via_serde: PeerId = serde_json::from_str(&format!("\"{upper}\"")).unwrap();
    assert_eq!(via_serde, id);
}

#[test]
fn fr_n6_alias_of_an_allowed_key_is_not_admitted() {
    let (canonical, alias) = key_with_alias();
    let allowed = PeerId::from_bytes(canonical).unwrap();
    let gate = ember_transport::PeerGate::allow_list([allowed]);
    assert!(gate.is_allowed(&allowed));
    // The alias cannot even be named as a PeerId, so it cannot be asked of the gate.
    assert!(PeerId::from_bytes(alias).is_err());
}

#[test]
fn fr_n6_alias_of_a_revoked_key_cannot_connect() {
    let (canonical, alias) = key_with_alias();
    let revoked = PeerId::from_bytes(canonical).unwrap();
    let gate = ember_transport::PeerGate::open();
    gate.revoke(&revoked);
    assert!(!gate.is_allowed(&revoked));
    assert!(PeerId::from_bytes(alias).is_err());
}
