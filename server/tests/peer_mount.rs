//! The project mount for a computer registered by peer (SPEC FR-N1, FR-N5, FR-X2), on the
//! in-memory fake transport: the mount's file API is opened the way `Computers` opens every
//! node client (over the transport through the server's dialer), file operations reach a real
//! ember node, and a peer that stops answering fails within the client's deadline instead of
//! hanging the mount.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ember_node::api::Node;
use ember_node::client::{ClientError, NODE_SERVICE};
use ember_node::config::NodeConfig;
use ember_node::proto::FileKind;
use ember_server::agents::AgentKind;
use ember_server::computers::mount::remote_fs::{CacheConfig, FsErr, RemoteFs, SetAttr, ROOT_ID};
use ember_server::computers::mount::{ActiveMount, MountSettings, Mounter, ProjectMounts};
use ember_server::computers::{self, Computer, Computers, Registry};
use ember_server::events::SessionStatus;
use ember_server::store::SessionRecord;
use ember_transport::mem::MemNetwork;
use ember_transport::{BiStream, Dialer, PeerGate, Transport};

const TOKEN: &str = "node-token";

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut).await.expect("timed out")
}

fn os(s: &str) -> &OsStr {
    OsStr::new(s)
}

struct Fx {
    computers: Arc<Computers>,
    /// A real ember node that admits the server; its root is `root`, with an empty `root/proj`.
    pi: Computer,
    /// A peer that accepts connections and streams for the node service but never answers.
    stuck: Computer,
    root: PathBuf,
    _node_t: Transport,
    _stuck_t: Transport,
    _held: Arc<Mutex<Vec<BiStream>>>,
    _net: MemNetwork,
    _dir: tempfile::TempDir,
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.root.join("proj")
    }
}

/// Server `Computers` with a transport dialer, a real node and a stalled peer, both registered
/// by peer address.
async fn start() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(root.join("proj")).unwrap();

    let net = MemNetwork::new();
    let (server_t, node_t, stuck_t) = (net.transport(), net.transport(), net.transport());

    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let gate = PeerGate::allow_list([server_t.peer_id()]);
    tokio::spawn(ember_node::transport::serve(&node_t, node, gate).unwrap());

    // Stalled peer: streams are accepted and kept open, nothing is ever read or written.
    let held: Arc<Mutex<Vec<BiStream>>> = Arc::default();
    let mut listener = stuck_t.listen(NODE_SERVICE).unwrap();
    let h = held.clone();
    tokio::spawn(async move {
        while let Some(conn) = listener.accept().await {
            let h = h.clone();
            tokio::spawn(async move {
                while let Ok(s) = conn.accept_bi().await {
                    h.lock().unwrap().push(s);
                }
            });
        }
    });

    let dialer = Dialer::new(server_t);
    let computers = Computers::with_transport(
        Registry::open_in_memory().unwrap(),
        computers::connector(Some(dialer.clone())),
        None,
        Some(dialer),
    );
    let pi = computers.register_peer("pi", &node_t.local_addr(), TOKEN).unwrap();
    assert!(pi.url.is_empty() && pi.peer.is_some());
    let stuck = computers.register_peer("stuck", &stuck_t.local_addr(), TOKEN).unwrap();

    // The node's listener is registered synchronously; retry briefly anyway.
    let client = computers.client(&pi.id).unwrap().with_deadline(Duration::from_secs(2));
    within(async {
        while client.health().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    Fx {
        computers,
        pi,
        stuck,
        root,
        _node_t: node_t,
        _stuck_t: stuck_t,
        _held: held,
        _net: net,
        _dir: dir,
    }
}

/// A mount filesystem over `c`, opened by the `Computers`' own connector.
fn remote_fs(f: &Fx, c: &Computer, deadline: Duration, op_timeout: Duration) -> RemoteFs {
    let node = (f.computers.node_fs())(c, deadline).unwrap();
    let cfg = CacheConfig { attr_ttl: Duration::ZERO, op_timeout, ..CacheConfig::default() };
    RemoteFs::new(node, f.proj(), cfg)
}

#[tokio::test]
async fn mount_filesystem_works_over_the_transport() {
    let f = start().await;
    let proj = f.proj();
    let fs = remote_fs(&f, &f.pi, Duration::from_secs(5), Duration::from_secs(10));

    assert_eq!(within(fs.getattr(ROOT_ID)).await.unwrap().kind, FileKind::Dir);

    // Create + write: the bytes land on the node's disk.
    let a = within(fs.create(ROOT_ID, os("a.txt"), SetAttr::default(), true)).await.unwrap();
    assert_eq!(a.kind, FileKind::File);
    within(fs.write(a.id, 0, b"hello peer")).await.unwrap();
    assert_eq!(std::fs::read(proj.join("a.txt")).unwrap(), b"hello peer");
    assert_eq!(
        within(fs.create(ROOT_ID, os("a.txt"), SetAttr::default(), true)).await.unwrap_err(),
        FsErr::Exist
    );

    // Read back through the mount, including a change made on the node behind its back.
    assert_eq!(within(fs.read(a.id, 0, 100)).await.unwrap(), (b"hello peer".to_vec(), true));
    std::fs::write(proj.join("a.txt"), b"changed on the node").unwrap();
    fs.invalidate_all();
    assert_eq!(within(fs.read(a.id, 0, 7)).await.unwrap().0, b"changed");

    // Rename into a new directory keeps the inode.
    let d = within(fs.mkdir(ROOT_ID, os("src"), None)).await.unwrap();
    within(fs.rename(ROOT_ID, os("a.txt"), d.id, os("b.txt"))).await.unwrap();
    assert!(!proj.join("a.txt").exists());
    assert_eq!(std::fs::read(proj.join("src/b.txt")).unwrap(), b"changed on the node");
    assert_eq!(fs.path_of(a.id).unwrap(), proj.join("src/b.txt"));
    let names: Vec<String> = within(fs.readdir(d.id))
        .await
        .unwrap()
        .into_iter()
        .map(|(n, _)| n.to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["b.txt".to_string()]);

    // Delete: a non-empty directory is refused with the node's errno, then emptied and removed.
    assert_eq!(within(fs.remove(ROOT_ID, os("src"))).await.unwrap_err(), FsErr::NotEmpty);
    within(fs.remove(d.id, os("b.txt"))).await.unwrap();
    within(fs.remove(ROOT_ID, os("src"))).await.unwrap();
    assert!(!proj.join("src").exists());
    assert_eq!(within(fs.lookup(ROOT_ID, os("src"))).await.unwrap_err(), FsErr::NoEnt);

    assert!(fs.health().is_none(), "no outage over a healthy transport");
}

#[tokio::test]
async fn a_stalled_peer_misses_the_deadline() {
    let f = start().await;

    // Without a deadline a request to the stalled peer just waits.
    let unbounded = f.computers.client(&f.stuck.id).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(300), unbounded.health()).await.is_err());

    // With one, it fails with a timeout, which counts as the node being unreachable.
    let bounded = unbounded.with_deadline(Duration::from_millis(200));
    let started = Instant::now();
    let e = within(bounded.health()).await.unwrap_err();
    assert!(matches!(e, ClientError::Timeout(_)), "{e:?}");
    assert!(e.is_timeout() && e.is_transport(), "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(5));

    // The mount's connector applies its deadline: the client gives up before the mount's own
    // (much longer) per-call deadline would, and the outage is recorded.
    let fs = remote_fs(&f, &f.stuck, Duration::from_millis(300), Duration::from_secs(30));
    let started = Instant::now();
    let e = within(fs.getattr(ROOT_ID)).await.unwrap_err();
    assert_eq!(e, FsErr::TimedOut);
    assert_eq!(e.errno(), libc::ETIMEDOUT);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert!(fs.health().is_some(), "the outage is recorded");

    // The real node is unaffected (the dialer caches one connection per peer).
    let ok = remote_fs(&f, &f.pi, Duration::from_millis(300), Duration::from_secs(30));
    within(ok.getattr(ROOT_ID)).await.unwrap();
}

