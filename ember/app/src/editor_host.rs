//! Runtime owner for a remote editor session.
use std::sync::{Arc, Mutex};
use dioxus_compose::prelude::*;
use ember_editor::{EditorSession, SessionConfig, SessionUpdate, WidgetEvent};
use ember_editor_conn::uri::UriComponents;
use tokio::sync::mpsc;
use crate::ui::editor::EditorTarget;

/// Commands are serialized with session updates on the application runtime.
pub enum HostCommand {
    List(String),
    Open(String),
    Close(String),
    Focus(String),
    Widget(UriComponents, u64, WidgetEvent),
    Resync(UriComponents),
    /// Explorer edits through the OSE file channel; each lists the parent directory again after.
    CreateFile { parent: String, name: String },
    CreateDir { parent: String, name: String },
    Rename { path: String, new_name: String },
    /// List every file below the project folder for quick open.
    IndexFiles,
    Delete { path: String, is_dir: bool },
    /// Search the project folder (FR-E7). `debounce` waits for a pause in typing first; a newer search
    /// replaces one still waiting.
    Search { id: u64, request: ember_editor::search::SearchRequest, debounce: bool },
    /// A debounced search whose pause is over (sent by the host to itself).
    SearchNow { id: u64, request: ember_editor::search::SearchRequest },
    CancelSearch(u64),
    Replace { id: u64, request: ember_editor::search::ReplaceRequest },
    /// Discard working-tree changes for a file by reading it through the session, reconstructing
    /// the base text from `diff`, and writing it back only if reading succeeds.
    DiscardFile { path: String, diff: crate::ui::vcs::FileDiff },
}

#[derive(Default)]
pub struct HostState {
    pub connected: bool,
    pub error: Option<String>,
    pub entries: Vec<(String, u32)>,
    /// Every directory listed so far, by path, for the explorer tree.
    pub listings: std::collections::HashMap<String, Vec<(String, u32)>>,
    /// The files below the project folder as palette items (sorted), as far as the index has got.
    pub file_items: Arc<Vec<crate::ui::palette::Item>>,
    /// The index is still being built.
    pub index_running: bool,
    /// The index stopped at its cap: the list is not complete.
    pub index_capped: bool,
    /// Failures of explorer edits, for the window to show once.
    pub edit_errors: Vec<String>,
    /// Paths an explorer edit deleted, for the window to close their tabs.
    pub gone: Vec<String>,
    /// (old, new) paths an explorer edit renamed, for the window to move their tabs.
    pub renamed: Vec<(String, String)>,
    /// The search panel's matches and the last replace's report (FR-E7).
    pub search: crate::ui::editor_search::SearchResults,
    pub updates: Vec<SessionUpdate>,
}

const CONNECT_ERROR: &str = "서버에 연결할 수 없습니다";

impl HostState {
    /// State for a target whose endpoint is empty: nothing to connect to.
    pub fn missing_endpoint() -> Self {
        Self { error: Some(CONNECT_ERROR.into()), ..Self::default() }
    }

    /// The initial connection failed.
    pub fn connect_failed(&mut self, error: impl std::fmt::Display) {
        self.error = Some(format!("{CONNECT_ERROR}: {error}"));
    }

    /// The connection is lost: the host is no longer connected.
    pub fn connection_lost(&mut self, reason: &str) {
        self.connected = false;
        self.error = Some(format!("{CONNECT_ERROR}: {reason}"));
    }

