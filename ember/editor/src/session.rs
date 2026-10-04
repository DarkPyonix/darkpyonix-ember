//! `EditorSession`: one workspace on one computer's OSE server, driven for one editor window.
//!
//! [`EditorSession::connect`] does what `crates/editor-conn/tests/live_ose.rs` proved works against a
//! real OSE server (steps 1-4 there):
//!
//! 1. `GET /version` → must be `ember_editor_conn::PINNED_COMMIT` (else
//!    `UnsupportedServerVersion`: fall back to "Open in VS Code").
//! 2. Management connection → IPC → `getEnvironmentData`, `scanExtensions`, `remoteFilesystem`
//!    `fileChange` subscription.
//! 3. Extension-host connection → Ready / init data / Initialized → `$initializeConfiguration`
//!    (defaults from every scanned extension's `contributes.configuration` plus Ember's editor
//!    defaults, then user settings) and `$initializeWorkspace` (one folder).
//!
//! After that one task owns everything: it feeds UI commands, extension-host requests, RPC
//! replies, file results, watcher events and timer ticks into the [`SessionCore`] and executes the
//! effects it returns, in order. The UI thread only ever does non-blocking channel sends
//! (dioxus-compose PR-3: no domain work on the UI thread) and reads [`SessionUpdate`]s.
//!
//! Not yet: reconnecting (a lost connection ends the session with
//! [`SessionUpdate::ConnectionLost`]; `ember_editor_conn::handshake::reconnect_loop` is the
//! building block), several workspace folders, configuration changes at runtime.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ember_editor_conn::exthost::{self, InitDataParams, WorkspaceData, WorkspaceFolder};
use ember_editor_conn::handshake::{self, ConnectOptions, ConnectionType, ExtensionHostStartParams};
use ember_editor_conn::ipc::{IpcClient, IpcLifecycle};
use ember_editor_conn::management::{self, RemoteAgentConnectionContext};
use ember_editor_conn::remote_fs::{RemoteFs, WatchEvent, WatchOptions, WriteOptions};
use ember_editor_conn::rpc::{Reply, RpcEvent, RpcPeer};
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::PINNED_VERSION;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::coords::WidgetPos;
use crate::engine::{CoreOptions, Effect, Input, SessionCore, SessionUpdate, Token};
use crate::languages::LanguageRegistry;
use crate::widget::WidgetEvent;
use crate::Error;

/// How to reach the server and which workspace to open.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// `host:port`: the HTTP `Host` header for the upgrade requests.
    pub host: String,
    /// The remote authority the server's URI transformers key on. Every file URI is
    /// `vscode-remote://<remote_authority><path>`. Usually equal to `host`.
    pub remote_authority: String,
    /// The server's `--connection-token`, if any.
    pub connection_token: Option<String>,
    /// Absolute path of the workspace folder on the server.
    pub workspace_path: String,
    pub workspace_name: String,
    pub app_language: String,
    /// User settings, dotted keys (`"json.validate.enable"`).
    pub user_settings: Vec<(String, Value)>,
    pub core: CoreOptions,
}

impl SessionConfig {
    pub fn new(host: impl Into<String>, workspace_path: impl Into<String>) -> Self {
        let host = host.into();
        let workspace_path = workspace_path.into();
        let workspace_name = workspace_path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("workspace").to_owned();
        Self {
            remote_authority: host.clone(),
            host,
            connection_token: None,
            workspace_path,
            workspace_name,
            app_language: "en".into(),
            user_settings: Vec::new(),
            core: CoreOptions::default(),
        }
    }
}

enum Command {
    Input(Input),
    Resync(UriComponents),
    Shutdown,
}

/// A connected editor session. Cheap to share by reference; every method is a non-blocking send.
pub struct EditorSession {
    tx: mpsc::UnboundedSender<Command>,
    authority: String,
    task: Option<JoinHandle<()>>,
}

impl EditorSession {
    /// Connect over plain TCP (`host` is dialled as is).
    pub async fn connect_tcp(config: SessionConfig) -> Result<(Self, mpsc::UnboundedReceiver<SessionUpdate>), Error> {
        let addr = config.host.clone();
        Self::connect(config, move || {
            let addr = addr.clone();
            async move {
                let s = tokio::net::TcpStream::connect(addr).await?;
                let _ = s.set_nodelay(true);
                Ok::<_, std::io::Error>(s)
            }
        })
        .await
    }

