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

use dioxus_compose::prelude::*;

use ember_client::wire::DailyUsage;
use ember_client::state::{LauncherStatus, Reachability, State};

use super::components::*;
use super::theme::*;
use super::components::ButtonKind;
use crate::model;
use crate::prefs::LastUsed;
use crate::server::CreateSession;
use crate::services::{run, services};
use crate::ui::shell::NavMenu;
use crate::ui::{Route, use_ui};

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
    /// Gemini CLI is not Antigravity (`agy`). The server has no Gemini kind yet (a Gemini CLI
    /// agent would be a configured ACP agent), so `gemini` is never sent: [`harness_block`]
    /// disables the Gemini row with "준비 중입니다".
    pub fn kind(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Gemini => "Gemini",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.kind() == kind)
    }

    /// Whether the harness has an effort setting (Gemini has none: one row per model).
    pub fn has_effort(self) -> bool {
        !matches!(self, Harness::Gemini)
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
        match self {
            PermMode::Ask => "명령마다 확인",
            PermMode::Edits => "자동 승인(편집만)",
            PermMode::Bypass => "바이패스(모두 허용)",
            PermMode::Plan => "계획 모드",
        }
    }

    /// Bypass is drawn in the warning colour so it is never chosen by accident.
    pub fn is_bypass(self) -> bool {
        self == PermMode::Bypass
    }
}

/// The modes a harness supports, in menu order. [provisional] until each CLI is verified
/// (FR-L16): Claude Code has the four, Codex and Gemini three.
pub fn modes_for(h: Harness) -> &'static [PermMode] {
    match h {
        Harness::ClaudeCode => &[
            PermMode::Ask,
            PermMode::Edits,
            PermMode::Plan,
            PermMode::Bypass,
        ],
        Harness::Codex | Harness::Gemini => &[PermMode::Ask, PermMode::Edits, PermMode::Bypass],
    }
}

pub fn default_mode(_h: Harness) -> PermMode {
    PermMode::Ask
}

/// The mode after the harness changed: kept when the new harness supports it, else the new
/// harness's default (FR-L16).
pub fn mode_after_harness_change(h: Harness, current: PermMode) -> PermMode {
    if modes_for(h).contains(&current) {
        current
    } else {
        default_mode(h)
    }
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
        match self.effort {
            Some(e) => format!("{} ({})", self.name, e),
            None => self.name.to_string(),
        }
    }

    /// "Claude Code · Opus 5.5 · High", the chip text.
    pub fn chip_label(&self) -> String {
        match self.effort {
            Some(e) => format!("{} · {} · {}", self.harness.label(), self.name, e),
            None => format!("{} · {}", self.harness.label(), self.name),
        }
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
    match h {
        Harness::ClaudeCode => &CLAUDE_MODELS,
        Harness::Codex => &CODEX_MODELS,
        Harness::Gemini => &GEMINI_MODELS,
    }
}

/// Every row of the picker: harness by harness, model by model, one row per effort.
pub fn model_catalog() -> Vec<ModelOption> {
    let mut out = Vec::new();
    for h in Harness::ALL {
        for (model, name) in models_of(h) {
            if h.has_effort() {
                for e in EFFORTS {
                    out.push(ModelOption {
                        harness: h,
                        model,
                        name,
                        effort: Some(e),
                    });
                }
            } else {
                out.push(ModelOption {
                    harness: h,
                    model,
                    name,
                    effort: None,
                });
            }
        }
    }
    out
}

/// The harness's default row: its first model at its default effort (Medium; Claude Code High).
pub fn default_option(h: Harness) -> ModelOption {
    let (model, name) = models_of(h)[0];
    let effort = match h {
        Harness::ClaudeCode => Some("High"),
        Harness::Codex => Some("Medium"),
        Harness::Gemini => None,
    };
    ModelOption {
        harness: h,
        model,
        name,
        effort,
    }
}

/// The rows to list: one harness when the rail picked one, else every harness; a non-empty
/// query searches across every harness regardless of the rail (FR-L15). Matching ignores case
/// and looks at model name, harness name and effort.
pub fn filter_models(
    all: &[ModelOption],
    harness: Option<Harness>,
    query: &str,
) -> Vec<ModelOption> {
    let q = query.trim().to_lowercase();
    all.iter()
        .filter(|o| {
            if q.is_empty() {
                harness.is_none_or(|h| o.harness == h)
            } else {
                let hay = format!(
                    "{} {} {} {}",
                    o.name,
                    o.model,
                    o.harness.label(),
                    o.effort.unwrap_or("")
                )
                .to_lowercase();
                q.split_whitespace().all(|w| hay.contains(w))
            }
        })
        .copied()
        .collect()
}

/// "Ctrl+1" to "Ctrl+9" for the first nine visible rows, nothing after.
pub fn shortcut_label(row: usize) -> Option<String> {
    (row < 9).then(|| format!("Ctrl+{}", row + 1))
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
    if h == Harness::Gemini {
        return Some("준비 중입니다");
    }
    if !agents.is_empty() {
        let installed = agents.iter().any(|(k, i)| k == h.kind() && *i);
        if !installed {
            return Some("이 컴퓨터에 설치되어 있지 않습니다");
        }
    }
    if limited.iter().any(|k| k == h.kind()) {
        return Some("계정의 사용 한도에 도달했습니다");
    }
    None
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
        Selection {
            project: DEFAULT_PROJECT.to_string(),
            option: default_option(Harness::ClaudeCode),
            mode: default_mode(Harness::ClaudeCode),
            computer: None,
        }
    }

    /// Choosing a row of the picker. A changed harness moves the mode chip to that harness
    /// (FR-L16); the same harness keeps the mode.
    pub fn with_option(&self, option: ModelOption) -> Selection {
        Selection {
            mode: mode_after_harness_change(option.harness, self.mode),
            option,
            ..self.clone()
        }
    }

    /// Choosing another harness resets model and effort to its defaults (FR-L15).
    pub fn with_harness(&self, h: Harness) -> Selection {
        if h == self.option.harness {
            return self.clone();
        }
        self.with_option(default_option(h))
    }

    pub fn with_mode(&self, mode: PermMode) -> Selection {
        if modes_for(self.option.harness).contains(&mode) {
            Selection {
                mode,
                ..self.clone()
            }
        } else {
            self.clone()
        }
    }
}

