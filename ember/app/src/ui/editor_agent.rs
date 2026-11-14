//! The editor window's agent tool window (SPEC FR-L31.25, #766) and "에이전트에게 묻기" (Action::Ask).
//!
//! Shows the project's agent sessions (members, status, latest turns), lets the user send a message
//! to a chosen member about the current file or selection with context attached (`#file:path` or
//! `#selection:path:start-end`), and opens the session screen for the full conversation.

use std::sync::{Arc, Mutex};

use dioxus_compose::prelude::*;
use ember_client::LauncherStatus;

fn one_line_preview(text: &str, max_chars: usize) -> String {
    let clean = text.replace('\n', " ").replace('\r', " ");
    let trimmed = clean.trim();
    if trimmed.chars().count() > max_chars {
        let head: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    } else {
        trimmed.to_string()
    }
}

/// Extracts the path of `path` relative to `folder`.
pub fn relative_path(path: &str, folder: &str) -> String {
    let folder = folder.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix(folder) {
        rest.trim_start_matches('/').to_string()
    } else {
        path.trim_start_matches('/').to_string()
    }
}

/// Formats the file context tag, matching Code-OSS chat: `#file:path`.
pub fn format_file_context(path: &str, folder: &str) -> String {
    let rel = relative_path(path, folder);
    format!("#file:{rel}")
}

/// Formats the selection context tag with line range, matching Code-OSS chat: `#selection:path:start-end`.
pub fn format_selection_context(path: &str, folder: &str, start_line: usize, end_line: usize) -> String {
    let rel = relative_path(path, folder);
    let (s, e) = (start_line.min(end_line), start_line.max(end_line));
    format!("#selection:{rel}:{s}-{e}")
}

/// Formats the context tag based on whether a selection range is active.
pub fn format_context(path: &str, folder: &str, selection: Option<(usize, usize)>) -> String {
    match selection {
        Some((start, end)) if start != end => format_selection_context(path, folder, start, end),
        _ => format_file_context(path, folder),
    }
}

/// One turn in a session preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurn {
    pub is_user: bool,
    pub sender: String,
    pub text: String,
    pub timestamp: String,
}

/// An agent session available for chatting in the editor panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub id: String,
    pub title: String,
    pub member: String,
    pub role: String,
    pub agent: String,
    pub status: String,
    pub status_badge: String,
    pub preview: String,
    pub turns: Vec<AgentTurn>,
}

/// The state of the editor's Agent tool window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentState {
    pub project: String,
    pub folder: String,
    pub sessions: Vec<AgentSession>,
    pub selected_session_id: Option<String>,
    pub attached_context: Option<String>,
    pub draft_prompt: String,
    pub status_note: Option<String>,
}

impl AgentState {
    pub fn new(project: &str, folder: &str) -> Self {
        let sessions = sample_sessions_for(project);
        let selected_session_id = sessions.first().map(|s| s.id.clone());
        Self {
            project: project.to_string(),
            folder: folder.to_string(),
            sessions,
            selected_session_id,
            attached_context: None,
            draft_prompt: String::new(),
            status_note: None,
        }
    }

