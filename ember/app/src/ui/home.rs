//! The Home screen (FR-L11, FR-L15, FR-L16, FR-L23, `docs/DESIGN.md` section 6.1): a chat-first
//! top (greeting, the large composer, project, model, mode and computer chips, suggestion
//! chips) and the dashboard below it.
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer. The components in the second half only hand them to dioxus-compose.
//!
//! Only numbers the client holds are shown. Tokens, cost and approval wait are not in the
//! client's state yet (FR-U7 has no endpoint), so those tiles say [`UNAVAILABLE`] and carry a
//! TODO, never an estimate.

use ember_client::state::{LauncherStatus, Reachability, State};

use crate::model;

/// What a tile shows while the server does not provide its data.
pub const UNAVAILABLE: &str = "준비 중입니다";

/// The built-in project that holds Home chats by default (FR-L13).
pub const DEFAULT_PROJECT: &str = "기본 프로젝트";

pub const GREETING: &str = "무엇을 도와드릴까요?";
pub const COMPOSER_PLACEHOLDER: &str =
    "무엇이든 물어보세요. 새 대화는 고른 프로젝트의 팀원이 됩니다.";

// ---------------------------------------------------------------------------------------------
// Harnesses, models and modes (FR-L15, FR-L16)
// ---------------------------------------------------------------------------------------------

/// The harnesses the pickers offer. The server's agent kind is [`Harness::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Gemini,
}

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::ClaudeCode, Harness::Codex, Harness::Gemini];

    /// The agent kind `POST /sessions` takes.
    /// TODO(Gemini): the server's built-in Google harness is Antigravity (`agy`); a Gemini CLI
    /// agent would be an ACP agent. Verify before FR-L15 is marked done (AGENTS.md rule 4).
    pub fn kind(self) -> &'static str {
        todo!()
    }

    pub fn label(self) -> &'static str {
        todo!()
    }

    pub fn from_kind(kind: &str) -> Option<Harness> {
        todo!()
    }

    /// Whether the harness has an effort setting (Gemini has none: one row per model).
    pub fn has_effort(self) -> bool {
        todo!()
    }
}

/// The permission modes (`docs/DESIGN.md` 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermMode {
    /// Every tool call asks.
    Ask,
    /// File edits are approved, commands still ask.
    Edits,
    /// Everything is approved (FR-A5 approve-all).
    Bypass,
    /// Plan only, no changes.
    Plan,
}

impl PermMode {
    pub fn label(self) -> &'static str {
        todo!()
    }

    /// Bypass is drawn in the warning colour so it is never chosen by accident.
    pub fn is_bypass(self) -> bool {
        todo!()
    }
}

/// The modes a harness supports, in menu order. [provisional] until each CLI is verified
/// (FR-L16): Claude Code has the four, Codex and Gemini three.
pub fn modes_for(h: Harness) -> &'static [PermMode] {
    todo!()
}

pub fn default_mode(_h: Harness) -> PermMode {
    todo!()
}

/// The mode after the harness changed: kept when the new harness supports it, else the new
/// harness's default (FR-L16).
pub fn mode_after_harness_change(h: Harness, current: PermMode) -> PermMode {
    todo!()
}

/// One selectable row of the model picker: a model and one effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelOption {
    pub harness: Harness,
    /// The id handed to the agent (`CreateSession.model`).
    pub model: &'static str,
    pub name: &'static str,
    pub effort: Option<&'static str>,
}

impl ModelOption {
    /// "Opus 5.5 (High)", the row title; a harness without effort shows the name alone.
    pub fn row_title(&self) -> String {
        todo!()
    }

    /// "Claude Code · Opus 5.5 · High", the chip text.
    pub fn chip_label(&self) -> String {
        todo!()
    }
}