#[tokio::test]
async fn a_peer_that_is_not_on_the_network_is_unreachable() {
    let f = start().await;
    let gone = f
        .computers
        .register_peer("gone", &ember_transport::SecretKey::generate().peer_id().into(), TOKEN)
        .unwrap();
    let fs = remote_fs(&f, &gone, Duration::from_secs(2), Duration::from_secs(10));
    let e = within(fs.getattr(ROOT_ID)).await.unwrap_err();
    assert!(matches!(e, FsErr::Unreachable(_)), "{e:?}");
    assert!(fs.health().is_some());
}

/// Records mounts instead of calling the kernel.
#[derive(Default)]
struct Recorder {
    mounted: Mutex<Vec<(PathBuf, Arc<RemoteFs>)>>,
}

struct Active;

#[async_trait]
impl ActiveMount for Active {
    async fn unmount(self: Box<Self>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Mounter for Recorder {
    fn name(&self) -> &'static str {
        "recorder"
    }

    async fn mount(&self, fs: Arc<RemoteFs>, at: &Path) -> anyhow::Result<Box<dyn ActiveMount>> {
        self.mounted.lock().unwrap().push((at.to_path_buf(), fs));
        Ok(Box::new(Active))
    }

    async fn force_unmount(&self, _at: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

fn record(id: &str, cwd: &Path) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        project: "acme".into(),
        agent: AgentKind::ClaudeCode,
        cwd: cwd.to_string_lossy().into_owned(),
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

#[tokio::test]
async fn claude_session_on_a_peer_computer_gets_its_project_mounted() {
    let f = start().await;
    let proj = f.proj();
    let rec = Arc::new(Recorder::default());
    let mut settings = MountSettings::from_vars(|_| None);
    settings.cache.attr_ttl = Duration::ZERO;
    settings.cache.op_timeout = Duration::from_millis(500);
    // The same connector `main` passes to `ProjectMounts::from_env`.
    f.computers.enable_mounts(ProjectMounts::new(rec.clone(), f.computers.node_fs(), settings)).unwrap();

    let env = within(f.computers.client(&f.pi.id).unwrap().env()).await.unwrap();
    f.computers.registry().set_session_computer("claude", &f.pi.id, Some(&env), None).unwrap();
    within(f.computers.prepare_start(&record("claude", &proj))).await.unwrap();

    let fs = {
        let mounted = rec.mounted.lock().unwrap();
        assert_eq!(mounted.len(), 1);
        assert_eq!(mounted[0].0, proj);
        mounted[0].1.clone()
    };
    let x = within(fs.create(ROOT_ID, os("x.rs"), SetAttr::default(), true)).await.unwrap();
    within(fs.write(x.id, 0, b"fn main() {}")).await.unwrap();
    assert_eq!(std::fs::read(proj.join("x.rs")).unwrap(), b"fn main() {}");

    // A session on the stalled peer fails its start promptly with "unreachable".
    f.computers.registry().set_session_computer("other", &f.stuck.id, None, None).unwrap();
    let other = f.root.join("elsewhere");
    let started = Instant::now();
    let e = within(f.computers.prepare_start(&record("other", &other))).await.unwrap_err();
    assert!(e.to_string().contains("unreachable"), "{e:#}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(rec.mounted.lock().unwrap().len(), 1);

    f.computers.shutdown().await;
}