    /// Store a directory listing, or surface its error.
    pub fn set_listing<E: std::fmt::Display>(&mut self, result: Result<Vec<(String, u32)>, E>) {
        match result {
            Ok(entries) => self.entries = entries,
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Store the listing of one directory for the tree, and keep it as the current entries.
    pub fn set_dir_listing<E: std::fmt::Display>(&mut self, dir: &str, result: Result<Vec<(String, u32)>, E>) {
        if let Ok(entries) = &result {
            self.listings.insert(dir.to_string(), entries.clone());
        }
        self.set_listing(result);
    }

    /// Forget the listings of a path that was deleted or renamed and of everything below it.
    pub fn forget_below(&mut self, path: &str) {
        self.listings.retain(|dir, _| !crate::ui::editor::is_at_or_below(dir, path));
    }

    /// Handle one session update: search and replace results go to the search panel, connection loss is
    /// recorded, every other update is queued in order.
    pub fn apply_update(&mut self, update: SessionUpdate) {
        if self.search.apply(&update) {
            return;
        }
        if let SessionUpdate::ConnectionLost { reason } = &update {
            self.connection_lost(reason);
        }
        self.updates.push(update);
    }

    /// Drain the queued updates in arrival order.
    pub fn take_updates(&mut self) -> Vec<SessionUpdate> {
        std::mem::take(&mut self.updates)
    }
}

/// How many files and directories the quick open index takes before it stops.
const INDEX_MAX_FILES: usize = 20_000;
const INDEX_MAX_DIRS: usize = 5_000;
/// The list is published to the window every this many directories while the walk goes on.
const INDEX_PUBLISH_EVERY: usize = 200;

/// Walks the project below `root`, breadth first, through the OSE file channel and keeps `shared`
/// up to date: the files so far every few hundred directories and the finished list at the end.
/// Directories the index does not enter are named by [`crate::ui::palette::skipped_directory`]; a
/// symbolic link to a directory is not followed.
async fn index_files(lister: ember_editor::DirLister, root: String, shared: Arc<Mutex<HostState>>, mut revision: SyncSignal<u64>) {
    use ember_editor_conn::remote_fs::file_type::{DIRECTORY, SYMBOLIC_LINK};
    let mut files: Vec<String> = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.clone()]);
    let mut visited = 0usize;
    let mut capped = false;
    while let Some(dir) = queue.pop_front() {
        if files.len() >= INDEX_MAX_FILES || visited >= INDEX_MAX_DIRS {
            capped = true;
            break;
        }
        visited += 1;
        let Ok(entries) = lister.read_dir(&dir).await else { continue };
        let siblings: Vec<(String, u32)> = entries.clone();
        for (name, kind) in entries {
            let path = crate::ui::editor_workbench::join_path(&dir, &name);
            if kind & DIRECTORY != 0 {
                if !crate::ui::palette::skipped_directory(&name, &siblings) && kind & SYMBOLIC_LINK == 0 {
                    queue.push_back(path);
                }
            } else if name != ".DS_Store" {
                files.push(path);
            }
        }
        if visited % INDEX_PUBLISH_EVERY == 0 {
            shared.lock().unwrap().file_items = Arc::new(crate::ui::palette::file_items(&root, &files));
            *revision.write() += 1;
        }
    }
    {
        let mut state = shared.lock().unwrap();
        state.file_items = Arc::new(crate::ui::palette::file_items(&root, &files));
        state.index_running = false;
        state.index_capped = capped;
    }
    *revision.write() += 1;
}

