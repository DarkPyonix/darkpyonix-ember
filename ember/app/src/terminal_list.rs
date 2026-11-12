//! The editor's terminal tabs (FR-P6, FR-L31.23): which of the node's terminals the panel lists,
//! the tab strip's model (new, kill, rename, switch) and the relay calls behind it.
//!
//! Code-OSS keeps one tab per terminal instance; Ember does the same with the node's persistent
//! terminals. The node keeps the PTYs, so a reopened window lists the folder's running editor
//! terminals again and reattaches to them instead of starting a fresh shell. The model is pure,
//! so the list, kill and reattach rules are testable without a server.

use crate::terminal_stream::{
    server_token, session_key, terms_list_url, terms_url, ClientError, PtySize, TermCreateRequest,
    TermCreateResponse, TermInfo, TermOrigin,
};

/// The node's origin string for terminals the editor started.
pub const EDITOR_ORIGIN: &str = "ide-ember";

/// Idempotency key of the editor's `n`th terminal for a folder. The first keeps the plain
/// [`session_key`] so a terminal from before the tab strip is still found; the rest add `#n`.
pub fn tab_key(project: &str, folder: &str, n: u32) -> String {
    let base = session_key(project, folder);
    if n == 0 { base } else { format!("{base}#{n}") }
}

/// The `n` of a key made by [`tab_key`] for this folder, `None` for any other key.
pub fn key_index(key: &str, project: &str, folder: &str) -> Option<u32> {
    let base = session_key(project, folder);
    let rest = key.strip_prefix(&base)?;
    if rest.is_empty() {
        return Some(0);
    }
    rest.strip_prefix('#')?.parse().ok().filter(|n| *n > 0)
}

/// True when `info` is a running terminal the editor started for this folder: those are the ones
/// the panel lists and reattaches to. Agent and user terminals of the project are not tabs here.
pub fn is_folder_terminal(info: &TermInfo, project: &str, folder: &str) -> bool {
    info.origin == EDITOR_ORIGIN
        && info.state == "running"
        && info.key.as_deref().is_some_and(|k| key_index(k, project, folder).is_some())
}

/// One tab of the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermTab {
    pub id: String,
    /// The node's title (the shell's OSC title or program name).
    pub title: String,
    /// The name the user gave it with rename; wins over `title`.
    pub label: Option<String>,
    pub key: Option<String>,
}

impl TermTab {
    pub fn from_info(info: &TermInfo) -> Self {
        Self { id: info.id.clone(), title: info.title.clone(), label: None, key: info.key.clone() }
    }
}

/// The tab strip: the folder's terminals in creation order and the active one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalTabs {
    tabs: Vec<TermTab>,
    active: Option<usize>,
}

impl TerminalTabs {
    /// Reattach after the window reopens: the folder's running editor terminals, oldest first,
    /// with the first active (Code-OSS restores its terminals in their old order).
    pub fn from_list(list: &[TermInfo], project: &str, folder: &str) -> Self {
        let mut mine: Vec<&TermInfo> = list.iter().filter(|t| is_folder_terminal(t, project, folder)).collect();
        mine.sort_by_key(|t| (t.created_ms, t.key.as_deref().and_then(|k| key_index(k, project, folder))));
        let tabs: Vec<TermTab> = mine.into_iter().map(TermTab::from_info).collect();
        let active = if tabs.is_empty() { None } else { Some(0) };
        Self { tabs, active }
    }

    pub fn tabs(&self) -> &[TermTab] {
        &self.tabs
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active(&self) -> Option<&TermTab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active().map(|t| t.id.as_str())
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Add a terminal the panel just created and make it active (Code-OSS focuses a new terminal).
    /// A terminal already listed (the node re-adopted it by key) is only activated.
    pub fn push(&mut self, tab: TermTab) {
        let i = match self.index(&tab.id) {
            Some(i) => i,
            None => {
                self.tabs.push(tab);
                self.tabs.len() - 1
            }
        };
        self.active = Some(i);
    }

    /// Switch to a tab. Unknown ids change nothing.
    pub fn select(&mut self, id: &str) -> bool {
        match self.index(id) {
            Some(i) => {
                self.active = Some(i);
                true
            }
            None => false,
        }
    }

    /// Drop a killed (or exited) terminal. When it was active, the tab now at its place becomes
    /// active, else the last one, as Code-OSS does when an instance is disposed.
    pub fn remove(&mut self, id: &str) -> Option<TermTab> {
        let i = self.index(id)?;
        let tab = self.tabs.remove(i);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a == i => Some(i.min(self.tabs.len() - 1)),
            Some(a) if a > i => Some(a - 1),
            other => other,
        };
        Some(tab)
    }

