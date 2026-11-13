//! FR-W4 / FR-X1 (#521): the local node (rooted at `$HOME`) refuses the server's own data dir and
//! the OSE data root, so `/fs` cannot read the server token, the OSE connection token files or
//! `ember.db`. Not a sandbox: terminals and agents run as the server user (D37).
//!
//! `/api/computers/local/fs/*` answers 400 for the local computer (the fs relay uses
//! `Computers::client`, which has no client for `local`), so the local node is tested through
//! `Computers::local_node_client()`, the same client the terminals use.
#![cfg(unix)]

use std::path::PathBuf;

use ember_node::client::ClientError;
use ember_node::proto::ErrorCode;
use ember_server::computers::local_node::denied_paths;
use ember_server::computers::{self, Computers, Registry};
use ember_server::store::Store;

#[test]
fn fr_w4_local_node_denied_paths_follow_the_data_dir_and_the_ose_root() {
    let p = |s: &str| Some(PathBuf::from(s));
    // Defaults: ~/.ember, and the OSE root in its sibling runtime dir (FR-A11).
    assert_eq!(
        denied_paths(None, None, p("/h")),
        vec![PathBuf::from("/h/.ember"), PathBuf::from("/h/.ember-runtime/ose-data")]
    );
    // EMBER_DATA_DIR replaces ~/.ember, and the runtime dir follows it.
    assert_eq!(denied_paths(p("/d"), None, p("/h")), vec![PathBuf::from("/d"), PathBuf::from("/d-runtime/ose-data")]);
    // An OSE root elsewhere is denied as well.
    assert_eq!(
        denied_paths(p("/d"), p("/o/ose"), p("/h")),
        vec![PathBuf::from("/d"), PathBuf::from("/o/ose")]
    );
    // An OSE root inside the data dir adds nothing.
    assert_eq!(denied_paths(p("/d"), p("/d/ose-data"), p("/h")), vec![PathBuf::from("/d")]);
    // No HOME and no override: nothing to deny (and the local node has no root either).
    assert!(denied_paths(None, None, None).is_empty());
}

#[tokio::test]
async fn fr_w4_local_node_denied_refuses_fs_inside_the_server_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
    let data = home.join(".ember");
    let ose = home.join("custom-ose");
    std::fs::create_dir_all(data.join("accounts")).unwrap();
    std::fs::create_dir_all(ose.join("local/tokens")).unwrap();
    std::fs::create_dir_all(home.join("project")).unwrap();
    std::fs::write(data.join("server.token"), "SERVER-TOKEN").unwrap();
    std::fs::write(ose.join("local/tokens/abc.token"), "OSE-TOKEN").unwrap();
    std::fs::write(home.join("project/readme.txt"), "hello").unwrap();
    // The other test in this binary reads no environment, so setting it here is race free.
    std::env::set_var("HOME", &home);
    std::env::set_var("EMBER_DATA_DIR", &data);
    std::env::set_var("EMBER_OSE_DATA_DIR", &ose);

    let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let c = computers.local_node_client().unwrap();

    assert!(c.read_file(home.join("project/readme.txt")).await.is_ok());
    for p in [data.join("server.token"), ose.join("local/tokens/abc.token"), data.join("accounts")] {
        let e = c.read_file(&p).await.map(drop).unwrap_err();
        assert!(matches!(&e, ClientError::Api { status: 403, .. }), "{p:?}: {e:?}");
        assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath), "{p:?}");
        let e = c.stat(&p).await.unwrap_err();
        assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath), "{p:?}");
    }
    let e = c.list(&data).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));
    let e = c.write_file(data.join("planted"), "x", None).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));
    assert!(!data.join("planted").exists());
}
