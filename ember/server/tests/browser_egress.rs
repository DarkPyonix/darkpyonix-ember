//! Browser egress through a computer (FR-R1), without Chrome: the server's loopback SOCKS5
//! listener for a registered computer → that node's `/egress` → a TCP target, exactly the path
//! Chrome's `--proxy-server=socks5://127.0.0.1:<port>` takes. Plus: a computer a browser egresses
//! through cannot be removed, and the browser manager resolves the computer to the listener.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ember_node::api::{self as node_api, Node};
use ember_node::config::NodeConfig;
use ember_node::egress::{self, reply, Target};
use ember_server::browser::{BrowserConfig, BrowserManager, Egress, EgressResolver, EgressStore, ScreencastOptions};
use ember_server::computers::{self, ComputerError, Computers, Registry, LOCAL};
use ember_server::store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "node-token";

async fn start_node() -> String {
    let node = Node::new(NodeConfig::new(TOKEN, vec![std::env::temp_dir()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(node_api::serve(listener, node));
    base
}

async fn echo() -> SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

fn proxy_addr(url: &str) -> SocketAddr {
    url.strip_prefix("socks5://").expect("socks5 URL").parse().unwrap()
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut).await.expect("timed out")
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_listener_egresses_through_the_node() {
    let base = start_node().await;
    let target = echo().await;
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let pi = computers.register("pi", &base, TOKEN).unwrap();

    // The local computer is direct; a registered one gets a loopback SOCKS5 listener, reused.
    assert_eq!(computers.egress_proxy(LOCAL).unwrap(), None);
    let url = computers.egress_proxy(&pi.id).unwrap().expect("a proxy URL");
    assert!(url.starts_with("socks5://127.0.0.1:"), "{url}");
    assert_eq!(computers.egress_proxy(&pi.id).unwrap().as_deref(), Some(url.as_str()));
    assert!(matches!(computers.egress_proxy("nope"), Err(ComputerError::NotFound(_))));

    within(async {
        for t in [Target::Ip(target), Target::Domain("localhost".into(), target.port())] {
            let mut s = tokio::net::TcpStream::connect(proxy_addr(&url)).await.unwrap();
            // What Chrome does: no-auth SOCKS5, then CONNECT.
            assert_eq!(egress::client_connect(&mut s, &t, None).await.unwrap(), reply::SUCCEEDED, "{t}");
            s.write_all(b"via the node").await.unwrap();
            let mut got = [0u8; 12];
            s.read_exact(&mut got).await.unwrap();
            assert_eq!(&got, b"via the node");
        }
    })
    .await;

    // The browser manager resolves the computer to that listener (no Chrome needed).
    let dir = tempfile::tempdir().unwrap();
    let cfg = BrowserConfig {
        root: dir.path().to_path_buf(),
        chrome: None,
        headless: true,
        window: (800, 600),
        screencast: ScreencastOptions::default(),
    };
    let m = BrowserManager::with_egress(
        cfg,
        Some(store.clone() as Arc<dyn EgressStore>),
        Some(computers.clone() as Arc<dyn EgressResolver>),
    );
    let choice = Egress::Computer { id: pi.id.clone() };
    let b = m.set_egress("acme", choice.clone(), false).await.unwrap();
    assert_eq!(b.resolve_egress(&choice).unwrap().as_deref(), Some(url.as_str()));
    assert_eq!(store.load("acme").unwrap(), Some(choice));

    // A computer in use as browser egress cannot be removed.
    assert!(matches!(computers.remove(&pi.id), Err(ComputerError::EgressInUse(_, ref p)) if p == &["acme".to_string()]));
    m.set_egress("acme", Egress::Direct, false).await.unwrap();
    computers.remove(&pi.id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_node_closes_the_proxied_connection() {
    // A registered computer whose node is not running: the listener accepts, the node stream
    // fails, and the client sees the connection close (Chrome: ERR_SOCKS_CONNECTION_FAILED).
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let computers = Computers::new(Registry::open_in_memory().unwrap(), computers::default_connector());
    let c = computers.register("gone", &format!("http://{dead}"), TOKEN).unwrap();
    let url = computers.egress_proxy(&c.id).unwrap().unwrap();
    within(async {
        let mut s = tokio::net::TcpStream::connect(proxy_addr(&url)).await.unwrap();
        let r = egress::client_connect(&mut s, &Target::Ip(dead), None).await;
        assert!(r.is_err(), "{r:?}");
    })
    .await;
}
