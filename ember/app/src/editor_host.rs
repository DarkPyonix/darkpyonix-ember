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
    pub updates: Vec<SessionUpdate>,
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
                shared.lock().unwrap().error = Some("서버에 연결할 수 없습니다".into());
                *revision.write() += 1;
                return;
            }
            let mut config = SessionConfig::new(target.endpoint, target.folder);
            config.connection_token = (!target.token.is_empty()).then_some(target.token);
            config.app_language = "ko".into();
            let (session, mut updates) = match EditorSession::connect_tcp(config).await {
                Ok(pair) => pair,
                Err(error) => {
                    shared.lock().unwrap().error = Some(format!("서버에 연결할 수 없습니다: {error}"));
                    *revision.write() += 1;
                    return;
                }
            };
            {
                let entries = session.read_dir(&session.workspace_uri()).await;
                let mut state = shared.lock().unwrap();
                state.connected = true;
                match entries {
                    Ok(entries) => state.entries = entries,
                    Err(error) => state.error = Some(error.to_string()),
                }
            }
            *revision.write() += 1;
            loop {
                tokio::select! {
                    command = rx.recv() => match command {
                        Some(HostCommand::List(path)) => {
                            let result = session.read_dir(&session.uri(&path)).await;
                            let mut state = shared.lock().unwrap();
                            match result { Ok(entries) => state.entries = entries, Err(e) => state.error = Some(e.to_string()) }
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
                            let mut state = shared.lock().unwrap();
                            if let SessionUpdate::ConnectionLost { reason } = &update {
                                state.connected = false;
                                state.error = Some(format!("서버에 연결할 수 없습니다: {reason}"));
                            }
                            state.updates.push(update);
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