/// The project chip's options: 기본 프로젝트 first, then the existing projects (most recent
/// first as given), without duplicates.
pub fn project_options(existing: &[String]) -> Vec<String> {
    let mut out = vec![DEFAULT_PROJECT.to_string()];
    for p in existing {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// The computer chip's text: the computer's name, or 이 서버 for the main server.
pub fn computer_chip_text(selected: Option<&str>, computers: &[(String, String)]) -> String {
    selected
        .and_then(|id| computers.iter().find(|(cid, _)| cid == id))
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| "이 서버".to_string())
}

/// A suggestion chip fills the composer with its text, replacing a draft.
pub const SUGGESTIONS: [&str; 3] = [
    "develop CI 실패를 요약해 주세요",
    "열린 PR을 리뷰해 주세요",
    "이번 주 사용량을 정리해 주세요",
];

pub fn apply_suggestion(_draft: &str, suggestion: &str) -> String {
    suggestion.to_string()
}

/// Window width (dp) under which the Home composer puts the mode and computer chips and the send
/// button on a second line. The toolkit has no wrapping row (`FlowRow`), so the split is by width.
pub const CHIP_WRAP_WIDTH: f32 = 1100.0;

pub fn split_chip_rows(width_dp: f32) -> bool {
    width_dp < CHIP_WRAP_WIDTH
}

/// The suggestion chips as lines: all on one line when wide, one chip per line when narrow, so
/// none scrolls out of view.
pub fn suggestion_lines<'a>(all: &[&'a str], width_dp: f32) -> Vec<Vec<&'a str>> {
    if split_chip_rows(width_dp) {
        all.iter().map(|t| vec![*t]).collect()
    } else {
        vec![all.to_vec()]
    }
}

/// The text to send: what was typed, else the chosen suggestion. The toolkit's `TextField` has
/// no initial value (GAPS: uncontrolled field), so a suggestion cannot be written into the
/// field; it is shown as a pending line and used when the field is empty.
pub fn effective_message(typed: &str, suggestion: Option<&str>) -> String {
    let t = typed.trim();
    if t.is_empty() {
        suggestion.unwrap_or("").trim().to_string()
    } else {
        t.to_string()
    }
}

/// Whether send does anything: some text, and no create already running.
pub fn can_send(message: &str, busy: bool) -> bool {
    !busy && !message.trim().is_empty()
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
    [remembered, latest_in_project, latest_any]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// The session title taken from the first message.
pub fn title_from_message(text: &str) -> String {
    model::one_line(text, 40)
}

/// Whether the new session must be switched to approve-all right after it is created: only
/// bypass (FR-L16, FR-A5). Plan and edits-only need CLI flags the create call does not carry.
/// TODO(server): `POST /sessions` has no mode field; ask and edits start in the agent's own
/// default, plan likewise, until the server takes a permission mode.
pub fn needs_approve_all(mode: PermMode) -> bool {
    mode.is_bypass()
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
        match s {
            LauncherStatus::Running => self.running += 1,
            LauncherStatus::WaitingForApproval => self.waiting += 1,
            LauncherStatus::FinishedUnread => self.unread += 1,
            LauncherStatus::Finished => self.done += 1,
            LauncherStatus::Failed => self.failed += 1,
            LauncherStatus::Idle | LauncherStatus::Unknown => self.idle += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.running + self.waiting + self.unread + self.done + self.failed + self.idle
    }
}

pub fn status_counts(state: &State) -> StatusCounts {
    let mut c = StatusCounts::default();
    for p in state.projects() {
        for id in &p.sessions {
            if let Some(v) = state.session(id).filter(|v| !v.record.archived) {
                c.add(v.status);
            }
        }
    }
    c
}

/// Success rate in percent: sessions that finished over sessions that finished or failed.
/// `None` while none has ended, so the tile says so instead of 0 or 100.
pub fn success_percent(finished: usize, failed: usize) -> Option<u32> {
    let ended = finished + failed;
    (ended > 0).then(|| ((finished as f64 / ended as f64) * 100.0).round() as u32)
}

/// (online, total) over the registered computers.
pub fn computers_online(state: &State) -> (usize, usize) {
    let all = state.computers();
    let online = all
        .iter()
        .filter(|c| c.reachability == Reachability::Online)
        .count();
    (online, all.len())
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
    let success = success_percent(counts.done + counts.unread, counts.failed);
    [
        Kpi {
            label: "실행 중 팀원",
            value: counts.running.to_string(),
            unit: "명",
            note: Some(format!("대기 {}", counts.waiting)),
            available: true,
        },
        Kpi {
            label: "온라인 컴퓨터",
            value: format!("{}/{}", online, computers),
            unit: "",
            note: None,
            available: true,
        },
        match success {
            Some(p) => Kpi {
                label: "성공률",
                value: p.to_string(),
                unit: "%",
                note: Some(format!("실패 {}", counts.failed)),
                available: true,
            },
            None => Kpi {
                label: "성공률",
                value: "집계 전".to_string(),
                unit: "",
                note: Some("끝난 세션이 없습니다".to_string()),
                available: false,
            },
        },
        Kpi {
            label: "승인 대기 중앙값",
            value: UNAVAILABLE.to_string(),
            unit: "",
            note: Some(format!("지금 {}건 대기", counts.waiting)),
            available: false,
        },
    ]
}

/// One bar of the usage-by-harness tile: sessions per harness. This is a session count, not
/// token usage; tokens are not in the client yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessBar {
    pub label: String,
    pub sessions: usize,
}

