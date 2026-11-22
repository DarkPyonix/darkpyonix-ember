//! `EditorSession`: one workspace on one computer's OSE server, driven for one editor window.
//!
//! [`EditorSession::connect`] does what `ember/editor-conn/tests/live_ose.rs` proved works against a
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
//! A lost connection ends the session with [`SessionUpdate::ConnectionLost`]. The window reconnects
//! by starting a new session and putting its buffers back with [`EditorSession::restore`] (FR-L31.6);
//! `ember_editor_conn::handshake::reconnect_loop` (resuming the same sockets with the reconnection
//! token) is not used, because the session drops its connection state when the socket goes.
//! Not yet: several workspace folders, configuration changes at runtime.

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
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::coords::WidgetPos;
use crate::engine::{CoreOptions, Effect, Input, SessionCore, SessionUpdate, Token};
use crate::fallback::{ConnectOutcome, FallbackTarget};
use crate::highlight::{HighlightJob, Worker};
use crate::languages::LanguageRegistry;
use crate::settings::{SettingSchema, SettingsLayer, SettingsScope, SettingsScopes};
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
    /// The user scope of the settings (FR-L31.17): the client machine's `settings.json`, read by
    /// the caller. Dotted keys (`"json.validate.enable"`) and language blocks.
    pub user_settings: crate::settings::SettingsLayer,
    /// Files on the client machine the session may open as `vscode-userdata:` documents (the
    /// user's `settings.json` and `keybindings.json`). No other local file is ever read or
    /// written: an extension on the server can ask the session to open any URI.
    pub user_files: Vec<std::path::PathBuf>,
    /// Read the workspace's `.vscode/settings.json` and the server's Machine `settings.json`
    /// while connecting, so the extension host starts with every scope. Off by default: a caller
    /// that only needs documents does not pay for two more reads.
    pub server_settings: bool,
    pub core: CoreOptions,
    /// Optional VS Code Web target URL (or base URL) used when the server version is unsupported
    /// or unknown and the session falls back to the VS Code target.
    pub vscode_url: Option<String>,
    /// Whether the folder is trusted (FR-L31.18). `false` starts the extension host in restricted
    /// mode: extensions that do not support untrusted workspaces are not run until
    /// [`EditorSession::decide_workspace_trust`] grants trust. Defaults to `true`; the editor
    /// window sets it from its per-device memory.
    pub workspace_trusted: bool,
    /// Extension ids the user disabled (FR-L31.14). They are scanned and listed, but left out of
    /// what the extension host is given, as the Code-OSS workbench does. Compared without regard
    /// to case.
    pub disabled_extensions: Vec<String>,
    /// Optional local directory for `MainThreadStorage` persistence (`globalState` and
    /// `workspaceState`). When `None` (default), storage persists under the server's
    /// `globalStorageHome` and `workspaceStorageHome` via `RemoteFs`.
    pub storage_dir: Option<std::path::PathBuf>,
    /// Optional shared broadcast bus for propagating `shared = true` (`globalState`) updates
    /// across sessions on the same server/storage scope via `ExtHostStorage.$acceptValue`.
    pub storage_hub: Option<crate::storage::StorageHub>,
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
            user_settings: crate::settings::SettingsLayer::default(),
            user_files: Vec::new(),
            server_settings: false,
            core: CoreOptions::default(),
            vscode_url: None,
            workspace_trusted: true,
            disabled_extensions: Vec::new(),
            storage_dir: None,
            storage_hub: None,
        }
    }

    pub fn with_vscode_url(mut self, url: impl Into<String>) -> Self {
        self.vscode_url = Some(url.into());
        self
    }
}

enum Command {
    Input(Input),
    Resync(UriComponents),
    CheckDirty { prefix: String, reply: oneshot::Sender<bool> },
    Settings(SettingsScope, SettingsLayer),
    Shutdown,
}

/// A settings file on the server (FR-L31.17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSettingsFile {
    /// `<workspace>/.vscode/settings.json`.
    Workspace,
    /// `<server data dir>/data/Machine/settings.json` (the remote environment's `settingsPath`).
    Machine,
}

/// How long one settings file may take to arrive while connecting.
const SETTINGS_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One scope from a settings file on the server. A missing file, a file that is not UTF-8 and a
/// file that does not parse all read as an empty scope, so the window still opens; for the last two
/// the reason comes back too, so the settings view can show its banner (FR-L31.17, #790).
async fn read_server_layer(fs: &RemoteFs, uri: &UriComponents) -> (SettingsLayer, Option<String>) {
    let bytes = match tokio::time::timeout(SETTINGS_READ_TIMEOUT, fs.read_file(uri)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            tracing::debug!("ember-editor: no settings at {}: {e}", uri.path);
            return (SettingsLayer::default(), None);
        }
        Err(_) => {
            tracing::warn!("ember-editor: reading settings at {} timed out", uri.path);
            return (SettingsLayer::default(), None);
        }
    };
    let (layer, error) = server_layer_from_bytes(&bytes);
    if let Some(e) = &error {
        tracing::warn!("ember-editor: settings at {} do not parse: {e}", uri.path);
    }
    (layer, error)
}

/// The scope a server settings file's bytes hold, and why it is empty when they are not UTF-8 or do
/// not parse (`None` for a file that parses).
pub fn server_layer_from_bytes(bytes: &[u8]) -> (SettingsLayer, Option<String>) {
    let Ok((text, _bom)) = crate::document::decode(bytes) else {
        return (SettingsLayer::default(), Some("UTF-8 텍스트가 아닙니다".to_string()));
    };
    match crate::settings::parse_layer(&text) {
        Ok(layer) => (layer, None),
        Err(e) => (SettingsLayer::default(), Some(e.to_string())),
    }
}

/// Whether a file-channel error says the file is not there.
fn is_not_found(error: &ember_editor_conn::Error) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("entrynotfound") || text.contains("enoent") || text.contains("filenotfound") || text.contains("no such file")
}

/// Lists directories on the remote server: [`EditorSession::lister`].
#[derive(Clone)]
pub struct DirLister {
    fs: Arc<RemoteFs>,
    authority: String,
}

impl DirLister {
    /// `(name, FileType)` entries of the directory at `path`.
    pub async fn read_dir(&self, path: &str) -> Result<Vec<(String, u32)>, Error> {
        let uri = UriComponents::remote(&self.authority, path);
        self.fs.readdir(&uri).await.map_err(Error::Conn)
    }
}

/// A connected editor session. Cheap to share by reference; every method is a non-blocking send.
pub struct EditorSession {
    tx: mpsc::UnboundedSender<Command>,
    authority: String,
    workspace_path: String,
    fs: Arc<RemoteFs>,
    /// The management IPC, kept for reading the extension scan again (FR-L31.14).
    ipc: IpcClient,
    app_language: String,
    /// Every extension the connect-time scan found, disabled ones included.
    installed: Vec<crate::extensions::InstalledExtension>,
    /// The extensions the extension host was started with (the scan without the disabled ids).
    running: Vec<crate::extensions::InstalledExtension>,
    task: Option<JoinHandle<()>>,
    /// The settings the extension host was started with (FR-L31.17).
    settings: SettingsScopes,
    /// Every setting the settings view lists: Ember's own, then the extensions'.
    catalogue: Vec<SettingSchema>,
    /// The server's Machine `settings.json`, when the server names it.
    machine_settings_path: Option<String>,
    /// Why the server's settings files read as empty at connect, when they did not parse (#790).
    workspace_settings_error: Option<String>,
    machine_settings_error: Option<String>,
    user_files: Vec<std::path::PathBuf>,
}

