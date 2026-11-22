//! A Claude Code session on another computer never edits the main server's disk (SPEC FR-X1,
//! FR-X2). Its file tools reach the node only through the project mount; when this server has
//! no mount (a build without `mount-nfs` / `mount-fuse`, `EMBER_MOUNT=off`, or an OS without a
//! mount client), the start is refused with a reason instead of running the agent locally.
//! Also checks that the release build turns the mount on for every server target.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ember_node::proto::{EnvInfo, Health};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::computers::{Computer, Computers, Connector, NodeApi, Registry};
use ember_server::events::AgentEvent;
use ember_server::session::{NewSession, SessionError, Sessions};
use ember_server::store::Store;
use tokio::sync::mpsc;

fn env_for(host: &str) -> EnvInfo {
    EnvInfo {
        os: "linux".into(),
        os_version: Some(format!("{host}-os 1.0")),
        kernel: Some("6.12".into()),
        arch: "aarch64".into(),
        hostname: host.into(),
        user: Some("me".into()),
        home: Some(format!("/home/{host}").into()),
        shell: Some("/bin/zsh".into()),
        roots: vec![format!("/home/{host}").into()],
        toolchains: BTreeMap::new(),
    }
}

/// A node that is always up.
struct UpNode(EnvInfo);

#[async_trait]
impl NodeApi for UpNode {
    async fn health(&self) -> anyhow::Result<Health> {
        Ok(Health { ok: true, version: "test".into() })
    }

    async fn env(&self) -> anyhow::Result<EnvInfo> {
        Ok(self.0.clone())
    }
}

fn connector() -> Connector {
    Arc::new(|c: Option<&Computer>| -> anyhow::Result<Arc<dyn NodeApi>> {
        Ok(Arc::new(UpNode(env_for(c.map(|c| c.name.as_str()).unwrap_or("server")))))
    })
}

/// Stands in for Claude Code: records every start (the process the server would spawn).
struct FakeClaude {
    starts: Arc<Mutex<Vec<StartRequest>>>,
}

#[async_trait]
impl AgentAdapter for FakeClaude {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    async fn detect(&self) -> Detected {
        ScriptedAdapter.detect().await
    }

    async fn start(&self, req: StartRequest, events: mpsc::Sender<AgentEvent>) -> anyhow::Result<Box<dyn AgentRun>> {
        self.starts.lock().unwrap().push(req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

#[tokio::test]
async fn fr_x1_without_a_mount_claude_code_on_another_computer_never_starts() {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(FakeClaude { starts: starts.clone() })];
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), adapters);
    // No `enable_mounts`: what `main` does when the build or `EMBER_MOUNT` has no mechanism.
    let computers = Computers::with_shim(Registry::new(store), connector(), Some("/opt/ember/bin/ember-exec".into()));
    computers.install(&sessions);
    assert!(computers.mounts().is_none());

    // The project path exists on this server too, so a local start would succeed and its file
    // tools would edit this server's copy: exactly what must not happen.
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let id = sessions
        .create(NewSession {
            project: "acme".into(),
            agent: AgentKind::ClaudeCode,
            cwd: cwd.clone(),
            model: None,
            title: "t".into(),
        })
        .unwrap()
        .id;
    let pi = computers.register("pi", "http://pi:8741", "tok").unwrap();
    computers.registry().set_session_computer(&id, &pi.id, Some(&env_for("pi")), None).unwrap();

    let err = sessions.send(&id, "edit main.rs").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(matches!(err, SessionError::Other(_)), "{err:?}");
    assert!(msg.contains("\"pi\""), "the reason names the computer: {msg}");
    assert!(msg.contains("project mount"), "the reason names what is missing: {msg}");
    assert!(starts.lock().unwrap().is_empty(), "no agent process was started on this server's {}", cwd.display());
    assert!(!sessions.is_live(&id).await);

    // A second try is refused the same way (nothing was cached as started).
    assert!(sessions.send(&id, "again").await.is_err());
    assert!(starts.lock().unwrap().is_empty());

    // Back on the main server, the same session starts: the refusal is only for other computers.
    computers.registry().set_session_computer(&id, ember_server::computers::LOCAL, None, None).unwrap();
    sessions.send(&id, "edit main.rs here").await.unwrap();
    assert_eq!(starts.lock().unwrap().len(), 1);
    assert_eq!(starts.lock().unwrap()[0].cwd, cwd);
}

fn repo_file(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The released ember-server has a project mount on every target it ships for: NFS on macOS,
/// FUSE on Linux. A new target (Windows) fails here until someone decides its mount.
#[test]
fn fr_x1_release_build_ships_a_project_mount() {
    let wf = repo_file(".github/workflows/publish-release.yml");
    let build = wf
        .split("\n  build:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  client:\n").next())
        .expect("publish-release.yml has a build job before the client job");
    let targets: Vec<&str> = build.lines().map(str::trim).filter(|l| l.starts_with("- { os:")).collect();
    assert!(!targets.is_empty(), "no build matrix found");
    for t in &targets {
        let want = if t.contains("os: macos") {
            "server_features: mount-nfs"
        } else if t.contains("os: ubuntu") {
            "server_features: mount-fuse"
        } else {
            panic!("release target without a decided project mount: {t}")
        };
        assert!(t.contains(want), "{t} must build ember-server with `{want}`");
    }
    assert!(build.contains("SERVER_FEATURES: ${{ matrix.server_features }}"), "the matrix value reaches the build");
    assert!(
        build.contains("cargo build --release --locked --features \"$SERVER_FEATURES\""),
        "ember-server is built with its mount feature"
    );
    let cargo = repo_file("ember/server/Cargo.toml");
    for f in ["\nmount-nfs = [", "\nmount-fuse = ["] {
        assert!(cargo.contains(f), "ember/server/Cargo.toml defines {f:?}");
    }
}