fn parent_of(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

/// After an explorer edit: record a failure for the window, then list the parent again so the tree
/// shows what is on disk.
async fn finish_edit(session: &ember_editor::EditorSession, shared: &Arc<Mutex<HostState>>, action: &str, parent: &str, result: Result<(), ember_editor::Error>) {
    if let Err(error) = result {
        // The window shows a Korean reason; the server's own text goes to the log.
        tracing::warn!("explorer edit failed ({action}): {error}");
        let text = match &error {
            ember_editor::Error::Path(path) => format!("{action} 실패: {path}"),
            other => crate::ui::editor::edit_failure_text(action, &other.to_string()),
        };
        shared.lock().unwrap().edit_errors.push(text);
    }
    let listed = session.read_dir(&session.uri(parent)).await;
    shared.lock().unwrap().set_dir_listing(parent, listed);
}

#[derive(Clone)]
pub struct EditorHost {
    pub state: Arc<Mutex<HostState>>,
    tx: mpsc::UnboundedSender<HostCommand>,
}

impl EditorHost {
    pub fn start(target: EditorTarget, mut revision: SyncSignal<u64>) -> Self {
        let state = Arc::new(Mutex::new(HostState::default()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let shared = state.clone();
        let self_tx = tx.clone();
        crate::services::services().rt.spawn(async move {
            if target.endpoint.trim().is_empty() {
                shared.lock().unwrap().error = HostState::missing_endpoint().error;
                *revision.write() += 1;
                return;
            }
            let root = target.folder.clone();
            let mut config = SessionConfig::new(target.endpoint, target.folder);
            config.connection_token = (!target.token.is_empty()).then_some(target.token);
            config.app_language = "ko".into();
            let (session, mut updates) = match EditorSession::connect_tcp(config).await {
                Ok(pair) => pair,
                Err(error) => {
                    shared.lock().unwrap().connect_failed(error);
                    *revision.write() += 1;
                    return;
                }
            };
            {
                let entries = session.read_dir(&session.workspace_uri()).await;
                let mut state = shared.lock().unwrap();
                state.connected = true;
                state.set_dir_listing(&root, entries);
            }
            *revision.write() += 1;
            let mut index_task: Option<tokio::task::JoinHandle<()>> = None;
            // The newest search the window asked for: a debounced one that is no longer it is dropped.
            let mut latest_search = 0u64;
            loop {
                tokio::select! {
                    command = rx.recv() => match command {
                        Some(HostCommand::List(path)) => {
                            let result = session.read_dir(&session.uri(&path)).await;
                            shared.lock().unwrap().set_dir_listing(&path, result);
                        }
                        Some(HostCommand::CreateFile { parent, name }) => {
                            let result = session.create_file(&parent, &name).await;
                            finish_edit(&session, &shared, "새 파일", &parent, result.map(|_| ())).await;
                        }
                        Some(HostCommand::CreateDir { parent, name }) => {
                            let result = session.create_dir(&parent, &name).await;
                            finish_edit(&session, &shared, "새 폴더", &parent, result.map(|_| ())).await;
                        }
                        Some(HostCommand::Rename { path, new_name }) => {
                            let parent = parent_of(&path);
                            let result = session.rename_path(&path, &new_name).await;
                            if let Ok(new_path) = &result {
                                let mut state = shared.lock().unwrap();
                                state.forget_below(&path);
                                state.renamed.push((path.clone(), new_path.clone()));
                            }
                            finish_edit(&session, &shared, "이름 변경", &parent, result.map(|_| ())).await;
                        }
                        Some(HostCommand::Delete { path, is_dir }) => {
                            let parent = parent_of(&path);
                            let result = session.delete_path(&path, is_dir).await;
                            if result.is_ok() {
                                let mut state = shared.lock().unwrap();
                                state.forget_below(&path);
                                state.gone.push(path.clone());
                            }
                            finish_edit(&session, &shared, "삭제", &parent, result).await;
                        }
                        Some(HostCommand::DiscardFile { path, diff }) => {
                            let parent = parent_of(&path);
                            if !diff.hunks.is_empty() {
                                let cur = session.read_file_text(&path).await;
                                let result = match crate::ui::editor_scm::reconstruct_base_from_read(|| cur, &diff) {
                                    Ok(base) => {
                                        let write_res = session.write_file(&path, base.as_bytes()).await;
                                        if write_res.is_ok() {
                                            let _ = session.reload(session.uri(&path));
                                        }
                                        write_res
                                    }
                                    Err(err) => Err(err),
                                };
                                finish_edit(&session, &shared, "변경 사항 취소", &parent, result).await;
                            }
                        }
                        Some(HostCommand::IndexFiles) => {
                            // A fresh index replaces the last one and a running one is cancelled; the walk runs
                            // in a task of its own so the session keeps answering while it goes.
                            if let Some(task) = index_task.take() { task.abort(); }
                            {
                                let mut state = shared.lock().unwrap();
                                state.file_items = Arc::new(Vec::new());
                                state.index_running = true;
                                state.index_capped = false;
                            }
                            *revision.write() += 1;
                            index_task = Some(tokio::spawn(index_files(session.lister(), root.clone(), shared.clone(), revision)));
                        }
                        Some(HostCommand::Search { id, request, debounce }) => {
                            latest_search = id;
                            if debounce {
                                let tx = self_tx.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_millis(crate::ui::editor_search::SEARCH_DEBOUNCE_MS)).await;
                                    let _ = tx.send(HostCommand::SearchNow { id, request });
                                });
                            } else {
                                let _ = session.search(id, request);
                            }
                        }
                        Some(HostCommand::SearchNow { id, request }) => {
                            if id == latest_search { let _ = session.search(id, request); }
                        }
                        Some(HostCommand::CancelSearch(id)) => {
                            if id == latest_search { latest_search = 0; }
                            let _ = session.cancel_search(id);
                        }
                        Some(HostCommand::Replace { id, request }) => { let _ = session.replace(id, request); }
                        Some(HostCommand::Open(path)) => { let _ = session.open(session.uri(&path)); }
                        Some(HostCommand::Close(path)) => { let _ = session.close(session.uri(&path)); }
                        Some(HostCommand::Focus(path)) => { let _ = session.focus(session.uri(&path)); }
                        Some(HostCommand::Widget(uri, generation, event)) => { let _ = session.widget_event(uri, generation, event); }
                        Some(HostCommand::Resync(uri)) => { let _ = session.resync(uri); }
                        None => break,
                    },
                    update = updates.recv() => match update {
                        Some(update) => {
                            // After a replace the search runs again so the list shows what is left.
                            let refresh = {
                                let mut state = shared.lock().unwrap();
                                state.apply_update(update);
                                state.search.take_refresh()
                            };
                            if let Some((id, request)) = refresh {
                                latest_search = id;
                                let _ = session.search(id, request);
                            }
                        }
                        None => break,
                    }
                }
                *revision.write() += 1;
            }
            if let Some(task) = index_task.take() { task.abort(); }
        });
        Self { state, tx }
    }

    pub fn send(&self, command: HostCommand) -> bool { self.tx.send(command).is_ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lost(reason: &str) -> SessionUpdate {
        SessionUpdate::ConnectionLost { reason: reason.into() }
    }

    #[test]
    fn fr_e1_connection_lost_clears_connected_and_sets_korean_error() {
        let mut state = HostState { connected: true, ..Default::default() };
        state.apply_update(lost("socket closed"));
        assert!(!state.connected);
        assert_eq!(state.error.as_deref(), Some("서버에 연결할 수 없습니다: socket closed"));
        assert_eq!(state.updates.len(), 1);
    }

    #[test]
    fn fr_e1_updates_are_queued_in_order_and_drained() {
        let mut state = HostState::default();
        state.apply_update(lost("first"));
        state.apply_update(lost("second"));
        let drained = state.take_updates();
        let reasons: Vec<_> = drained
            .iter()
            .map(|u| match u {
                SessionUpdate::ConnectionLost { reason } => reason.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(reasons, ["first", "second"]);
        assert!(state.updates.is_empty());
    }

    #[test]
    fn fr_e1_listing_error_surfaces_and_success_keeps_entries() {
        let mut state = HostState::default();
        state.set_listing(Ok::<_, String>(vec![("src".into(), 2)]));
        assert_eq!(state.entries, vec![("src".to_string(), 2)]);
        assert!(state.error.is_none());
        state.set_listing(Err::<Vec<(String, u32)>, _>("permission denied"));
        assert_eq!(state.error.as_deref(), Some("permission denied"));
        assert_eq!(state.entries.len(), 1);
    }

    #[test]
    fn fr_e7_search_updates_go_to_the_panel_not_the_queue() {
        use ember_editor::search::{SearchEnd, SearchRequest};
        let mut state = HostState::default();
        state.search.start(4, SearchRequest::default());
        state.apply_update(SessionUpdate::SearchDone { id: 4, end: SearchEnd::Complete { limit_hit: false } });
        assert!(state.updates.is_empty());
        assert!(!state.search.running);
        state.apply_update(lost("x"));
        assert_eq!(state.updates.len(), 1);
    }

    #[test]
    fn fr_e1_empty_endpoint_error_text() {
        let state = HostState::missing_endpoint();
        assert_eq!(state.error.as_deref(), Some("서버에 연결할 수 없습니다"));
        assert!(!state.connected);
    }

    #[test]
    fn fr_e1_connect_failure_appends_the_cause() {
        let mut state = HostState::default();
        state.connect_failed("refused");
        assert_eq!(state.error.as_deref(), Some("서버에 연결할 수 없습니다: refused"));
    }
}
