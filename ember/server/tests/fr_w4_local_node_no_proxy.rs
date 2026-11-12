//! FR-W4 (#474): the local node client ignores proxy variables, so the Bearer token never goes to
//! a proxy named in the server environment. Its own test binary, because it sets process-wide
//! environment variables.
//! Written test first; not compiled by the author (sub-agents do not build).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ember_node::proto::*;
use ember_server::computers::{self, Computers, Registry};
use ember_server::store::Store;

#[tokio::test]
async fn fr_w4_local_node_client_does_not_use_a_proxy_from_the_environment() {
    // A "proxy" that counts connections and answers nothing useful.
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = proxy.accept().await {
            h.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        std::env::set_var(k, &proxy_url);
    }
    for k in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(k);
    }

    let c = Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector());
    let client = c.local_node_client().unwrap();
    let list = tokio::time::timeout(Duration::from_secs(20), client.terms(&TermListQuery::default()))
        .await
        .expect("answered")
        .expect("the local node answers directly, not through the proxy");
    assert!(list.is_empty());
    let health = client.with_deadline(Duration::from_secs(10)).terms(&TermListQuery::default()).await;
    assert!(health.is_ok(), "with_deadline keeps no_proxy: {health:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no connection reached the proxy");
}
