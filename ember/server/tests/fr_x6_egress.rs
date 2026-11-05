//! FR-X6 / INTENT D19: the agent CLI's API traffic leaves through the session's current
//! computer. A loopback HTTP CONNECT proxy per session tunnels over the node's `/egress`
//! (in-process fake nodes here; a node that denies loopback shows up as 403, so the status tells
//! which node dialled).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use ember_node::api::{self as node_api, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::egress::EgressPolicy;
use ember_server::agents::{AcpName, AgentKind};
use ember_server::egress_proxy::{Route, SessionEgress};
use ember_server::events::SessionStatus;
use ember_server::store::SessionRecord;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TOKEN: &str = "node-token";

/// A node; `deny_loopback` makes its egress refuse 127.0.0.0/8 (the "wrong" computer in tests).
async fn start_node(deny_loopback: bool) -> NodeClient {
    let mut cfg = NodeConfig::new(TOKEN, vec![std::env::temp_dir()]);
    if deny_loopback {
        cfg.egress = EgressPolicy::with_deny_list("loopback").unwrap();
    }
    let node = Node::new(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(node_api::serve(listener, node));
    NodeClient::new(&base, TOKEN).unwrap()
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

/// `(user:pass, host:port)` of a `http://user:pass@host:port` proxy URL.
fn split(url: &str) -> (String, String) {
    let rest = url.strip_prefix("http://").unwrap();
    let (cred, addr) = rest.split_once('@').unwrap();
    (cred.to_string(), addr.to_string())
}

/// CONNECT `target` through the proxy; returns the status line and the open stream.
async fn connect(proxy_url: &str, target: SocketAddr) -> (String, TcpStream) {
    connect_to(proxy_url, &target.to_string()).await
}

/// CONNECT an authority string (`host:port`, a domain or an address).
async fn connect_to(proxy_url: &str, target: &str) -> (String, TcpStream) {
    let (cred, addr) = split(proxy_url);
    let mut s = TcpStream::connect(addr).await.unwrap();
    let auth = base64::engine::general_purpose::STANDARD.encode(cred);
    let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: Basic {auth}\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        tokio::time::timeout(Duration::from_secs(20), s.read_exact(&mut b)).await.expect("timed out").unwrap();
        head.push(b[0]);
    }
    let status = String::from_utf8(head).unwrap().lines().next().unwrap().to_string();
    (status, s)
}

async fn echoes(s: &mut TcpStream) {
    s.write_all(b"ping").await.unwrap();
    let mut got = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(20), s.read_exact(&mut got)).await.expect("timed out").unwrap();
    assert_eq!(&got, b"ping");
}

fn route_to(current: Arc<Mutex<Option<NodeClient>>>) -> Route {
    Arc::new(move |_sid| Ok(current.lock().unwrap().clone()))
}