    pub fn selected_session(&self) -> Option<&AgentSession> {
        let id = self.selected_session_id.as_deref()?;
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn select_session(&mut self, id: &str) {
        if self.sessions.iter().any(|s| s.id == id) {
            self.selected_session_id = Some(id.to_string());
            self.status_note = None;
        }
    }

    pub fn attach_context(&mut self, context: String) {
        self.attached_context = Some(context);
    }

    pub fn remove_context(&mut self) {
        self.attached_context = None;
    }

    pub fn compose_full_prompt(&self, text: &str) -> String {
        let trimmed = text.trim();
        match &self.attached_context {
            Some(ctx) if !ctx.is_empty() => {
                if trimmed.is_empty() {
                    ctx.clone()
                } else {
                    format!("{ctx}\n\n{trimmed}")
                }
            }
            _ => trimmed.to_string(),
        }
    }

    pub fn send_prompt(&mut self, text: &str) -> Option<String> {
        let full = self.compose_full_prompt(text);
        if full.is_empty() {
            return None;
        }
        let session_id = self
            .selected_session_id
            .clone()
            .or_else(|| self.sessions.first().map(|s| s.id.clone()))?;

        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) {
            session.turns.push(AgentTurn {
                is_user: true,
                sender: "나".to_string(),
                text: full.clone(),
                timestamp: "방금".to_string(),
            });
            session.preview = one_line_preview(&full, 80);
            session.status = "실행 중".to_string();
            session.status_badge = "run".to_string();
        }

        self.draft_prompt.clear();
        self.attached_context = None;
        self.status_note = Some("전송했습니다".to_string());
        Some(full)
    }
}

/// Deterministic sample sessions for a project when no server is connected.
pub fn sample_sessions_for(project: &str) -> Vec<AgentSession> {
    vec![
        AgentSession {
            id: format!("{project}-leader"),
            title: "아키텍처 및 구현 조율".to_string(),
            member: "ember-리더".to_string(),
            role: "리더".to_string(),
            agent: "Claude Code".to_string(),
            status: "승인 대기".to_string(),
            status_badge: "wait".to_string(),
            preview: "라우트를 연결했습니다. 통합 테스트를 실행해 상태를 확인합니다.".to_string(),
            turns: vec![
                AgentTurn {
                    is_user: true,
                    sender: "나".to_string(),
                    text: "에디터 에이전트 도구 창과 컨텍스트 연동을 구현해줘.".to_string(),
                    timestamp: "10분 전".to_string(),
                },
                AgentTurn {
                    is_user: false,
                    sender: "ember-리더".to_string(),
                    text: "라우트를 연결했습니다. 통합 테스트를 실행해 상태를 확인합니다.".to_string(),
                    timestamp: "8분 전".to_string(),
                },
            ],
        },
        AgentSession {
            id: format!("{project}-impl"),
            title: "에디터 도구 창 UI 구현".to_string(),
            member: "ember-구현".to_string(),
            role: "구현".to_string(),
            agent: "Codex".to_string(),
            status: "실행 중".to_string(),
            status_badge: "run".to_string(),
            preview: "에이전트 패널 뷰를 목업 스타일에 맞춰 렌더링하고 있습니다.".to_string(),
            turns: vec![
                AgentTurn {
                    is_user: true,
                    sender: "나".to_string(),
                    text: "editor_workbench와 연결할 패널을 만들어줘.".to_string(),
                    timestamp: "15분 전".to_string(),
                },
                AgentTurn {
                    is_user: false,
                    sender: "ember-구현".to_string(),
                    text: "에이전트 패널 뷰를 목업 스타일에 맞춰 렌더링하고 있습니다.".to_string(),
                    timestamp: "12분 전".to_string(),
                },
            ],
        },
        AgentSession {
            id: format!("{project}-design"),
            title: "아일랜드 패널 토큰 점검".to_string(),
            member: "ember-디자인".to_string(),
            role: "디자인".to_string(),
            agent: "Claude Code".to_string(),
            status: "대기".to_string(),
            status_badge: "ok".to_string(),
            preview: "패널의 여백과 색상 토큰을 검토했습니다.".to_string(),
            turns: vec![
                AgentTurn {
                    is_user: false,
                    sender: "ember-디자인".to_string(),
                    text: "패널의 여백과 색상 토큰을 검토했습니다.".to_string(),
                    timestamp: "1시간 전".to_string(),
                },
            ],
        },
    ]
}

