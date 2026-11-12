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
use tokio::sync::mpsc;
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
/// file that does not parse all read as an empty scope: the window still opens, and the settings
/// view says what is wrong with the file when the person opens it.
async fn read_server_layer(fs: &RemoteFs, uri: &UriComponents) -> SettingsLayer {
    let bytes = match tokio::time::timeout(SETTINGS_READ_TIMEOUT, fs.read_file(uri)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            tracing::debug!("ember-editor: no settings at {}: {e}", uri.path);
            return SettingsLayer::default();
        }
        Err(_) => {
            tracing::warn!("ember-editor: reading settings at {} timed out", uri.path);
            return SettingsLayer::default();
        }
    };
    let Ok((text, _bom)) = crate::document::decode(&bytes) else {
        tracing::warn!("ember-editor: settings at {} are not UTF-8", uri.path);
        return SettingsLayer::default();
    };
    match crate::settings::parse_layer(&text) {
        Ok(layer) => layer,
        Err(e) => {
            tracing::warn!("ember-editor: settings at {} do not parse: {e}", uri.path);
            SettingsLayer::default()
        }
    }
}

/// Whether a file-channel error says the file is not there.
fn is_not_found(error: &ember_editor_conn::Error) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("entrynotfound") || text.contains("enoent") || text.contains("filenotfound") || text.contains("no such file")
}

/// A connected editor session. Cheap to share by reference; every method is a non-blocking send.
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
    user_files: Vec<std::path::PathBuf>,
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
        if config.server_settings {
            let workspace_file = format!("{}/{}", workspace_path.trim_end_matches('/'), crate::settings::WORKSPACE_SETTINGS_FILE);
            settings.workspace = read_server_layer(&fs, &UriComponents::remote(&authority, &workspace_file)).await;
            if let Some(path) = &machine_settings_path {
                settings.remote = read_server_layer(&fs, &UriComponents::remote(&authority, path)).await;
            }
        }
        let commands = crate::commands::parse_contributed_commands(&extensions);
        let mut core = SessionCore::new(LanguageRegistry::from_extensions(&extensions), config.core.clone(), Instant::now());
        core.set_settings(settings.clone());
        core.set_grammar_catalog(crate::textmate::GrammarCatalog::from_extensions(&extensions));
        core.set_workspace_trust(crate::trust::WorkspaceTrust::new(config.workspace_trusted, extensions.clone()));
        let driver = Driver {
            core,
            highlight: None,
            peer: peer.clone(),
            fs: Arc::clone(&fs),
            ipc: ipc.clone(),
            updates: updates_tx,
            reqs: HashMap::new(),
            watches: Arc::new(Mutex::new(HashMap::new())),
            commands,
            settings: settings.clone(),
            scope_table: scope_table.clone(),
            user_files: config.user_files.iter().map(|p| crate::settings::user_data_uri(p).key()).collect(),
        };
        let task = tokio::spawn(driver.run(rx, rpc_events, changes, ipc_lifecycle));

        let configuration = settings.configuration_data(&scope_table);
        for call in exthost::bootstrap_calls(Some(&workspace), config.workspace_trusted, &configuration) {
            call.start(&peer)?.wait().await?;
        }

        let app_language = config.app_language.clone();
        Ok((Self { tx, authority, workspace_path, fs, ipc, app_language, installed, running, task: Some(task), settings, catalogue, machine_settings_path, user_files: config.user_files.clone() }, updates_rx))
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

    /// Refuses a directory that is a symbolic link: an item made in it would land wherever the link points.
    async fn refuse_symlink_parent(&self, parent: &str) -> Result<(), Error> {
        let stat = self.fs.stat(&self.uri(parent)).await.map_err(Error::Conn)?;
        if stat.file_type & ember_editor_conn::remote_fs::file_type::SYMBOLIC_LINK != 0 {
            return Err(Error::Path(crate::paths::PathError::Symlink));
        }
        Ok(())
    }

    /// Creates an empty file named `name` in the directory `parent`. Never overwrites: an existing
    /// file is an error. Returns the new path.
    pub async fn create_file(&self, parent: &str, name: &str) -> Result<String, Error> {
        let path = crate::paths::child_path(&self.workspace_path, parent, name).map_err(Error::Path)?;
        self.refuse_symlink_parent(parent).await?;
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
        self.refuse_symlink_parent(parent).await?;
        self.fs.mkdir(&self.uri(&path)).await.map_err(Error::Conn)?;
        Ok(path)
    }

    /// Renames `path` to `new_name` in the same directory. Never overwrites. A symbolic link is
    /// renamed itself. Returns the new path.
    pub async fn rename_path(&self, path: &str, new_name: &str) -> Result<String, Error> {
        let to = crate::paths::renamed_path(&self.workspace_path, path, new_name).map_err(Error::Path)?;
        let from = crate::paths::target_path(&self.workspace_path, path).map_err(Error::Path)?;
        self.fs.rename(&self.uri(&from), &self.uri(&to), false).await.map_err(Error::Conn)?;
        Ok(to)
    }

    /// Deletes one file or directory (a directory with everything in it). There is no trash: it
    /// cannot be undone. A symbolic link is removed itself and never recursed into.
    pub async fn delete_path(&self, path: &str, is_dir: bool) -> Result<(), Error> {
        let path = crate::paths::target_path(&self.workspace_path, path).map_err(Error::Path)?;
        let stat = self.fs.stat(&self.uri(&path)).await.map_err(Error::Conn)?;
        let is_link = stat.file_type & ember_editor_conn::remote_fs::file_type::SYMBOLIC_LINK != 0;
        let opts = ember_editor_conn::remote_fs::DeleteOptions { recursive: is_dir && !is_link, use_trash: false, atomic: false };
        self.fs.delete(&self.uri(&path), &opts).await.map_err(Error::Conn)
    }

    /// Moves a file or directory at `src` into `dest_dir`.
    pub async fn move_path(&self, src: &str, dest_dir: &str) -> Result<String, Error> {
        let from = crate::paths::target_path(&self.workspace_path, src).map_err(Error::Path)?;
        self.refuse_symlink_parent(dest_dir).await?;
        let to = crate::paths::paste_destination(&self.workspace_path, &from, dest_dir, &[], true).map_err(Error::Path)?;
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
        self.refuse_symlink_parent(&dest_norm).await?;
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
    ipc: IpcClient,
    updates: mpsc::UnboundedSender<SessionUpdate>,
    /// core token → RPC request id, for `Cancel`.
    reqs: HashMap<Token, u32>,
    /// uri key → watch request id.
    watches: Arc<Mutex<HashMap<String, String>>>,
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

impl Driver {
    async fn run(
        mut self,
        mut cmds: mpsc::UnboundedReceiver<Command>,
        mut rpc_events: mpsc::UnboundedReceiver<RpcEvent>,
        changes: mpsc::UnboundedReceiver<WatchEvent>,
        ipc_lifecycle: mpsc::UnboundedReceiver<IpcLifecycle>,
    ) {
        let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Input>();
        self.highlight = Some(spawn_highlighter(internal_tx.clone()));
        if !self.commands.is_empty() {
            let _ = self.updates.send(SessionUpdate::Commands { commands: self.commands.clone() });
        }
        let _ = self.updates.send(SessionUpdate::Languages { languages: self.core.language_list() });
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
                // A file on the client machine is not watched: only this window writes it.
                Effect::Watch { uri } | Effect::Unwatch { uri } if uri.scheme == crate::settings::SCHEME_USER_DATA => {}
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
