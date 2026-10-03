//! Computers and switching a session between them (SPEC FR-X1–FR-X3, FR-S7 v0).
//!
//! A *computer* is an ember node (`node/`) this server can reach, with a bearer token, either by
//! HTTP URL or by transport peer ([`ember_transport::PeerAddr`], SPEC `FR-N1`): a peer-addressed
//! node is dialed through the server's transport ([`crate::transport`]) for `ember-node/1`, and
//! the node admits the server only if the server's peer id is on its allow-list (`FR-N3`). The
//! server itself is the implicit computer [`LOCAL`]. Each session has a *current computer*; a
//! session that was never switched has none recorded and behaves exactly as before (local, no
//! environment block).
//!
//! How an agent's tools reach the current computer (`docs/design/INTERCEPTION.md`):
//!
//! - **Codex**: the start request carries a [`RemoteExec`]; the Codex adapter registers it with
//!   `environment/add`. The exec-server URL is a loopback relay in this process ([`relay`]) that
//!   forwards to the node's `/v1/exec-server`, which runs `codex exec-server --listen stdio`
//!   there. Shell, PTY and file changes then run on the node.
//! - **Claude Code**: `CLAUDE_CODE_SHELL_PREFIX` points at the `ember-exec` shim ([`shim`]), so
//!   the Bash tool runs on the node via `/v1/exec`. The shim is a separate process that speaks
//!   HTTP, so for a peer-addressed node it is given a loopback bridge in this process
//!   ([`bridge`]) that carries each TCP connection over one transport stream. Read, Edit, Write,
//!   Glob and Grep reach the node through the project mount ([`mount`]): the session's cwd is
//!   mounted here at the same path before the agent starts (when a mount mechanism is enabled —
//!   `EMBER_MOUNT`; otherwise they stay local).
//!
//! Switching (FR-X3) records the new computer, describes it with its `/v1/env`, and releases the
//! session's agent process; the next message starts (natively resumes) it with the new computer.
//! The description is contributed through the session's instructions hooks
//! ([`Sessions::add_instructions_hook`]), which are evaluated at every start, so a switch
//! **replaces** it (FR-S7) rather than appending to it. A session that already ran also gets a one-time notice in front of
//! its next message: the computer changed and earlier file observations must be re-read
//! (FR-S7 v0).
//!
//! A computer can also be a project browser's network egress (FR-R1): [`Computers`] runs one
//! loopback SOCKS5 listener per such computer ([`egress`]) that forwards to the node's
//! `/v1/egress`, and resolves `Egress::Computer` for the browser manager
//! ([`crate::browser::EgressResolver`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ember_node::client::NodeClient;
use ember_node::proto::{EnvInfo, Health, PROTOCOL_VERSION};
use ember_transport::{Dialer, PeerAddr};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::agents::{AgentKind, RemoteExec, StartRequest};
use crate::events::SessionStatus;
use crate::session::{InstructionsHook, MessageHook, PrepareHook, Sessions, StartConfigHook};
use crate::store::{now_ms, SessionRecord, Store};

pub mod api;
pub mod bridge;
pub mod egress;
pub mod mount;
pub mod relay;
pub mod schema;
pub mod shim;

/// Id of the implicit computer that is this server.
pub const LOCAL: &str = "local";
/// Display name of [`LOCAL`].
pub const LOCAL_NAME: &str = "this server";

/// How long a reachability probe (`/v1/health`) may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `/v1/env` may take (it probes toolchain versions).
pub const ENV_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------------
// Records

/// A registered ember node.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Computer {
    pub id: String,
    pub name: String,
    /// `http://host:port` of the node API; empty when the node is addressed by [`Computer::peer`].
    pub url: String,
    /// The node's transport address, for a node reached over the transport (`FR-N1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerAddr>,
    /// Bearer token for the node API. Never serialised to clients.
    #[serde(skip_serializing)]
    pub token: String,
    pub created_at: i64,
}

/// What clients see for a computer, including [`LOCAL`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ComputerView {
    pub id: String,
    pub name: String,
    /// Empty for [`LOCAL`] and for peer-addressed computers.
    pub url: String,
    /// The node's peer id (hex), for a computer reached over the transport.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    pub local: bool,
}

impl ComputerView {
    pub fn local() -> Self {
        ComputerView { id: LOCAL.into(), name: LOCAL_NAME.into(), url: String::new(), peer: None, local: true }
    }

    fn of(c: &Computer) -> Self {
        ComputerView {
            id: c.id.clone(),
            name: c.name.clone(),
            url: c.url.clone(),
            peer: c.peer.as_ref().map(|p| p.peer.to_string()),
            local: false,
        }
    }
}

/// A computer plus what a probe found.
#[derive(Debug, Clone, Serialize)]
pub struct ComputerStatus {
    #[serde(flatten)]
    pub computer: ComputerView,
    /// `None` when not probed.
    pub reachable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvInfo>,
}