/// (id, display name) per harness, default model first.
/// TODO(server): the server exposes no model list (`GET /agents` reports only kind, installed
/// and version), so this is a local static list taken from the design mockup's demo data
/// (`.scratch/design-mockup/data.js`). The ids are CLI aliases and must be checked against each
/// CLI (AGENTS.md rule 4). Replace with the server's list when `FR-L17` profiles land.
const CLAUDE_MODELS: [(&str, &str); 3] = [
    ("opus", "Opus 5.5"),
    ("sonnet", "Sonnet 5.5"),
    ("haiku", "Haiku 5.5"),
];
const CODEX_MODELS: [(&str, &str); 3] = [
    ("gpt-5", "GPT-5"),
    ("gpt-5-codex", "GPT-5 Codex"),
    ("gpt-5-mini", "GPT-5 mini"),
];
const GEMINI_MODELS: [(&str, &str); 2] = [
    ("gemini-2.5-pro", "Gemini Pro"),
    ("gemini-2.5-flash", "Gemini Flash"),
];

/// The efforts of a harness that has them, lowest first.
pub const EFFORTS: [&str; 3] = ["Low", "Medium", "High"];

fn models_of(h: Harness) -> &'static [(&'static str, &'static str)] {
    todo!()
}

/// Every row of the picker: harness by harness, model by model, one row per effort.
pub fn model_catalog() -> Vec<ModelOption> {
    todo!()
}

/// The harness's default row: its first model at its default effort (Medium; Claude Code High).
pub fn default_option(h: Harness) -> ModelOption {
    todo!()
}

/// The rows to list: one harness when the rail picked one, else every harness; a non-empty
/// query searches across every harness regardless of the rail (FR-L15). Matching ignores case
/// and looks at model name, harness name and effort.
pub fn filter_models(
    all: &[ModelOption],
    harness: Option<Harness>,
    query: &str,
) -> Vec<ModelOption> {
    todo!()
}

/// "Ctrl+1" to "Ctrl+9" for the first nine visible rows, nothing after.
pub fn shortcut_label(row: usize) -> Option<String> {
    todo!()
}

/// Why a harness cannot be used now: not installed on the main server, or its account hit a
/// limit. `None` when usable. `agents` is `(kind, installed)` from `GET /agents`; empty means
/// the server has not answered yet, which blocks nothing. `limited` lists the kinds whose
/// default account is limited (`GET /accounts`).
pub fn harness_block(
    h: Harness,
    agents: &[(String, bool)],
    limited: &[String],
) -> Option<&'static str> {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// The composer's choices (FR-L11)
// ---------------------------------------------------------------------------------------------

/// What the chips hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub project: String,
    pub option: ModelOption,
    pub mode: PermMode,
    /// Computer id; `None` is the main server.
    pub computer: Option<String>,
}

impl Selection {
    /// The first view: 기본 프로젝트, Claude Code's default, ask mode, the main server.
    pub fn initial() -> Selection {
        todo!()
    }

    /// Choosing a row of the picker. A changed harness moves the mode chip to that harness
    /// (FR-L16); the same harness keeps the mode.
    pub fn with_option(&self, option: ModelOption) -> Selection {
        todo!()
    }

    /// Choosing another harness resets model and effort to its defaults (FR-L15).
    pub fn with_harness(&self, h: Harness) -> Selection {
        todo!()
    }

    pub fn with_mode(&self, mode: PermMode) -> Selection {
        todo!()
    }
}

/// The project chip's options: 기본 프로젝트 first, then the existing projects (most recent
/// first as given), without duplicates.
pub fn project_options(existing: &[String]) -> Vec<String> {
    todo!()
}

/// The computer chip's text: the computer's name, or 이 서버 for the main server.
pub fn computer_chip_text(selected: Option<&str>, computers: &[(String, String)]) -> String {
    todo!()
}

/// A suggestion chip fills the composer with its text, replacing a draft.
pub const SUGGESTIONS: [&str; 3] = [
    "develop CI 실패를 요약해 주세요",
    "열린 PR을 리뷰해 주세요",
    "이번 주 사용량을 정리해 주세요",
];

pub fn apply_suggestion(_draft: &str, suggestion: &str) -> String {
    todo!()
}

/// Whether send does anything: some text, and no create already running.
pub fn can_send(draft: &str, busy: bool) -> bool {
    todo!()
}