pub fn harness_usage(state: &State) -> Vec<HarnessBar> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for p in state.projects() {
        for id in &p.sessions {
            let Some(v) = state.session(id).filter(|v| !v.record.archived) else {
                continue;
            };
            let kind = v.record.agent.clone();
            match counts.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => counts.push((kind, 1)),
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
        .into_iter()
        .map(|(kind, sessions)| HarnessBar {
            label: Harness::from_kind(&kind)
                .map(|h| h.label().to_string())
                .unwrap_or(kind),
            sessions,
        })
        .collect()
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (proleptic Gregorian), `None` if malformed.
fn day_number(ymd: &str) -> Option<i64> {
    let mut it = ymd.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `YYYY-MM-DD` of a day number from [`day_number`].
fn day_string(n: i64) -> String {
    let z = n + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's UTC date as `YYYY-MM-DD`.
pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    day_string(secs.div_euclid(86_400))
}

/// ChatGPT plan tokens (input plus output) for each of the last `days` UTC days ending at
/// `today`, oldest first, as `(YYYY-MM-DD, tokens)`. Only rows of accounts in `chatgpt_ids`
/// count: the other harnesses report no tokens here yet (FR-U7). A day with no row is 0.
pub fn chatgpt_token_days(rows: &[DailyUsage], chatgpt_ids: &[String], today: &str, days: usize) -> Vec<(String, u64)> {
    let Some(end) = day_number(today) else {
        return Vec::new();
    };
    (0..days as i64)
        .map(|i| day_string(end - (days as i64 - 1) + i))
        .map(|day| {
            let total = rows
                .iter()
                .filter(|r| r.day == day && r.account_id.as_ref().is_some_and(|a| chatgpt_ids.contains(a)))
                .map(DailyUsage::total_tokens)
                .sum();
            (day, total)
        })
        .collect()
}

/// A bar's rectangle inside a chart `width` by `height` dp: `(x, y, w, h)`, bars side by side
/// with `gap` between, heights relative to the largest value, at least 2 dp for a non-zero
/// value so it stays visible. All zero gives zero-height bars.
pub fn bar_rects(values: &[usize], width: f32, height: f32, gap: f32) -> Vec<(f32, f32, f32, f32)> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let bar_w = ((width - gap * (n as f32 - 1.0)) / n as f32).max(1.0);
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let h = if max == 0 || *v == 0 {
                0.0
            } else {
                (height * (*v as f32 / max as f32)).max(2.0)
            };
            (i as f32 * (bar_w + gap), height - h, bar_w, h)
        })
        .collect()
}

/// Hatch lines for a bar `(x, y, w, h)`: 45 degree segments `(x1, y1, x2, y2)` rising to the right,
/// `step` dp apart along the vertical, each clipped to the rectangle. Empty for a degenerate rect
/// or a non-positive step.
pub fn hatch_lines(x: f32, y: f32, w: f32, h: f32, step: f32) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    if w <= 0.0 || h <= 0.0 || step <= 0.0 {
        return out;
    }
    // The line `d` passes (x, y + h - d) and runs (+1, -1); it is inside for t in [lo, hi].
    let mut d = -w + step;
    while d < h {
        let lo = (-d).max(0.0);
        let hi = w.min(h - d);
        if hi > lo {
            out.push((x + lo, y + h - d - lo, x + hi, y + h - d - hi));
        }
        d += step;
    }
    out
}