    /// Connect with `dial` opening a fresh byte stream to the server each time it is called (three
    /// times: `GET /version`, management, extension host). TCP, TLS or an ember-transport stream.
    pub async fn connect<D, F, S>(config: SessionConfig, dial: D) -> Result<(Self, mpsc::UnboundedReceiver<SessionUpdate>), Error>
    where
        D: Fn() -> F,
        F: Future<Output = std::io::Result<S>>,
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let authority = config.remote_authority.clone();
        let mut base = ConnectOptions::new(config.host.clone());
        base.connection_token = config.connection_token.clone();

        // 1. version gate
        let commit = handshake::verify_server(dial().await?, &base).await?;
        let opts_for = || {
            let mut o = base.clone();
            o.commit = Some(commit.clone());
            o.reconnection_token = uuid::Uuid::new_v4().to_string();
            o
        };

        // 2. management
        let (mconn, first) = handshake::connect(dial().await?, &opts_for(), ConnectionType::Management, None).await?;
        if first.get("type").and_then(Value::as_str) != Some("ok") {
            return Err(Error::Refused(first.to_string()));
        }
        let ctx = RemoteAgentConnectionContext {
            remote_authority: authority.clone(),
            client_id: format!("ember-{}", uuid::Uuid::new_v4().simple()),
        }
        .to_ipc()?;
        let (ipc, ipc_lifecycle) = IpcClient::start(mconn, ctx);
        let env = management::get_environment_data(&ipc, &authority, None).await?;
        let extensions = management::scan_extensions(&ipc, &config.app_language).await?;
        let fs = Arc::new(RemoteFs::new(ipc.clone()));
        let changes = fs.subscribe_changes().await?;

        // 3. extension host
        let ws_uri = UriComponents::remote(&authority, &config.workspace_path);
        let workspace = WorkspaceData {
            id: format!("ember-{}", uuid::Uuid::new_v4().simple()),
            name: config.workspace_name.clone(),
            folders: vec![WorkspaceFolder { uri: ws_uri, name: config.workspace_name.clone(), index: 0 }],
        };
        let start_params =
            serde_json::to_value(ExtensionHostStartParams { language: config.app_language.clone(), ..Default::default() })?;
        let (econn, _first) =
            handshake::connect(dial().await?, &opts_for(), ConnectionType::ExtensionHost, Some(start_params)).await?;
        let init = InitDataParams {
            version: PINNED_VERSION.into(),
            quality: Some("stable".into()),
            commit: Some(commit.clone()),
            remote_authority: authority.clone(),
            env,
            workspace: Some(workspace.clone()),
            extensions: extensions.clone(),
            app_language: config.app_language.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
            machine_id: uuid::Uuid::new_v4().simple().to_string(),
            log_level: 3,
        }
        .to_json();
        let (peer, rpc_events) = exthost::initialize(econn, &init).await?;

        // The driver answers extension-host requests from here on, including any made while the
        // two bootstrap calls below are outstanding.
        let (tx, rx) = mpsc::unbounded_channel();
        let (updates_tx, updates_rx) = mpsc::unbounded_channel();
        let core = SessionCore::new(LanguageRegistry::from_extensions(&extensions), config.core.clone(), Instant::now());
        let driver = Driver {
            core,
            peer: peer.clone(),
            fs,
            ipc,
            updates: updates_tx,
            reqs: HashMap::new(),
            watches: Arc::new(Mutex::new(HashMap::new())),
        };
        let task = tokio::spawn(driver.run(rx, rpc_events, changes, ipc_lifecycle));

        let mut defaults = exthost::contributed_configuration_defaults(&extensions);
        // Core editor settings the workbench registers itself (language-overridable = 6).
        defaults.push(("editor.tabSize".into(), json!(config.core.tab_width), Some(6)));
        defaults.push(("editor.insertSpaces".into(), json!(config.core.insert_spaces), Some(6)));
        defaults.push(("editor.inlineSuggest.enabled".into(), json!(config.core.inline_completions), Some(6)));
        defaults.push(("editor.codeLens".into(), json!(true), Some(6)));
        let configuration = exthost::configuration_init_data(&defaults, &config.user_settings);
        for call in exthost::bootstrap_calls(Some(&workspace), true, &configuration) {
            call.start(&peer)?.wait().await?;
        }

