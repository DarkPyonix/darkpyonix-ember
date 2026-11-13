//! FR-P5 (#26): with a state dir, the built-in local node writes `<state dir>/local.json` (the
//! node's endpoint file, mode 0600), so `ember-term` started in an OSE terminal with
//! `EMBER_NODE_STATE_DIR` set to that folder reaches the node, and the file's token works on the
//! node's own `/terms`. Written test first; not compiled
//! by the author.

use std::sync::Arc;

use ember_node::config::{LocalEndpoint, LOCAL_ENDPOINT_FILE};
use ember_server::computers::local_node::LocalNodeOptions;
use ember_server::computers::{self, Computers, Registry};
use ember_server::store::Store;

fn computers(opts: LocalNodeOptions) -> Arc<Computers> {
    let c = Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector());
    c.set_local_node_options(opts);
    c
}

#[tokio::test]
async fn fr_p5_local_node_writes_its_endpoint_file_for_ember_term() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("local-node");
    let c = computers(LocalNodeOptions { state_dir: Some(state.clone()), keeper: None });
    c.local_node_client().unwrap();
    let addr = c.local_node_addr().expect("started");
    let ep = LocalEndpoint::read(&state).expect("local.json written");
    assert_eq!(ep.url, format!("http://{addr}"));
    assert_eq!(ep.token.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(state.join(LOCAL_ENDPOINT_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The token in the file is the node's: an ember-term with it may list terminals.
    let r = reqwest::Client::builder().no_proxy().build().unwrap()
        .get(format!("{}/terms", ep.url)).bearer_auth(&ep.token).send().await.unwrap();
    assert_eq!(r.status(), 200);
}