/// Loads sessions from the live server client if connected, or uses sample sessions.
pub fn load_project_sessions(project: &str) -> Vec<AgentSession> {
    if let Some(services) = crate::services::try_services() {
        let loaded: Vec<AgentSession> = services.client.read(|st| {
            let empty_labels = std::collections::HashMap::new();
            let cx = crate::model::RowContext {
                account_labels: &empty_labels,
            };
            crate::model::project_sessions(st, project, &cx, false)
                .into_iter()
                .map(|r| {
                    let (status, badge) = match r.status {
                        LauncherStatus::Running => ("실행 중".to_string(), "run".to_string()),
                        LauncherStatus::WaitingForApproval => ("승인 대기".to_string(), "wait".to_string()),
                        LauncherStatus::Failed => ("실패".to_string(), "fail".to_string()),
                        _ => ("대기".to_string(), "ok".to_string()),
                    };
                    AgentSession {
                        id: r.id,
                        title: r.title,
                        member: r.agent.clone(),
                        role: "팀원".to_string(),
                        agent: r.agent,
                        status,
                        status_badge: badge,
                        preview: r.preview.unwrap_or_default(),
                        turns: Vec::new(),
                    }
                })
                .collect()
        });
        if !loaded.is_empty() {
            return loaded;
        }
    }
    sample_sessions_for(project)
}

/// Handle to the Agent tool state for the editor workbench.
#[derive(Clone)]
pub struct AgentTool {
    pub state: Arc<Mutex<AgentState>>,
    pub revision: SyncSignal<u64>,
}

impl AgentTool {
    pub fn new(project: &str, folder: &str, revision: SyncSignal<u64>) -> Self {
        let mut st = AgentState::new(project, folder);
        st.sessions = load_project_sessions(project);
        if let Some(first) = st.sessions.first() {
            st.selected_session_id = Some(first.id.clone());
        }
        Self {
            state: Arc::new(Mutex::new(st)),
            revision,
        }
    }

    pub fn snapshot(&self) -> AgentState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn bump(&self) {
        let mut rev = self.revision;
        *rev.write() += 1;
    }

    pub fn attach_context(&self, context: String) {
        self.state.lock().unwrap().attach_context(context);
        self.bump();
    }

    pub fn remove_context(&self) {
        self.state.lock().unwrap().remove_context();
        self.bump();
    }

    pub fn select_session(&self, id: &str) {
        self.state.lock().unwrap().select_session(id);
        self.bump();
    }

    pub fn set_draft(&self, text: &str) {
        self.state.lock().unwrap().draft_prompt = text.to_string();
        self.bump();
    }

    pub fn send_prompt(&self, text: &str) -> Option<String> {
        let (session_id, full_prompt) = {
            let mut st = self.state.lock().unwrap();
            let full = st.send_prompt(text)?;
            let id = st.selected_session_id.clone().unwrap_or_default();
            (id, full)
        };
        self.bump();

        if let Some(services) = crate::services::try_services() {
            let sid = session_id.clone();
            let msg = full_prompt.clone();
            services.spawn(async move {
                let s = crate::services::services();
                let _ = s.client.send_message(&sid, &msg).await;
            });
            services.drive_outbox(&session_id);
        }
        Some(full_prompt)
    }

    pub fn open_session(&self, id: &str) {
        let name = {
            let st = self.state.lock().unwrap();
            st.sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.member.clone())
                .unwrap_or_else(|| id.to_string())
        };

        if let Some(s) = crate::services::try_services() {
            s.open_session(id);
            s.ui.open_conversation(id);
        }
        crate::ui::cr::notice::post(format!("{name} 세션을 엽니다"));
    }

    pub fn refresh(&self) {
        let project = self.state.lock().unwrap().project.clone();
        let sessions = load_project_sessions(&project);
        {
            let mut st = self.state.lock().unwrap();
            st.sessions = sessions;
            if st.selected_session_id.is_none() {
                st.selected_session_id = st.sessions.first().map(|s| s.id.clone());
            }
            st.status_note = Some("세션 목록을 새로 고쳤습니다".to_string());
        }
        self.bump();
    }
}

pub mod view {
    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::ModifiersInteraction;
    use dioxus_compose::html::prelude::dioxus_elements::{Key, Modifiers};
    use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
    use dioxus_compose::html::prelude::*;