fn parent_of_path(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
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

    /// Connect over plain TCP, returning [`ConnectOutcome::Fallback`] if the server's version is
    /// unsupported or unknown.
    pub async fn connect_tcp_or_fallback(config: SessionConfig) -> Result<ConnectOutcome, Error> {
        let addr = config.host.clone();
        Self::connect_or_fallback(config, move || {
            let addr = addr.clone();
            async move {
                let s = tokio::net::TcpStream::connect(addr).await?;
                let _ = s.set_nodelay(true);
                Ok::<_, std::io::Error>(s)
            }
        })
        .await
    }

    /// Connect with `dial`, checking the server version at connect time and falling back to the VS
    /// Code target ([`ConnectOutcome::Fallback`]) when the server commit differs from
    /// [`ember_editor_conn::PINNED_COMMIT`] or is unknown.
    pub async fn connect_or_fallback<D, F, S>(config: SessionConfig, dial: D) -> Result<ConnectOutcome, Error>
    where
        D: Fn() -> F,
        F: Future<Output = std::io::Result<S>>,
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let vscode_url = config.vscode_url.clone();
        let workspace_path = config.workspace_path.clone();
        match Self::connect(config, dial).await {
            Ok((session, updates)) => Ok(ConnectOutcome::Connected { session, updates }),
            Err(err) => {
                if let Some(target) = FallbackTarget::from_error(&err, vscode_url.as_deref(), &workspace_path) {
                    Ok(ConnectOutcome::Fallback(target))
                } else {
                    Err(err)
                }
            }
        }
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
        let workspace_path = config.workspace_path.clone();
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
        let storage_roots = match &config.storage_dir {
            Some(dir) => crate::storage::StorageRoots::from_local_dir(dir, &authority, &config.workspace_path),
            None => crate::storage::StorageRoots::from_env(
                &authority,
                &env.global_storage_home,
                &env.workspace_storage_home,
                &config.workspace_path,
            ),
        };
        let local_storage = config.storage_dir.is_some();
        let storage_hub = config.storage_hub.clone();
        let storage_session_id = storage_hub.as_ref().map_or(0, crate::storage::StorageHub::next_session_id);
        let storage_rx = storage_hub.as_ref().map(crate::storage::StorageHub::subscribe);
        // `IRemoteAgentEnvironment.settingsPath` (remoteAgentEnvironment.ts): the server's Machine settings.json.
        let machine_settings_path =
            env.raw.get("settingsPath").and_then(|u| u.get("path")).and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_owned);
        let scanned = management::scan_extensions(&ipc, &config.app_language).await?;
        let installed = crate::extensions::parse_installed(&scanned);
        // A disabled extension is listed but not run: the extension host, the commands, the
        // languages and the grammars below only see the enabled ones (FR-L31.14).
        let extensions = crate::extensions::enabled_only(&scanned, &config.disabled_extensions);
        drop(scanned);
        let running = crate::extensions::parse_installed(&extensions);
        let fs = Arc::new(RemoteFs::new(ipc.clone()));
        let changes = fs.subscribe_changes().await?;

        // 3. extension host
        let ws_uri = UriComponents::remote(&authority, &config.workspace_path);
        // FR-L31.24: watch the workspace folder recursively, as Code-OSS's workbench does, before
        // any file is opened. The OSE serves it with its recursive watcher, whose events cover
        // every file below the folder, so `Effect::Watch` asks for nothing more there. A request
        // for one file is served with `fs.watch()`, which the OSE refuses on macOS for every path
        // below `/Volumes/` (#789). A failure is not fatal: files are then watched one by one.
        let workspace_watch = match fs.watch(&ws_uri, &crate::watch::workspace_watch_options()).await {
            Ok(_) => Some(config.workspace_path.clone()),
            Err(e) => {
                tracing::warn!("ember-editor: cannot watch the workspace folder {}: {e}", ws_uri.path);
                None
            }
        };
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
        let mut init = init;
        ember_editor_conn::trust::restrict_init_data(&mut init, &extensions, config.workspace_trusted);
        let (peer, rpc_events) = exthost::initialize(econn, &init).await?;

        // The driver answers extension-host requests from here on, including any made while the
        // two bootstrap calls below are outstanding.
        let (tx, rx) = mpsc::unbounded_channel();
        let (updates_tx, updates_rx) = mpsc::unbounded_channel();

        // Settings (FR-L31.17): the defaults every extension and Ember register, the caller's user
        // scope, and the server's two files when asked for.
        let catalogue = crate::settings::setting_catalogue(&extensions);
        let mut default_values: Vec<(String, Value)> = Vec::new();
        let mut scope_table: Vec<(String, Option<u8>)> = Vec::new();
        for (key, default, scope) in exthost::contributed_configuration_defaults(&extensions) {
            default_values.push((key.clone(), default));
            scope_table.push((key, scope));
        }
        for schema in crate::settings::ember_settings() {
            // The start-up options stay the defaults of the three settings they cover.
            let default = match schema.key.as_str() {
                "editor.tabSize" => json!(config.core.tab_width),
                "editor.insertSpaces" => json!(config.core.insert_spaces),
                "editor.inlineSuggest.enabled" => json!(config.core.inline_completions),
                _ => schema.default.clone(),
            };
            default_values.push((schema.key.clone(), default));
            scope_table.push((schema.key, schema.scope));
        }
        let mut settings = SettingsScopes {
            defaults: SettingsLayer::from_values(default_values),
            user: config.user_settings.clone(),
            ..SettingsScopes::default()
        };
        let (mut workspace_settings_error, mut machine_settings_error) = (None, None);
        if config.server_settings {
            let workspace_file = format!("{}/{}", workspace_path.trim_end_matches('/'), crate::settings::WORKSPACE_SETTINGS_FILE);
            (settings.workspace, workspace_settings_error) = read_server_layer(&fs, &UriComponents::remote(&authority, &workspace_file)).await;
            if let Some(path) = &machine_settings_path {
                (settings.remote, machine_settings_error) = read_server_layer(&fs, &UriComponents::remote(&authority, path)).await;
            }
        }
        let commands = crate::commands::parse_contributed_commands(&extensions);
        let mut core = SessionCore::new(LanguageRegistry::from_extensions(&extensions), config.core.clone(), Instant::now());
        core.set_settings(settings.clone());
        core.set_grammar_catalog(crate::textmate::GrammarCatalog::from_extensions(&extensions));
        core.set_workspace_trust(crate::trust::WorkspaceTrust::new(config.workspace_trusted, extensions.clone()));
        let global_storage_dir = storage_roots.global_dir.clone();
        let local_storage_watch_dir = local_storage.then(|| std::path::PathBuf::from(&storage_roots.global_dir.path));
        core.set_storage_roots(storage_roots);
        core.register_storage_extensions(running.iter().map(|e| e.id.clone()));
        let driver = Driver {
            core,
            highlight: None,
            peer: peer.clone(),
            fs: Arc::clone(&fs),
            authority: authority.clone(),
            local_storage,
            global_storage_dir,
            shared_storage_watch: Arc::new(tokio::sync::OnceCell::new()),
            local_storage_watch_dir,
            storage_hub,
            storage_session_id,
            ipc: ipc.clone(),
            updates: updates_tx,
            reqs: HashMap::new(),
            watches: Arc::new(Mutex::new(HashMap::new())),
            workspace_watch,
            commands,
            settings: settings.clone(),
            scope_table: scope_table.clone(),
            user_files: config.user_files.iter().map(|p| crate::settings::user_data_uri(p).key()).collect(),
        };
        let task = tokio::spawn(driver.run(rx, rpc_events, changes, ipc_lifecycle, storage_rx));

        let configuration = settings.configuration_data(&scope_table);
        for call in exthost::bootstrap_calls(Some(&workspace), config.workspace_trusted, &configuration) {
            call.start(&peer)?.wait().await?;
        }

        let app_language = config.app_language.clone();
        Ok((Self { tx, authority, workspace_path, fs, ipc, app_language, installed, running, task: Some(task), settings, catalogue, machine_settings_path, workspace_settings_error, machine_settings_error, user_files: config.user_files.clone() }, updates_rx))
    }

    /// Every extension the connect-time scan found, disabled ones included (FR-L31.14).
    pub fn installed_extensions(&self) -> &[crate::extensions::InstalledExtension] {
        &self.installed
    }

    /// The extensions the extension host was started with: what is running until it restarts.
    pub fn running_extensions(&self) -> &[crate::extensions::InstalledExtension] {
        &self.running
    }

    /// Reads the scan again on the management connection (`remoteExtensionsScanner.scanExtensions`),
    /// for the list after an install or an uninstall. The extension host is not told: what it runs
    /// stays [`EditorSession::running_extensions`] until it starts again.
    pub async fn rescan_extensions(&self) -> Result<Vec<crate::extensions::InstalledExtension>, Error> {
        let scanned = management::scan_extensions(&self.ipc, &self.app_language).await.map_err(Error::Conn)?;
        Ok(crate::extensions::parse_installed(&scanned))
    }

    /// The settings the extension host was started with: the defaults, the user scope the caller
    /// gave, and the server's two scopes when [`SessionConfig::server_settings`] asked for them.
    pub fn initial_settings(&self) -> &SettingsScopes {
        &self.settings
    }

    /// Every setting the settings view lists.
    pub fn setting_catalogue(&self) -> &[SettingSchema] {
        &self.catalogue
    }

    /// Where a settings file of the server is. `None` when the server names no Machine settings file.
    pub fn server_settings_path(&self, file: ServerSettingsFile) -> Option<String> {
        match file {
            ServerSettingsFile::Workspace => {
                Some(format!("{}/{}", self.workspace_path.trim_end_matches('/'), crate::settings::WORKSPACE_SETTINGS_FILE))
            }
            ServerSettingsFile::Machine => self.machine_settings_path.clone(),
        }
    }

    /// Why a server settings file read at connect gave an empty scope (not UTF-8, does not parse);
    /// `None` when it parsed or is not there.
    pub fn server_settings_error(&self, file: ServerSettingsFile) -> Option<&str> {
        match file {
            ServerSettingsFile::Workspace => self.workspace_settings_error.as_deref(),
            ServerSettingsFile::Machine => self.machine_settings_error.as_deref(),
        }
    }

    /// The text of a settings file on the server; `None` when the file is not there.
    pub async fn read_server_settings(&self, file: ServerSettingsFile) -> Result<Option<String>, Error> {
        let Some(path) = self.server_settings_path(file) else { return Ok(None) };
        match self.fs.read_file(&self.uri(&path)).await {
            Ok(bytes) => {
                let (text, _bom) = crate::document::decode(&bytes)
                    .map_err(|_| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "file is not UTF-8")))?;
                Ok(Some(text))
            }
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(Error::Conn(e)),
        }
    }

    /// Writes the workspace's `.vscode/settings.json`, making the `.vscode` folder first.
    pub async fn write_workspace_settings(&self, text: &str) -> Result<(), Error> {
        let Some(path) = self.server_settings_path(ServerSettingsFile::Workspace) else { return Ok(()) };
        let folder = path.rsplit_once('/').map(|(dir, _)| dir.to_owned()).unwrap_or_default();
        // The folder is usually there already; a real failure shows in the write below.
        let _ = self.fs.mkdir(&self.uri(&folder)).await;
        self.write_file(&path, text.as_bytes()).await
    }

    /// Replace one scope of the settings (FR-L31.17). The session tells the extension host what
    /// changed (`$acceptConfigurationChanged`) and uses the new tab size and insert-spaces for
    /// documents opened afterwards. The default scope cannot be replaced.
    pub fn set_settings(&self, scope: SettingsScope, layer: SettingsLayer) -> Result<(), Error> {
        self.send(Command::Settings(scope, layer))
    }

    /// The URI the session opens `path` under: `vscode-userdata:` for one of the caller's own
    /// settings files on the client machine, `vscode-remote:` for everything else.
    pub fn document_uri(&self, path: &str) -> UriComponents {
        self.user_files
            .iter()
            .map(|file| crate::settings::user_data_uri(file))
            .find(|uri| uri.path == path)
            .unwrap_or_else(|| self.uri(path))
    }

    /// The remote authority URIs of this session use.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// `vscode-remote://<authority><path>` for a server path.
    pub fn uri(&self, path: &str) -> UriComponents {
        UriComponents::remote(&self.authority, path)
    }

    /// `vscode-remote://<authority><workspace_path>` for the root workspace folder.
    pub fn workspace_uri(&self) -> UriComponents {
        self.uri(&self.workspace_path)
    }

    /// A cheap handle that can list directories on its own, so a long walk runs in a task of its
    /// own and does not hold the session's command loop.
    pub fn lister(&self) -> DirLister {
        DirLister { fs: Arc::clone(&self.fs), authority: self.authority.clone() }
    }

    /// List entries `(name, FileType)` in a directory on the remote server.
    pub async fn read_dir(&self, uri: &UriComponents) -> Result<Vec<(String, u32)>, Error> {
        self.fs.readdir(uri).await.map_err(Error::Conn)
    }

    /// Stat a file or directory on the remote server.
    pub async fn stat(&self, uri: &UriComponents) -> Result<ember_editor_conn::remote_fs::Stat, Error> {
        self.fs.stat(uri).await.map_err(Error::Conn)
    }

    /// Refuses a path where any directory from the workspace root down to `dir` is a symbolic link.
    async fn refuse_symlink_ancestors(&self, dir: &str) -> Result<(), Error> {
        let ws = if self.workspace_path == "/" { "" } else { self.workspace_path.trim_end_matches('/') };
        let target_dir = dir.trim_end_matches('/');
        if target_dir == ws {
            return Ok(());
        }
        let rest = match target_dir.strip_prefix(ws) {
            Some(r) if r.starts_with('/') => &r[1..],
            _ => return Err(Error::Path(crate::paths::PathError::OutsideFolder)),
        };
        let mut current = ws.to_string();
        for part in rest.split('/') {
            if part.is_empty() {
                continue;
            }
            current.push('/');
            current.push_str(part);
            let stat = self.fs.stat(&self.uri(&current)).await.map_err(Error::Conn)?;
            if stat.file_type & ember_editor_conn::remote_fs::file_type::SYMBOLIC_LINK != 0 {
                return Err(Error::Path(crate::paths::PathError::Symlink));
            }
        }
        Ok(())
    }

    /// Creates an empty file named `name` in the directory `parent`. Never overwrites: an existing
    /// file is an error. Returns the new path.
    pub async fn create_file(&self, parent: &str, name: &str) -> Result<String, Error> {
        let path = crate::paths::child_path(&self.workspace_path, parent, name).map_err(Error::Path)?;
        self.refuse_symlink_ancestors(parent).await?;
        let opts = WriteOptions { create: true, overwrite: false, unlock: false, atomic: false };
        self.fs.write_file(&self.uri(&path), Vec::new(), &opts).await.map_err(Error::Conn)?;
        Ok(path)
    }

    /// Reads the UTF-8 text of `path` on the remote server.
    pub async fn read_file_text(&self, path: &str) -> Result<String, Error> {
        let path = crate::paths::target_path(&self.workspace_path, path).map_err(Error::Path)?;
        let bytes = self.fs.read_file(&self.uri(&path)).await.map_err(Error::Conn)?;
        let (text, _bom) = crate::document::decode(&bytes)
            .map_err(|_| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "file is not UTF-8")))?;
        Ok(text)
    }

    /// Writes content to `path`. Creates or overwrites the file.
    pub async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), Error> {
        let path = crate::paths::target_path(&self.workspace_path, path).map_err(Error::Path)?;
        let opts = WriteOptions { create: true, overwrite: true, unlock: false, atomic: false };
        self.fs.write_file(&self.uri(&path), content.to_vec(), &opts).await.map_err(Error::Conn)?;
        Ok(())
    }

    /// Creates a directory named `name` in the directory `parent`. Returns the new path.
    pub async fn create_dir(&self, parent: &str, name: &str) -> Result<String, Error> {
        let path = crate::paths::child_path(&self.workspace_path, parent, name).map_err(Error::Path)?;
        self.refuse_symlink_ancestors(parent).await?;
        self.fs.mkdir(&self.uri(&path)).await.map_err(Error::Conn)?;
        Ok(path)
    }

    /// Renames `path` to `new_name` in the same directory. Never overwrites. A symbolic link is
    /// renamed itself. Refuses rename if any open document at or below `path` is dirty. Returns the new path.
    pub async fn rename_path(&self, path: &str, new_name: &str) -> Result<String, Error> {
        let to = crate::paths::renamed_path(&self.workspace_path, path, new_name).map_err(Error::Path)?;
        let from = crate::paths::target_path(&self.workspace_path, path).map_err(Error::Path)?;
        let parent = parent_of_path(&from);
        self.refuse_symlink_ancestors(&parent).await?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::CheckDirty { prefix: from.clone(), reply: tx })?;
        if rx.await.map_err(|_| Error::Closed)? {
            return Err(Error::Path(crate::paths::PathError::Dirty));
        }
        self.fs.rename(&self.uri(&from), &self.uri(&to), false).await.map_err(Error::Conn)?;
        Ok(to)
    }

    /// Deletes one file or directory (a directory with everything in it). There is no trash: it
    /// cannot be undone. A symbolic link is removed itself and never recursed into.
    pub async fn delete_path(&self, path: &str, is_dir: bool) -> Result<(), Error> {
        let path = crate::paths::target_path(&self.workspace_path, path).map_err(Error::Path)?;
        let parent = parent_of_path(&path);
        self.refuse_symlink_ancestors(&parent).await?;
        let stat = self.fs.stat(&self.uri(&path)).await.map_err(Error::Conn)?;
        let is_link = stat.file_type & ember_editor_conn::remote_fs::file_type::SYMBOLIC_LINK != 0;
        let opts = ember_editor_conn::remote_fs::DeleteOptions { recursive: is_dir && !is_link, use_trash: false, atomic: false };
        self.fs.delete(&self.uri(&path), &opts).await.map_err(Error::Conn)
    }

    /// Moves a file or directory at `src` into `dest_dir`.
    pub async fn move_path(&self, src: &str, dest_dir: &str) -> Result<String, Error> {
        let from = crate::paths::target_path(&self.workspace_path, src).map_err(Error::Path)?;
        self.refuse_symlink_ancestors(&parent_of_path(&from)).await?;
        self.refuse_symlink_ancestors(dest_dir).await?;
        let to = crate::paths::paste_destination(&self.workspace_path, &from, dest_dir, &[], true).map_err(Error::Path)?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::CheckDirty { prefix: from.clone(), reply: tx })?;
        if rx.await.map_err(|_| Error::Closed)? {
            return Err(Error::Path(crate::paths::PathError::Dirty));
        }
        self.fs.rename(&self.uri(&from), &self.uri(&to), false).await.map_err(Error::Conn)?;
        Ok(to)
    }

    /// Helper that recursively copies an entry from `from` to `to`.
    async fn copy_entry(&self, from: &str, to: &str) -> Result<(), Error> {
        let mut stack = vec![(from.to_string(), to.to_string())];
        let opts = WriteOptions { create: true, overwrite: false, unlock: false, atomic: false };
        while let Some((src, dst)) = stack.pop() {
            let stat = self.fs.stat(&self.uri(&src)).await.map_err(Error::Conn)?;
            if stat.file_type & ember_editor_conn::remote_fs::file_type::SYMBOLIC_LINK != 0 {
                return Err(Error::Path(crate::paths::PathError::Symlink));
            }
            if stat.file_type & ember_editor_conn::remote_fs::file_type::DIRECTORY != 0 {
                self.fs.mkdir(&self.uri(&dst)).await.map_err(Error::Conn)?;
                let entries = self.fs.readdir(&self.uri(&src)).await.map_err(Error::Conn)?;
                for (name, _) in entries {
                    let child_src = format!("{}/{}", src.trim_end_matches('/'), name);
                    let child_dst = format!("{}/{}", dst.trim_end_matches('/'), name);
                    stack.push((child_src, child_dst));
                }
            } else {
                let bytes = self.fs.read_file(&self.uri(&src)).await.map_err(Error::Conn)?;
                self.fs.write_file(&self.uri(&dst), bytes, &opts).await.map_err(Error::Conn)?;
            }
        }
        Ok(())
    }

    /// Copies a file or directory at `src` into `dest_dir`.
    pub async fn copy_path(&self, src: &str, dest_dir: &str) -> Result<String, Error> {
        let from = crate::paths::target_path(&self.workspace_path, src).map_err(Error::Path)?;
        let dest_norm = crate::paths::folder_path(&self.workspace_path, dest_dir).map_err(Error::Path)?;
        self.refuse_symlink_ancestors(&parent_of_path(&from)).await?;
        self.refuse_symlink_ancestors(&dest_norm).await?;
        let entries = self.fs.readdir(&self.uri(&dest_norm)).await.map_err(Error::Conn)?;
        let existing: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        let to = crate::paths::paste_destination(&self.workspace_path, &from, &dest_norm, &existing, false).map_err(Error::Path)?;
        self.copy_entry(&from, &to).await?;
        Ok(to)
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

    /// Reload an open document from disk even if its buffer has unsaved edits (e.g. after discard).
    pub fn reload(&self, uri: UriComponents) -> Result<(), Error> {
        self.send(Command::Input(Input::Reload { uri }))
    }

    /// Show `text` as the buffer of `uri`, opening the file if needed (FR-L31.6): the window's buffer
    /// kept across a reconnect. Dirty unless it equals the file on disk.
    pub fn restore(&self, uri: UriComponents, text: String) -> Result<(), Error> {
        self.send(Command::Input(Input::Restore { uri, text }))
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

    /// Run a command by its id, dispatching to the extension host or workbench.
    pub fn execute_command(&self, id: impl Into<String>, arguments: Vec<Value>) -> Result<(), Error> {
        self.send(Command::Input(Input::ExecuteCommand { id: id.into(), arguments }))
    }

    /// Search the workspace folder (FR-E7): answered with [`SessionUpdate::SearchHits`] and one
    /// [`SessionUpdate::SearchDone`] for `id`. A running search is cancelled.
    pub fn search(&self, id: u64, request: crate::search::SearchRequest) -> Result<(), Error> {
        let folder = self.workspace_uri();
        self.send(Command::Input(Input::Search { id, request, folder }))
    }

    /// Stop search `id`; answered with [`SessionUpdate::SearchDone`] (`Cancelled`).
    pub fn cancel_search(&self, id: u64) -> Result<(), Error> {
        self.send(Command::Input(Input::CancelSearch { id }))
    }

    /// Answer extension message `id` (FR-L31.16): the chosen button's handle, or `None` when it
    /// was closed without one.
    pub fn answer_message(&self, id: u64, handle: Option<i64>) -> Result<(), Error> {
        self.send(Command::Input(Input::AnswerMessage { id, handle }))
    }

    /// Replace the chosen matches (FR-E7); answered with [`SessionUpdate::ReplaceDone`].
    pub fn replace(&self, id: u64, request: crate::search::ReplaceRequest) -> Result<(), Error> {
        self.send(Command::Input(Input::Replace { id, request }))
    }

    /// Change the line ending of an open document (FR-L31.7); answered with
    /// [`SessionUpdate::Format`] and [`SessionUpdate::Dirty`].
    pub fn set_eol(&self, uri: UriComponents, eol: crate::text_format::Eol) -> Result<(), Error> {
        self.send(Command::Input(Input::SetEol { uri, eol }))
    }

    /// Reopen or save an open document in `encoding` (FR-L31.7).
    pub fn set_encoding(
        &self,
        uri: UriComponents,
        encoding: crate::text_format::Encoding,
        mode: crate::text_format::EncodingMode,
    ) -> Result<(), Error> {
        self.send(Command::Input(Input::SetEncoding { uri, encoding, mode }))
    }

    /// Put an open document under `language_id`, or the one its path gives with `None` (FR-L31.7).
    pub fn set_language(&self, uri: UriComponents, language_id: Option<String>) -> Result<(), Error> {
        self.send(Command::Input(Input::SetLanguage { uri, language_id }))
    }

    /// The user's answer to the workspace trust prompt (FR-L31.18). `true` runs the extensions
    /// restricted mode held back and tells the host (`$onDidGrantWorkspaceTrust`); either answer
    /// settles the extensions' waiting `$requestWorkspaceTrust` calls.
    pub fn decide_workspace_trust(&self, trusted: bool) -> Result<(), Error> {
        self.send(Command::Input(Input::DecideWorkspaceTrust { trusted }))
    }

    /// Ask for a language feature (FR-L31.13); answered with [`SessionUpdate::Language`].
    pub fn language(&self, request: crate::language::LanguageRequest) -> Result<(), Error> {
        self.send(Command::Input(Input::Language(request)))
    }

    /// An event of an extension webview or its host (FR-E8), usually from `WebviewDriver::poll` or
    /// `WebviewDriver::set_view_state`; answered with [`SessionUpdate::Webview`].
    pub fn webview(&self, event: crate::webview::WebviewEvent) -> Result<(), Error> {
        self.send(Command::Input(Input::Webview(event)))
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
    /// Jobs for the highlighting thread (started in [`Driver::run`]).
    highlight: Option<std::sync::mpsc::SyncSender<HighlightJob>>,
    peer: RpcPeer,
    fs: Arc<RemoteFs>,
    authority: String,
    local_storage: bool,
    global_storage_dir: UriComponents,
    shared_storage_watch: Arc<tokio::sync::OnceCell<()>>,
    local_storage_watch_dir: Option<std::path::PathBuf>,
    storage_hub: Option<crate::storage::StorageHub>,
    storage_session_id: u64,
    ipc: IpcClient,
    updates: mpsc::UnboundedSender<SessionUpdate>,
    /// core token → RPC request id, for `Cancel`.
    reqs: HashMap<Token, u32>,
    /// uri key → watch request id.
    watches: Arc<Mutex<HashMap<String, String>>>,
    /// The workspace folder, when its recursive watch was accepted (FR-L31.24).
    workspace_watch: Option<String>,
    commands: Vec<crate::commands::ContributedCommand>,
    /// The settings the extension host has (FR-L31.17).
    settings: SettingsScopes,
    /// `configurationScopes`: the scope of every registered key.
    scope_table: Vec<(String, Option<u8>)>,
    /// The keys of the `vscode-userdata:` URIs the session may read and write on this machine.
    user_files: Vec<String>,
}

/// Reads one of the caller's settings files on the client machine. A file that is not there yet
/// reads as empty, so `settings.json` opens before anything was ever written to it.
async fn read_user_file(path: std::path::PathBuf) -> Result<Vec<u8>, String> {
    let read = tokio::task::spawn_blocking(move || match std::fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    });
    read.await.unwrap_or_else(|e| Err(e.to_string()))
}

/// Writes one of the caller's settings files on the client machine, making its folder first.
async fn write_user_file(path: std::path::PathBuf, bytes: Vec<u8>) -> Result<(), String> {
    let write = tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, bytes).map_err(|e| e.to_string())
    });
    write.await.unwrap_or_else(|e| Err(e.to_string()))
}