/// The folder a new session starts in. The server needs one (`CreateSession.cwd`) and Home has
/// no folder field, so the order is: what was last used in the project, the project's latest
/// session, then the latest session anywhere. `None` means Home cannot decide and the new-session
/// dialog takes over.
pub fn resolve_cwd(
    remembered: Option<&str>,
    latest_in_project: Option<&str>,
    latest_any: Option<&str>,
) -> Option<String> {
    todo!()
}

/// The session title taken from the first message.
pub fn title_from_message(text: &str) -> String {
    todo!()
}

/// Whether the new session must be switched to approve-all right after it is created: only
/// bypass (FR-L16, FR-A5). Plan and edits-only need CLI flags the create call does not carry.
/// TODO(server): `POST /sessions` has no mode field; ask and edits start in the agent's own
/// default, plan likewise, until the server takes a permission mode.
pub fn needs_approve_all(mode: PermMode) -> bool {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// Dashboard (FR-L23)
// ---------------------------------------------------------------------------------------------

/// Session counts by what the user cares about, archived sessions left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub running: usize,
    pub waiting: usize,
    pub unread: usize,
    pub done: usize,
    pub failed: usize,
    pub idle: usize,
}

impl StatusCounts {
    pub fn add(&mut self, s: LauncherStatus) {
        todo!()
    }

    pub fn total(&self) -> usize {
        todo!()
    }
}

pub fn status_counts(state: &State) -> StatusCounts {
    todo!()
}

/// Success rate in percent: sessions that finished over sessions that finished or failed.
/// `None` while none has ended, so the tile says so instead of 0 or 100.
pub fn success_percent(finished: usize, failed: usize) -> Option<u32> {
    todo!()
}

/// (online, total) over the registered computers.
pub fn computers_online(state: &State) -> (usize, usize) {
    todo!()
}

/// One big number of the KPI row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kpi {
    pub label: &'static str,
    pub value: String,
    pub unit: &'static str,
    /// A second line (waiting count, ...), when there is one.
    pub note: Option<String>,
    /// `false`: the client has no data; `value` is [`UNAVAILABLE`].
    pub available: bool,
}

/// The four KPIs: running members, online computers, success rate, approval wait.
/// Approval wait shows how many are waiting now (the client has that) but not the median
/// (TODO(server): no approval timing in the client's state).
pub fn kpis(counts: &StatusCounts, online: usize, computers: usize) -> [Kpi; 4] {
    todo!()
}

/// One bar of the usage-by-harness tile: sessions per harness. This is a session count, not
/// token usage; tokens are not in the client yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessBar {
    pub label: String,
    pub sessions: usize,
}

pub fn harness_usage(state: &State) -> Vec<HarnessBar> {
    todo!()
}

/// A bar's rectangle inside a chart `width` by `height` dp: `(x, y, w, h)`, bars side by side
/// with `gap` between, heights relative to the largest value, at least 2 dp for a non-zero
/// value so it stays visible. All zero gives zero-height bars.
pub fn bar_rects(values: &[usize], width: f32, height: f32, gap: f32) -> Vec<(f32, f32, f32, f32)> {
    todo!()
}

/// Ring segments as `(start_degrees, sweep_degrees)` from 12 o'clock clockwise, one per count;
/// a zero count has a zero sweep. All zero gives no segments.
pub fn ring_segments(counts: &[usize]) -> Vec<(f32, f32)> {
    todo!()
}

/// A working member for the crew row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMember {
    pub id: String,
    pub title: String,
    pub project: String,
    pub agent: String,
    pub waiting: bool,
}

/// Running and waiting sessions, waiting first, then most recently active; at most `max`.
pub fn crew(state: &State, max: usize) -> Vec<CrewMember> {
    todo!()
}

/// The text under the crew row: "컴퓨터 2/3 온라인".
pub fn computers_line(online: usize, total: usize) -> String {
    todo!()
}