    /// Rename a tab. An empty or blank name goes back to the node's title, like Code-OSS.
    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let Some(i) = self.index(id) else { return false };
        let name = name.trim();
        self.tabs[i].label = if name.is_empty() { None } else { Some(name.to_owned()) };
        true
    }

    /// What the tab shows: the user's name, the node's title, else "터미널 n".
    pub fn label(&self, id: &str) -> Option<String> {
        let i = self.index(id)?;
        let tab = &self.tabs[i];
        Some(
            tab.label
                .clone()
                .or_else(|| Some(tab.title.trim().to_owned()).filter(|t| !t.is_empty()))
                .unwrap_or_else(|| format!("터미널 {}", i + 1)),
        )
    }

    /// Bring the list up to date with the node's: drop tabs whose terminal ended or was killed
    /// elsewhere, add folder terminals another window started, refresh titles. Labels stay.
    pub fn sync(&mut self, list: &[TermInfo], project: &str, folder: &str) {
        let live: Vec<&TermInfo> = list.iter().filter(|t| is_folder_terminal(t, project, folder)).collect();
        let gone: Vec<String> =
            self.tabs.iter().filter(|t| !live.iter().any(|l| l.id == t.id)).map(|t| t.id.clone()).collect();
        for id in gone {
            self.remove(&id);
        }
        for info in live {
            match self.index(&info.id) {
                Some(i) => self.tabs[i].title = info.title.clone(),
                None => {
                    self.tabs.push(TermTab::from_info(info));
                    if self.active.is_none() {
                        self.active = Some(self.tabs.len() - 1);
                    }
                }
            }
        }
    }

    /// The key for the next new terminal: the smallest `n` no listed tab uses.
    pub fn next_key(&self, project: &str, folder: &str) -> String {
        let used: Vec<u32> =
            self.tabs.iter().filter_map(|t| t.key.as_deref().and_then(|k| key_index(k, project, folder))).collect();
        let n = (0..).find(|n| !used.contains(n)).unwrap_or(0);
        tab_key(project, folder, n)
    }
}

/// The create request for an editor terminal with `key` in `folder`.
pub fn create_request(folder: &str, project: &str, key: String) -> TermCreateRequest {
    TermCreateRequest {
        program: None,
        cwd: folder.into(),
        env: Default::default(),
        env_clear: false,
        size: Some(PtySize { rows: 24, cols: 80 }),
        origin: TermOrigin::IdeEmber,
        project: Some(project.to_owned()),
        title: None,
        key: Some(key),
        tags: Default::default(),
    }
}

/// `POST {base}/api/computers/{computer}/terms/{id}/kill`.
pub fn kill_url(base: &str, computer: &str, term_id: &str) -> String {
    format!("{}/{}/kill", terms_url(base, computer), encode(term_id))
}

/// `DELETE {base}/api/computers/{computer}/terms/{id}`: forget an ended terminal.
pub fn remove_url(base: &str, computer: &str, term_id: &str) -> String {
    format!("{}/{}", terms_url(base, computer), encode(term_id))
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn authed(req: reqwest::RequestBuilder, base: &str) -> reqwest::RequestBuilder {
    match server_token(base) {
        Some(token) => req.bearer_auth(token),
        None => req,
    }
}

/// The project's running editor terminals on `computer`, through the relay.
pub async fn list(base: &str, computer: &str, project: &str) -> Result<Vec<TermInfo>, ClientError> {
    let url = terms_list_url(base, computer, project, Some(EDITOR_ORIGIN), true);
    Ok(authed(reqwest::Client::new().get(url), base).send().await?.error_for_status()?.json().await?)
}

/// Create (or re-adopt by key) an editor terminal.
pub async fn create(base: &str, computer: &str, folder: &str, project: &str, key: String) -> Result<TermInfo, ClientError> {
    let req = authed(reqwest::Client::new().post(terms_url(base, computer)), base).json(&create_request(folder, project, key));
    let created: TermCreateResponse = req.send().await?.error_for_status()?.json().await?;
    Ok(created.term)
}

/// Kill a terminal (the trash button), then ask the node to forget it. The node refuses the
/// removal while the process is still exiting; the running-only list hides it either way.
pub async fn kill(base: &str, computer: &str, term_id: &str) -> Result<(), ClientError> {
    authed(reqwest::Client::new().post(kill_url(base, computer, term_id)), base).send().await?.error_for_status()?;
    let _ = authed(reqwest::Client::new().delete(remove_url(base, computer, term_id)), base).send().await;
    Ok(())
}
