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
}

#[derive(Default)]
pub struct HostState {
    pub connected: bool,
    pub error: Option<String>,
    pub entries: Vec<(String, u32)>,
    /// Every directory listed so far, by path, for the explorer tree.
    pub listings: std::collections::HashMap<String, Vec<(String, u32)>>,
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

    /// Handle one session update: connection loss is recorded, every update is queued in order.
    pub fn apply_update(&mut self, update: SessionUpdate) {
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
            loop {
                tokio::select! {
                    command = rx.recv() => match command {
                        Some(HostCommand::List(path)) => {
                            let result = session.read_dir(&session.uri(&path)).await;
                            shared.lock().unwrap().set_dir_listing(&path, result);
                        }
                        Some(HostCommand::Open(path)) => { let _ = session.open(session.uri(&path)); }
                        Some(HostCommand::Close(path)) => { let _ = session.close(session.uri(&path)); }
                        Some(HostCommand::Focus(path)) => { let _ = session.focus(session.uri(&path)); }
                        Some(HostCommand::Widget(uri, generation, event)) => { let _ = session.widget_event(uri, generation, event); }
                        Some(HostCommand::Resync(uri)) => { let _ = session.resync(uri); }
                        None => break,
                    },
                    update = updates.recv() => match update {
                        Some(update) => {
                            shared.lock().unwrap().apply_update(update);
                        }
                        None => break,
                    }
                }
                *revision.write() += 1;
            }
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