/// Ring segments as `(start_degrees, sweep_degrees)` from 12 o'clock clockwise, one per count;
/// a zero count has a zero sweep. All zero gives no segments.
pub fn ring_segments(counts: &[usize]) -> Vec<(f32, f32)> {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return Vec::new();
    }
    let mut start = -90.0_f32;
    counts
        .iter()
        .map(|c| {
            let sweep = 360.0 * (*c as f32 / total as f32);
            let seg = (start, sweep);
            start += sweep;
            seg
        })
        .collect()
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
    let mut rows: Vec<(bool, i64, CrewMember)> = Vec::new();
    for p in state.projects() {
        for id in &p.sessions {
            let Some(v) = state.session(id).filter(|v| !v.record.archived) else {
                continue;
            };
            let waiting = v.status == LauncherStatus::WaitingForApproval;
            if waiting || v.status == LauncherStatus::Running {
                rows.push((
                    waiting,
                    v.record.updated_at,
                    CrewMember {
                        id: v.record.id.clone(),
                        title: v.record.title.clone(),
                        project: v.record.project.clone(),
                        agent: v.record.agent.clone(),
                        waiting,
                    },
                ));
            }
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    rows.into_iter().take(max).map(|r| r.2).collect()
}

/// The text under the crew row: "컴퓨터 2/3 온라인".
pub fn computers_line(online: usize, total: usize) -> String {
    format!("컴퓨터 {}/{} 온라인", online, total)
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
    model::project_cards(state)
        .into_iter()
        .take(max)
        .map(|c| ProjectTile {
            name: c.name,
            total: c.summary.total,
            running: c.summary.running,
            waiting: c.summary.waiting,
        })
        .collect()
}

/// "팀원 3명 · 실행 1 · 대기 1", the tile's second line.
pub fn project_tile_line(t: &ProjectTile) -> String {
    format!(
        "팀원 {}명 · 실행 {} · 대기 {}",
        t.total, t.running, t.waiting
    )
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// The Home screen: the chat-first top, then the dashboard.
#[component]
pub fn Home() -> Element {
    let window = use_window_size();
    let pad = if window.is_compact() { SP[3] } else { SP[6] };
    rsx! {
        ScrollColumn {
            fill_max_width: true,
            fill_max_height: true,
            Column {
                fill_max_width: true,
                padding: pad,
                spacing: SP[5],
                HomeComposer {}
                HomeDashboard {}
            }
        }
    }
}

/// One entry of a chip's menu.
#[component]
pub fn MenuItem(
    #[props(into)] text: String,
    #[props(default)] selected: bool,
    #[props(default)] danger: bool,
    #[props(default = true)] enabled: bool,
    on_click: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let text = if selected {
        format!("✓ {text}")
    } else {
        text
    };
    let color = if danger {
        paint(theme.palette.red.strong)
    } else {
        paint(theme.palette.text)
    };
    rsx! {
        Button {
            text,
            enabled,
            variant: ButtonVariant::Text,
            fill_max_width: true,
            height: H_CTL,
            color,
            on_click: move |()| on_click.call(()),
        }
    }
}

#[component]
fn HomeComposer() -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let theme = use_ember_theme();
    let compact = window.is_compact();

    let mut selection = use_signal(Selection::initial);
    let mut typed = use_signal(String::new);
    let mut suggestion = use_signal(|| None::<String>);
    // Changing this rebuilds the (uncontrolled) text field, which is how it is cleared.
    let mut generation = use_signal(|| 0_u64);
    let mut busy = use_signal(|| false);
    let mut project_menu = use_signal(|| false);
    let mut mode_menu = use_signal(|| false);
    let mut computer_menu = use_signal(|| false);
    let mut picker_open = use_signal(|| false);
    let mut picker_harness = use_signal(|| None::<Harness>);
    let mut picker_query = use_signal(String::new);

    // ---- live data ----------------------------------------------------------------------------
    let (existing_projects, computers): (Vec<String>, Vec<(String, String)>) = {
        let _ = *ui.live.launcher.read();
        services().client.read(|s| {
            let projects = model::project_cards(s)
                .into_iter()
                .map(|c| c.name)
                .collect();
            let computers = s
                .computers()
                .iter()
                .filter(|c| c.id != "local")
                .map(|c| (c.id.clone(), c.name.clone()))
                .collect();
            (projects, computers)
        })
    };
    let agents: Vec<(String, bool)> = ui
        .live
        .agents
        .read()
        .iter()
        .map(|a| (a.kind.clone(), a.installed))
        .collect();
    let limited: Vec<String> = {
        let accounts = ui.live.accounts.read();
        Harness::ALL
            .iter()
            .map(|h| h.kind())
            .filter(|k| {
                let of_kind: Vec<_> = accounts.iter().filter(|a| a.agent == *k).collect();
                !of_kind.is_empty() && of_kind.iter().all(|a| a.limited)
            })
            .map(String::from)
            .collect()
    };

    let sel = selection();
    let projects = project_options(&existing_projects);
    let message = effective_message(&typed(), suggestion().as_deref());
    let sendable = can_send(&message, busy());

    // ---- send ---------------------------------------------------------------------------------
    let send = use_callback({
        move |()| {
            let message = effective_message(&typed.peek(), suggestion.peek().as_deref());
            if !can_send(&message, *busy.peek()) {
                return;
            }
            let sel = selection.peek().clone();
            let (remembered, latest_in_project, latest_any) = {
                let prefs = ui.prefs.peek();
                let remembered = prefs.last_used.get(&sel.project).map(|l| l.cwd.clone());
                services().client.read(|s| {
                    let any = model::recent_sessions(
                        s,
                        &model::RowContext {
                            account_labels: &Default::default(),
                        },
                        1,
                    )
                    .first()
                    .map(|r| r.cwd.clone());
                    (remembered, model::latest_cwd(s, &sel.project), any)
                })
            };
            let Some(cwd) = resolve_cwd(
                remembered.as_deref(),
                latest_in_project.as_deref(),
                latest_any.as_deref(),
            ) else {
                // Home has no folder field; the new-session dialog asks for one.
                Message::new("작업 폴더를 알 수 없습니다. 새 세션 창에서 폴더를 정해 주세요")
                    .with_duration(MessageDuration::Long)
                    .show();
                ui.open_new_session(Some(sel.project.clone()));
                return;
            };
            busy.set(true);
            let body = CreateSession {
                project: sel.project.clone(),
                agent: sel.option.harness.kind().to_string(),
                cwd: cwd.clone(),
                model: Some(sel.option.model.to_string()),
                title: Some(title_from_message(&message)),
                account: None,
            };
            let computer_id = sel.computer.clone();
            let bypass = needs_approve_all(sel.mode);
            let remember = LastUsed {
                agent: body.agent.clone(),
                account: None,
                computer: computer_id.clone(),
                model: body.model.clone(),
                cwd,
            };
            let project_name = sel.project.clone();
            run(
                async move {
                    let s = services();
                    let rec = s.create_session(&body).await?;
                    let mut notice = None;
                    // The demo (#199) has one computer and no node to switch.
                    if let Some(cid) = computer_id.filter(|_| s.demo.is_none()) {
                        if let Err(e) = s.server.switch_computer(&rec.id, &cid).await {
                            notice = Some(format!(
                                "메인 서버에서 시작했습니다. 컴퓨터를 바꾸지 못했습니다 ({e})"
                            ));
                        }
                    }
                    if bypass {
                        if let Err(e) = s.set_approve_all(&rec.id, true).await {
                            notice = Some(format!("모두 허용을 켜지 못했습니다 ({e})"));
                        }
                    }
                    Ok::<_, String>((rec, notice))
                },
                move |r| {
                    busy.set(false);
                    match r {
                        Ok((rec, notice)) => {
                            ui.update_prefs(|p| {
                                p.last_used.insert(project_name.clone(), remember);
                                p.last_project = Some(project_name);
                            });
                            let s = services();
                            s.outbox.lock().unwrap().push(&rec.id, message);
                            s.bump_outbox();
                            typed.set(String::new());
                            suggestion.set(None);
                            generation += 1;
                            ui.open_conversation(&rec.id);
                            s.drive_outbox(&rec.id);
                            if let Some(n) = notice {
                                Message::new(n).with_duration(MessageDuration::Long).show();
                            }
                        }
                        Err(e) => Message::new(format!("세션을 시작하지 못했습니다 ({e})"))
                            .with_duration(MessageDuration::Long)
                            .show(),
                    }
                },
            );
        }
    });

    // ---- picker rows --------------------------------------------------------------------------
    let all = model_catalog();
    let rows = filter_models(&all, picker_harness(), &picker_query());

    let computer_text = computer_chip_text(sel.computer.as_deref(), &computers);
    let mode_label = sel.mode.label();
    let placeholder = match suggestion() {
        Some(s) => s,
        None => COMPOSER_PLACEHOLDER.to_string(),
    };

    let add_button = rsx! {
        IconButton {
            icon: IconRole::Add,
            small: true,
            // TODO(FR-L11): attachments need the file picker and an upload API.
            on_click: move |()| Message::new(UNAVAILABLE).show(),
        }
    };
    let send_part = rsx! {
        if busy() {
            ProgressIndicator { determinate: false, circular: true, width: 18.0, height: 18.0 }
        }
        AppButton {
            text: "보내기",
            icon: IconRole::Send,
            kind: ButtonKind::Primary,
            enabled: sendable,
            on_click: move |()| send.call(()),
        }
    };
    let two_lines = split_chip_rows(window.width_dp);
    let chips_project = rsx! {
            Menu {
                expanded: project_menu(),
                on_dismiss: move |_| project_menu.set(false),
                anchor: rsx! {
                    PickChip {
                        text: sel.project.clone(),
                        icon: IconRole::Library,
                        on_click: move |()| project_menu.set(true),
                    }
                },
                for p in projects.iter().cloned() {
                    MenuItem {
                        key: "{p}",
                        text: p.clone(),
                        selected: p == sel.project,
                        on_click: move |()| {
                            let mut s = selection.peek().clone();
                            s.project = p.clone();
                            selection.set(s);
                            project_menu.set(false);
                        },
                    }
                }
            }
            PickChip {
                text: sel.option.chip_label(),
                icon: IconRole::Compose,
                icon_only: compact,
                on_click: move |()| picker_open.set(true),
            }
    };
    let chips_mode = rsx! {
            Menu {
                expanded: mode_menu(),
                on_dismiss: move |_| mode_menu.set(false),
                anchor: rsx! {
                    PickChip {
                        text: mode_label,
                        icon: IconRole::Settings,
                        icon_only: compact,
                        selected: sel.mode.is_bypass(),
                        on_click: move |()| mode_menu.set(true),
                    }
                },
                for m in modes_for(sel.option.harness).iter().copied() {
                    MenuItem {
                        key: "{m.label()}",
                        text: m.label(),
                        selected: m == sel.mode,
                        danger: m.is_bypass(),
                        on_click: move |()| {
                            let s = selection.peek().with_mode(m);
                            selection.set(s);
                            mode_menu.set(false);
                        },
                    }
                }
            }
            Menu {
                expanded: computer_menu(),
                on_dismiss: move |_| computer_menu.set(false),
                anchor: rsx! {
                    PickChip {
                        text: computer_text.clone(),
                        icon: IconRole::Sidebar,
                        icon_only: compact,
                        on_click: move |()| computer_menu.set(true),
                    }
                },
                MenuItem {
                    text: "이 서버",
                    selected: sel.computer.is_none(),
                    on_click: move |()| {
                        let mut s = selection.peek().clone();
                        s.computer = None;
                        selection.set(s);
                        computer_menu.set(false);
                    },
                }
                for (id, name) in computers.iter().cloned() {
                    MenuItem {
                        key: "{id}",
                        text: name.clone(),
                        selected: sel.computer.as_deref() == Some(id.as_str()),
                        on_click: move |()| {
                            let mut s = selection.peek().clone();
                            s.computer = Some(id.clone());
                            selection.set(s);
                            computer_menu.set(false);
                        },
                    }
                }
            }
    };

    rsx! {
        Column {
            fill_max_width: true,
            spacing: SP[4],
            Label { text: GREETING, fs: Fs::Xl3, weight: Weight::Medium }
            EmberCard { fill_max_width: true,
                Column {
                    fill_max_width: true,
                    padding: PAD_CARD_H,
                    spacing: SP[2],
                    Spacer { height: SP[2] }
                    for g in [generation()] {
                        TextField {
                            key: "home-composer-{g}",
                            fill_max_width: true,
                            height: 96.0,
                            multiline: true,
                            // The field's fill and border follow the theme; the toolkit has no
                            // text-colour prop, so the ink comes from the renderer's theme.
                            background: paint(theme.palette.surface_2),
                            border_width: 1.0,
                            border_color: paint(theme.palette.line),
                            placeholder: placeholder.clone(),
                            on_value_change: move |v: String| typed.set(v),
                            on_submit: move |_| send.call(()),
                        }
                    }
                    if suggestion().is_some() {
                        Row {
                            fill_max_width: true,
                            alignment: Alignment::CenterStart,
                            spacing: SP[1],
                            Label { text: "제안을 고르셨습니다. 입력하지 않으면 이 내용으로 시작합니다", fs: Fs::Xs, color: paint(theme.palette.muted), max_lines: 2 }
                            Spacer { weight: 1.0 }
                            PickChip { text: "취소", on_click: move |()| suggestion.set(None) }
                        }
                    }
                    if two_lines {
                        Row {
                            fill_max_width: true,
                            alignment: Alignment::CenterStart,
                            spacing: SP[1],
                            {add_button}
                            {chips_project}
                        }
                        Row {
                            fill_max_width: true,
                            alignment: Alignment::CenterStart,
                            spacing: SP[1],
                            {chips_mode}
                            Spacer { weight: 1.0 }
                            {send_part}
                        }
                    } else {
                        Row {
                            fill_max_width: true,
                            alignment: Alignment::CenterStart,
                            spacing: SP[1],
                            {add_button}
                            {chips_project}
                            {chips_mode}
                            Spacer { weight: 1.0 }
                            {send_part}
                        }
                    }
                    Spacer { height: SP[2] }
                }
            }
            for line in suggestion_lines(&SUGGESTIONS, window.width_dp) {
                Row {
                    fill_max_width: true,
                    spacing: SP[2],
                    for text in line {
                        PickChip {
                            key: "{text}",
                            text,
                            on_click: move |()| suggestion.set(Some(apply_suggestion(&typed.peek(), text))),
                        }
                    }
                }
            }
        }

        EmberSheet {
            title: "모델 선택",
            open: picker_open(),
            on_dismiss: move |()| picker_open.set(false),
            Column {
                fill_max_width: true,
                padding_role: SpaceRole::Md,
                spacing: SP[2],
                ScrollRow {
                    fill_max_width: true,
                    Row {
                        spacing: SP[1],
                        PickChip {
                            text: "전체",
                            selected: picker_harness().is_none(),
                            on_click: move |()| picker_harness.set(None),
                        }
                        for h in Harness::ALL {
                            PickChip {
                                key: "{h.label()}",
                                text: h.label(),
                                selected: picker_harness() == Some(h),
                                on_click: move |()| picker_harness.set(Some(h)),
                            }
                        }
                    }
                }
                TextField {
                    fill_max_width: true,
                    background: paint(theme.palette.surface_2),
                    border_width: 1.0,
                    border_color: paint(theme.palette.line),
                    placeholder: "모델 검색",
                    on_value_change: move |v: String| picker_query.set(v),
                }
                if rows.is_empty() {
                    Label { text: "맞는 모델이 없습니다", fs: Fs::Sm, color: paint(theme.palette.muted) }
                }
                ScrollColumn {
                    fill_max_width: true,
                    height: 320.0,
                    Column {
                        fill_max_width: true,
                        spacing: SP[1],
                        for (i, o) in rows.iter().copied().enumerate() {
                        ModelRow {
                            key: "{i}-{o.model}",
                            option: o,
                            shortcut: shortcut_label(i),
                            selected: o == sel.option,
                            block: harness_block(o.harness, &agents, &limited),
                            on_pick: move |()| {
                                let s = selection.peek().with_option(o);
                                selection.set(s);
                                picker_open.set(false);
                            },
                        }
                        }
                    }
                }
            }
        }
    }
}

/// One row of the model picker: "model (effort)", harness and shortcut; dimmed with its reason
/// when the harness cannot be used (FR-L15).
#[component]
fn ModelRow(
    option: ModelOption,
    shortcut: Option<String>,
    selected: bool,
    block: Option<&'static str>,
    on_pick: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let text = match &shortcut {
        Some(k) => format!(
            "{}   {}   {}",
            option.row_title(),
            option.harness.label(),
            k
        ),
        None => format!("{}   {}", option.row_title(), option.harness.label()),
    };
    rsx! {
        Column {
            fill_max_width: true,
            AppButton {
                text,
                fill_max_width: true,
                kind: if selected { ButtonKind::Primary } else { ButtonKind::Default },
                enabled: block.is_none(),
                on_click: move |()| on_pick.call(()),
            }
            if let Some(reason) = block {
                Label { text: reason, fs: Fs::Xs, color: paint(theme.palette.red.strong) }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------------------------

/// Everything the dashboard reads from the client in one pass.
struct Dash {
    counts: StatusCounts,
    online: usize,
    computers: usize,
    harness: Vec<HarnessBar>,
    crew: Vec<CrewMember>,
    projects: Vec<ProjectTile>,
}

fn read_dash(state: &State) -> Dash {
    let (online, computers) = computers_online(state);
    Dash {
        counts: status_counts(state),
        online,
        computers,
        harness: harness_usage(state),
        crew: crew(state, 7),
        projects: project_tiles(state, 12),
    }
}

#[component]
fn HomeDashboard() -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let compact = window.is_compact();
    let _ = *ui.live.launcher.read();
    let d = services().client.read(read_dash);
    let k = kpis(&d.counts, d.online, d.computers);

    rsx! {
        Column {
            fill_max_width: true,
            spacing: SP[4],
            Label { text: "오늘의 팀", fs: Fs::Xl, weight: Weight::Semibold }
            if compact {
                Row { fill_max_width: true, spacing: SP[3],
                    KpiView { kpi: k[0].clone() }
                    KpiView { kpi: k[1].clone() }
                }
                Row { fill_max_width: true, spacing: SP[3],
                    KpiView { kpi: k[2].clone() }
                    KpiView { kpi: k[3].clone() }
                }
            } else {
                Row { fill_max_width: true, spacing: SP[3],
                    for kpi in k.iter().cloned() {
                        KpiView { key: "{kpi.label}", kpi }
                    }
                }
            }
            TilePair {
                compact,
                start_weight: 2.0,
                end_weight: 1.0,
                start: rsx! { TokenTrendTile {} },
                end: rsx! { StatusTile { counts: d.counts.clone() } },
            }
            TilePair {
                compact,
                start_weight: 1.0,
                end_weight: 1.0,
                start: rsx! { HarnessTile { bars: d.harness.clone() } },
                end: rsx! { CrewTile { crew: d.crew.clone(), online: d.online, computers: d.computers } },
            }
            ProjectsTile { tiles: d.projects.clone() }
        }
    }
}

/// Two tiles side by side, or stacked on a compact window (FR-L10).
#[component]
fn TilePair(
    compact: bool,
    start_weight: f32,
    end_weight: f32,
    start: Element,
    end: Element,
) -> Element {
    if compact {
        rsx! {
            Column { fill_max_width: true, spacing: SP[3], {start} {end} }
        }
    } else {
        rsx! {
            Row {
                fill_max_width: true,
                spacing: SP[3],
                Column { weight: start_weight, {start} }
                Column { weight: end_weight, {end} }
            }
        }
    }
}

#[component]
fn KpiView(kpi: Kpi) -> Element {
    let theme = use_ember_theme();
    let muted = paint(theme.palette.muted);
    rsx! {
        Column {
            weight: 1.0,
            spacing: SP[0],
            Label { text: kpi.label, fs: Fs::Sm, color: muted }
            if kpi.available {
                Row {
                    alignment: Alignment::CenterStart,
                    spacing: SP[0],
                    Label { text: kpi.value.clone(), fs: Fs::Xl3, weight: Weight::Semibold }
                    if !kpi.unit.is_empty() {
                        Label { text: kpi.unit, fs: Fs::Md, color: muted }
                    }
                }
            } else {
                Label { text: kpi.value.clone(), fs: Fs::Md, color: muted }
            }
            if let Some(note) = kpi.note.clone() {
                Label { text: note, fs: Fs::Xs, color: muted }
            }
        }
    }
}

/// Token bars per day for the ChatGPT plan accounts, 7 or 14 days (FR-U7). Cost and the other
/// harnesses are not reported yet and say so.
#[component]
fn TokenTrendTile() -> Element {
    let ui = use_ui();
    let theme = use_ember_theme();
    let p = theme.palette;
    let mut days = use_signal(|| 7usize);
    let rows = ui.live.usage.read().clone();
    let ids: Vec<String> = ui.live.chatgpt.read().iter().map(|a| a.id.clone()).collect();
    if ids.is_empty() {
        return rsx! { UnavailableTile { title: "비용과 토큰 추이" } };
    }
    let series = chatgpt_token_days(&rows, &ids, &today_utc(), *days.read());
    const W: f32 = 420.0;
    const H: f32 = 90.0;
    let values: Vec<usize> = series.iter().map(|(_, t)| *t as usize).collect();
    let mut list = DrawList::builder().rect(paint(p.line), 0.0, H, W, 1.0, 0.0);
    for (x, y, w, h) in bar_rects(&values, W, H, 6.0) {
        if h > 0.0 {
            list = list.round_rect(paint(p.blue.strong), x, y, w, h, 3.0, 1.0);
            for (x1, y1, x2, y2) in hatch_lines(x, y, w, h, 5.0) {
                list = list.line(paint(p.blue.strong), x1, y1, x2, y2, 1.0);
            }
        }
    }
    let commands = list.build();
    let total: u64 = series.iter().map(|(_, t)| *t).sum();
    let first = series.first().map(|(d, _)| d.clone()).unwrap_or_default();
    let last = series.last().map(|(d, _)| d.clone()).unwrap_or_default();
    rsx! {
        TileFrame { title: "비용과 토큰 추이",
            Row { fill_max_width: true, spacing: SP[1],
                for n in [7usize, 14] {
                    AppButton {
                        key: "{n}",
                        text: format!("{n}일"),
                        kind: if *days.read() == n { ButtonKind::Primary } else { ButtonKind::Default },
                        small: true,
                        on_click: move |()| days.set(n),
                    }
                }
            }
            Canvas { width: W, height: H + 1.0, commands }
            Row { width: W,
                Label { text: first, fs: Fs::Xs2, color: paint(p.muted), max_lines: 1 }
                Spacer { weight: 1.0 }
                Label { text: last, fs: Fs::Xs2, color: paint(p.muted), max_lines: 1 }
            }
            Label { text: format!("ChatGPT 토큰 합계 {}", super::agents::format_tokens(total)), fs: Fs::Sm }
            Label { text: format!("비용과 다른 하네스의 토큰은 {UNAVAILABLE}"), fs: Fs::Xs2, color: paint(p.muted), max_lines: 2 }
        }
    }
}

/// A tile for data the server does not provide yet: the title and "준비 중입니다".
/// TODO(server, FR-U7): tokens and cost per day are not in the client's state; draw
/// the hatched token bars with the cost line when an endpoint exists. Charts then use `Canvas`
/// (GAPS: a line over bars needs `Path`, `DrawCommand` has only polylines; fall back to bars).
#[component]
fn UnavailableTile(#[props(into)] title: String) -> Element {
    let theme = use_ember_theme();
    rsx! {
        EmberCard { fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: PAD_CARD_H,
                spacing: SP[2],
                Label { text: title, fs: Fs::Lg, weight: Weight::Semibold }
                Label { text: UNAVAILABLE, fs: Fs::Md, color: paint(theme.palette.muted) }
            }
        }
    }
}

#[component]
fn TileFrame(#[props(into)] title: String, children: Element) -> Element {
    rsx! {
        EmberCard { fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: PAD_CARD_H,
                spacing: SP[2],
                Label { text: title, fs: Fs::Lg, weight: Weight::Semibold }
                {children}
            }
        }
    }
}

/// Usage by harness: bars of sessions per harness. A session count, not tokens (see
/// [`UnavailableTile`]).
#[component]
fn HarnessTile(bars: Vec<HarnessBar>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    const W: f32 = 280.0;
    const H: f32 = 80.0;
    let values: Vec<usize> = bars.iter().map(|b| b.sessions).collect();
    let rects = bar_rects(&values, W, H, 8.0);
    let mut list = DrawList::builder().rect(paint(p.line), 0.0, H, W, 1.0, 0.0);
    for (x, y, w, h) in rects {
        if h > 0.0 {
            list = list.round_rect(paint(p.blue.strong), x, y, w, h, 4.0, 0.0);
        }
    }
    let commands = list.build();
    rsx! {
        TileFrame { title: "하네스별 사용량",
            if bars.is_empty() {
                Label { text: EMPTY_HARNESS_USAGE, fs: Fs::Md, color: paint(p.muted) }
            } else {
                Canvas { width: W, height: H + 1.0, commands }
                Row {
                    width: W,
                    for b in bars.iter().cloned() {
                        Label { key: "{b.label}", text: format!("{} {}", b.label, b.sessions), fs: Fs::Xs, color: paint(p.muted), max_lines: 1 }
                        Spacer { weight: 1.0 }
                    }
                }
                Label { text: "세션 수 기준입니다. 토큰 사용량은 준비 중입니다", fs: Fs::Xs2, color: paint(p.muted), max_lines: 2 }
            }
        }
    }
}

/// Member status: a ring and a legend.
#[component]
fn StatusTile(counts: StatusCounts) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let parts: [(&str, usize, Color); 5] = [
        ("실행", counts.running, p.blue.strong),
        ("승인 대기", counts.waiting, p.orange.strong),
        ("새 결과", counts.unread, p.sky.strong),
        ("완료", counts.done, p.grey.strong),
        ("실패", counts.failed, p.red.strong),
    ];
    let values: Vec<usize> = parts.iter().map(|x| x.1).collect();
    let mut list = DrawList::builder().circle(paint(p.track), 48.0, 48.0, 40.0, 10.0);
    for ((start, sweep), part) in ring_segments(&values).into_iter().zip(parts.iter()) {
        if sweep > 0.0 {
            list = list.arc(paint(part.2), 48.0, 48.0, 40.0, start, sweep, 10.0);
        }
    }
    let commands = list.build();
    rsx! {
        TileFrame { title: "팀원 상태",
            Row {
                fill_max_width: true,
                spacing: SP[3],
                alignment: Alignment::CenterStart,
                Canvas { width: 96.0, height: 96.0, commands }
                Column {
                    spacing: SP[0],
                    for part in parts.iter() {
                        Label { key: "{part.0}", text: format!("{} {}", part.0, part.1), fs: Fs::Sm, color: paint(part.2) }
                    }
                }
            }
        }
    }
}

/// The working crew: running and waiting members, each opening its conversation.
#[component]
fn CrewTile(crew: Vec<CrewMember>, online: usize, computers: usize) -> Element {
    let ui = use_ui();
    let theme = use_ember_theme();
    let muted = paint(theme.palette.muted);
    rsx! {
        TileFrame { title: "지금 일하는 팀원",
            if crew.is_empty() {
                Label { text: EMPTY_CREW, fs: Fs::Md, color: muted }
            }
            for m in crew.iter().cloned() {
                Row {
                    key: "{m.id}",
                    fill_max_width: true,
                    spacing: SP[1],
                    alignment: Alignment::CenterStart,
                    AppButton {
                        text: model::one_line(&m.title, 24),
                        small: true,
                        on_click: {
                            let id = m.id.clone();
                            move |()| ui.open_conversation(&id)
                        },
                    }
                    Label { text: m.project.clone(), fs: Fs::Xs, color: muted, max_lines: 1 }
                    Spacer { weight: 1.0 }
                    if m.waiting {
                        Pill { text: "승인 대기", tone: PillTone::Orange, small: true }
                    } else {
                        Pill { text: "실행", tone: PillTone::Blue, small: true }
                    }
                }
            }
            Label { text: computers_line(online, computers), fs: Fs::Xs, color: muted }
        }
    }
}

/// The project list: each opens the Projects menu on that project.
#[component]
fn ProjectsTile(tiles: Vec<ProjectTile>) -> Element {
    let ui = use_ui();
    let theme = use_ember_theme();
    rsx! {
        TileFrame { title: "프로젝트",
            if tiles.is_empty() {
                Label { text: EMPTY_PROJECTS, fs: Fs::Md, color: paint(theme.palette.muted) }
            }
            for t in tiles.iter().cloned() {
                Row {
                    key: "{t.name}",
                    fill_max_width: true,
                    spacing: SP[2],
                    alignment: Alignment::CenterStart,
                    AppButton {
                        text: t.name.clone(),
                        small: true,
                        on_click: {
                            let name = t.name.clone();
                            move |()| {
                                let mut sel = ui.selected_project;
                                sel.set(Some(name.clone()));
                                ui.go(Route::Projects);
                                let mut menu = ui.menu;
                                menu.set(NavMenu::Projects);
                            }
                        },
                    }
                    Label { text: project_tile_line(&t), fs: Fs::Xs, color: paint(theme.palette.muted), max_lines: 1 }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_u7_hatch_lines_stay_inside_the_bar() {
        let (x, y, w, h) = (10.0, 20.0, 30.0, 50.0);
        let lines = hatch_lines(x, y, w, h, 5.0);
        assert!(!lines.is_empty());
        let eps = 1e-3;
        for (x1, y1, x2, y2) in &lines {
            for (px, py) in [(*x1, *y1), (*x2, *y2)] {
                assert!(px >= x - eps && px <= x + w + eps, "x {px}");
                assert!(py >= y - eps && py <= y + h + eps, "y {py}");
            }
            assert!(x2 > x1 && y2 < y1, "rises to the right");
        }
        let expected = ((w + h) / 5.0) as usize;
        assert!(lines.len() + 2 >= expected && lines.len() <= expected + 1);
        assert!(hatch_lines(x, y, 0.0, h, 5.0).is_empty());
        assert!(hatch_lines(x, y, w, h, 0.0).is_empty());
    }

    // ---- FR-L16: modes per harness ----

    #[test]
    fn fr_u7_chatgpt_token_days_fills_gaps_and_filters_accounts() {
        let row = |a: Option<&str>, day: &str, i: u64, o: u64| DailyUsage {
            account_id: a.map(str::to_string),
            day: day.into(),
            input_tokens: i,
            output_tokens: o,
            reports: 1,
        };
        let rows = [
            row(Some("g1"), "2026-02-28", 10, 5),
            row(Some("g1"), "2026-03-01", 100, 20),
            row(Some("g2"), "2026-03-01", 1, 1),
            row(Some("codex-a"), "2026-03-01", 999, 999),
            row(None, "2026-03-01", 999, 999),
        ];
        let ids = ["g1".to_string(), "g2".to_string()];
        let s = chatgpt_token_days(&rows, &ids, "2026-03-01", 3);
        assert_eq!(
            s,
            vec![("2026-02-27".to_string(), 0), ("2026-02-28".to_string(), 15), ("2026-03-01".to_string(), 122)]
        );
        assert!(chatgpt_token_days(&rows, &ids, "nope", 7).is_empty());
        assert_eq!(day_string(day_number("2024-02-29").unwrap() + 1), "2024-03-01");
        assert_eq!(day_number("1970-01-02"), Some(1));
    }

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
        assert!(
            rows.iter()
                .all(|o| o.model == "gpt-5-codex" && o.effort == Some("High"))
        );
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
        let agents = vec![
            ("claude-code".to_string(), true),
            ("codex".to_string(), false),
        ];
        assert_eq!(harness_block(Harness::ClaudeCode, &agents, &[]), None);
        assert!(harness_block(Harness::Codex, &agents, &[]).is_some());
        // Not listed at all: not installed.
        assert!(harness_block(Harness::Gemini, &agents, &[]).is_some());
        let limited = vec!["claude-code".to_string()];
        assert!(harness_block(Harness::ClaudeCode, &agents, &limited).is_some());
    }

    #[test]
    fn fr_l15_nothing_is_blocked_before_the_server_answers() {
        for h in Harness::ALL.into_iter().filter(|h| *h != Harness::Gemini) {
            assert_eq!(harness_block(h, &[], &[]), None);
        }
    }

    #[test]
    fn fr_l15_gemini_is_not_antigravity_and_is_disabled() {
        assert_ne!(Harness::Gemini.kind(), "antigravity");
        assert_eq!(Harness::ClaudeCode.kind(), "claude-code");
        assert_eq!(Harness::Codex.kind(), "codex");
        // Disabled even when the server lists it as installed, and before the server answers.
        assert_eq!(
            harness_block(Harness::Gemini, &[], &[]),
            Some("준비 중입니다")
        );
        let agents = vec![("gemini".to_string(), true)];
        assert_eq!(
            harness_block(Harness::Gemini, &agents, &[]),
            Some("준비 중입니다")
        );
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
    fn fr_l11_narrow_window_wraps_chips_and_suggestions() {
        assert!(split_chip_rows(800.0));
        assert!(!split_chip_rows(1400.0));
        assert_eq!(
            suggestion_lines(&SUGGESTIONS, 800.0).len(),
            SUGGESTIONS.len()
        );
        assert_eq!(suggestion_lines(&SUGGESTIONS, 1400.0).len(), 1);
    }

    #[test]
    fn fr_l11_a_suggestion_replaces_the_draft() {
        assert_eq!(apply_suggestion("abc", SUGGESTIONS[1]), SUGGESTIONS[1]);
        assert_eq!(SUGGESTIONS.len(), 3);
    }

    #[test]
    fn fr_l11_typed_text_wins_over_a_pending_suggestion() {
        assert_eq!(effective_message(" hi ", Some("s")), "hi");
        assert_eq!(effective_message("", Some(" s ")), "s");
        assert_eq!(effective_message("  ", None), "");
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
        assert_eq!(
            resolve_cwd(Some("/a"), Some("/b"), Some("/c")).as_deref(),
            Some("/a")
        );
        assert_eq!(
            resolve_cwd(None, Some("/b"), Some("/c")).as_deref(),
            Some("/b")
        );
        assert_eq!(
            resolve_cwd(Some(" "), None, Some("/c")).as_deref(),
            Some("/c")
        );
        assert_eq!(resolve_cwd(None, None, None), None);
    }

    #[test]
    fn fr_l11_title_is_the_first_line_of_the_message() {
        assert_eq!(
            title_from_message("고쳐 주세요\n자세히"),
            "고쳐 주세요\u{2026}"
        );
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
        assert_eq!(
            (c.running, c.waiting, c.unread, c.done, c.failed, c.idle),
            (2, 1, 1, 1, 1, 2)
        );
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
        assert!(
            bar_rects(&[0, 0], 100.0, 40.0, 4.0)
                .iter()
                .all(|b| b.3 == 0.0)
        );
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
                !s.ends_with('다')
                    || s.ends_with("입니다")
                    || s.ends_with("합니다")
                    || s.ends_with("습니다"),
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
