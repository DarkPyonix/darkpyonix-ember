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