    use super::{AgentSession, AgentTurn};
    use crate::ui::editor_workbench::page::icon;
    use crate::ui::editor_workbench::Shared;

    const PANEL_STYLE: &str =
        "display:flex; flex-direction:column; height:100%; min-height:0; min-width:0; overflow:hidden; background:var(--surface);";
    const LIST_BOX: &str =
        "flex:0 0 auto; max-height:220px; overflow-y:auto; overflow-x:hidden; padding:var(--sp1); border-bottom:1px solid var(--line);";
    const ROW_STYLE: &str =
        "display:flex; align-items:flex-start; gap:var(--sp2); padding:var(--sp2); border-radius:var(--r-sm); cursor:pointer; width:100%; min-width:0; box-sizing:border-box;";
    const TRANSCRIPT_BOX: &str =
        "flex:1; min-height:80px; overflow-y:auto; overflow-x:hidden; padding:var(--sp2); display:flex; flex-direction:column; gap:var(--sp2); background:var(--surface-2);";
    const COMPOSER_BOX: &str =
        "display:flex; flex-direction:column; gap:var(--sp2); padding:var(--sp2); border-top:1px solid var(--line); background:var(--surface); flex:none;";
    const CONTEXT_CHIP: &str =
        "display:inline-flex; align-items:center; gap:6px; padding:2px 8px; border-radius:var(--pill); background:var(--sel); font-size:var(--fs-xs); font-family:var(--mono); width:fit-content; border:1px solid var(--line);";

    fn session_item(session: &AgentSession, is_selected: bool, s: &Shared) -> Element {
        let (sel_agent, open_agent) = (s.agent.clone(), s.agent.clone());
        let (sid, open_sid) = (session.id.clone(), session.id.clone());
        let (member, role) = (session.member.clone(), session.role.clone());
        let (title, preview, badge) = (session.title.clone(), session.preview.clone(), session.status_badge.clone());
        let dot_class = format!("ed-pdot {badge}");
        let row_class = if is_selected { "ed-prow sel" } else { "ed-prow" };

        rsx! {
            div {
                key: "{sid}",
                role: "listitem",
                class: "{row_class}",
                style: "{ROW_STYLE}",
                "aria-label": "{member} {role}",
                onclick: move |_| sel_agent.select_session(&sid),
                span { class: "{dot_class}", style: "margin-top:6px;", title: "{session.status}" }
                div { style: "flex:1; min-width:0; display:flex; flex-direction:column; gap:2px;",
                    div { style: "display:flex; align-items:center; gap:var(--sp2); width:100%;",
                        b { style: "font-size:var(--fs-sm); font-weight:var(--fw-semibold); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;", "{member}" }
                        span { style: "font-size:var(--fs-xs); color:var(--muted); white-space:nowrap;", "{role}" }
                    }
                    if !title.is_empty() {
                        div { style: "font-size:var(--fs-xs); color:var(--text-2); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;", "{title}" }
                    }
                    if !preview.is_empty() {
                        div { style: "font-size:var(--fs-xs); color:var(--muted); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;", "{preview}" }
                    }
                }
                button {
                    class: "ed-ib",
                    title: "세션 열기",
                    "aria-label": "{member} 세션 열기",
                    style: "margin-top:2px;",
                    onclick: move |e| {
                        e.stop_propagation();
                        open_agent.open_session(&open_sid);
                    },
                    {icon("spark")}
                }
            }
        }
    }

    fn turn_bubble(turn: &AgentTurn) -> Element {
        let is_user = turn.is_user;
        let bg = if is_user { "background:var(--ink); color:var(--ink-text);" } else { "background:var(--surface); color:var(--text); border:1px solid var(--line);" };
        let align = if is_user { "align-self:flex-end; max-width:85%;" } else { "align-self:flex-start; max-width:85%;" };
        let bubble_style = format!("{bg} {align} padding:6px 10px; border-radius:var(--r-sm); font-size:var(--fs-xs); word-break:break-word; white-space:pre-wrap;");

        rsx! {
            div { style: "{bubble_style}",
                div { style: "font-weight:var(--fw-medium); margin-bottom:2px; opacity:0.8;", "{turn.sender}" }
                div { "{turn.text}" }
            }
        }
    }