/// A session's recorded current computer.
#[derive(Debug, Clone)]
pub struct SessionComputer {
    pub computer_id: String,
    /// The computer's `/v1/env` at the time of the switch.
    pub env: Option<EnvInfo>,
    /// Notice still to be delivered with the next message (FR-S7 v0).
    pub notice: Option<String>,
    pub switched_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ComputerError {
    #[error("computer {0} not found")]
    NotFound(String),
    #[error("session {0} not found")]
    SessionNotFound(String),
    #[error("session {0} is mid-turn; switch computers when the turn is over")]
    Busy(String),
    #[error("computer {0} is unreachable: {1}")]
    Unreachable(String, String),
    #[error("computer {0} is the current computer of {1} session(s)")]
    InUse(String, usize),
    #[error("computer {0} is the browser egress of project(s) {1:?}; change their egress first")]
    EgressInUse(String, Vec<String>),
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for ComputerError {
    fn from(e: rusqlite::Error) -> Self {
        ComputerError::Other(e.into())
    }
}

// ---------------------------------------------------------------------------------------------
// Registry (SQLite)

/// Computers and each session's current computer, in the main [`Store`]'s database (tables
/// from store migration 3, [`schema::MIGRATION`]).
pub struct Registry {
    store: Arc<Store>,
}

impl Registry {
    /// The registry over the server's store (shares its connection, like `accounts`).
    pub fn new(store: Arc<Store>) -> Registry {
        Registry { store }
    }

    /// A registry over its own in-memory store (tests).
    pub fn open_in_memory() -> anyhow::Result<Registry> {
        Ok(Registry { store: Arc::new(Store::open_in_memory()?) })
    }

    pub fn insert(&self, name: &str, url: &str, token: &str) -> Result<Computer, ComputerError> {
        self.insert_with(name, url, None, token)
    }

    /// A computer reached over the transport at `peer` (its `url` is empty).
    pub fn insert_peer(&self, name: &str, peer: &PeerAddr, token: &str) -> Result<Computer, ComputerError> {
        self.insert_with(name, "", Some(peer.clone()), token)
    }

    fn insert_with(
        &self,
        name: &str,
        url: &str,
        peer: Option<PeerAddr>,
        token: &str,
    ) -> Result<Computer, ComputerError> {
        let c = Computer {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            url: url.to_string(),
            peer,
            token: token.to_string(),
            created_at: now_ms(),
        };
        let peer_json = c.peer.as_ref().map(serde_json::to_string).transpose().map_err(anyhow::Error::from)?;
        let res = self.store.conn().execute(
            "INSERT INTO computers (id, name, url, token, created_at, peer_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![c.id, c.name, c.url, c.token, c.created_at, peer_json],
        );
        match res {
            Ok(_) => Ok(c),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ComputerError::BadRequest(format!("a computer named {name:?} already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<Computer>, ComputerError> {
        let conn = self.store.conn();
        Ok(conn
            .query_row(
                "SELECT id, name, url, token, created_at, peer_json FROM computers WHERE id = ?1",
                params![id],
                row_to_computer,
            )
            .optional()?)
    }

    pub fn list(&self) -> Result<Vec<Computer>, ComputerError> {
        let conn = self.store.conn();
        let mut stmt =
            conn.prepare("SELECT id, name, url, token, created_at, peer_json FROM computers ORDER BY name")?;
        let rows = stmt.query_map([], row_to_computer)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Remove a computer no session is on. Returns false if it did not exist.
    pub fn remove(&self, id: &str) -> Result<bool, ComputerError> {
        let conn = self.store.conn();
        let users: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_computer WHERE computer_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if users > 0 {
            return Err(ComputerError::InUse(id.to_string(), users as usize));
        }
        drop(conn);
        // Removing it would make those browsers fail to start (they never fall back to direct).
        let projects = crate::browser::egress::projects_using_computer(&self.store, id)?;
        if !projects.is_empty() {
            return Err(ComputerError::EgressInUse(id.to_string(), projects));
        }
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        // Its project assignments go with it (FR-L4); the API pushes the affected projects.
        tx.execute("DELETE FROM project_computers WHERE computer_id = ?1", params![id])?;
        let removed = tx.execute("DELETE FROM computers WHERE id = ?1", params![id])? > 0;
        tx.commit()?;
        Ok(removed)
    }

    pub fn session_computer(&self, session_id: &str) -> Result<Option<SessionComputer>, ComputerError> {
        let conn = self.store.conn();
        let row = conn
            .query_row(
                "SELECT computer_id, env_json, notice, switched_at FROM session_computer
                 WHERE session_id = ?1",
                params![session_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(|(computer_id, env_json, notice, switched_at)| SessionComputer {
            computer_id,
            env: env_json.and_then(|j| serde_json::from_str(&j).ok()),
            notice,
            switched_at,
        }))
    }

    /// Record `computer_id` as the session's current computer, replacing any earlier record
    /// (and any undelivered notice).
    pub fn set_session_computer(
        &self,
        session_id: &str,
        computer_id: &str,
        env: Option<&EnvInfo>,
        notice: Option<&str>,
    ) -> Result<(), ComputerError> {
        let env_json = env.map(serde_json::to_string).transpose().map_err(anyhow::Error::from)?;
        self.store.conn().execute(
            "INSERT INTO session_computer (session_id, computer_id, env_json, notice, switched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET computer_id = ?2, env_json = ?3, notice = ?4,
                                                   switched_at = ?5",
            params![session_id, computer_id, env_json, notice, now_ms()],
        )?;
        Ok(())
    }

    /// Take the pending notice, if any: it is returned once.
    pub fn take_notice(&self, session_id: &str) -> Result<Option<String>, ComputerError> {
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let notice: Option<String> = tx
            .query_row(
                "SELECT notice FROM session_computer WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if notice.is_some() {
            tx.execute(
                "UPDATE session_computer SET notice = NULL WHERE session_id = ?1",
                params![session_id],
            )?;
        }
        tx.commit()?;
        Ok(notice)
    }
}

fn row_to_computer(r: &rusqlite::Row<'_>) -> rusqlite::Result<Computer> {
    let peer_json: Option<String> = r.get(5)?;
    let peer = match peer_json {
        Some(j) => Some(serde_json::from_str::<PeerAddr>(&j).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?),
        None => None,
    };
    Ok(Computer {
        id: r.get(0)?,
        name: r.get(1)?,
        url: r.get(2)?,
        peer,
        token: r.get(3)?,
        created_at: r.get(4)?,
    })
}

// ---------------------------------------------------------------------------------------------
// Talking to a computer

/// The part of the node API the registry needs; implemented by [`NodeClient`] and by fakes.
#[async_trait]
pub trait NodeApi: Send + Sync {
    async fn health(&self) -> anyhow::Result<Health>;
    async fn env(&self) -> anyhow::Result<EnvInfo>;
}

#[async_trait]
impl NodeApi for NodeClient {
    async fn health(&self) -> anyhow::Result<Health> {
        Ok(NodeClient::health(self).await?)
    }

    async fn env(&self) -> anyhow::Result<EnvInfo> {
        Ok(NodeClient::env(self).await?)
    }
}

/// This server as a computer: always reachable, described the way a node describes itself.
pub struct LocalNode {
    pub roots: Vec<PathBuf>,
}

#[async_trait]
impl NodeApi for LocalNode {
    async fn health(&self) -> anyhow::Result<Health> {
        Ok(Health { ok: true, version: env!("CARGO_PKG_VERSION").into(), protocol: PROTOCOL_VERSION })
    }

    async fn env(&self) -> anyhow::Result<EnvInfo> {
        Ok(ember_node::envinfo::describe(self.roots.clone()).await)
    }
}

/// Opens an API handle for a computer; `None` is [`LOCAL`].
pub type Connector =
    Arc<dyn Fn(Option<&Computer>) -> anyhow::Result<Arc<dyn NodeApi>> + Send + Sync>;

/// A [`NodeClient`] for `c`: over HTTP for a URL, over the transport (through `dialer`) for a
/// peer. A peer-addressed computer without a transport is an error.
pub fn node_client(c: &Computer, dialer: Option<&Dialer>) -> anyhow::Result<NodeClient> {
    match &c.peer {
        Some(addr) => {
            let dialer = dialer.ok_or_else(|| {
                anyhow::anyhow!(
                    "computer {} is reached over the transport, but this server has none (set {}=1)",
                    c.name,
                    crate::transport::ENABLE_ENV
                )
            })?;
            Ok(NodeClient::over_transport(dialer.clone(), addr.clone(), &c.token))
        }
        None => Ok(NodeClient::new(&c.url, &c.token)?),
    }
}

/// The real connector without a transport: [`NodeClient`] over HTTP for nodes, [`LocalNode`]
/// (roots = `$HOME`) for this server. Peer-addressed computers are unreachable.
pub fn default_connector() -> Connector {
    connector(None)
}

/// The real connector: [`node_client`] for nodes (HTTP, or the transport through `dialer`),
/// [`LocalNode`] (roots = `$HOME`) for this server.
pub fn connector(dialer: Option<Dialer>) -> Connector {
    Arc::new(move |c: Option<&Computer>| -> anyhow::Result<Arc<dyn NodeApi>> {
        match c {
            Some(c) => Ok(Arc::new(node_client(c, dialer.as_ref())?)),
            None => Ok(Arc::new(LocalNode {
                roots: std::env::var_os("HOME").map(PathBuf::from).into_iter().collect(),
            })),
        }
    })
}

// ---------------------------------------------------------------------------------------------
// What the agent is told

/// The replaceable environment block for a computer (FR-X3). Given to the agent as
/// system-level instructions at every start; a switch replaces it.
pub fn env_description(name: &str, env: &EnvInfo) -> String {
    let mut out = String::new();
    out.push_str("# Current computer\n");
    out.push_str(&format!(
        "You are working on the computer \"{name}\" (hostname {}). Every file path, the working \
         directory, environment variables and every command refer to this computer. This \
         description replaces any earlier description of a computer.\n",
        env.hostname
    ));
    let os = env.os_version.clone().unwrap_or_else(|| env.os.clone());
    match &env.kernel {
        Some(k) => out.push_str(&format!("- OS: {os}, {} (kernel {k})\n", env.arch)),
        None => out.push_str(&format!("- OS: {os}, {}\n", env.arch)),
    }
    let mut who = Vec::new();
    if let Some(u) = &env.user {
        who.push(format!("user {u}"));
    }
    if let Some(h) = &env.home {
        who.push(format!("home {}", h.display()));
    }
    if let Some(s) = &env.shell {
        who.push(format!("login shell {s}"));
    }
    if !who.is_empty() {
        out.push_str(&format!("- {}\n", who.join(", ")));
    }
    if !env.roots.is_empty() {
        let roots: Vec<String> = env.roots.iter().map(|r| r.display().to_string()).collect();
        out.push_str(&format!("- Accessible roots: {}\n", roots.join(", ")));
    }
    if !env.toolchains.is_empty() {
        let tools: Vec<String> = env
            .toolchains
            .iter()
            .map(|(name, t)| match &t.version {
                Some(v) => format!("{name} ({v})"),
                None => name.clone(),
            })
            .collect();
        out.push_str(&format!("- Toolchains: {}\n", tools.join(", ")));
    }
    out
}

/// The one-time notice after a switch (FR-S7 v0).
pub fn switch_notice(from: &str, to: &str) -> String {
    format!(
        "[Ember system notice] This session's computer changed from \"{from}\" to \"{to}\". \
         File contents, directory listings, processes and command output you observed earlier \
         came from \"{from}\" and may differ here: re-read any file before editing it, and re-run \
         commands whose results you rely on. Use the current computer description, not the \
         earlier one."
    )
}

// ---------------------------------------------------------------------------------------------
// The service

/// Result of a switch.
#[derive(Debug, Clone, Serialize)]
pub struct SwitchOutcome {
    pub session_id: String,
    pub previous: ComputerView,
    pub computer: ComputerView,
    /// False when the session was already on that computer (nothing happened).
    pub changed: bool,
    /// The notice queued for the next message, if the session had already run.
    pub notice: Option<String>,
    /// The new environment block.
    pub env_description: String,
}

/// A session's current computer, for clients.
#[derive(Debug, Clone, Serialize)]
pub struct CurrentComputer {
    pub session_id: String,
    pub computer: ComputerView,
    /// True when nothing is recorded (the session never switched; it runs locally).
    pub implicit: bool,
    pub switched_at: Option<i64>,
    pub env_description: Option<String>,
    pub notice_pending: bool,
}

pub struct Computers {
    registry: Registry,
    connector: Connector,
    /// Absolute path of the `ember-exec` shim for Claude Code sessions.
    shim: Option<PathBuf>,
    /// Codex exec-server relays, by computer id.
    relays: Mutex<HashMap<String, relay::ExecServerRelay>>,
    /// Project mounts for Claude Code's file tools, when enabled ([`Computers::enable_mounts`]).
    mounts: std::sync::OnceLock<(Arc<mount::ProjectMounts>, mount::CtlSocket)>,
    /// The server's transport dialer, for peer-addressed computers (`None`: no transport).
    dialer: Option<Dialer>,
    /// Loopback HTTP bridges to peer-addressed nodes (for the `ember-exec` shim), by computer id.
    bridges: Mutex<HashMap<String, bridge::NodeBridge>>,
    /// Browser egress listeners (loopback SOCKS5 → node `/v1/egress`), by computer id.
    egress: Mutex<HashMap<String, egress::EgressListener>>,
}

impl Computers {
    pub fn new(registry: Registry, connector: Connector) -> Arc<Computers> {
        Self::with_shim(registry, connector, None)
    }

    pub fn with_shim(registry: Registry, connector: Connector, shim: Option<PathBuf>) -> Arc<Computers> {
        Self::with_transport(registry, connector, shim, None)
    }

    /// With the server's transport dialer, so computers registered by peer work: the relay and
    /// the shim bridge dial through it. `connector` should be [`connector`]`(dialer)` (or a fake).
    pub fn with_transport(
        registry: Registry,
        connector: Connector,
        shim: Option<PathBuf>,
        dialer: Option<Dialer>,
    ) -> Arc<Computers> {
        Arc::new(Computers {
            registry,
            connector,
            shim,
            relays: Mutex::new(HashMap::new()),
            mounts: std::sync::OnceLock::new(),
            dialer,
            bridges: Mutex::new(HashMap::new()),
            egress: Mutex::new(HashMap::new()),
        })
    }

    /// Mount Claude Code sessions' project directories from their computers (before
    /// [`Computers::install`]). Starts the shim's control socket.
    pub fn enable_mounts(&self, mounts: Arc<mount::ProjectMounts>) -> anyhow::Result<()> {
        let ctl = mount::CtlSocket::start(Arc::downgrade(&mounts))?;
        self.mounts.set((mounts, ctl)).map_err(|_| anyhow::anyhow!("project mounts already enabled"))
    }

    pub fn mounts(&self) -> Option<&Arc<mount::ProjectMounts>> {
        self.mounts.get().map(|(m, _)| m)
    }

    /// Unmount every project (server shutdown).
    pub async fn shutdown(&self) {
        if let Some(m) = self.mounts() {
            m.shutdown().await;
        }
    }

    /// Mount (or release) the session's project directory for its next agent start: a Claude
    /// Code session on a node gets its cwd mounted here; on this server it needs none.
    pub async fn prepare_start(&self, rec: &SessionRecord) -> anyhow::Result<()> {
        let Some(mounts) = self.mounts() else { return Ok(()) };
        if rec.agent != AgentKind::ClaudeCode {
            return Ok(());
        }
        let computer = match self.registry.session_computer(&rec.id)? {
            Some(sc) => self.resolve(&sc.computer_id)?,
            None => None,
        };
        match computer {
            Some(c) => Ok(mounts.acquire(&rec.id, &c, std::path::Path::new(&rec.cwd)).await?),
            None => {
                mounts.release(&rec.id).await;
                Ok(())
            }
        }
    }

    /// The transport dialer, if this server has a transport.
    pub fn dialer(&self) -> Option<&Dialer> {
        self.dialer.as_ref()
    }

    /// A [`NodeClient`] for a registered computer (HTTP or transport).
    pub fn client(&self, id: &str) -> Result<NodeClient, ComputerError> {
        let c = self.resolve(id)?.ok_or_else(|| {
            ComputerError::BadRequest("the local computer has no node client".into())
        })?;
        Ok(node_client(&c, self.dialer.as_ref())?)
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Register this service's hooks on `sessions`: the environment block as an instructions
    /// hook, the remote executor / shell shim as a start-config hook, and the FR-S7 notice as a
    /// message hook.
    pub fn install(self: &Arc<Self>, sessions: &Arc<Sessions>) {
        sessions.add_instructions_hook(self.instructions_hook());
        sessions.add_prepare_hook(self.prepare_hook());
        sessions.add_start_config_hook(self.start_hook());
        sessions.add_message_hook(self.message_hook());
        if let Some(mounts) = self.mounts() {
            // Unmount projects whose sessions' agents are gone (idle release, exit, deletion).
            let (mounts, sessions) = (Arc::downgrade(mounts), Arc::downgrade(sessions));
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tick.tick().await;
                    let (Some(m), Some(s)) = (mounts.upgrade(), sessions.upgrade()) else { break };
                    m.sweep(|id| {
                        let s = s.clone();
                        async move { s.is_live(&id).await }
                    })
                    .await;
                }
            });
        }
    }

    pub fn register(&self, name: &str, url: &str, token: &str) -> Result<Computer, ComputerError> {
        let name = name.trim();
        if name.is_empty() || name == LOCAL || name == LOCAL_NAME {
            return Err(ComputerError::BadRequest(format!("invalid computer name {name:?}")));
        }
        if token.is_empty() {
            return Err(ComputerError::BadRequest("token must not be empty".into()));
        }
        // Validates the URL form the client accepts.
        NodeClient::new(url, token).map_err(|e| ComputerError::BadRequest(e.to_string()))?;
        self.registry.insert(name, url.trim_end_matches('/'), token)
    }

    /// Register a node reached over the transport (`FR-N1`) by its peer address. The node must
    /// list this server's peer id among its allowed peers (`FR-N3`).
    pub fn register_peer(&self, name: &str, peer: &PeerAddr, token: &str) -> Result<Computer, ComputerError> {
        let name = name.trim();
        if name.is_empty() || name == LOCAL || name == LOCAL_NAME {
            return Err(ComputerError::BadRequest(format!("invalid computer name {name:?}")));
        }
        if token.is_empty() {
            return Err(ComputerError::BadRequest("token must not be empty".into()));
        }
        if self.dialer.is_none() {
            return Err(ComputerError::BadRequest(format!(
                "this server has no transport; set {}=1 to register computers by peer",
                crate::transport::ENABLE_ENV
            )));
        }
        self.registry.insert_peer(name, peer, token)
    }

    pub fn remove(&self, id: &str) -> Result<(), ComputerError> {
        if id == LOCAL {
            return Err(ComputerError::BadRequest("the local computer cannot be removed".into()));
        }
        if !self.registry.remove(id)? {
            return Err(ComputerError::NotFound(id.to_string()));
        }
        self.relays.lock().unwrap().remove(id);
        self.bridges.lock().unwrap().remove(id);
        self.egress.lock().unwrap().remove(id);
        Ok(())
    }

    /// The proxy URL through which a browser egresses from computer `id` (FR-R1): `None` for
    /// [`LOCAL`] (direct), else `socks5://127.0.0.1:<port>` of this computer's loopback listener,
    /// started on first use and kept until the computer is removed or the server stops.
    pub fn egress_proxy(&self, id: &str) -> Result<Option<String>, ComputerError> {
        let Some(c) = self.resolve(id)? else {
            return Ok(None);
        };
        let mut listeners = self.egress.lock().unwrap();
        if let Some(l) = listeners.get(&c.id) {
            return Ok(Some(l.proxy_url()));
        }
        let node = NodeClient::new(&c.url, &c.token).map_err(|e| ComputerError::BadRequest(e.to_string()))?;
        let l = egress::EgressListener::start(node)?;
        let url = l.proxy_url();
        tracing::info!(computer = %c.name, "browser egress listener on {url}");
        listeners.insert(c.id.clone(), l);
        Ok(Some(url))
    }

    /// Resolve an id ([`LOCAL`] or a registered computer).
    fn resolve(&self, id: &str) -> Result<Option<Computer>, ComputerError> {
        if id == LOCAL {
            return Ok(None);
        }
        match self.registry.get(id)? {
            Some(c) => Ok(Some(c)),
            None => Err(ComputerError::NotFound(id.to_string())),
        }
    }

    fn view(&self, id: &str) -> ComputerView {
        match self.registry.get(id) {
            Ok(Some(c)) => ComputerView::of(&c),
            _ if id == LOCAL => ComputerView::local(),
            // A record pointing at a removed computer cannot happen (remove refuses); show the id.
            _ => ComputerView { id: id.into(), name: id.into(), url: String::new(), peer: None, local: false },
        }
    }

    /// Probe one computer: `/v1/health`, and `/v1/env` when `with_env`.
    async fn probe(&self, c: Option<&Computer>, with_env: bool) -> ComputerStatus {
        let view = c.map(ComputerView::of).unwrap_or_else(ComputerView::local);
        let api = match (self.connector)(c) {
            Ok(api) => api,
            Err(e) => return unreachable_status(view, e.to_string()),
        };
        let health = match tokio::time::timeout(PROBE_TIMEOUT, api.health()).await {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return unreachable_status(view, format!("{e:#}")),
            Err(_) => return unreachable_status(view, "health check timed out".into()),
        };
        if !health.ok || health.protocol != PROTOCOL_VERSION {
            return unreachable_status(
                view,
                format!("node protocol {} (this server speaks {PROTOCOL_VERSION})", health.protocol),
            );
        }
        let mut status = ComputerStatus {
            computer: view,
            reachable: Some(true),
            error: None,
            version: Some(health.version),
            env: None,
        };
        if with_env {
            match tokio::time::timeout(ENV_TIMEOUT, api.env()).await {
                Ok(Ok(env)) => status.env = Some(env),
                Ok(Err(e)) => status.error = Some(format!("env: {e:#}")),
                Err(_) => status.error = Some("env timed out".into()),
            }
        }
        status
    }

    /// Every computer, [`LOCAL`] first; reachability probed concurrently when `probe`.
    pub async fn list(&self, probe: bool) -> Result<Vec<ComputerStatus>, ComputerError> {
        let computers = self.registry.list()?;
        if !probe {
            let mut out = vec![not_probed(ComputerView::local())];
            out.extend(computers.iter().map(|c| not_probed(ComputerView::of(c))));
            return Ok(out);
        }
        let local = self.probe(None, false);
        let nodes = futures::future::join_all(computers.iter().map(|c| self.probe(Some(c), false)));
        let (local, nodes) = futures::join!(local, nodes);
        let mut out = vec![local];
        out.extend(nodes);
        Ok(out)
    }

    /// One computer with its health and environment.
    pub async fn status(&self, id: &str) -> Result<ComputerStatus, ComputerError> {
        let c = self.resolve(id)?;
        Ok(self.probe(c.as_ref(), true).await)
    }

    pub fn current(&self, sessions: &Sessions, session_id: &str) -> Result<CurrentComputer, ComputerError> {
        let rec = sessions
            .store()
            .session(session_id)?
            .ok_or_else(|| ComputerError::SessionNotFound(session_id.to_string()))?;
        Ok(match self.registry.session_computer(&rec.id)? {
            None => CurrentComputer {
                session_id: rec.id,
                computer: ComputerView::local(),
                implicit: true,
                switched_at: None,
                env_description: None,
                notice_pending: false,
            },
            Some(sc) => {
                let view = self.view(&sc.computer_id);
                CurrentComputer {
                    session_id: rec.id,
                    env_description: sc.env.as_ref().map(|e| env_description(&view.name, e)),
                    computer: view,
                    implicit: false,
                    switched_at: Some(sc.switched_at),
                    notice_pending: sc.notice.is_some(),
                }
            }
        })
    }

    /// Make `computer_id` the session's current computer (FR-X3).
    ///
    /// Refuses while the session is mid-turn, or when the computer is unreachable. On success the
    /// session's agent process is released so the next message starts it on the new computer
    /// with the new environment block; a session that already ran gets the FR-S7 notice.
    pub async fn switch(
        &self,
        sessions: &Arc<Sessions>,
        session_id: &str,
        computer_id: &str,
    ) -> Result<SwitchOutcome, ComputerError> {
        let rec = sessions
            .store()
            .session(session_id)?
            .ok_or_else(|| ComputerError::SessionNotFound(session_id.to_string()))?;
        if matches!(rec.status, SessionStatus::Running | SessionStatus::WaitingForApproval) {
            return Err(ComputerError::Busy(session_id.to_string()));
        }
        let target = self.resolve(computer_id)?;
        let target_view = target.as_ref().map(ComputerView::of).unwrap_or_else(ComputerView::local);
        let current = self.registry.session_computer(session_id)?;
        let previous_view = match &current {
            Some(sc) => self.view(&sc.computer_id),
            None => ComputerView::local(),
        };

        // Already there (and described): nothing to do.
        if let Some(sc) = &current {
            if let (true, Some(env)) = (sc.computer_id == target_view.id, &sc.env) {
                return Ok(SwitchOutcome {
                    session_id: session_id.to_string(),
                    env_description: env_description(&target_view.name, env),
                    previous: previous_view,
                    computer: target_view,
                    changed: false,
                    notice: None,
                });
            }
        }

        let status = self.probe(target.as_ref(), true).await;
        if status.reachable != Some(true) {
            return Err(ComputerError::Unreachable(
                target_view.name.clone(),
                status.error.unwrap_or_default(),
            ));
        }
        let env = status.env.ok_or_else(|| {
            ComputerError::Unreachable(
                target_view.name.clone(),
                status.error.clone().unwrap_or_else(|| "no environment".into()),
            )
        })?;
        let description = env_description(&target_view.name, &env);
        let moved = previous_view.id != target_view.id;
        // Only an agent that has already worked somewhere has observations to invalidate.
        let notice = (moved && rec.native_id.is_some())
            .then(|| switch_notice(&previous_view.name, &target_view.name));
        self.registry.set_session_computer(session_id, &target_view.id, Some(&env), notice.as_deref())?;
        // The next message starts the agent with the new computer (and natively resumes it).
        sessions.release(session_id).await.map_err(|e| ComputerError::Other(anyhow::anyhow!("{e:#}")))?;
        // Its agent is stopped, so its old project mount can go (the next start mounts anew).
        if let Some(m) = self.mounts() {
            m.release(session_id).await;
        }
        tracing::info!(session = %session_id, from = %previous_view.id, to = %target_view.id, "switched computer");
        Ok(SwitchOutcome {
            session_id: session_id.to_string(),
            previous: previous_view,
            computer: target_view,
            changed: true,
            notice,
            env_description: description,
        })
    }

    /// Environment for a Claude Code process whose Bash tool runs on `c` through `ember-exec`.
    fn claude_env(&self, c: &Computer, env: Option<&EnvInfo>) -> anyhow::Result<Vec<(String, String)>> {
        let shim = self.shim.as_ref().ok_or_else(|| {
            anyhow::anyhow!("the ember-exec shim was not found; set EMBER_EXEC_BIN to run Claude Code on {}", c.name)
        })?;
        anyhow::ensure!(shim.is_absolute(), "EMBER_EXEC_BIN must be an absolute path");
        // The shim speaks HTTP: a peer-addressed node is reached through a loopback bridge.
        let url = if c.peer.is_some() { self.bridge_url(c)? } else { c.url.clone() };
        let mut vars = vec![
            ("CLAUDE_CODE_SHELL_PREFIX".to_string(), shim.to_string_lossy().into_owned()),
            (shim::ENV_NODE_URL.to_string(), url),
            (shim::ENV_NODE_TOKEN.to_string(), c.token.clone()),
        ];
        if let Some(sh) = env.and_then(|e| e.shell.clone()) {
            vars.push((shim::ENV_REMOTE_SHELL.to_string(), sh));
        }
        Ok(vars)
    }

    /// Loopback exec-server URL for `c`, starting its relay on first use.
    fn relay_url(&self, c: &Computer) -> anyhow::Result<String> {
        let mut relays = self.relays.lock().unwrap();
        if let Some(r) = relays.get(&c.id) {
            return Ok(r.url().to_string());
        }
        let r = relay::ExecServerRelay::start(node_client(c, self.dialer.as_ref())?)?;
        let url = r.url().to_string();
        relays.insert(c.id.clone(), r);
        Ok(url)
    }

    /// Loopback HTTP URL that reaches peer-addressed `c` over the transport, starting its bridge
    /// on first use.
    fn bridge_url(&self, c: &Computer) -> anyhow::Result<String> {
        let mut bridges = self.bridges.lock().unwrap();
        if let Some(b) = bridges.get(&c.id) {
            return Ok(b.url().to_string());
        }
        let peer = c.peer.clone().ok_or_else(|| anyhow::anyhow!("computer {} has no peer address", c.name))?;
        let dialer = self.dialer.clone().ok_or_else(|| {
            anyhow::anyhow!("computer {} is reached over the transport, but this server has none", c.name)
        })?;
        let b = bridge::NodeBridge::start(dialer, peer)?;
        let url = b.url().to_string();
        bridges.insert(c.id.clone(), b);
        Ok(url)
    }

    /// The environment block for a session's current computer (FR-X3), or `None` when the
    /// session never switched or the computer was not described. Evaluated at every start, so a
    /// switch replaces the block.
    pub fn instructions(&self, rec: &SessionRecord) -> anyhow::Result<Option<String>> {
        let Some(sc) = self.registry.session_computer(&rec.id)? else {
            return Ok(None);
        };
        let Some(env) = sc.env.as_ref() else {
            return Ok(None);
        };
        let computer = self.resolve(&sc.computer_id)?;
        let name = computer.as_ref().map(|c| c.name.as_str()).unwrap_or(LOCAL_NAME);
        Ok(Some(env_description(name, env)))
    }

    /// Route the agent's tools to a session's current computer: the Codex remote executor or the
    /// Claude Code shell shim. The environment block is [`Computers::instructions`].
    pub fn configure_start(&self, rec: &SessionRecord, req: &mut StartRequest) -> anyhow::Result<()> {
        let Some(sc) = self.registry.session_computer(&rec.id)? else {
            return Ok(());
        };
        let computer = self.resolve(&sc.computer_id)?;
        req.env.push(("EMBER_COMPUTER_ID".into(), sc.computer_id.clone()));
        let Some(c) = computer else {
            return Ok(());
        };
        match rec.agent {
            AgentKind::ClaudeCode => {
                req.env.extend(self.claude_env(&c, sc.env.as_ref())?);
                if let Some((_, ctl)) = self.mounts.get() {
                    req.env.push((mount::ENV_CTL.into(), ctl.path.to_string_lossy().into_owned()));
                }
            }
            AgentKind::Codex => {
                req.remote = Some(RemoteExec {
                    environment_id: format!("ember-{}", c.id),
                    exec_server_url: self.relay_url(&c)?,
                });
            }
            AgentKind::Scripted => {}
        }
        Ok(())
    }

    fn instructions_hook(self: &Arc<Self>) -> InstructionsHook {
        let this = self.clone();
        Arc::new(move |rec: &SessionRecord| match this.instructions(rec) {
            Ok(text) => text,
            Err(e) => {
                // The start-config hook reads the same record and fails the start on error.
                tracing::warn!(session = %rec.id, "describing the session's computer failed: {e:#}");
                None
            }
        })
    }

    fn prepare_hook(self: &Arc<Self>) -> PrepareHook {
        let this = self.clone();
        Arc::new(move |rec: SessionRecord| {
            let this = this.clone();
            Box::pin(async move { this.prepare_start(&rec).await })
        })
    }

    fn start_hook(self: &Arc<Self>) -> StartConfigHook {
        let this = self.clone();
        Arc::new(move |rec: &SessionRecord, req: &mut StartRequest| this.configure_start(rec, req))
    }

    fn message_hook(self: &Arc<Self>) -> MessageHook {
        let this = self.clone();
        Arc::new(move |session_id: &str| {
            let switched = match this.registry.take_notice(session_id) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(session = %session_id, "reading computer notice failed: {e:#}");
                    None
                }
            };
            let outage = this.mounts().and_then(|m| m.outage_notice(session_id));
            match (switched, outage) {
                (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
                (a, b) => a.or(b),
            }
        })
    }
}

impl crate::browser::EgressResolver for Computers {
    fn proxy_for_computer(&self, computer_id: &str) -> anyhow::Result<Option<String>> {
        Ok(self.egress_proxy(computer_id)?)
    }
}

fn unreachable_status(computer: ComputerView, error: String) -> ComputerStatus {
    ComputerStatus { computer, reachable: Some(false), error: Some(error), version: None, env: None }
}

fn not_probed(computer: ComputerView) -> ComputerStatus {
    ComputerStatus { computer, reachable: None, error: None, version: None, env: None }
}

/// Where the `ember-exec` shim is: `EMBER_EXEC_BIN`, else next to the running executable.
pub fn find_shim() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EMBER_EXEC_BIN") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("ember-exec");
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub(crate) fn sample_env() -> EnvInfo {
        let mut toolchains = BTreeMap::new();
        toolchains.insert(
            "cargo".to_string(),
            ember_node::proto::Toolchain { path: "/usr/bin/cargo".into(), version: Some("cargo 1.90.0".into()) },
        );
        EnvInfo {
            os: "linux".into(),
            os_version: Some("Debian GNU/Linux 13".into()),
            kernel: Some("6.12.0".into()),
            arch: "aarch64".into(),
            hostname: "pi".into(),
            user: Some("pi".into()),
            home: Some("/home/pi".into()),
            shell: Some("/bin/bash".into()),
            roots: vec!["/home/pi".into()],
            toolchains,
        }
    }

    #[test]
    fn registry_round_trip_and_unique_names() {
        let r = Registry::open_in_memory().unwrap();
        let a = r.insert("pi", "http://10.0.0.2:8741", "t1").unwrap();
        assert_eq!(r.get(&a.id).unwrap().unwrap(), a);
        assert!(matches!(r.insert("pi", "http://x:1", "t"), Err(ComputerError::BadRequest(_))));
        let b = r.insert("mac", "http://10.0.0.3:8741", "t2").unwrap();
        let names: Vec<_> = r.list().unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["mac", "pi"]);
        // The token never reaches clients.
        let json = serde_json::to_value(&a).unwrap();
        assert!(json.get("token").is_none());
        assert!(r.remove(&b.id).unwrap());
        assert!(!r.remove(&b.id).unwrap());
    }

    #[test]
    fn peer_addressed_computer_round_trips() {
        let r = Registry::open_in_memory().unwrap();
        let peer = ember_transport::SecretKey::generate().peer_id();
        let addr = PeerAddr {
            peer,
            relays: vec!["https://relay.example".into()],
            direct: vec!["192.0.2.1:7777".parse().unwrap()],
        };
        let c = r.insert_peer("gpu", &addr, "t").unwrap();
        let back = r.get(&c.id).unwrap().unwrap();
        assert_eq!(back, c);
        assert_eq!(back.peer.as_ref(), Some(&addr));
        assert_eq!(back.url, "");
        let view = serde_json::to_value(ComputerView::of(&back)).unwrap();
        assert_eq!(view["peer"], peer.to_string());
        // Without a transport, a peer computer cannot be reached.
        assert!(node_client(&back, None).is_err());
    }

    #[test]
    fn session_computer_replaces_and_notice_is_taken_once() {
        let r = Registry::open_in_memory().unwrap();
        let c = r.insert("pi", "http://10.0.0.2:8741", "t").unwrap();
        assert!(r.session_computer("s1").unwrap().is_none());
        r.set_session_computer("s1", &c.id, Some(&sample_env()), Some("moved")).unwrap();
        let sc = r.session_computer("s1").unwrap().unwrap();
        assert_eq!(sc.computer_id, c.id);
        assert_eq!(sc.env.unwrap().hostname, "pi");
        assert!(matches!(r.remove(&c.id), Err(ComputerError::InUse(_, 1))));
        assert_eq!(r.take_notice("s1").unwrap().as_deref(), Some("moved"));
        assert_eq!(r.take_notice("s1").unwrap(), None);
        r.set_session_computer("s1", LOCAL, None, None).unwrap();
        let sc = r.session_computer("s1").unwrap().unwrap();
        assert_eq!(sc.computer_id, LOCAL);
        assert!(sc.env.is_none());
        assert!(r.remove(&c.id).unwrap());
    }

    #[test]
    fn env_description_names_the_computer_and_its_tools() {
        let d = env_description("pi", &sample_env());
        assert!(d.contains("\"pi\" (hostname pi)"), "{d}");
        assert!(d.contains("Debian GNU/Linux 13, aarch64 (kernel 6.12.0)"), "{d}");
        assert!(d.contains("user pi, home /home/pi, login shell /bin/bash"), "{d}");
        assert!(d.contains("cargo (cargo 1.90.0)"), "{d}");
        assert!(d.contains("replaces any earlier description"), "{d}");
    }

    fn record(id: &str, agent: AgentKind, cwd: &std::path::Path) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            project: "acme".into(),
            agent,
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
    async fn claude_sessions_on_a_node_get_their_cwd_mounted() {
        let f = mount::tests::fx(false);
        let proj = f.base.join("proj");
        let c = Computers::with_shim(Registry::open_in_memory().unwrap(), default_connector(), Some("/x/ember-exec".into()));
        c.enable_mounts(f.mounts.clone()).unwrap();
        let pi = c.register("pi", "http://pi:8741", "tok").unwrap();
        for s in ["claude", "codex"] {
            c.registry().set_session_computer(s, &pi.id, Some(&sample_env()), None).unwrap();
        }

        // Codex needs no mount (its tools run on the node through the exec-server).
        c.prepare_start(&record("codex", AgentKind::Codex, &proj)).await.unwrap();
        assert!(f.mounted.lock().unwrap().is_empty());

        let claude = record("claude", AgentKind::ClaudeCode, &proj);
        c.prepare_start(&claude).await.unwrap();
        assert_eq!(*f.mounted.lock().unwrap(), std::slice::from_ref(&proj));
        let mut req = StartRequest { cwd: proj.clone(), ..Default::default() };
        c.configure_start(&claude, &mut req).unwrap();
        let ctl = req.env.iter().find(|(k, _)| k == mount::ENV_CTL).map(|(_, v)| v.clone()).unwrap();
        assert!(ctl.ends_with("ctl.sock"), "{ctl}");

        // Back on this server: the mount is released.
        c.registry().set_session_computer("claude", LOCAL, None, None).unwrap();
        c.prepare_start(&claude).await.unwrap();
        assert!(f.mounted.lock().unwrap().is_empty());
        assert_eq!(f.unmounts.load(std::sync::atomic::Ordering::SeqCst), 1);

        // A missing project on the node fails the start with a clear error.
        c.registry().set_session_computer("claude", &pi.id, Some(&sample_env()), None).unwrap();
        let e = c.prepare_start(&record("claude", AgentKind::ClaudeCode, &f.base.join("gone"))).await.unwrap_err();
        assert!(e.to_string().contains("does not exist on computer"), "{e:#}");
        c.shutdown().await;
    }

    #[test]
    fn notice_names_both_computers_and_asks_to_reread() {
        let n = switch_notice("mac", "pi");
        assert!(n.contains("from \"mac\" to \"pi\""));
        assert!(n.contains("re-read any file before editing it"));
    }
}