/// Whether a file-channel error says the directory or file already exists.
fn is_already_exists(error: &ember_editor_conn::Error) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("eexist") || text.contains("fileexists") || text.contains("already exists")
}

/// Create `dir_path` and any missing parent directories on the remote server via `RemoteFs::mkdir`.
async fn ensure_remote_dir(fs: &RemoteFs, authority: &str, dir_path: &str) -> Result<(), String> {
    let trimmed = dir_path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return Ok(());
    }
    let uri = UriComponents::remote(authority, trimmed);
    match fs.mkdir(&uri).await {
        Ok(()) => return Ok(()),
        Err(e) if is_already_exists(&e) => return Ok(()),
        Err(e) if !is_not_found(&e) => return Err(e.to_string()),
        Err(_) => {}
    }
    let mut missing = vec![trimmed.to_owned()];
    let mut cur = parent_of_path(trimmed);
    while !cur.is_empty() && cur != "/" {
        let cur_uri = UriComponents::remote(authority, &cur);
        match fs.mkdir(&cur_uri).await {
            Ok(()) => break,
            Err(e) if is_already_exists(&e) => break,
            Err(e) if is_not_found(&e) => {
                missing.push(cur.clone());
                cur = parent_of_path(&cur);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    for path in missing.into_iter().rev() {
        let u = UriComponents::remote(authority, &path);
        match fs.mkdir(&u).await {
            Ok(()) => {}
            Err(e) if is_already_exists(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

/// Lazily ensure the remote global `ember-storage` directory exists and is watched via `RemoteFs`,
/// once per session on the first `shared = true` storage access.
async fn ensure_shared_storage_watch(
    fs: &RemoteFs,
    authority: &str,
    global_dir: &UriComponents,
    once: &tokio::sync::OnceCell<()>,
) {
    let _ = once
        .get_or_init(|| async {
            if let Err(e) = ensure_remote_dir(fs, authority, &global_dir.path).await {
                tracing::warn!(
                    "ember-editor: cannot ensure global storage dir {}: {e}",
                    global_dir.path
                );
                return;
            }
            let watch_opts = WatchOptions {
                recursive: true,
                excludes: Vec::new(),
                includes: None,
                correlation_id: None,
            };
            if let Err(e) = fs.watch(global_dir, &watch_opts).await {
                tracing::warn!(
                    "ember-editor: cannot watch global storage dir {}: {e}",
                    global_dir.path
                );
            }
        })
        .await;
}

/// Atomically write (`Some`) or delete (`None`) one storage file on the remote server.
async fn write_remote_storage(
    fs: &RemoteFs,
    authority: &str,
    uri: &UriComponents,
    bytes: Option<Vec<u8>>,
) -> Result<(), String> {
    let Some(bytes) = bytes else {
        let opts = ember_editor_conn::remote_fs::DeleteOptions::default();
        return match fs.delete(uri, &opts).await {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    };
    let parent = parent_of_path(&uri.path);
    ensure_remote_dir(fs, authority, &parent).await?;
    let name = uri.path.rsplit('/').next().unwrap_or("storage.json");
    let tmp_path = if parent == "/" {
        format!("/.{name}.tmp.{}", uuid::Uuid::new_v4().simple())
    } else {
        format!("{parent}/.{name}.tmp.{}", uuid::Uuid::new_v4().simple())
    };
    let tmp_uri = UriComponents::remote(authority, &tmp_path);
    fs.write_file(&tmp_uri, bytes, &WriteOptions::default()).await.map_err(|e| e.to_string())?;
    if let Err(e) = fs.rename(&tmp_uri, uri, true).await {
        let _ = fs.delete(&tmp_uri, &ember_editor_conn::remote_fs::DeleteOptions::default()).await;
        return Err(e.to_string());
    }
    Ok(())
}

/// Why a `vscode-userdata:` URI the session was not given is refused.
const USER_FILE_REFUSED: &str = "this file on the client machine is not one of the editor's settings files";

/// How many jobs may wait for the highlighting thread. A job that does not fit is not lost silently: its
/// document is rebuilt from the session's text.
const HIGHLIGHT_QUEUE: usize = 1024;
/// How long a grammar file may take to arrive.
const GRAMMAR_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The thread that tokenizes documents: a first pass over a long file takes a second, which the session's own
/// task must not spend. Jobs arrive in order; the ones that queued up while it worked are run as one batch.
fn spawn_highlighter(answers: mpsc::UnboundedSender<Input>) -> std::sync::mpsc::SyncSender<HighlightJob> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<HighlightJob>(HIGHLIGHT_QUEUE);
    let spawned = std::thread::Builder::new().name("ember-highlight".into()).spawn(move || {
        let mut worker = Worker::new();
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            batch.extend(rx.try_iter());
            for output in crate::highlight::guard(&mut worker, batch, Worker::run) {
                if answers.send(Input::Highlighted(output)).is_err() {
                    return;
                }
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("ember-editor: cannot start the highlighting thread: {e}");
    }
    tx
}

async fn recv_opt<T>(rx: &mut Option<mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match rx.as_mut() {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

async fn recv_broadcast_opt(
    rx: &mut Option<tokio::sync::broadcast::Receiver<crate::storage::SharedStorageChange>>,
) -> Result<crate::storage::SharedStorageChange, tokio::sync::broadcast::error::RecvError> {
    match rx.as_mut() {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Maximum time shutdown waits for `deactivate()` and pending storage writes before closing IPC.
const SHUTDOWN_STORAGE_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Polling interval for the lightweight local global-storage directory watcher used when
/// `SessionConfig::storage_dir` is set.
const LOCAL_STORAGE_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

fn snapshot_local_storage_dir(dir: &std::path::Path) -> HashMap<String, (u64, Option<std::time::SystemTime>)> {
    let mut map = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                map.insert(name, (meta.len(), meta.modified().ok()));
            }
        }
    }
    map
}

fn spawn_local_storage_watcher(
    dir: std::path::PathBuf,
    internal_tx: mpsc::UnboundedSender<Input>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut prev = snapshot_local_storage_dir(&dir);
        let mut interval = tokio::time::interval(LOCAL_STORAGE_WATCH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            if internal_tx.is_closed() {
                break;
            }
            let dir_clone = dir.clone();
            let curr = match tokio::task::spawn_blocking(move || snapshot_local_storage_dir(&dir_clone)).await {
                Ok(m) => m,
                Err(_) => break,
            };
            let mut changes = Vec::new();
            for (name, sig) in &curr {
                match prev.get(name) {
                    None => changes.push(ember_editor_conn::remote_fs::FileChange {
                        kind: ember_editor_conn::remote_fs::FileChangeType::Added,
                        resource: UriComponents::file(dir.join(name).to_string_lossy().as_ref()),
                        correlation_id: None,
                    }),
                    Some(old) if old != sig => changes.push(ember_editor_conn::remote_fs::FileChange {
                        kind: ember_editor_conn::remote_fs::FileChangeType::Updated,
                        resource: UriComponents::file(dir.join(name).to_string_lossy().as_ref()),
                        correlation_id: None,
                    }),
                    _ => {}
                }
            }
            for name in prev.keys() {
                if !curr.contains_key(name) {
                    changes.push(ember_editor_conn::remote_fs::FileChange {
                        kind: ember_editor_conn::remote_fs::FileChangeType::Deleted,
                        resource: UriComponents::file(dir.join(name).to_string_lossy().as_ref()),
                        correlation_id: None,
                    });
                }
            }
            prev = curr;
            if !changes.is_empty() && internal_tx.send(Input::FilesChanged(changes)).is_err() {
                break;
            }
        }
    })
}

impl Driver {
    async fn run(
        mut self,
        mut cmds: mpsc::UnboundedReceiver<Command>,
        mut rpc_events: mpsc::UnboundedReceiver<RpcEvent>,
        changes: mpsc::UnboundedReceiver<WatchEvent>,
        ipc_lifecycle: mpsc::UnboundedReceiver<IpcLifecycle>,
        mut storage_rx: Option<tokio::sync::broadcast::Receiver<crate::storage::SharedStorageChange>>,
    ) {
        let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Input>();
        self.highlight = Some(spawn_highlighter(internal_tx.clone()));
        let local_watch_task = self
            .local_storage_watch_dir
            .clone()
            .map(|dir| spawn_local_storage_watcher(dir, internal_tx.clone()));
        if !self.commands.is_empty() {
            let _ = self.updates.send(SessionUpdate::Commands { commands: self.commands.clone() });
        }
        let _ = self.updates.send(SessionUpdate::Languages { languages: self.core.language_list() });
        let mut changes = Some(changes);
        let mut lifecycle = Some(ipc_lifecycle);
        let mut exthost_alive = true;
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
                    Some(Command::CheckDirty { prefix, reply }) => {
                        let dirty = self.core.is_any_dirty_at_or_below(&prefix);
                        let _ = reply.send(dirty);
                        None
                    }
                    Some(Command::Settings(scope, layer)) => {
                        self.apply_settings(scope, layer, &internal_tx);
                        None
                    }
                    Some(Command::Shutdown) | None => break,
                },
                ev = rpc_events.recv() => match ev {
                    Some(RpcEvent::Request(r)) => Some(Input::ExtHostRequest(r)),
                    Some(RpcEvent::Cancel(_)) => None,
                    Some(RpcEvent::Lost(reason)) => {
                        exthost_alive = false;
                        let _ = self.updates.send(SessionUpdate::ConnectionLost { reason: format!("extension host connection lost ({reason:?})") });
                        break;
                    }
                    Some(RpcEvent::Disconnected) | None => {
                        exthost_alive = false;
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
                s = recv_broadcast_opt(&mut storage_rx) => match s {
                    Ok(change) if change.origin_session != self.storage_session_id => {
                        Some(Input::SharedStorageChanged {
                            extension_id: change.extension_id,
                            value: change.value,
                        })
                    }
                    Ok(_) => None,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!("ember-editor: storage hub lagged by {skipped} updates; resyncing shared keys");
                        Some(Input::ResyncSharedStorage)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        storage_rx = None;
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
        if let Some(task) = local_watch_task {
            task.abort();
        }
        if exthost_alive {
            exthost::send_terminate(self.peer.connection());
        }
        self.drain_shutdown(&internal_tx, &mut internal_rx, &mut rpc_events, exthost_alive).await;
        self.peer.connection().close();
        self.ipc.connection().close();
    }

    /// Drain queued `rpc_events` (including any pipelined `$setValue` and writes emitted by the
    /// extension host during `deactivate()`) and all pending storage reads/writes/quarantines/retries
    /// until the extension host disconnects and no storage writes remain pending (bounded by 10 s).
    async fn drain_shutdown(
        &mut self,
        internal_tx: &mpsc::UnboundedSender<Input>,
        internal_rx: &mut mpsc::UnboundedReceiver<Input>,
        rpc_events: &mut mpsc::UnboundedReceiver<RpcEvent>,
        mut exthost_alive: bool,
    ) {
        if !exthost_alive && !self.core.has_pending_storage_writes() {
            return;
        }
        let deadline = tokio::time::Instant::now() + SHUTDOWN_STORAGE_DRAIN_TIMEOUT;
        while exthost_alive || self.core.has_pending_storage_writes() {
            let wake = self.core.storage_next_wake();
            let retry_sleep = async move {
                match wake {
                    Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    tracing::warn!("ember-editor: timed out draining extension host deactivate and storage writes at shutdown");
                    break;
                }
                ev = rpc_events.recv(), if exthost_alive => match ev {
                    Some(RpcEvent::Request(r)) => {
                        if r.proxy == Some(exthost::proxy::MAIN_THREAD_STORAGE) {
                            let effects = self.core.handle(Instant::now(), Input::ExtHostRequest(r));
                            self.execute(effects, internal_tx);
                        } else {
                            let reply = exthost::default_reply(r.proxy, &r.method);
                            self.peer.respond(r.req, Ok(reply));
                        }
                    }
                    Some(RpcEvent::Cancel(_)) => {}
                    Some(RpcEvent::Lost(_)) | Some(RpcEvent::Disconnected) | None => {
                        exthost_alive = false;
                    }
                },
                i = internal_rx.recv() => match i {
                    Some(input @ (Input::StorageWritten { .. } | Input::StorageQuarantined { .. } | Input::StorageRead { .. })) => {
                        let effects = self.core.handle(Instant::now(), input);
                        self.execute(effects, internal_tx);
                    }
                    Some(_) => {}
                    None => break,
                },
                _ = retry_sleep => {
                    let effects = self.core.handle(Instant::now(), Input::Tick);
                    self.execute(effects, internal_tx);
                }
            }
        }
    }

    /// The local file behind a `vscode-userdata:` URI: `Some(Ok(path))` for one of the caller's
    /// settings files, `Some(Err(..))` for any other file of that scheme, `None` for a server URI.
    fn user_file(&self, uri: &UriComponents) -> Option<Result<std::path::PathBuf, String>> {
        let path = crate::settings::user_data_path(uri)?;
        Some(if self.user_files.contains(&uri.key()) { Ok(path) } else { Err(USER_FILE_REFUSED.to_owned()) })
    }

    /// Replace one scope, tell the extension host what changed, and give the core the new settings.
    fn apply_settings(&mut self, scope: SettingsScope, layer: SettingsLayer, internal: &mpsc::UnboundedSender<Input>) {
        if scope == SettingsScope::Default {
            return;
        }
        let previous = self.settings.clone();
        *self.settings.layer_mut(scope) = layer;
        let change = self.settings.change_from(&previous);
        if change.is_empty() {
            return;
        }
        self.core.set_settings(self.settings.clone());
        let data = self.settings.configuration_data(&self.scope_table);
        let call = exthost::accept_configuration_changed(&data, &change.keys, &change.overrides);
        self.execute(vec![Effect::Rpc { token: None, call }], internal);
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
                Effect::Highlight(job) => {
                    if let Some(tx) = &self.highlight {
                        if let Err(std::sync::mpsc::TrySendError::Full(job)) = tx.try_send(job) {
                            // The thread is far behind: drop the job and have the document built again from the
                            // session's text instead of leaving the colours wrong.
                            if let Some((key, seq)) = job.target() {
                                let rebuild = crate::highlight::HighlightOutput::rebuild(key, seq);
                                let _ = internal.send(Input::Highlighted(rebuild));
                            }
                        }
                    }
                }
                Effect::ReadFile { uri, purpose } => {
                    if let Some(local) = self.user_file(&uri) {
                        let tx = internal.clone();
                        tokio::spawn(async move {
                            let result = match local {
                                Ok(path) => read_user_file(path).await,
                                Err(refused) => Err(refused),
                            };
                            let _ = tx.send(Input::FileRead { uri, purpose, result });
                        });
                        continue;
                    }
                    let (fs, tx) = (Arc::clone(&self.fs), internal.clone());
                    // A grammar file comes from an extension on the server: ask how big it is before reading it.
                    let cap = matches!(purpose, crate::engine::ReadPurpose::Grammar { .. }).then_some(crate::textmate::MAX_GRAMMAR_BYTES);
                    tokio::spawn(async move {
                        let read = async {
                            if let Some(cap) = cap {
                                match fs.stat(&uri).await {
                                    Ok(st) if st.size > cap => return Err("the grammar file is too large".to_owned()),
                                    Ok(_) => {}
                                    Err(e) => return Err(e.to_string()),
                                }
                            }
                            fs.read_file(&uri).await.map_err(|e| e.to_string())
                        };
                        // A grammar read is bounded in time as well as in size.
                        let result = match cap {
                            Some(_) => match tokio::time::timeout(GRAMMAR_READ_TIMEOUT, read).await {
                                Ok(r) => r,
                                Err(_) => Err("reading the grammar file timed out".to_owned()),
                            },
                            None => read.await,
                        };
                        let _ = tx.send(Input::FileRead { uri, purpose, result });
                    });
                }
                Effect::WriteFile { uri, bytes, token } => {
                    if let Some(local) = self.user_file(&uri) {
                        let tx = internal.clone();
                        tokio::spawn(async move {
                            let result = match local {
                                Ok(path) => write_user_file(path, bytes).await,
                                Err(refused) => Err(refused),
                            };
                            let _ = tx.send(Input::FileWritten { uri, token, result });
                        });
                        continue;
                    }
                    let (fs, tx) = (Arc::clone(&self.fs), internal.clone());
                    tokio::spawn(async move {
                        let result = fs.write_file(&uri, bytes, &WriteOptions::default()).await.map_err(|e| e.to_string());
                        let _ = tx.send(Input::FileWritten { uri, token, result });
                    });
                }
                Effect::ReadStorage { uri, shared, extension_id } => {
                    let tx = internal.clone();
                    if self.local_storage {
                        let path = std::path::PathBuf::from(&uri.path);
                        tokio::spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                crate::storage::read_local_optional(&path).map_err(|e| e.to_string())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                            let _ = tx.send(Input::StorageRead { shared, extension_id, result });
                        });
                    } else {
                        let fs = Arc::clone(&self.fs);
                        let authority = self.authority.clone();
                        let global_dir = self.global_storage_dir.clone();
                        let watch_once = Arc::clone(&self.shared_storage_watch);
                        tokio::spawn(async move {
                            if shared {
                                ensure_shared_storage_watch(&fs, &authority, &global_dir, &watch_once).await;
                            }
                            let result = match fs.read_file(&uri).await {
                                Ok(bytes) => Ok(Some(bytes)),
                                Err(e) if is_not_found(&e) => Ok(None),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = tx.send(Input::StorageRead { shared, extension_id, result });
                        });
                    }
                }
                Effect::ReloadSharedStorage { uri, extension_id, generation } => {
                    let tx = internal.clone();
                    if self.local_storage {
                        let path = std::path::PathBuf::from(&uri.path);
                        tokio::spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                crate::storage::read_local_optional(&path).map_err(|e| e.to_string())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                            let _ = tx.send(Input::SharedStorageReloaded { extension_id, generation, result });
                        });
                    } else {
                        let fs = Arc::clone(&self.fs);
                        tokio::spawn(async move {
                            let result = match fs.read_file(&uri).await {
                                Ok(bytes) => Ok(Some(bytes)),
                                Err(e) if is_not_found(&e) => Ok(None),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = tx.send(Input::SharedStorageReloaded { extension_id, generation, result });
                        });
                    }
                }
                Effect::QuarantineStorage { from, to, shared, extension_id } => {
                    let tx = internal.clone();
                    if self.local_storage {
                        let from_path = std::path::PathBuf::from(&from.path);
                        let to_path = std::path::PathBuf::from(&to.path);
                        tokio::spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                crate::storage::quarantine_local(&from_path, &to_path).map_err(|e| e.to_string())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                            let _ = tx.send(Input::StorageQuarantined { shared, extension_id, result });
                        });
                    } else {
                        let fs = Arc::clone(&self.fs);
                        tokio::spawn(async move {
                            let result = match fs.rename(&from, &to, true).await {
                                Ok(()) => Ok(()),
                                Err(e) if is_not_found(&e) => Ok(()),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = tx.send(Input::StorageQuarantined { shared, extension_id, result });
                        });
                    }
                }
                Effect::WriteStorage { uri, bytes, token } => {
                    let tx = internal.clone();
                    if self.local_storage {
                        let path = std::path::PathBuf::from(&uri.path);
                        tokio::spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                crate::storage::write_local_atomic(&path, bytes.as_deref()).map_err(|e| e.to_string())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                            let _ = tx.send(Input::StorageWritten { token, result });
                        });
                    } else {
                        let (fs, authority) = (Arc::clone(&self.fs), self.authority.clone());
                        let global_dir = self.global_storage_dir.clone();
                        let watch_once = Arc::clone(&self.shared_storage_watch);
                        let is_global = uri.path.starts_with(global_dir.path.trim_end_matches('/'));
                        tokio::spawn(async move {
                            if is_global {
                                ensure_shared_storage_watch(&fs, &authority, &global_dir, &watch_once).await;
                            }
                            let result = write_remote_storage(&fs, &authority, &uri, bytes).await;
                            let _ = tx.send(Input::StorageWritten { token, result });
                        });
                    }
                }
                Effect::BroadcastStorage { extension_id, value } => {
                    if let Some(hub) = &self.storage_hub {
                        hub.publish(self.storage_session_id, extension_id, value);
                    }
                }
                // A file on the client machine is not watched: only this window writes it.
                Effect::Watch { uri } | Effect::Unwatch { uri } if uri.scheme == crate::settings::SCHEME_USER_DATA => {}
                Effect::Watch { uri } => {
                    // FR-L31.24: the workspace folder's recursive watch already reports this file.
                    if self.workspace_watch.as_deref().is_some_and(|ws| crate::watch::covered_by_workspace_watch(ws, &uri.path)) {
                        continue;
                    }
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