#[tokio::test]
async fn fr_x6_agent_env_points_at_the_session_proxy() {
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(None))));
    let env = eg.env_for("s1").unwrap();
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let url = get("HTTPS_PROXY").expect("HTTPS_PROXY");
    assert!(url.starts_with("http://ember:") && url.contains("@127.0.0.1:"), "{url}");
    assert_eq!(get("HTTP_PROXY"), Some(url.clone()));
    assert_eq!(get("https_proxy"), Some(url.clone()));
    assert!(get("NO_PROXY").unwrap().contains("127.0.0.1"));
    // Same session: same proxy. Another session: its own proxy and credential.
    assert_eq!(eg.proxy_url("s1").unwrap(), url);
    assert_ne!(eg.proxy_url("s2").unwrap(), url);
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_connect_is_dialled_by_the_node() {
    let node = start_node(false).await;
    let target = echo().await;
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(Some(node)))));
    let url = eg.proxy_url("s1").unwrap();
    let (status, mut s) = connect(&url, target).await;
    assert!(status.contains("200"), "{status}");
    echoes(&mut s).await;

    // A node that refuses the destination: the proxy answers 403, not a direct connection.
    let denying = start_node(true).await;
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(Some(denying)))));
    let (status, _) = connect(&eg.proxy_url("s1").unwrap(), target).await;
    assert!(status.contains("403"), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_main_server_location_dials_directly() {
    let target = echo().await;
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(None))));
    let (status, mut s) = connect(&eg.proxy_url("s1").unwrap(), target).await;
    assert!(status.contains("200"), "{status}");
    echoes(&mut s).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_switching_computer_routes_new_connections_to_the_new_node() {
    let (a, b) = (start_node(false).await, start_node(true).await);
    let target = echo().await;
    let current = Arc::new(Mutex::new(Some(a)));
    let eg = SessionEgress::new(route_to(current.clone()));
    let url = eg.proxy_url("s1").unwrap();

    let (status, mut first) = connect(&url, target).await;
    assert!(status.contains("200"), "on A: {status}");

    *current.lock().unwrap() = Some(b);
    let (status, _) = connect(&url, target).await;
    assert!(status.contains("403"), "after the switch B decides: {status}");
    // The tunnel opened on A keeps working.
    echoes(&mut first).await;

    // Back on the main server: direct.
    *current.lock().unwrap() = None;
    let (status, mut s) = connect(&url, target).await;
    assert!(status.contains("200"), "{status}");
    echoes(&mut s).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_proxy_needs_the_session_credential() {
    let target = echo().await;
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(None))));
    let url = eg.proxy_url("s1").unwrap();
    let (_, addr) = split(&url);
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 407"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_ending_a_session_refuses_its_proxy_port() {
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(None))));
    let url = eg.proxy_url("s1").unwrap();
    let (_, addr) = split(&url);
    let target = echo().await;
    let (status, _) = connect(&url, target).await;
    assert!(status.contains("200"), "{status}");

    let end = eg.clone().end_hook();
    end("s1");
    // The proxy task is aborted on the runtime, so its listener closes a little later.
    tokio::time::timeout(Duration::from_secs(5), async {
        while TcpStream::connect(addr.as_str()).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the ended session's proxy port still accepts connections");
}

fn record(id: &str, agent: AgentKind) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        project: "acme".into(),
        agent,
        cwd: "/tmp".into(),
        model: None,
        native_id: None,
        status: SessionStatus::Idle,
        title: "t".into(),
        created_at: 0,
        updated_at: 0,
        last_seq: 0,
        account_id: None,
        account_reason: None,
        pinned: false,
        archived: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_proxy_env_injected_for_all_supported_agents() {
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(None))));
    let hook = eg.clone().start_hook();
    let acp = AcpName::register("fr-x6-acp").expect("a valid ACP name");
    let agents = [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Antigravity, AgentKind::Acp(acp)];
    for (i, agent) in agents.into_iter().enumerate() {
        let env = hook(&record(&format!("agent{i}"), agent));
        let url = env.iter().find(|(k, _)| k == "HTTPS_PROXY").map(|(_, v)| v.clone());
        let url = url.unwrap_or_else(|| panic!("{agent:?} gets no HTTPS_PROXY"));
        assert!(url.starts_with("http://ember:"), "{url}");
        assert!(env.iter().any(|(k, v)| k == "NO_PROXY" && v.contains("127.0.0.1")));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_domain_target_resolves_on_node_not_server() {
    // A domain is sent to the node unresolved; the node resolves `localhost` and applies its
    // loopback deny list, so the 403 shows the node (not this server) did the lookup and dial.
    let echo_addr = echo().await;
    let denying = start_node(true).await;
    let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(Some(denying)))));
    let (status, _) = connect_to(&eg.proxy_url("s1").unwrap(), &format!("localhost:{}", echo_addr.port())).await;
    assert!(status.contains("403"), "{status}");
    // A name that resolves nowhere is a node-side failure too, never a server-side dial.
    let (status, _) = connect_to(&eg.proxy_url("s1").unwrap(), "ember-fr-x6.invalid:443").await;
    assert!(status.contains("502") || status.contains("403"), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_x6_ending_a_session_closes_its_open_tunnels() {
    for node in [None, Some(start_node(false).await)] {
        let target = echo().await;
        let eg = SessionEgress::new(route_to(Arc::new(Mutex::new(node))));
        let url = eg.proxy_url("s1").unwrap();
        let (status, mut tunnel) = connect(&url, target).await;
        assert!(status.contains("200"), "{status}");
        echoes(&mut tunnel).await;

        eg.clone().end_hook()("s1");
        // The open tunnel is torn down: reads see EOF or a reset, not a live echo.
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            let mut b = [0u8; 1];
            loop {
                match tunnel.read(&mut b).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "the ended session's tunnel stayed open");
    }
}
