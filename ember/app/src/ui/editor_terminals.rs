//! The editor terminal panel's tabs at run time (FR-P6, FR-L31.23): the [`TerminalTabs`] model
//! fed by the relay, one attachment per tab, and the rename field. Lives on the UI thread; the
//! relay calls run on the app runtime and report back through the shared model and `on_change`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::terminal_list::{self, TermTab, TerminalTabs};
use crate::terminal_stream::{self, ChangeCallback, TerminalHandle};

/// Where the first list of the folder's terminals is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    Loading,
    Ready,
    /// The fixed Korean sentence from [`terminal_list::panel_error`].
    Failed(String),
}

struct Remote {
    tabs: TerminalTabs,
    load: Load,
    /// The last failed new or kill, shown under the strip until the next action.
    error: Option<String>,
}

struct Panel {
    base: String,
    computer: String,
    folder: String,
    project: String,
    remote: Arc<Mutex<Remote>>,
    on_change: ChangeCallback,
    handles: HashMap<String, TerminalHandle>,
    /// The tab being renamed and the text typed so far.
    renaming: Option<(String, String)>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn lock(remote: &Mutex<Remote>) -> std::sync::MutexGuard<'_, Remote> {
    remote.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the strip draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripView {
    pub tabs: Vec<(String, String)>,
    pub active: Option<String>,
    pub load: Load,
    pub error: Option<String>,
    pub renaming: Option<(String, String)>,
}

/// Start the panel for a folder on first use: list the node's terminals for it and reattach to
/// those that survived, or create the first one when there are none.
pub fn ensure(base: &str, computer: &str, folder: &str, project: &str, on_change: ChangeCallback) {
    PANEL.with(|p| {
        let mut slot = p.borrow_mut();
        if slot.is_some() {
            return;
        }
        let remote = Arc::new(Mutex::new(Remote { tabs: TerminalTabs::default(), load: Load::Loading, error: None }));
        let panel = Panel {
            base: base.to_owned(),
            computer: computer.to_owned(),
            folder: folder.to_owned(),
            project: project.to_owned(),
            remote: remote.clone(),
            on_change: on_change.clone(),
            handles: HashMap::new(),
            renaming: None,
        };
        let (b, c, f, pr) = (panel.base.clone(), panel.computer.clone(), panel.folder.clone(), panel.project.clone());
        crate::services::services().rt.spawn(async move {
            let result = async {
                let list = terminal_list::list(&b, &c, &pr).await?;
                let mut tabs = TerminalTabs::from_list(&list, &pr, &f);
                if tabs.is_empty() {
                    let info = terminal_list::create(&b, &c, &f, &pr, terminal_list::tab_key(&pr, &f, 0)).await?;
                    tabs.push(TermTab::from_info(&info));
                }
                Ok::<_, terminal_stream::ClientError>(tabs)
            }
            .await;
            {
                let mut r = lock(&remote);
                match result {
                    Ok(tabs) => {
                        r.tabs = tabs;
                        r.load = Load::Ready;
                    }
                    Err(e) => r.load = Load::Failed(terminal_list::panel_error(terminal_list::TermAction::List, &e)),
                }
            }
            on_change();
        });
        *slot = Some(panel);
    });
}

/// The strip's current state.
pub fn view() -> Option<StripView> {
    PANEL.with(|p| {
        let p = p.borrow();
        let panel = p.as_ref()?;
        let r = lock(&panel.remote);
        let tabs = r.tabs.tabs().iter().map(|t| (t.id.clone(), r.tabs.label(&t.id).unwrap_or_default())).collect();
        Some(StripView {
            tabs,
            active: r.tabs.active_id().map(str::to_owned),
            load: r.load.clone(),
            error: r.error.clone(),
            renaming: panel.renaming.clone(),
        })
    })
}

/// The attachment of the active tab, attached on first show and kept while another tab is shown.
pub fn active_handle() -> Option<TerminalHandle> {
    PANEL.with(|p| {
        let mut p = p.borrow_mut();
        let panel = p.as_mut()?;
        let id = lock(&panel.remote).tabs.active_id()?.to_owned();
        let (base, computer, on_change) = (panel.base.clone(), panel.computer.clone(), panel.on_change.clone());
        Some(
            panel
                .handles
                .entry(id.clone())
                .or_insert_with(|| terminal_stream::attach_existing(&base, &computer, &id, Some(on_change)))
                .clone(),
        )
    })
}

/// Drop the active tab's attachment so the next show attaches again (the 다시 연결 button).
pub fn reattach_active() {
    PANEL.with(|p| {
        if let Some(panel) = p.borrow_mut().as_mut() {
            let id = lock(&panel.remote).tabs.active_id().map(str::to_owned);
            if let Some(id) = id {
                panel.handles.remove(&id);
            }
        }
    });
}

/// Switch tabs.
pub fn select(id: &str) {
    PANEL.with(|p| {
        if let Some(panel) = p.borrow().as_ref() {
            lock(&panel.remote).tabs.select(id);
        }
    });
}

/// The + button: a new terminal in the folder, active once the node has it.
pub fn new_terminal() {
    PANEL.with(|p| {
        let p = p.borrow();
        let Some(panel) = p.as_ref() else { return };
        let key = {
            let mut r = lock(&panel.remote);
            r.error = None;
            r.tabs.next_key(&panel.project, &panel.folder)
        };
        let (b, c, f, pr) = (panel.base.clone(), panel.computer.clone(), panel.folder.clone(), panel.project.clone());
        let (remote, on_change) = (panel.remote.clone(), panel.on_change.clone());
        crate::services::services().rt.spawn(async move {
            let result = terminal_list::create(&b, &c, &f, &pr, key).await;
            {
                let mut r = lock(&remote);
                match result {
                    Ok(info) => r.tabs.push(TermTab::from_info(&info)),
                    Err(e) => r.error = Some(terminal_list::panel_error(terminal_list::TermAction::Create, &e)),
                }
            }
            on_change();
        });
    });
}

/// The trash button: the tab goes at once, the node kills the process.
pub fn kill(id: &str) {
    PANEL.with(|p| {
        let mut p = p.borrow_mut();
        let Some(panel) = p.as_mut() else { return };
        lock(&panel.remote).tabs.remove(id);
        lock(&panel.remote).error = None;
        panel.handles.remove(id);
        if panel.renaming.as_ref().is_some_and(|(r, _)| r == id) {
            panel.renaming = None;
        }
        let (b, c, term) = (panel.base.clone(), panel.computer.clone(), id.to_owned());
        let (remote, on_change) = (panel.remote.clone(), panel.on_change.clone());
        crate::services::services().rt.spawn(async move {
            if let Err(e) = terminal_list::kill(&b, &c, &term).await {
                lock(&remote).error = Some(terminal_list::panel_error(terminal_list::TermAction::Kill, &e));
            }
            on_change();
        });
    });
}

/// Start renaming a tab with its current label in the field.
pub fn begin_rename(id: &str) {
    PANEL.with(|p| {
        if let Some(panel) = p.borrow_mut().as_mut() {
            let label = lock(&panel.remote).tabs.label(id);
            if let Some(label) = label {
                panel.renaming = Some((id.to_owned(), label));
            }
        }
    });
}

pub fn rename_text(text: String) {
    PANEL.with(|p| {
        if let Some((_, t)) = p.borrow_mut().as_mut().and_then(|panel| panel.renaming.as_mut()) {
            *t = text;
        }
    });
}

/// Enter commits the name, Escape (`commit == false`) drops it.
pub fn end_rename(commit: bool) {
    PANEL.with(|p| {
        if let Some(panel) = p.borrow_mut().as_mut() {
            if let Some((id, text)) = panel.renaming.take() {
                if commit {
                    lock(&panel.remote).tabs.rename(&id, &text);
                }
            }
        }
    });
}
