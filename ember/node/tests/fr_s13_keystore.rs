//! FR-S13: secret keys in the OS keystore with a file fallback (#784). Every test uses the
//! in-memory store, so no real keystore is needed.

use std::sync::Arc;

use ember_node::keystore::{
    account_for, choose, load_transport_key_with, migrated_marker, KeyStore, MemoryKeyStore, Mode,
};

#[test]
fn fr_s13_mode_parses_file_os_auto_and_defaults_to_auto() {
    assert_eq!(Mode::parse(None).unwrap(), Mode::Auto);
    assert_eq!(Mode::parse(Some("")).unwrap(), Mode::Auto);
    assert_eq!(Mode::parse(Some("file")).unwrap(), Mode::File);
    assert_eq!(Mode::parse(Some("os")).unwrap(), Mode::Os);
    assert!(Mode::parse(Some("vault")).is_err());
}

#[test]
fn fr_s13_auto_picks_the_file_when_the_os_store_is_unavailable() {
    let down: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore { unavailable: true, ..Default::default() });
    assert!(choose(Mode::Auto, down.clone()).unwrap().is_none());
    assert!(choose(Mode::Os, down).is_err(), "os mode refuses to start without a keystore");
    let up: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    assert!(choose(Mode::Auto, up.clone()).unwrap().is_some());
    assert!(choose(Mode::File, up).unwrap().is_none());
}

#[test]
fn fr_s13_file_fallback_creates_and_reuses_the_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transport.key");
    let a = load_transport_key_with(None, &path).unwrap();
    assert!(path.is_file());
    let b = load_transport_key_with(None, &path).unwrap();
    assert_eq!(a.to_bytes(), b.to_bytes());
}

#[test]
fn fr_s13_existing_file_migrates_into_the_store_and_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transport.key");
    let original = load_transport_key_with(None, &path).unwrap();
    let store = MemoryKeyStore::default();
    let migrated = load_transport_key_with(Some(&store), &path).unwrap();
    assert_eq!(migrated.to_bytes(), original.to_bytes(), "same identity after migration");
    assert!(!path.exists(), "file removed after a verified read-back");
    assert!(migrated_marker(&path).is_file(), "marker left beside it");
    let stored = store.get(&account_for(&path)).unwrap().unwrap();
    assert_eq!(stored, original.to_bytes().to_vec());
    // The next start reads the store.
    let again = load_transport_key_with(Some(&store), &path).unwrap();
    assert_eq!(again.to_bytes(), original.to_bytes());
}

#[test]
fn fr_s13_read_back_mismatch_keeps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transport.key");
    let original = load_transport_key_with(None, &path).unwrap();
    let store = MemoryKeyStore { corrupt_set: true, ..Default::default() };
    let key = load_transport_key_with(Some(&store), &path).unwrap();
    assert_eq!(key.to_bytes(), original.to_bytes());
    assert!(path.is_file(), "the file is never deleted without a matching read-back");
    assert!(!migrated_marker(&path).exists());
}

#[test]
fn fr_s13_write_failure_keeps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transport.key");
    let original = load_transport_key_with(None, &path).unwrap();
    let store = MemoryKeyStore { fail_set: true, ..Default::default() };
    let key = load_transport_key_with(Some(&store), &path).unwrap();
    assert_eq!(key.to_bytes(), original.to_bytes());
    assert!(path.is_file());
}

#[test]
fn fr_s13_new_key_goes_straight_to_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transport.key");
    let store = MemoryKeyStore::default();
    let a = load_transport_key_with(Some(&store), &path).unwrap();
    assert!(!path.exists(), "no file when the store holds the key");
    let b = load_transport_key_with(Some(&store), &path).unwrap();
    assert_eq!(a.to_bytes(), b.to_bytes());
}

#[test]
fn fr_s13_two_data_dirs_get_two_accounts() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    assert_ne!(account_for(&a.path().join("transport.key")), account_for(&b.path().join("transport.key")));
}