        Ok((Self { tx, authority, task: Some(task) }, updates_rx))
    }

    /// The remote authority URIs of this session use.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// `vscode-remote://<authority><path>` for a server path.
    pub fn uri(&self, path: &str) -> UriComponents {
        UriComponents::remote(&self.authority, path)
    }

    fn send(&self, c: Command) -> Result<(), Error> {
        self.tx.send(c).map_err(|_| Error::Closed)
    }

    /// Open a file; answered with [`SessionUpdate::Opened`] (or `OpenFailed`).
    pub fn open(&self, uri: UriComponents) -> Result<(), Error> {
        self.send(Command::Input(Input::Open { uri }))
    }

    pub fn close(&self, uri: UriComponents) -> Result<(), Error> {
        self.send(Command::Input(Input::Close { uri }))
    }

    /// The editor showing `uri` got focus.
    pub fn focus(&self, uri: UriComponents) -> Result<(), Error> {
        self.send(Command::Input(Input::Focus { uri }))
    }

    /// Forward a widget event from the `CodeEditor` node rendered with key `generation`.
    pub fn widget_event(&self, uri: UriComponents, generation: u64, event: WidgetEvent) -> Result<(), Error> {
        self.send(Command::Input(Input::Widget { uri, generation, event }))
    }

    /// Ask for inline completions at `pos` now (explicit trigger).
    pub fn trigger_inline_completion(&self, uri: UriComponents, generation: u64, pos: WidgetPos) -> Result<(), Error> {
        self.send(Command::Input(Input::TriggerInline { uri, generation, pos }))
    }

    /// After [`SessionUpdate::Desync`]: re-send the session's text as a new generation.
    pub fn resync(&self, uri: UriComponents) -> Result<(), Error> {
        self.send(Command::Resync(uri))
    }

    /// Terminate the extension host, close both connections, and wait for the driver to end.
    pub async fn shutdown(mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
    }
}