    pub fn panel(s: &Shared) -> Element {
        let mut stripes = s.stripes;
        let tool = s.agent.clone();
        let st = tool.snapshot();
        let selected_id = st.selected_session_id.clone();
        let selected_session = st.selected_session().cloned();
        let attached_context = st.attached_context.clone();
        let draft_prompt = st.draft_prompt.clone();
        let status_note = st.status_note.clone();

        let (refresh_tool, close_tool, input_tool, enter_tool, send_tool, open_tool) = (
            tool.clone(),
            tool.clone(),
            tool.clone(),
            tool.clone(),
            tool.clone(),
            tool.clone(),
        );
        let send_text = draft_prompt.clone();
        let enter_text = draft_prompt.clone();
        let target_sid = selected_id.clone().unwrap_or_default();

        rsx! {
            section { class: "ed-tw ed-left", style: "{PANEL_STYLE}", "aria-label": "에이전트",
                div { class: "ed-twh",
                    b { class: "ed-twt", "에이전트" }
                    div { class: "spacer" }
                    button {
                        class: "ed-ib",
                        title: "새로 고침",
                        "aria-label": "에이전트 세션 새로 고침",
                        onclick: move |_| refresh_tool.refresh(),
                        {icon("refresh")}
                    }
                    button {
                        class: "ed-ib",
                        title: "숨기기",
                        "aria-label": "에이전트 창 숨기기",
                        onclick: move |_| stripes.write().hide(crate::ui::editor::Side::Left),
                        {icon("close")}
                    }
                }

                div { class: "ed-mlab", style: "padding:6px var(--sp3) 2px;", "팀원 세션" }
                div { role: "list", style: "{LIST_BOX}", "aria-label": "팀원 세션 목록",
                    for session in &st.sessions {
                        {session_item(session, Some(&session.id) == selected_id.as_ref(), s)}
                    }
                }

                if let Some(session) = &selected_session {
                    div { class: "ed-mlab", style: "padding:6px var(--sp3) 2px; border-top:1px solid var(--line);", "{session.member}과의 대화" }
                    div { style: "{TRANSCRIPT_BOX}", role: "log", "aria-label": "대화 기록",
                        if session.turns.is_empty() {
                            div { style: "color:var(--muted); font-size:var(--fs-xs); text-align:center; padding:var(--sp4);", "대화 기록이 없습니다. 아래에서 메시지를 보내세요." }
                        } else {
                            for turn in &session.turns {
                                {turn_bubble(turn)}
                            }
                        }
                    }
                }

                div { style: "{COMPOSER_BOX}",
                    if let Some(ctx) = &attached_context {
                        div { style: "{CONTEXT_CHIP}",
                            span { style: "white-space:nowrap; overflow:hidden; text-overflow:ellipsis; max-width:200px;", "{ctx}" }
                            button {
                                class: "ed-tx",
                                title: "컨텍스트 제거",
                                "aria-label": "컨텍스트 제거",
                                onclick: move |_| close_tool.remove_context(),
                                {icon("close")}
                            }
                        }
                    }
                    textarea {
                        style: "width:100%; min-height:54px; max-height:120px; border:1px solid var(--line); border-radius:var(--r-sm); padding:var(--sp2); background:var(--surface-2); color:var(--text); font:inherit; font-size:var(--fs-xs); resize:vertical; outline:none; box-sizing:border-box;",
                        placeholder: "선택한 에이전트에게 묻기...",
                        value: "{draft_prompt}",
                        oninput: move |e| input_tool.set_draft(&e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && !e.modifiers().contains(Modifiers::SHIFT) {
                                e.stop_propagation();
                                enter_tool.send_prompt(&enter_text);
                            }
                        },
                    }
                    div { style: "display:flex; align-items:center; gap:var(--sp2); width:100%;",
                        if let Some(note) = &status_note {
                            span { style: "font-size:var(--fs-xs); color:var(--c-green); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;", "{note}" }
                        }
                        div { class: "spacer" }
                        button {
                            class: "ed-pj",
                            style: "border:1px solid var(--line); height:28px; padding:0 var(--sp3); font-size:var(--fs-xs);",
                            onclick: move |_| open_tool.open_session(&target_sid),
                            "세션 열기"
                        }
                        button {
                            class: "ed-pj",
                            style: "background:var(--ink); color:var(--ink-text); height:28px; padding:0 var(--sp3); font-weight:var(--fw-medium); font-size:var(--fs-xs);",
                            onclick: move |_| { send_tool.send_prompt(&send_text); },
                            "전송"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_file_and_selection_contexts_match_code_oss_chat() {
        assert_eq!(
            format_file_context("/Volumes/darkpyonix/ember/src/main.rs", "/Volumes/darkpyonix"),
            "#file:ember/src/main.rs"
        );
        assert_eq!(
            format_file_context("ember/src/main.rs", ""),
            "#file:ember/src/main.rs"
        );
        assert_eq!(
            format_selection_context("/Volumes/darkpyonix/ember/src/main.rs", "/Volumes/darkpyonix", 10, 25),
            "#selection:ember/src/main.rs:10-25"
        );
        // Inverted selection order is normalized
        assert_eq!(
            format_selection_context("ember/src/main.rs", "", 50, 20),
            "#selection:ember/src/main.rs:20-50"
        );
    }

    #[test]
    fn format_context_picks_selection_when_range_is_present() {
        assert_eq!(
            format_context("/app/lib.rs", "/app", Some((5, 12))),
            "#selection:lib.rs:5-12"
        );
        assert_eq!(
            format_context("/app/lib.rs", "/app", Some((5, 5))),
            "#file:lib.rs"
        );
        assert_eq!(
            format_context("/app/lib.rs", "/app", None),
            "#file:lib.rs"
        );
    }

    #[test]
    fn agent_state_selects_and_manages_attached_context() {
        let mut st = AgentState::new("test-proj", "/ws");
        assert!(!st.sessions.is_empty());
        assert_eq!(st.attached_context, None);

        st.attach_context("#file:src/lib.rs".to_string());
        assert_eq!(st.attached_context.as_deref(), Some("#file:src/lib.rs"));

        assert_eq!(
            st.compose_full_prompt("이 코드의 버그를 찾아줘"),
            "#file:src/lib.rs\n\n이 코드의 버그를 찾아줘"
        );

        st.remove_context();
        assert_eq!(st.attached_context, None);
        assert_eq!(st.compose_full_prompt("hello"), "hello");
    }

    #[test]
    fn agent_state_sends_prompt_and_records_turn() {
        let mut st = AgentState::new("test-proj", "/ws");
        let first_id = st.sessions.first().unwrap().id.clone();
        st.attach_context("#selection:src/lib.rs:1-10".to_string());

        let sent = st.send_prompt("리팩터링 제안해줘");
        assert_eq!(
            sent.as_deref(),
            Some("#selection:src/lib.rs:1-10\n\n리팩터링 제안해줘")
        );
        assert_eq!(st.attached_context, None);
        assert_eq!(st.draft_prompt, "");
        assert_eq!(st.status_note.as_deref(), Some("전송했습니다"));

        let session = st.sessions.iter().find(|s| s.id == first_id).unwrap();
        assert!(session.turns.iter().any(|t| t.text.contains("리팩터링 제안해줘")));
        assert_eq!(session.status, "실행 중");
        assert_eq!(session.status_badge, "run");
    }
}