/// Empty states, in 합니다체.
pub const EMPTY_CREW: &str = "지금 일하는 팀원이 없습니다";
pub const EMPTY_PROJECTS: &str = "아직 프로젝트가 없습니다";
pub const EMPTY_HARNESS_USAGE: &str = "아직 세션이 없습니다";

/// A project tile: its name and how many members are in which state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTile {
    pub name: String,
    pub total: usize,
    pub running: usize,
    pub waiting: usize,
}

pub fn project_tiles(state: &State, max: usize) -> Vec<ProjectTile> {
    todo!()
}

/// "팀원 3명 · 실행 1 · 대기 1", the tile's second line.
pub fn project_tile_line(t: &ProjectTile) -> String {
    todo!()
}

// ---- components follow -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- FR-L16: modes per harness ----

    #[test]
    fn fr_l16_claude_code_has_four_modes_codex_and_gemini_three() {
        assert_eq!(modes_for(Harness::ClaudeCode).len(), 4);
        assert_eq!(modes_for(Harness::Codex).len(), 3);
        assert_eq!(modes_for(Harness::Gemini).len(), 3);
        assert!(modes_for(Harness::ClaudeCode).contains(&PermMode::Plan));
        assert!(!modes_for(Harness::Codex).contains(&PermMode::Plan));
        assert!(!modes_for(Harness::Gemini).contains(&PermMode::Plan));
    }

    #[test]
    fn fr_l16_every_harness_lists_its_default_and_bypass_last() {
        for h in Harness::ALL {
            let m = modes_for(h);
            assert!(m.contains(&default_mode(h)));
            assert_eq!(m.last(), Some(&PermMode::Bypass));
            assert_eq!(m.iter().filter(|x| x.is_bypass()).count(), 1);
        }
    }

    #[test]
    fn fr_l16_unsupported_mode_falls_back_to_the_harness_default() {
        assert_eq!(
            mode_after_harness_change(Harness::Codex, PermMode::Plan),
            PermMode::Ask
        );
        assert_eq!(
            mode_after_harness_change(Harness::Codex, PermMode::Bypass),
            PermMode::Bypass
        );
        assert_eq!(
            mode_after_harness_change(Harness::ClaudeCode, PermMode::Plan),
            PermMode::Plan
        );
    }

    #[test]
    fn fr_l16_mode_labels() {
        assert_eq!(PermMode::Ask.label(), "명령마다 확인");
        assert_eq!(PermMode::Edits.label(), "자동 승인(편집만)");
        assert_eq!(PermMode::Bypass.label(), "바이패스(모두 허용)");
        assert_eq!(PermMode::Plan.label(), "계획 모드");
    }

    #[test]
    fn fr_l16_only_bypass_switches_approve_all_on() {
        assert!(needs_approve_all(PermMode::Bypass));
        for m in [PermMode::Ask, PermMode::Edits, PermMode::Plan] {
            assert!(!needs_approve_all(m));
        }
    }

    // ---- FR-L15: model picker ----

    #[test]
    fn fr_l15_catalog_has_one_row_per_model_and_effort() {
        let all = model_catalog();
        let claude = all
            .iter()
            .filter(|o| o.harness == Harness::ClaudeCode)
            .count();
        assert_eq!(claude, 3 * EFFORTS.len());
        // Gemini has no effort: one row per model.
        let gemini: Vec<_> = all
            .iter()
            .filter(|o| o.harness == Harness::Gemini)
            .collect();
        assert_eq!(gemini.len(), 2);
        assert!(gemini.iter().all(|o| o.effort.is_none()));
    }

    #[test]
    fn fr_l15_default_option_is_in_the_catalog() {
        let all = model_catalog();
        for h in Harness::ALL {
            assert!(all.contains(&default_option(h)), "{h:?}");
        }
    }

    #[test]
    fn fr_l15_chip_and_row_text() {
        let o = default_option(Harness::ClaudeCode);
        assert_eq!(o.chip_label(), "Claude Code · Opus 5.5 · High");
        assert_eq!(o.row_title(), "Opus 5.5 (High)");
        let g = default_option(Harness::Gemini);
        assert_eq!(g.chip_label(), "Gemini · Gemini Pro");
        assert_eq!(g.row_title(), "Gemini Pro");
    }

    #[test]
    fn fr_l15_rail_lists_only_that_harness() {
        let all = model_catalog();
        let rows = filter_models(&all, Some(Harness::Codex), "");
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|o| o.harness == Harness::Codex));
        assert_eq!(filter_models(&all, None, "").len(), all.len());
    }

    #[test]
    fn fr_l15_search_finds_models_across_harnesses_ignoring_the_rail() {
        let all = model_catalog();
        let rows = filter_models(&all, Some(Harness::Codex), "opus");
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|o| o.harness == Harness::ClaudeCode));
        // Case-insensitive; several words must all match.
        let rows = filter_models(&all, None, "gpt-5-codex HIGH");
        assert!(!rows.is_empty());
        assert!(rows
            .iter()
            .all(|o| o.model == "gpt-5-codex" && o.effort == Some("High")));
        assert!(filter_models(&all, None, "zzz").is_empty());
        // A blank query is not a search.
        assert_eq!(filter_models(&all, Some(Harness::Gemini), "  ").len(), 2);
    }

    #[test]
    fn fr_l15_shortcuts_are_ctrl_1_to_9_for_the_first_nine_rows() {
        assert_eq!(shortcut_label(0).as_deref(), Some("Ctrl+1"));
        assert_eq!(shortcut_label(8).as_deref(), Some("Ctrl+9"));
        assert_eq!(shortcut_label(9), None);
    }

    #[test]
    fn fr_l15_changing_harness_resets_model_and_effort_to_its_defaults() {
        let s = Selection::initial().with_option(ModelOption {
            harness: Harness::ClaudeCode,
            model: "haiku",
            name: "Haiku 5.5",
            effort: Some("Low"),
        });
        let t = s.with_harness(Harness::Codex);
        assert_eq!(t.option, default_option(Harness::Codex));
        // The same harness keeps the row.
        assert_eq!(s.with_harness(Harness::ClaudeCode), s);
    }

    #[test]
    fn fr_l15_blocked_harness_is_dimmed_with_a_reason() {
        let agents = vec![("claude-code".to_string(), true), ("codex".to_string(), false)];
        assert_eq!(harness_block(Harness::ClaudeCode, &agents, &[]), None);
        assert!(harness_block(Harness::Codex, &agents, &[]).is_some());
        // Not listed at all: not installed.
        assert!(harness_block(Harness::Gemini, &agents, &[]).is_some());
        let limited = vec!["claude-code".to_string()];
        assert!(harness_block(Harness::ClaudeCode, &agents, &limited).is_some());
    }

    #[test]
    fn fr_l15_nothing_is_blocked_before_the_server_answers() {
        for h in Harness::ALL {
            assert_eq!(harness_block(h, &[], &[]), None);
        }
    }

    #[test]
    fn fr_l15_harness_kinds_round_trip() {
        for h in Harness::ALL {
            assert_eq!(Harness::from_kind(h.kind()), Some(h));
        }
        assert_eq!(Harness::from_kind("nope"), None);
    }

    // ---- FR-L11: chips, suggestions, send ----

    #[test]
    fn fr_l11_first_view_is_the_default_project_and_claude_code() {
        let s = Selection::initial();
        assert_eq!(s.project, "기본 프로젝트");
        assert_eq!(s.option.harness, Harness::ClaudeCode);
        assert_eq!(s.mode, PermMode::Ask);
        assert_eq!(s.computer, None);
    }

    #[test]
    fn fr_l11_switching_harness_moves_the_mode_chip() {
        let s = Selection::initial().with_mode(PermMode::Plan);
        assert_eq!(s.mode, PermMode::Plan);
        let t = s.with_harness(Harness::Codex);
        assert_eq!(t.mode, PermMode::Ask);
        // A mode the harness lacks cannot be chosen.
        assert_eq!(t.with_mode(PermMode::Plan).mode, PermMode::Ask);
        // Bypass survives the switch.
        let b = Selection::initial().with_mode(PermMode::Bypass);
        assert_eq!(b.with_harness(Harness::Gemini).mode, PermMode::Bypass);
    }

    #[test]
    fn fr_l11_project_chip_lists_the_default_first_without_duplicates() {
        let existing = vec![
            "ember".to_string(),
            "기본 프로젝트".to_string(),
            "core".to_string(),
            "ember".to_string(),
        ];
        assert_eq!(
            project_options(&existing),
            ["기본 프로젝트", "ember", "core"]
        );
        assert_eq!(project_options(&[]), ["기본 프로젝트"]);
    }

    #[test]
    fn fr_l11_computer_chip_names_the_computer_or_the_main_server() {
        let cs = vec![("c1".to_string(), "mac-mini".to_string())];
        assert_eq!(computer_chip_text(Some("c1"), &cs), "mac-mini");
        assert_eq!(computer_chip_text(None, &cs), "이 서버");
        assert_eq!(computer_chip_text(Some("gone"), &cs), "이 서버");
    }

    #[test]
    fn fr_l11_a_suggestion_replaces_the_draft() {
        assert_eq!(apply_suggestion("abc", SUGGESTIONS[1]), SUGGESTIONS[1]);
        assert_eq!(SUGGESTIONS.len(), 3);
    }

    #[test]
    fn fr_l11_send_needs_text_and_no_create_in_flight() {
        assert!(can_send("hello", false));
        assert!(!can_send("   \n", false));
        assert!(!can_send("hello", true));
        assert!(!can_send("", false));
    }

    #[test]
    fn fr_l11_cwd_falls_back_through_remembered_project_then_any() {
        assert_eq!(resolve_cwd(Some("/a"), Some("/b"), Some("/c")).as_deref(), Some("/a"));
        assert_eq!(resolve_cwd(None, Some("/b"), Some("/c")).as_deref(), Some("/b"));
        assert_eq!(resolve_cwd(Some(" "), None, Some("/c")).as_deref(), Some("/c"));
        assert_eq!(resolve_cwd(None, None, None), None);
    }

    #[test]
    fn fr_l11_title_is_the_first_line_of_the_message() {
        assert_eq!(title_from_message("고쳐 주세요\n자세히"), "고쳐 주세요\u{2026}");
        assert_eq!(title_from_message("짧은 말"), "짧은 말");
    }

    // ---- FR-L23: dashboard ----

    #[test]
    fn fr_l23_success_percent() {
        assert_eq!(success_percent(3, 1), Some(75));
        assert_eq!(success_percent(0, 2), Some(0));
        assert_eq!(success_percent(5, 0), Some(100));
        assert_eq!(success_percent(0, 0), None);
    }

    #[test]
    fn fr_l23_status_counts_total_and_buckets() {
        let mut c = StatusCounts::default();
        for s in [
            LauncherStatus::Running,
            LauncherStatus::Running,
            LauncherStatus::WaitingForApproval,
            LauncherStatus::FinishedUnread,
            LauncherStatus::Finished,
            LauncherStatus::Failed,
            LauncherStatus::Idle,
            LauncherStatus::Unknown,
        ] {
            c.add(s);
        }
        assert_eq!((c.running, c.waiting, c.unread, c.done, c.failed, c.idle), (2, 1, 1, 1, 1, 2));
        assert_eq!(c.total(), 8);
    }

    #[test]
    fn fr_l23_kpis_use_only_what_the_client_has() {
        let c = StatusCounts {
            running: 2,
            waiting: 1,
            done: 3,
            failed: 1,
            ..Default::default()
        };
        let k = kpis(&c, 2, 3);
        assert_eq!((k[0].value.as_str(), k[0].unit), ("2", "명"));
        assert_eq!(k[0].note.as_deref(), Some("대기 1"));
        assert_eq!(k[1].value, "2/3");
        assert_eq!((k[2].value.as_str(), k[2].unit), ("75", "%"));
        assert!(k.iter().take(3).all(|x| x.available));
        // The median approval wait is not in the client: no number, a note and the state.
        assert_eq!(k[3].value, UNAVAILABLE);
        assert!(!k[3].available);
        assert_eq!(k[3].note.as_deref(), Some("지금 1건 대기"));
    }

    #[test]
    fn fr_l23_success_rate_with_nothing_ended_is_unavailable_not_zero() {
        let k = kpis(&StatusCounts::default(), 0, 0);
        assert!(!k[2].available);
        assert_ne!(k[2].value, "0");
        assert_eq!(k[1].value, "0/0");
    }

    #[test]
    fn fr_l23_bars_scale_to_the_largest_and_never_vanish() {
        let r = bar_rects(&[4, 2, 1, 0], 100.0, 40.0, 4.0);
        assert_eq!(r.len(), 4);
        // Largest fills the height; its top is 0.
        assert_eq!((r[0].1, r[0].3), (0.0, 40.0));
        assert_eq!(r[1].3, 20.0);
        assert!(r[2].3 >= 2.0);
        assert_eq!(r[3].3, 0.0);
        // Left to right, gap apart, inside the width.
        assert!(r[1].0 > r[0].0 + r[0].2);
        assert!(r[3].0 + r[3].2 <= 100.0 + 0.01);
        assert!(bar_rects(&[], 100.0, 40.0, 4.0).is_empty());
        assert!(bar_rects(&[0, 0], 100.0, 40.0, 4.0).iter().all(|b| b.3 == 0.0));
    }

    #[test]
    fn fr_l23_ring_segments_start_at_twelve_and_cover_the_circle() {
        let s = ring_segments(&[1, 1, 2]);
        assert_eq!(s[0].0, -90.0);
        assert_eq!(s[0].1, 90.0);
        assert_eq!(s[1].0, 0.0);
        assert_eq!(s[2].1, 180.0);
        let sum: f32 = s.iter().map(|x| x.1).sum();
        assert!((sum - 360.0).abs() < 0.01);
        assert!(ring_segments(&[0, 0]).is_empty());
        assert_eq!(ring_segments(&[0, 3])[0].1, 0.0);
    }

    #[test]
    fn fr_l23_texts() {
        assert_eq!(computers_line(2, 3), "컴퓨터 2/3 온라인");
        let t = ProjectTile {
            name: "ember".into(),
            total: 3,
            running: 1,
            waiting: 1,
        };
        assert_eq!(project_tile_line(&t), "팀원 3명 · 실행 1 · 대기 1");
    }

    // ---- NFR-L4 ----

    #[test]
    fn nfr_l4_home_text_has_no_plain_da_endings() {
        let mut strings: Vec<String> = vec![
            UNAVAILABLE,
            DEFAULT_PROJECT,
            GREETING,
            COMPOSER_PLACEHOLDER,
            EMPTY_CREW,
            EMPTY_PROJECTS,
            EMPTY_HARNESS_USAGE,
        ]
        .into_iter()
        .map(String::from)
        .collect();
        strings.extend(SUGGESTIONS.iter().map(|s| s.to_string()));
        for h in Harness::ALL {
            for m in modes_for(h) {
                strings.push(m.label().to_string());
            }
        }
        let c = StatusCounts::default();
        for k in kpis(&c, 0, 0) {
            strings.push(k.label.to_string());
            strings.push(k.value.clone());
            strings.extend(k.note.clone());
        }
        strings.push(computers_line(1, 2));
        for r in [
            harness_block(Harness::Codex, &[("x".into(), true)], &[]),
            harness_block(Harness::Codex, &[], &["codex".into()]),
        ]
        .into_iter()
        .flatten()
        {
            strings.push(r.to_string());
        }
        for s in strings {
            assert!(
                !s.ends_with('다') || s.ends_with("입니다") || s.ends_with("합니다") || s.ends_with("습니다"),
                "{s}"
            );
        }
    }

    #[test]
    fn no_em_dash_in_home_strings() {
        for s in [UNAVAILABLE, GREETING, COMPOSER_PLACEHOLDER] {
            assert!(!s.contains('\u{2014}'));
        }
    }
}