impl Drop for EditorSession {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

struct Driver {
    core: SessionCore,
    peer: RpcPeer,
    fs: Arc<RemoteFs>,
    ipc: IpcClient,
    updates: mpsc::UnboundedSender<SessionUpdate>,
    /// core token → RPC request id, for `Cancel`.
    reqs: HashMap<Token, u32>,
    /// uri key → watch request id.
    watches: Arc<Mutex<HashMap<String, String>>>,
}

async fn recv_opt<T>(rx: &mut Option<mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match rx.as_mut() {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

impl Driver {
    async fn run(
        mut self,
        mut cmds: mpsc::UnboundedReceiver<Command>,
        mut rpc_events: mpsc::UnboundedReceiver<RpcEvent>,
        changes: mpsc::UnboundedReceiver<WatchEvent>,
        ipc_lifecycle: mpsc::UnboundedReceiver<IpcLifecycle>,
    ) {
        let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Input>();
        let mut changes = Some(changes);
        let mut lifecycle = Some(ipc_lifecycle);
        loop {
            let wake = self.core.next_wake();
            let sleep = async move {
                match wake {
                    Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            let input: Option<Input> = tokio::select! {
                c = cmds.recv() => match c {
                    Some(Command::Input(i)) => Some(i),
                    Some(Command::Resync(uri)) => {
                        let effects = self.core.resync(Instant::now(), &uri);
                        self.execute(effects, &internal_tx);
                        None
                    }
                    Some(Command::Shutdown) | None => break,
                },
                ev = rpc_events.recv() => match ev {
                    Some(RpcEvent::Request(r)) => Some(Input::ExtHostRequest(r)),
                    Some(RpcEvent::Cancel(_)) => None,
                    Some(RpcEvent::Lost(reason)) => {
                        let _ = self.updates.send(SessionUpdate::ConnectionLost { reason: format!("extension host connection lost ({reason:?})") });
                        break;
                    }
                    Some(RpcEvent::Disconnected) | None => {
                        let _ = self.updates.send(SessionUpdate::ConnectionLost { reason: "extension host disconnected".into() });
                        break;
                    }
                },
                w = recv_opt(&mut changes) => match w {
                    Some(WatchEvent::Changes(list)) => Some(Input::FilesChanged(list)),
                    Some(WatchEvent::Error(e)) => {
                        tracing::warn!("ember-editor: file watcher error: {e}");
                        None
                    }
                    None => {
                        changes = None;
                        None
                    }
                },
                l = recv_opt(&mut lifecycle) => match l {
                    Some(l) => {
                        let _ = self.updates.send(SessionUpdate::ConnectionLost { reason: format!("management connection: {l:?}") });
                        break;
                    }
                    None => {
                        lifecycle = None;
                        None
                    }
                },
                i = internal_rx.recv() => i,
                _ = sleep => Some(Input::Tick),
            };
            let Some(input) = input else { continue };
            if let Input::RpcReply { token, .. } = &input {
                self.reqs.remove(token);
            }
            let effects = self.core.handle(Instant::now(), input);
            self.execute(effects, &internal_tx);
        }
        exthost::terminate(self.peer.connection());
        self.ipc.connection().close();
    }

    fn execute(&mut self, effects: Vec<Effect>, internal: &mpsc::UnboundedSender<Input>) {
        for e in effects {
            match e {
                Effect::Rpc { token, call } => {
                    let name = format!("{}.{}", call.proxy, call.method);
                    match call.start(&self.peer) {
                        Ok(pending) => match token {
                            Some(t) => {
                                self.reqs.insert(t, pending.req);
                                let tx = internal.clone();
                                tokio::spawn(async move {
                                    let result = pending.wait().await.map(Reply::into_json).map_err(|e| e.to_string());
                                    let _ = tx.send(Input::RpcReply { token: t, result });
                                });
                            }
                            None => {
                                tokio::spawn(async move {
                                    if let Err(e) = pending.wait().await {
                                        tracing::warn!("ember-editor: {name} failed: {e}");
                                    }
                                });
                            }
                        },
                        Err(err) => {
                            tracing::warn!("ember-editor: cannot send {name}: {err}");
                            if let Some(t) = token {
                                let _ = internal.send(Input::RpcReply { token: t, result: Err(err.to_string()) });
                            }
                        }
                    }
                }
                Effect::Cancel(t) => {
                    if let Some(req) = self.reqs.remove(&t) {
                        self.peer.cancel(req);
                    }
                }
                Effect::Respond { req, result } => self.peer.respond(req, result),
                Effect::ReadFile { uri, purpose } => {
                    let (fs, tx) = (Arc::clone(&self.fs), internal.clone());
                    tokio::spawn(async move {
                        let result = fs.read_file(&uri).await.map_err(|e| e.to_string());
                        let _ = tx.send(Input::FileRead { uri, purpose, result });
                    });
                }
                Effect::WriteFile { uri, bytes, token } => {
                    let (fs, tx) = (Arc::clone(&self.fs), internal.clone());
                    tokio::spawn(async move {
                        let result = fs.write_file(&uri, bytes, &WriteOptions::default()).await.map_err(|e| e.to_string());
                        let _ = tx.send(Input::FileWritten { uri, token, result });
                    });
                }
                Effect::Watch { uri } => {
                    let (fs, watches) = (Arc::clone(&self.fs), Arc::clone(&self.watches));
                    tokio::spawn(async move {
                        match fs.watch(&uri, &WatchOptions::default()).await {
                            Ok(req) => {
                                watches.lock().unwrap().insert(uri.key(), req);
                            }
                            Err(e) => tracing::warn!("ember-editor: cannot watch {}: {e}", uri.path),
                        }
                    });
                }
                Effect::Unwatch { uri } => {
                    let (fs, watches) = (Arc::clone(&self.fs), Arc::clone(&self.watches));
                    tokio::spawn(async move {
                        let req = watches.lock().unwrap().remove(&uri.key());
                        if let Some(req) = req {
                            let _ = fs.unwatch(&req).await;
                        }
                    });
                }
                Effect::Ui(u) => {
                    let _ = self.updates.send(u);
                }
            }
        }
    }
}
