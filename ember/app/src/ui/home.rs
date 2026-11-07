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

use std::time::Duration;

use ember_client::wire::DailyUsage;
use ember_client::state::{LauncherStatus, Reachability, State};

use crate::model;

/// What a tile shows while the server does not provide its data.
pub const UNAVAILABLE: &str = "준비 중입니다";

/// The built-in project that holds Home chats by default (FR-L13).
pub const DEFAULT_PROJECT: &str = "기본 프로젝트";

/// The Home integrations area (FR-L23, #498). Owner decision (2026-11-06): GitHub is the one
/// integration in scope for November, its auth reuses the session computer's `gh` (Ember stores
/// no token). Slack, Discord, Linear, calendar and mail come in December or later.
pub const INTEGRATIONS_TITLE: &str = "연동";
pub const GITHUB_NAME: &str = "GitHub";
pub const GITHUB_NO_INFO: &str = "아직 정보가 없습니다";
/// A value the server gave as null while GitHub is connected (FR-L35): asked, not known.
pub const GITHUB_UNKNOWN: &str = "알 수 없음";
/// Appended to the card header in the demo, whose numbers are a fixed sample (FR-L35).
pub const GITHUB_SAMPLE_MARK: &str = "(예시)";
pub const GITHUB_NO_ACTIVITY: &str = "연동이 만든 일이 아직 없습니다";
pub const GITHUB_LOGIN_HINT: &str = "이 컴퓨터의 터미널에서 gh auth login을 실행하십시오";
pub const GITHUB_INSTALL_HINT: &str = "gh를 설치하고 로그인하십시오";
pub const GITHUB_UNAVAILABLE_HINT: &str = "연결 상태를 확인할 수 없습니다";
/// The control only reveals the `gh auth login` guidance, so it does not say 연결 or 다시 로그인.
pub const GITHUB_HOW: &str = "방법 보기";
/// Minimum gap between two timer reads of the GitHub card (the server caches for 90 s).
pub const GITHUB_MIN_GAP: Duration = Duration::from_secs(90);
pub const GITHUB_SAMPLE_WARN_REASON: &str = "로그인이 만료되어 재인증이 필요합니다";
pub const GITHUB_SAMPLE_OFF_REASON: &str = "gh에 로그인되어 있지 않습니다";
/// Repositories listed on the card; the server may send more.
pub const GITHUB_MAX_REPOS: usize = 3;
/// CI cells per repository, newest first.
pub const GITHUB_MAX_CI: usize = 14;
pub const LATER_PILL: &str = "준비 중입니다";
pub const LATER_LINE: &str = "12월 이후에 지원합니다";
/// The services that come after GitHub, in the mockup order.
pub const LATER_INTEGRATIONS: [&str; 5] = ["Slack", "Discord", "Linear", "캘린더", "메일"];

/// `GET /api/integrations/github` `state` (INTENT D41, FR-L35).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubConn {
    /// Connected.
    On,
    /// Connected before, needs a new login.
    Warn,
    /// Not connected (no `gh`, or not logged in).
    #[default]
    Off,
}

/// One CI run result of a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CiResult {
    Success,
    Failure,
    Cancelled,
    #[serde(other)]
    Pending,
}

/// One repository of the body. A null number is unknown; `error` replaces the numbers.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct GithubRepo {
    pub name_with_owner: String,
    pub default_branch: String,
    pub open_prs: Option<u32>,
    pub review_requested: Option<u32>,
    /// Newest first, up to 14.
    pub ci: Option<Vec<CiResult>>,
    pub error: Option<String>,
    /// `open_prs` stopped at the server's 100 limit: the real number is at least that.
    pub capped: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct GithubTotals {
    pub repos: Option<u32>,
    pub open_prs: Option<u32>,
    pub review_requested: Option<u32>,
    /// Percent, 0 to 100; null when no run is known.
    pub ci_pass_percent: Option<f32>,
}

/// Mirror of the `GET /api/integrations/github` body. The server gives no activity list.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct GithubState {
    pub state: GithubConn,
    /// Korean sentence, set for `warn` and `off`.
    pub reason: Option<String>,
    pub account: Option<String>,
    pub repos: Vec<GithubRepo>,
    pub totals: GithubTotals,
    /// Epoch seconds.
    pub checked_at: Option<i64>,
}

/// The live slot of the GitHub card: nothing asked yet, the last good answer, or a first call
/// that failed (a later failure keeps the last answer).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum GithubFetch {
    #[default]
    NotAsked,
    Loaded(GithubState),
    Failed,
}

impl GithubFetch {
    /// What the card draws from: `None` until the server answered.
    pub fn state(&self) -> Option<&GithubState> {
        match self {
            GithubFetch::Loaded(g) => Some(g),
            _ => None,
        }
    }

    /// The slot after a call: a failure keeps a held answer, else becomes `Failed`.
    pub fn settle(old: &GithubFetch, got: Result<GithubState, String>) -> GithubFetch {
        match got {
            Ok(g) => GithubFetch::Loaded(g),
            Err(_) if matches!(old, GithubFetch::Loaded(_)) => old.clone(),
            Err(_) => GithubFetch::Failed,
        }
    }
}

/// The card header line in the demo: the sample is marked, never shown as real.
pub fn sample_header(line: &str) -> String {
    format!("{line} {GITHUB_SAMPLE_MARK}")
}

/// The sample GitHub card state selected by a capture hook (FR-L35, #621).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubSampleKind {
    Ok,
    Warn,
    Off,
}

/// Pure parse of the `EMBER_CR_GITHUB` environment hook value.
pub fn parse_github_sample_hook(value: Option<&str>, active: bool) -> Option<GithubSampleKind> {
    if !active {
        return None;
    }
    match value? {
        "ok" | "on" => Some(GithubSampleKind::Ok),
        "warn" => Some(GithubSampleKind::Warn),
        "off" => Some(GithubSampleKind::Off),
        _ => None,
    }
}

/// The deterministic sample `GithubState` for capture hooks and demo mode (FR-L35, #621).
pub fn sample_github_state(kind: GithubSampleKind) -> GithubState {
    let repo = |name: &str, branch: &str, prs, rev, ci: Vec<CiResult>| GithubRepo {
        name_with_owner: name.into(),
        default_branch: branch.into(),
        open_prs: Some(prs),
        review_requested: Some(rev),
        ci: Some(ci),
        error: None,
        capped: false,
    };
    match kind {
        GithubSampleKind::Ok => GithubState {
            state: GithubConn::On,
            reason: None,
            account: Some("데모 계정".into()),
            repos: vec![
                repo("demo/ember", "develop", 4, 1, vec![CiResult::Success, CiResult::Success, CiResult::Failure, CiResult::Success]),
                repo("demo/notes", "main", 1, 0, vec![CiResult::Success, CiResult::Success, CiResult::Success]),
            ],
            totals: GithubTotals {
                repos: Some(2),
                open_prs: Some(5),
                review_requested: Some(1),
                ci_pass_percent: Some(86.0),
            },
            checked_at: None,
        },
        GithubSampleKind::Warn => GithubState {
            state: GithubConn::Warn,
            reason: Some(GITHUB_SAMPLE_WARN_REASON.into()),
            account: Some("데모 계정".into()),
            repos: vec![
                repo("demo/ember", "develop", 4, 1, vec![CiResult::Success, CiResult::Success, CiResult::Failure, CiResult::Success]),
                repo("demo/notes", "main", 1, 0, vec![CiResult::Success, CiResult::Success, CiResult::Success]),
            ],
            totals: GithubTotals {
                repos: Some(2),
                open_prs: Some(5),
                review_requested: Some(1),
                ci_pass_percent: Some(86.0),
            },
            checked_at: None,
        },
        GithubSampleKind::Off => GithubState {
            state: GithubConn::Off,
            reason: Some(GITHUB_SAMPLE_OFF_REASON.into()),
            account: None,
            repos: Vec::new(),
            totals: GithubTotals::default(),
            checked_at: None,
        },
    }
}

/// Pure helper to resolve the sample state for captures and demo.
pub fn github_sample_hook_with(value: Option<&str>, active: bool) -> Option<GithubState> {
    parse_github_sample_hook(value, active).map(sample_github_state)
}

/// Read the `EMBER_CR_GITHUB` environment variable for capture hooks.
pub fn github_sample_from_env() -> Option<GithubState> {
    github_sample_hook_with(
        std::env::var("EMBER_CR_GITHUB").ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// Whether to fetch the GitHub card (FR-L35, FR-G13, #621).
///
/// When `force` is true (Home screen mounts or is shown again), the fetch runs at once,
/// ignoring `last_fetch_age` (`GITHUB_LAST`).
/// When `force` is false (the periodic timer), the fetch only runs while Home is shown
/// and at least `min_gap` (90 s) has elapsed since the previous fetch.
/// In demo mode, neither fetches.
pub fn should_fetch_github(
    force: bool,
    route_is_home: bool,
    last_fetch_age: Option<Duration>,
    min_gap: Duration,
    demo: bool,
) -> bool {
    if demo {
        return false;
    }
    if force {
        return true;
    }
    route_is_home && last_fetch_age.is_none_or(|a| a >= min_gap)
}

/// Periodic timer check for GitHub card refresh (FR-G13).
pub fn should_poll_github(route_is_home: bool, last_fetch_age: Option<Duration>, min_gap: Duration, demo: bool) -> bool {
    should_fetch_github(false, route_is_home, last_fetch_age, min_gap, demo)
}

/// Server text shown on the card: control and bidi characters removed, whitespace collapsed,
/// capped at `max` characters with an ellipsis (FR-L35).
pub fn clean_text(s: &str, max: usize) -> String {
    let kept: String = s
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF)
                || c.is_whitespace() && *c != '\u{85}'
        })
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let one = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        one
    } else {
        let mut cut: String = one.chars().take(max.saturating_sub(1)).collect();
        cut.push('\u{2026}');
        cut
    }
}

/// Map the client's `GET /api/integrations/github` body to the card's state. Nulls stay null.
pub fn github_state_from(s: &ember_client::api::GithubSummary) -> GithubState {
    use ember_client::api::GithubState as Conn;
    let n = |v: Option<u64>| v.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    GithubState {
        state: match s.state {
            Conn::On => GithubConn::On,
            Conn::Warn => GithubConn::Warn,
            Conn::Off => GithubConn::Off,
        },
        reason: s.reason.as_deref().map(|r| clean_text(r, 120)),
        account: s.account.as_deref().map(|a| clean_text(a, 80)),
        repos: s
            .repos
            .iter()
            .map(|r| GithubRepo {
                name_with_owner: clean_text(r.name_with_owner.as_deref().unwrap_or_default(), 80),
                default_branch: clean_text(r.default_branch.as_deref().unwrap_or_default(), 80),
                open_prs: n(r.open_prs),
                review_requested: n(r.review_requested),
                ci: r.ci.as_ref().map(|v| {
                    v.iter()
                        .map(|c| match c.as_str() {
                            "success" => CiResult::Success,
                            "failure" => CiResult::Failure,
                            "cancelled" => CiResult::Cancelled,
                            _ => CiResult::Pending,
                        })
                        .collect()
                }),
                error: r.error.as_deref().map(|e| clean_text(e, 120)),
                capped: r.capped,
            })
            .collect(),
        totals: GithubTotals {
            repos: n(s.totals.repos),
            open_prs: n(s.totals.open_prs),
            review_requested: n(s.totals.review_requested),
            ci_pass_percent: s.totals.ci_pass_percent.map(|p| p as f32),
        },
        checked_at: (s.checked_at > 0).then_some(s.checked_at),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubPill {
    /// Grey: the server cannot say yet.
    Soon,
    /// Grey: not connected.
    Off,
    /// Orange: needs a new login.
    Warn,
    /// Green: connected.
    On,
}

/// One repository row: either numbers and CI cells, or the server's error.
#[derive(Clone, Debug, PartialEq)]
pub struct GithubRepoRow {
    pub name: String,
    pub open_prs: String,
    pub review_requested: String,
    /// At most [`GITHUB_MAX_CI`], newest first.
    pub ci: Vec<CiResult>,
    pub error: Option<String>,
}

/// The 방법 보기 control. Pressing it only shows `guidance`; Ember never logs in itself.
#[derive(Clone, Debug, PartialEq)]
pub struct GithubAction {
    pub label: &'static str,
    pub guidance: String,
}

/// What the GitHub card draws. Pure, so the honest states are testable.
#[derive(Clone, Debug, PartialEq)]
pub struct GithubCard {
    pub pill: &'static str,
    pub tone: GithubPill,
    /// The count line under the area header.
    pub header_line: String,
    /// The server's reason under the title (warn and off).
    pub note: Option<String>,
    pub account: Option<String>,
    pub action: Option<GithubAction>,
    /// Repositories, open PRs, review requests, CI pass rate.
    pub kpis: [(&'static str, String); 4],
    pub repos: Vec<GithubRepoRow>,
    /// "외 N개" under the listed rows when the server sent more than are shown.
    pub more_repos: Option<String>,
    pub activity_empty: &'static str,
}

/// "외 N개" when `total` repositories exist and only `shown` are listed. `total` is the
/// server's count when known, else the length of the list it sent.
pub fn more_repos_text(shown: usize, total: Option<usize>) -> Option<String> {
    let extra = total?.checked_sub(shown).filter(|n| *n > 0)?;
    Some(format!("외 {extra}개"))
}

impl GithubCard {
    pub fn from_state(state: Option<&GithubState>) -> GithubCard {
        let labels = ["저장소", "열린 PR", "리뷰 요청", "CI 통과율"];
        let none = || GITHUB_NO_INFO.to_string();
        let Some(g) = state else {
            return GithubCard {
                pill: UNAVAILABLE,
                tone: GithubPill::Soon,
                header_line: "GitHub 연결 확인 전".to_string(),
                note: None,
                account: None,
                action: None,
                kpis: labels.map(|l| (l, none())),
                repos: Vec::new(),
                more_repos: None,
                activity_empty: GITHUB_NO_ACTIVITY,
            };
        };
        let (pill, tone, header_line, label) = match g.state {
            GithubConn::On => ("연결됨", GithubPill::On, "GitHub 연결됨", None),
            GithubConn::Warn => ("재인증 필요", GithubPill::Warn, "GitHub 재인증 필요", Some(GITHUB_HOW)),
            GithubConn::Off => ("연결 안 됨", GithubPill::Off, "GitHub 연결 안 됨", Some(GITHUB_HOW)),
        };
        let note = (g.state != GithubConn::On)
            .then(|| g.reason.clone().unwrap_or_else(|| GITHUB_UNAVAILABLE_HINT.to_string()));
        let action = label.map(|label| GithubAction {
            label,
            guidance: if g.reason.as_deref().is_some_and(|r| r.contains("설치")) {
                GITHUB_INSTALL_HINT.to_string()
            } else {
                GITHUB_LOGIN_HINT.to_string()
            },
        });
        let shows_numbers = g.state != GithubConn::Off;
        // Connected: a null is asked and not known (알 수 없음). Not connected: nothing was read.
        let unknown = || if shows_numbers { GITHUB_UNKNOWN.to_string() } else { none() };
        let count = |v: Option<u32>| match v {
            Some(n) if shows_numbers => n.to_string(),
            _ => unknown(),
        };
        let any_capped = g.repos.iter().any(|r| r.capped);
        let prs = |v: Option<u32>, capped: bool| match v {
            Some(n) if shows_numbers && capped => format!("{n}+"),
            other => count(other),
        };
        let t = &g.totals;
        let rate = match t.ci_pass_percent {
            Some(r) if shows_numbers => format!("{}%", r.round() as i64),
            _ => unknown(),
        };
        let repos = if shows_numbers {
            g.repos
                .iter()
                .take(GITHUB_MAX_REPOS)
                .map(|r| GithubRepoRow {
                    name: clean_text(&r.name_with_owner, 80),
                    open_prs: prs(r.open_prs, r.capped),
                    review_requested: count(r.review_requested),
                    ci: r.ci.iter().flatten().take(GITHUB_MAX_CI).copied().collect(),
                    error: r.error.as_deref().map(|e| clean_text(e, 120)),
                })
                .collect()
        } else {
            Vec::new()
        };
        let more_repos = if shows_numbers { more_repos_text(repos.len(), t.repos.map(|n| n as usize).or(Some(g.repos.len()))) } else { None };
        GithubCard {
            pill,
            tone,
            header_line: header_line.to_string(),
            note,
            account: g.account.as_deref().map(|a| clean_text(a, 80)),
            action,
            kpis: [
                (labels[0], count(t.repos)),
                (labels[1], prs(t.open_prs, any_capped)),
                (labels[2], count(t.review_requested)),
                (labels[3], rate),
            ],
            repos,
            more_repos,
            activity_empty: GITHUB_NO_ACTIVITY,
        }
    }
}

/// What tapping the composer's attach button says (FR-L11, #500): picking files needs the
/// toolkit's file picker (compose-rust#483, planned for v0.0.8).
pub const ATTACH_UNSUPPORTED: &str = "파일 고르기는 아직 지원하지 않습니다";

pub const GREETING: &str = "무엇을 도와줄까요?";
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

    /// The one-line description under the row title (copied from mockup v8, `data.js` modes).
    pub fn description(self) -> &'static str {
        match self {
            PermMode::Ask => "파일을 고치거나 명령을 실행하기 전에 매번 묻습니다",
            PermMode::Edits => "파일 편집은 바로 하고, 명령은 실행 전에 묻습니다",
            PermMode::Bypass => "묻지 않고 모두 실행합니다. 믿을 수 있는 환경에서만 쓰세요",
            PermMode::Plan => "읽기만 하고 계획을 세운 뒤 승인을 기다립니다",
        }
    }

    /// Bypass is drawn in the warning colour so it is never chosen by accident.
    pub fn is_bypass(self) -> bool {
        self == PermMode::Bypass
    }
}

/// The modes a harness supports, in menu order (mockup v8: ask, edits, bypass, plan).
/// [provisional] until each CLI is verified (FR-L16): Claude Code has the four, Codex and
/// Gemini three.
pub fn modes_for(h: Harness) -> &'static [PermMode] {
    match h {
        Harness::ClaudeCode => &[
            PermMode::Ask,
            PermMode::Edits,
            PermMode::Bypass,
            PermMode::Plan,
        ],
        Harness::Codex | Harness::Gemini => &[PermMode::Ask, PermMode::Edits, PermMode::Bypass],
    }
}

/// One row of the permission picker (FR-L16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeRow {
    pub mode: PermMode,
    pub title: &'static str,
    pub description: &'static str,
    /// Drawn with the red shield (bypass).
    pub danger: bool,
    /// The row of the current mode, drawn with a check mark.
    pub chosen: bool,
}

/// The header of the permission picker: "권한 모드 · Claude Code".
pub fn mode_header(h: Harness) -> String {
    format!("권한 모드 \u{b7} {}", h.label())
}

/// The title of the bypass row while it waits for the second tap.
pub const BYPASS_CONFIRM_TITLE: &str = "다시 눌러 바이패스 적용";
/// The description of the bypass row while it waits for the second tap.
pub const BYPASS_CONFIRM_TEXT: &str = "한 번 더 누르면 모든 작업을 묻지 않고 실행합니다";

/// The rows of the permission picker for a harness, in menu order, with `current` marked.
pub fn mode_picker_rows(h: Harness, current: PermMode) -> Vec<ModeRow> {
    mode_picker_rows_pending(h, current, None)
}

/// [`mode_picker_rows`] with the mode that waits for its confirming tap: that row (only bypass
/// ever waits) says so in its title and description.
pub fn mode_picker_rows_pending(h: Harness, current: PermMode, pending: Option<PermMode>) -> Vec<ModeRow> {
    modes_for(h)
        .iter()
        .map(|m| {
            let confirming = pending == Some(*m) && m.is_bypass();
            ModeRow {
                mode: *m,
                title: if confirming { BYPASS_CONFIRM_TITLE } else { m.label() },
                description: if confirming { BYPASS_CONFIRM_TEXT } else { m.description() },
                danger: m.is_bypass(),
                chosen: *m == current,
            }
        })
        .collect()
}

/// A mode waiting for its confirming tap: the mode and the time (ms) of the first tap.
pub type ModePending = Option<(PermMode, i64)>;

/// A tap on a mode row at `now` ms (FR-L16): `(pending, applied)`. Bypass is applied only by a
/// second tap on the row that is already waiting and only `AMEND_CONFIRM_DELAY_MS` after the
/// first, so a fast double tap cannot skip the question (the rule of #424); an earlier second
/// tap keeps waiting. Any other row applies at once and clears the wait. Tapping the mode that
/// is already current applies nothing new and needs no confirmation.
pub fn mode_tap(current: PermMode, pending: ModePending, tapped: PermMode, now: i64) -> (ModePending, Option<PermMode>) {
    if !tapped.is_bypass() || tapped == current {
        return (None, Some(tapped));
    }
    match pending {
        Some((m, at)) if m == tapped => {
            if now - at >= crate::ui::vcs::AMEND_CONFIRM_DELAY_MS {
                (None, Some(tapped))
            } else {
                (pending, None)
            }
        }
        _ => (Some((tapped, now)), None),
    }
}

/// Pointing at another row withdraws the confirmation.
pub fn mode_hover(pending: ModePending, hovered: PermMode) -> ModePending {
    pending.filter(|(p, _)| *p == hovered)
}

/// Width of the shell's navigation at a window width, by the layout that width asks for.
pub fn nav_width_for(window_dp: f32) -> f32 {
    use crate::ui::shell::{nav_mode, ShellMode};
    match nav_mode(window_dp, false) {
        ShellMode::BottomTabs => 0.0,
        ShellMode::Rail => crate::ui::theme::RAIL_W,
        ShellMode::Sidebar => crate::ui::theme::SIDE_W,
    }
}

/// The width a popover gets: what it wants, but never more than the window leaves beside the
/// navigation, less a 24 dp margin (and not below 280). The toolkit keeps a popup's start
/// edge inside the window, so a popover that fits never crosses the window's edge.
pub fn popover_width(want: f32, window_dp: f32) -> f32 {
    want.min(window_dp - nav_width_for(window_dp) - 24.0).max(280.0)
}

/// The composer's chips in order: project, model, mode, computer.
pub const CHIP_COUNT: usize = 4;
/// The most text (composer pixels, [`composer_text_px`]) each chip shows: project 170, model 240
/// ("Claude Code · Opus 5.5 · 높음" measures about 215), mode 140, computer 150.
pub const CHIP_TEXT_CAPS: [f32; CHIP_COUNT] = [170.0, 240.0, 140.0, 150.0];
/// The least text each chip keeps when a line is very narrow: project and computer four Korean
/// glyphs (4 x 13.5), model and mode enough for "Claud...".
pub const CHIP_TEXT_MINS: [f32; CHIP_COUNT] = [54.0, 40.0, 40.0, 54.0];
/// Padding 8, icon 14, gap 4, chevron 12 with its gap 4 and padding 8 around the text (13 dp chips, cr/home.rs ComposerChip).
const CHIP_FRAME: f32 = 50.0;
const CHIP_GAP: f32 = 6.0;

/// A label's width at the composer's chip font (13 dp, weight 500; measured on the PR #629 captures): about 13.5 dp per Korean glyph and 6.8 per ASCII
/// character or middle dot. The send sheet's estimate (`files::chip_text_px`, 16 and 9) is
/// deliberately more pessimistic and stays as it is.
pub fn composer_text_px(text: &str) -> f32 {
    text.chars().map(|c| if c.is_ascii() || c == '\u{b7}' { 6.8 } else { 13.5 }).sum()
}

/// `text` cut with an ellipsis so [`composer_text_px`] stays within `max_px`.
pub fn composer_fit_text(text: &str, max_px: f32) -> String {
    if composer_text_px(text) <= max_px {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        let next = format!("{out}{c}\u{2026}");
        if composer_text_px(&next) > max_px {
            break;
        }
        out.push(c);
    }
    out.push('\u{2026}');
    out
}

/// The width the chips may use in a composer card `composer_w` wide: less its 16 padding either
/// side, the 36 dp add and send buttons and the gaps between them.
pub fn composer_chips_avail(composer_w: f32) -> f32 {
    composer_w - 32.0 - 72.0 - 2.0 * CHIP_GAP
}

/// The text width each chip may use (project, model, mode, computer): its cap, or what a whole
/// line leaves if that is less, never below the minimum. A label is cut only at its cap; when
/// the chips do not fit one line the row wraps ([`composer_chip_rows`]) rather than cutting
/// labels that have room or dropping a chip.
pub fn composer_chip_budget(composer_w: f32) -> [f32; CHIP_COUNT] {
    let line = composer_chips_avail(composer_w) - CHIP_FRAME;
    let mut out = CHIP_TEXT_CAPS;
    for (o, m) in out.iter_mut().zip(CHIP_TEXT_MINS) {
        *o = o.min(line).max(m);
    }
    out
}

/// The chips (by index) per line, `widths` being each chip's full width: a line breaks when the
/// next chip would not fit, so a narrow card shows two lines of chips instead of dropping any.
pub fn composer_chip_rows(widths: &[f32], composer_w: f32) -> Vec<Vec<usize>> {
    crate::ui::files::chip_rows(widths, CHIP_GAP, composer_chips_avail(composer_w))
}

/// A chip's full width for `text` (icon, padding and the text).
pub fn composer_chip_width(text: &str) -> f32 {
    CHIP_FRAME + composer_text_px(text)
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
    /// "Opus 5.5 (높음)", the row title; a harness without effort shows the name alone.
    pub fn row_title(&self) -> String {
        match self.effort {
            Some(e) => format!("{} ({})", self.name, effort_name(e)),
            None => self.name.to_string(),
        }
    }

    /// "Claude Code · Opus 5.5 · 높음", the chip text.
    pub fn chip_label(&self) -> String {
        match self.effort {
            Some(e) => format!("{} · {} · {}", self.harness.label(), self.name, effort_name(e)),
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

/// The Korean name of an effort, as in mockup v8 (낮음, 보통, 높음). The English key stays the
/// id handed to the agent and kept in settings; only what the user reads is Korean (FR-L15).
/// An unknown key is shown as it is.
pub fn effort_name(key: &str) -> &str {
    match key {
        "Low" => "낮음",
        "Medium" => "보통",
        "High" => "높음",
        other => other,
    }
}

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
                    "{} {} {} {} {}",
                    o.name,
                    o.model,
                    o.harness.label(),
                    o.effort.unwrap_or(""),
                    o.effort.map(effort_name).unwrap_or("")
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
    "develop CI 실패 요약해줘",
    "열린 PR 리뷰해줘",
    "이번 주 사용량 정리해줘",
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
pub fn day_number(ymd: &str) -> Option<i64> {
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
// Dashboard
// ---------------------------------------------------------------------------------------------

/// Everything the dashboard reads from the client in one pass.
pub struct Dash {
    pub counts: StatusCounts,
    pub online: usize,
    pub computers: usize,
    pub harness: Vec<HarnessBar>,
    pub crew: Vec<CrewMember>,
    pub projects: Vec<ProjectTile>,
}

pub fn read_dash(state: &State) -> Dash {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> GithubRepo {
        GithubRepo {
            name_with_owner: name.into(),
            default_branch: "main".into(),
            open_prs: Some(2),
            review_requested: Some(0),
            ci: Some(vec![CiResult::Success, CiResult::Failure]),
            error: None,
            capped: false,
        }
    }

    fn ready_github() -> GithubState {
        GithubState {
            state: GithubConn::On,
            reason: None,
            account: Some("octo".into()),
            repos: vec![repo("octo/a")],
            totals: GithubTotals {
                repos: Some(3),
                open_prs: None,
                review_requested: Some(0),
                ci_pass_percent: None,
            },
            checked_at: Some(1_700_000_000),
        }
    }

    #[test]
    fn fr_l35_github_card_without_server_is_coming_soon() {
        let c = GithubCard::from_state(None);
        assert_eq!(c.pill, "준비 중입니다");
        assert_eq!(c.tone, GithubPill::Soon);
        assert_eq!(c.header_line, "GitHub 연결 확인 전");
        assert!(!c.header_line.chars().any(|ch| ch.is_ascii_digit()), "no count");
        assert!(c.kpis.iter().all(|(_, v)| v == "아직 정보가 없습니다"));
        assert!(c.repos.is_empty() && c.action.is_none());
        assert_eq!(c.activity_empty, "연동이 만든 일이 아직 없습니다");
    }

    #[test]
    fn fr_l35_github_card_on_shows_numbers_and_null_is_no_information() {
        let g = ready_github();
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.pill, "연결됨");
        assert_eq!(c.tone, GithubPill::On);
        assert_eq!(c.account.as_deref(), Some("octo"));
        assert!(c.note.is_none() && c.action.is_none());
        assert_eq!(c.kpis[0].1, "3");
        assert_eq!(c.kpis[1].1, "알 수 없음", "null is not 0");
        assert_eq!(c.kpis[2].1, "0", "a real zero is shown as 0");
        assert_eq!(c.kpis[3].1, "알 수 없음");
        let mut g = ready_github();
        g.totals.ci_pass_percent = Some(87.6);
        assert_eq!(GithubCard::from_state(Some(&g)).kpis[3].1, "88%");
    }

    #[test]
    fn fr_l35_github_card_warn_shows_relogin_and_the_hint() {
        let mut g = ready_github();
        g.state = GithubConn::Warn;
        g.reason = Some("로그인이 만료되었습니다".into());
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.pill, "재인증 필요");
        assert_eq!(c.tone, GithubPill::Warn);
        assert_eq!(c.note.as_deref(), Some("로그인이 만료되었습니다"));
        let a = c.action.unwrap();
        assert_eq!(a.label, "방법 보기");
        assert_eq!(a.guidance, "이 컴퓨터의 터미널에서 gh auth login을 실행하십시오");
    }

    #[test]
    fn fr_l35_github_card_warn_kpis_read_unknown() {
        let mut g = ready_github();
        g.state = GithubConn::Warn;
        g.totals = GithubTotals::default();
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.pill, "재인증 필요");
        assert!(c.kpis.iter().all(|(_, v)| v == "알 수 없음"));
    }

    #[test]
    fn fr_l35_github_text_is_cleaned_and_capped() {
        assert_eq!(clean_text("a\u{202E}b\u{0007}c\n\t d", 80), "abc d");
        assert_eq!(clean_text("  x   y ", 80), "x y");
        let long = "가".repeat(200);
        let r = clean_text(&long, 120);
        assert_eq!(r.chars().count(), 120);
        assert!(r.ends_with('\u{2026}'));
        assert_eq!(clean_text(&"n".repeat(80), 80).chars().count(), 80);
        assert_eq!(clean_text(&"n".repeat(81), 80).chars().count(), 80);
    }

    #[test]
    fn fr_l35_github_card_off_says_how_and_shows_no_numbers() {
        let mut g = ready_github();
        g.state = GithubConn::Off;
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.pill, "연결 안 됨");
        assert_eq!(c.action.as_ref().unwrap().label, "방법 보기");
        assert!(c.action.as_ref().unwrap().guidance.contains("gh auth login"));
        assert!(c.kpis.iter().all(|(_, v)| v == "아직 정보가 없습니다"));
        assert!(c.repos.is_empty());
        assert!(!c.header_line.contains("연결됨"));
        g.reason = Some("gh가 설치되어 있지 않습니다. 설치해 주십시오".into());
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.action.unwrap().guidance, "gh를 설치하고 로그인하십시오");
        assert_eq!(c.note.as_deref(), Some("gh가 설치되어 있지 않습니다. 설치해 주십시오"));
    }

    #[test]
    fn fr_l35_github_card_repo_error_row_replaces_numbers() {
        let mut g = ready_github();
        g.repos[0].error = Some("저장소를 읽지 못했습니다".into());
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.repos[0].error.as_deref(), Some("저장소를 읽지 못했습니다"));
        assert_eq!(c.repos[0].name, "octo/a");
    }

    #[test]
    fn fr_l35_github_card_ci_cells_newest_first_and_at_most_14() {
        let mut g = ready_github();
        g.repos[0].ci = Some((0..20).map(|i| if i == 0 { CiResult::Failure } else { CiResult::Success }).collect());
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.repos[0].ci.len(), 14);
        assert_eq!(c.repos[0].ci[0], CiResult::Failure, "newest first");
        g.repos[0].ci = None;
        assert!(GithubCard::from_state(Some(&g)).repos[0].ci.is_empty());
    }

    #[test]
    fn fr_l35_github_card_lists_at_most_three_repos() {
        let mut g = ready_github();
        g.repos = (0..5).map(|i| repo(&format!("octo/r{i}"))).collect();
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.repos.len(), 3);
        assert_eq!(c.repos[2].name, "octo/r2");
        assert_eq!(c.repos[0].open_prs, "2");
    }

    #[test]
    fn fr_l35_github_card_says_how_many_more_repos() {
        assert_eq!(more_repos_text(3, Some(8)).as_deref(), Some("외 5개"));
        assert_eq!(more_repos_text(3, Some(3)), None);
        assert_eq!(more_repos_text(2, None), None);
        let mut g = ready_github();
        g.repos = (0..5).map(|i| repo(&format!("octo/r{i}"))).collect();
        g.totals.repos = Some(9);
        assert_eq!(GithubCard::from_state(Some(&g)).more_repos.as_deref(), Some("외 6개"));
        g.totals.repos = None;
        assert_eq!(GithubCard::from_state(Some(&g)).more_repos.as_deref(), Some("외 2개"));
        g.repos.truncate(2);
        assert_eq!(GithubCard::from_state(Some(&g)).more_repos, None);
    }

    fn summary() -> ember_client::api::GithubSummary {
        use ember_client::api as a;
        a::GithubSummary {
            state: a::GithubState::Warn,
            reason: Some("재로그인".into()),
            account: Some("octo".into()),
            repos: vec![a::GithubRepo {
                name_with_owner: Some("o/r".into()),
                default_branch: Some("main".into()),
                open_prs: Some(100),
                review_requested: None,
                capped: true,
                ci: Some(vec!["success".into(), "failure".into(), "cancelled".into(), "queued".into()]),
                error: None,
                projects: vec![],
            }],
            totals: a::GithubTotals { repos: Some(1), open_prs: Some(100), review_requested: None, ci_pass_percent: None },
            checked_at: 1_700_000_000,
        }
    }

    #[test]
    fn fr_l35_github_mapping_keeps_state_ci_and_nulls() {
        let g = github_state_from(&summary());
        assert_eq!(g.state, GithubConn::Warn);
        assert_eq!(g.reason.as_deref(), Some("재로그인"));
        assert_eq!(g.repos[0].name_with_owner, "o/r");
        assert_eq!(g.repos[0].review_requested, None);
        assert_eq!(
            g.repos[0].ci.as_deref(),
            Some(&[CiResult::Success, CiResult::Failure, CiResult::Cancelled, CiResult::Pending][..])
        );
        assert_eq!(g.totals.ci_pass_percent, None);
        assert_eq!(g.checked_at, Some(1_700_000_000));
    }

    #[test]
    fn fr_l35_github_mapping_capped_reads_100_plus_and_null_reads_unknown() {
        let g = github_state_from(&summary());
        let c = GithubCard::from_state(Some(&g));
        assert_eq!(c.repos[0].open_prs, "100+");
        assert_eq!(c.kpis[1].1, "100+");
        assert_eq!(c.repos[0].review_requested, "알 수 없음");
        assert_eq!(c.kpis[3].1, "알 수 없음");
    }

    #[test]
    fn fr_l35_github_mapping_off_keeps_no_information() {
        let mut s = summary();
        s.state = ember_client::api::GithubState::Off;
        s.repos.clear();
        s.totals = Default::default();
        let c = GithubCard::from_state(Some(&github_state_from(&s)));
        assert!(c.kpis.iter().all(|(_, v)| v == "아직 정보가 없습니다"));
    }

    #[test]
    fn fr_l35_github_fetch_failure_keeps_the_last_value() {
        let held = GithubFetch::Loaded(ready_github());
        assert_eq!(GithubFetch::settle(&held, Err("x".into())), held);
        assert_eq!(GithubFetch::settle(&GithubFetch::NotAsked, Err("x".into())), GithubFetch::Failed);
        assert!(GithubFetch::Failed.state().is_none());
        assert!(GithubCard::from_state(GithubFetch::Failed.state()).kpis.iter().all(|(_, v)| v == "아직 정보가 없습니다"));
    }

    #[test]
    fn fr_l35_github_sample_header_is_marked() {
        assert_eq!(sample_header("GitHub 연결됨"), "GitHub 연결됨 (예시)");
    }

    #[test]
    fn fr_l35_github_fetch_force_on_mount_ignores_cooldown_while_timer_respects_gap() {
        let gap = Duration::from_secs(90);
        // Force on mount / shown again: always fetches at once, ignoring GITHUB_LAST age.
        assert!(should_fetch_github(true, true, Some(Duration::from_secs(10)), gap, false));
        assert!(should_fetch_github(true, true, Some(Duration::ZERO), gap, false));
        assert!(should_fetch_github(true, true, None, gap, false));
        assert!(should_fetch_github(true, false, Some(Duration::from_secs(10)), gap, false));
        // Demo never fetches from network, even with force.
        assert!(!should_fetch_github(true, true, Some(Duration::from_secs(10)), gap, true));

        // Periodic timer (force = false): only fetches on Home, after 90 s gap, not in demo.
        assert!(!should_fetch_github(false, true, Some(Duration::from_secs(10)), gap, false));
        assert!(should_fetch_github(false, true, Some(Duration::from_secs(90)), gap, false));
        assert!(should_fetch_github(false, true, Some(Duration::from_secs(100)), gap, false));
        assert!(should_fetch_github(false, true, None, gap, false));
        assert!(!should_fetch_github(false, false, Some(Duration::from_secs(100)), gap, false));
        assert!(!should_fetch_github(false, true, Some(Duration::from_secs(100)), gap, true));
    }

    #[test]
    fn fr_l35_github_account_default_branch_and_error_are_cleaned_and_capped() {
        use ember_client::api as a;
        let dirty_account = "  octo\u{202E}user\t \n name  ";
        let long_account = "a".repeat(100);
        let dirty_branch = "  feature/\u{0007}branch\n ";
        let long_branch = "b".repeat(100);
        let dirty_error = " 오류:\u{202A} 실패했습니다 \t";
        let long_error = "오류: ".to_string() + &"가".repeat(200);

        let mut s = summary();
        s.account = Some(dirty_account.into());
        s.repos[0].default_branch = Some(dirty_branch.into());
        s.repos[0].error = Some(dirty_error.into());

        let g = github_state_from(&s);
        assert_eq!(g.account.as_deref(), Some("octouser name"));
        assert_eq!(g.repos[0].default_branch, "feature/branch");
        assert_eq!(g.repos[0].error.as_deref(), Some("오류: 실패했습니다"));

        let card = GithubCard::from_state(Some(&g));
        assert_eq!(card.account.as_deref(), Some("octouser name"));
        assert_eq!(card.repos[0].error.as_deref(), Some("오류: 실패했습니다"));

        // Long text is capped with an ellipsis (80 for account and branch, 120 for error).
        s.account = Some(long_account);
        s.repos[0].default_branch = Some(long_branch);
        s.repos[0].error = Some(long_error);

        let g2 = github_state_from(&s);
        assert_eq!(g2.account.as_deref().unwrap().chars().count(), 80);
        assert!(g2.account.as_deref().unwrap().ends_with('\u{2026}'));
        assert_eq!(g2.repos[0].default_branch.chars().count(), 80);
        assert!(g2.repos[0].default_branch.ends_with('\u{2026}'));
        assert_eq!(g2.repos[0].error.as_deref().unwrap().chars().count(), 120);
        assert!(g2.repos[0].error.as_deref().unwrap().ends_with('\u{2026}'));

        let card2 = GithubCard::from_state(Some(&g2));
        assert_eq!(card2.account.as_deref().unwrap().chars().count(), 80);
        assert!(card2.account.as_deref().unwrap().ends_with('\u{2026}'));
        assert_eq!(card2.repos[0].error.as_deref().unwrap().chars().count(), 120);
        assert!(card2.repos[0].error.as_deref().unwrap().ends_with('\u{2026}'));
    }

    #[test]
    fn fr_l35_github_sample_hook_selects_ok_warn_off_states() {
        assert_eq!(parse_github_sample_hook(Some("ok"), true), Some(GithubSampleKind::Ok));
        assert_eq!(parse_github_sample_hook(Some("on"), true), Some(GithubSampleKind::Ok));
        assert_eq!(parse_github_sample_hook(Some("warn"), true), Some(GithubSampleKind::Warn));
        assert_eq!(parse_github_sample_hook(Some("off"), true), Some(GithubSampleKind::Off));
        assert_eq!(parse_github_sample_hook(Some("other"), true), None);
        assert_eq!(parse_github_sample_hook(None, true), None);
        assert_eq!(parse_github_sample_hook(Some("ok"), false), None);
        assert_eq!(parse_github_sample_hook(Some("warn"), false), None);
        assert!(github_sample_hook_with(Some("ok"), false).is_none());
        assert!(github_sample_hook_with(Some("warn"), false).is_none());
        assert!(github_sample_hook_with(Some("off"), false).is_none());

        // Ok sample state: connected, numbers shown, no action note.
        let ok = sample_github_state(GithubSampleKind::Ok);
        assert_eq!(ok.state, GithubConn::On);
        assert_eq!(ok.account.as_deref(), Some("데모 계정"));
        assert_eq!(ok.repos.len(), 2);
        let card_ok = GithubCard::from_state(Some(&ok));
        assert_eq!(card_ok.pill, "연결됨");
        assert_eq!(card_ok.tone, GithubPill::On);
        assert_eq!(card_ok.note, None);
        assert_eq!(card_ok.action, None);
        assert_eq!(sample_header(&card_ok.header_line), "GitHub 연결됨 (예시)");

        // Warn sample state: reauth needed, shows note and 방법 보기 action.
        let warn = sample_github_state(GithubSampleKind::Warn);
        assert_eq!(warn.state, GithubConn::Warn);
        assert_eq!(warn.account.as_deref(), Some("데모 계정"));
        assert!(warn.reason.is_some());
        let card_warn = GithubCard::from_state(Some(&warn));
        assert_eq!(card_warn.pill, "재인증 필요");
        assert_eq!(card_warn.tone, GithubPill::Warn);
        assert_eq!(card_warn.note.as_deref(), Some(GITHUB_SAMPLE_WARN_REASON));
        assert_eq!(card_warn.action.as_ref().unwrap().label, "방법 보기");
        assert_eq!(sample_header(&card_warn.header_line), "GitHub 재인증 필요 (예시)");

        // Off sample state: not connected, no repos, kpis unread, shows note and guidance.
        let off = sample_github_state(GithubSampleKind::Off);
        assert_eq!(off.state, GithubConn::Off);
        assert_eq!(off.account, None);
        assert!(off.repos.is_empty());
        let card_off = GithubCard::from_state(Some(&off));
        assert_eq!(card_off.pill, "연결 안 됨");
        assert_eq!(card_off.tone, GithubPill::Off);
        assert_eq!(card_off.note.as_deref(), Some(GITHUB_SAMPLE_OFF_REASON));
        assert_eq!(card_off.action.as_ref().unwrap().label, "방법 보기");
        assert!(card_off.kpis.iter().all(|(_, v)| v == "아직 정보가 없습니다"));
        assert!(card_off.repos.is_empty());
        assert_eq!(sample_header(&card_off.header_line), "GitHub 연결 안 됨 (예시)");
    }

    #[test]
    fn fr_l35_github_state_decodes_the_server_body() {
        let g: GithubState = serde_json::from_str(
            r#"{"state":"warn","reason":"재로그인","account":null,
            "repos":[{"name_with_owner":"o/r","default_branch":"main","open_prs":null,
            "review_requested":1,"ci":["success","failure","cancelled","pending"],"error":null}],
            "totals":{"repos":1,"open_prs":0,"review_requested":1,"ci_pass_percent":null},
            "checked_at":1700000000}"#,
        )
        .unwrap();
        assert_eq!(g.state, GithubConn::Warn);
        assert_eq!(g.repos[0].open_prs, None);
        assert_eq!(
            g.repos[0].ci.as_deref(),
            Some(&[CiResult::Success, CiResult::Failure, CiResult::Cancelled, CiResult::Pending][..])
        );
        assert_eq!(g.totals.ci_pass_percent, None);
        assert_eq!(g.checked_at, Some(1_700_000_000));
    }

    #[test]
    fn fr_l23_other_integrations_are_coming_soon() {
        assert_eq!(LATER_INTEGRATIONS, ["Slack", "Discord", "Linear", "캘린더", "메일"]);
        assert_eq!(LATER_PILL, "준비 중입니다");
        assert_eq!(LATER_LINE, "12월 이후에 지원합니다");
        assert_eq!(INTEGRATIONS_TITLE, "연동");
        for s in [LATER_LINE, GITHUB_LOGIN_HINT, GITHUB_INSTALL_HINT, GITHUB_NO_INFO, GITHUB_NO_ACTIVITY] {
            assert!(!s.contains('\u{2014}'));
        }
    }

    #[test]
    fn fr_l11_attach_is_disabled_with_a_reason() {
        assert_ne!(ATTACH_UNSUPPORTED, UNAVAILABLE);
        assert!(ATTACH_UNSUPPORTED.contains("지원하지 않습니다"));
        assert!(ATTACH_UNSUPPORTED.ends_with("습니다"));
    }

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
    fn fr_l16_every_harness_lists_its_default_and_bypass_before_plan() {
        for h in Harness::ALL {
            let m = modes_for(h);
            assert!(m.contains(&default_mode(h)));
            // Mockup v8 order: ask, edits, bypass, then plan where the harness has it.
            let bypass = m.iter().position(|x| x.is_bypass()).unwrap();
            if let Some(plan) = m.iter().position(|x| *x == PermMode::Plan) {
                assert!(bypass < plan);
            } else {
                assert_eq!(bypass, m.len() - 1);
            }
            assert_eq!(m.iter().filter(|x| x.is_bypass()).count(), 1);
        }
    }

    #[test]
    fn fr_l16_picker_order_is_ask_edits_bypass_plan() {
        let rows = mode_picker_rows(Harness::ClaudeCode, PermMode::Ask);
        let order: Vec<PermMode> = rows.iter().map(|r| r.mode).collect();
        assert_eq!(order, [PermMode::Ask, PermMode::Edits, PermMode::Bypass, PermMode::Plan]);
        let codex: Vec<PermMode> = mode_picker_rows(Harness::Codex, PermMode::Ask).iter().map(|r| r.mode).collect();
        assert_eq!(codex, [PermMode::Ask, PermMode::Edits, PermMode::Bypass]);
    }

    #[test]
    fn fr_l16_every_row_has_a_korean_description() {
        for h in Harness::ALL {
            for r in mode_picker_rows(h, PermMode::Ask) {
                assert!(!r.description.is_empty());
                assert!(r.description.chars().any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c)), "{}", r.description);
                assert_ne!(r.description, r.title);
            }
        }
    }

    #[test]
    fn fr_l16_only_the_bypass_row_is_danger() {
        for h in Harness::ALL {
            let rows = mode_picker_rows(h, PermMode::Ask);
            assert_eq!(rows.iter().filter(|r| r.danger).count(), 1);
            assert!(rows.iter().find(|r| r.danger).unwrap().mode == PermMode::Bypass);
        }
    }

    #[test]
    fn fr_l16_exactly_the_current_mode_is_marked_chosen() {
        for m in modes_for(Harness::ClaudeCode) {
            let rows = mode_picker_rows(Harness::ClaudeCode, *m);
            assert_eq!(rows.iter().filter(|r| r.chosen).count(), 1);
            assert_eq!(rows.iter().find(|r| r.chosen).unwrap().mode, *m);
        }
        // A mode the harness lacks marks nothing.
        assert!(mode_picker_rows(Harness::Codex, PermMode::Plan).iter().all(|r| !r.chosen));
    }

    const T0: i64 = 10_000;

    #[test]
    fn fr_l16_first_tap_on_bypass_only_asks_for_confirmation() {
        let (pending, applied) = mode_tap(PermMode::Ask, None, PermMode::Bypass, T0);
        assert_eq!((pending, applied), (Some((PermMode::Bypass, T0)), None));
    }

    #[test]
    fn fr_l16_a_fast_second_tap_keeps_waiting() {
        let armed = Some((PermMode::Bypass, T0));
        let (pending, applied) = mode_tap(PermMode::Ask, armed, PermMode::Bypass, T0 + 100);
        assert_eq!((pending, applied), (armed, None));
        let last = crate::ui::vcs::AMEND_CONFIRM_DELAY_MS - 1;
        assert_eq!(mode_tap(PermMode::Ask, armed, PermMode::Bypass, T0 + last).1, None);
    }

    #[test]
    fn fr_l16_second_tap_after_600_ms_applies_bypass() {
        let armed = Some((PermMode::Bypass, T0));
        assert_eq!(mode_tap(PermMode::Ask, armed, PermMode::Bypass, T0 + 700), (None, Some(PermMode::Bypass)));
        assert_eq!(
            mode_tap(PermMode::Ask, armed, PermMode::Bypass, T0 + crate::ui::vcs::AMEND_CONFIRM_DELAY_MS).1,
            Some(PermMode::Bypass)
        );
    }

    #[test]
    fn fr_l16_tapping_another_row_applies_it_and_clears_the_wait() {
        for m in [PermMode::Ask, PermMode::Edits, PermMode::Plan] {
            assert_eq!(mode_tap(PermMode::Ask, Some((PermMode::Bypass, T0)), m, T0 + 50), (None, Some(m)));
            assert_eq!(mode_tap(PermMode::Ask, None, m, T0), (None, Some(m)));
        }
    }

    #[test]
    fn fr_l16_tapping_the_current_bypass_needs_no_confirmation() {
        assert_eq!(mode_tap(PermMode::Bypass, None, PermMode::Bypass, T0), (None, Some(PermMode::Bypass)));
    }

    #[test]
    fn fr_l16_pointing_at_another_row_withdraws_the_confirmation() {
        let armed = Some((PermMode::Bypass, T0));
        assert_eq!(mode_hover(armed, PermMode::Ask), None);
        assert_eq!(mode_hover(armed, PermMode::Bypass), armed);
        assert_eq!(mode_hover(None, PermMode::Ask), None);
    }

    #[test]
    fn fr_l16_a_waiting_bypass_row_says_so() {
        let rows = mode_picker_rows_pending(Harness::ClaudeCode, PermMode::Ask, Some(PermMode::Bypass));
        let b = rows.iter().find(|r| r.mode == PermMode::Bypass).unwrap();
        assert_eq!(b.title, "다시 눌러 바이패스 적용");
        assert_eq!(b.description, "한 번 더 누르면 모든 작업을 묻지 않고 실행합니다");
        assert!(b.danger);
        let others = rows.iter().filter(|r| r.mode != PermMode::Bypass);
        assert!(others.clone().all(|r| r.title == r.mode.label()));
        let idle = mode_picker_rows(Harness::ClaudeCode, PermMode::Ask);
        assert_eq!(idle.iter().find(|r| r.danger).unwrap().title, PermMode::Bypass.label());
    }

    #[test]
    fn fr_l15_popover_width_fits_the_window_beside_the_navigation() {
        // 561: rail 72 + margin 24 leaves 465. 700: 604 leaves room for 560. 900 and 1024: rail.
        // 1440: sidebar 234.
        assert_eq!(popover_width(560.0, 561.0), 465.0);
        assert_eq!(popover_width(560.0, 700.0), 560.0);
        assert_eq!(popover_width(560.0, 900.0), 560.0);
        assert_eq!(popover_width(560.0, 1024.0), 560.0);
        assert_eq!(popover_width(560.0, 1440.0), 560.0);
        assert_eq!(popover_width(340.0, 561.0), 340.0);
        for w in [561.0_f32, 600.0, 700.0, 900.0, 1024.0, 1440.0] {
            assert!(popover_width(560.0, w) + nav_width_for(w) <= w);
        }
    }

    #[test]
    fn fr_l15_chip_budget_gives_every_chip_its_icon_and_some_text() {
        for w in [200.0_f32, 300.0, 400.0, 450.0, 520.0, 560.0, 640.0, 720.0] {
            let b = composer_chip_budget(w);
            for i in 0..CHIP_COUNT {
                assert!(b[i] >= CHIP_TEXT_MINS[i] && b[i] <= CHIP_TEXT_CAPS[i], "{w} {i} {}", b[i]);
            }
            // Project and computer keep at least four Korean glyphs.
            assert!(b[0] >= 54.0 && b[3] >= 54.0);
        }
    }

    #[test]
    fn fr_l15_chip_budget_is_the_caps_when_a_line_has_room() {
        let b = composer_chip_budget(718.0);
        assert_eq!(b, CHIP_TEXT_CAPS);
        assert_eq!(b[1], 240.0);
        assert_eq!(b[2], 140.0);
    }

    #[test]
    fn fr_l15_composer_estimate_matches_the_measured_label() {
        let w = composer_text_px("Claude Code \u{b7} Opus 5.5 \u{b7} 높음");
        assert!((180.0..=200.0).contains(&w), "{w}");
        // The send sheet's pessimistic estimate is a different, larger function.
        assert!(crate::ui::files::chip_text_px("가나다") > composer_text_px("가나다"));
    }

    /// The demo labels of the composer chips.
    fn demo_chip_labels() -> [String; CHIP_COUNT] {
        [
            DEFAULT_PROJECT.to_string(),
            default_option(Harness::ClaudeCode).chip_label(),
            PermMode::Ask.label().to_string(),
            "이 컴퓨터".to_string(),
        ]
    }

    #[test]
    fn fr_l15_default_demo_labels_are_not_truncated_at_718_and_640() {
        for w in [718.0_f32, 640.0, 700.0] {
            let b = composer_chip_budget(w);
            for (t, max) in demo_chip_labels().iter().zip(b) {
                assert_eq!(&composer_fit_text(t, max), t, "{w} {t}");
            }
        }
    }

    #[test]
    fn fr_l15_demo_labels_fit_one_line_at_900_and_two_at_718() {
        // The 13 dp chips carry an icon and a chevron (CHIP_FRAME 50), so the four demo chips need
        // about 660 dp: one line from a 900 dp card, and two lines (never cut) at 718.
        let rows = |w: f32| {
            let b = composer_chip_budget(w);
            let widths: Vec<f32> = demo_chip_labels().iter().zip(b).map(|(t, m)| composer_chip_width(&composer_fit_text(t, m))).collect();
            composer_chip_rows(&widths, w).len()
        };
        assert_eq!(rows(900.0), 1);
        assert_eq!(rows(718.0), 2);
    }

    #[test]
    fn fr_l15_fit_text_cuts_with_an_ellipsis_only_over_the_budget() {
        // The ellipsis counts like a Korean glyph (13.5): two glyphs plus it would be 40.5 > 40.
        assert_eq!(composer_fit_text("가나다라마바사", 40.0), "가\u{2026}");
        assert_eq!(composer_fit_text("가나다라마바사", 41.0), "가나\u{2026}");
        assert_eq!(composer_fit_text("abc", 100.0), "abc");
    }

    #[test]
    fn fr_l15_chip_rows_wrap_instead_of_dropping_a_chip() {
        for w in [300.0_f32, 340.0, 400.0, 480.0, 560.0, 720.0] {
            let b = composer_chip_budget(w);
            let widths: Vec<f32> = b.iter().map(|t| CHIP_FRAME + t).collect();
            let rows = composer_chip_rows(&widths, w);
            let mut all: Vec<usize> = rows.iter().flatten().copied().collect();
            all.sort();
            assert_eq!(all, vec![0, 1, 2, 3], "{w}");
        }
        // 300 dp is too narrow for four chips in one line.
        let b = composer_chip_budget(300.0);
        let widths: Vec<f32> = b.iter().map(|t| CHIP_FRAME + t).collect();
        assert!(composer_chip_rows(&widths, 300.0).len() > 1);
    }

    #[test]
    fn fr_l15_chip_width_counts_the_frame_and_the_text() {
        assert_eq!(composer_chip_width(""), CHIP_FRAME);
        assert_eq!(composer_chip_width("가나"), CHIP_FRAME + 27.0);
    }

    #[test]
    fn fr_l16_header_names_the_harness() {
        assert_eq!(mode_header(Harness::ClaudeCode), format!("권한 모드 \u{b7} {}", Harness::ClaudeCode.label()));
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
    fn fr_l15_effort_names_are_korean() {
        assert_eq!(effort_name("Low"), "낮음");
        assert_eq!(effort_name("Medium"), "보통");
        assert_eq!(effort_name("High"), "높음");
        assert_eq!(effort_name("Max"), "Max");
        for e in EFFORTS {
            assert_ne!(effort_name(e), e);
        }
    }

    #[test]
    fn fr_l15_no_row_or_chip_shows_an_english_effort() {
        for o in model_catalog() {
            for t in [o.row_title(), o.chip_label()] {
                for en in EFFORTS {
                    assert!(!t.contains(en), "{t}");
                }
            }
        }
    }

    #[test]
    fn fr_l15_search_finds_a_row_by_its_korean_effort() {
        let all = model_catalog();
        let hits = filter_models(&all, None, "높음");
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|o| o.effort == Some("High")));
    }

    #[test]
    fn fr_l15_chip_and_row_text() {
        let o = default_option(Harness::ClaudeCode);
        assert_eq!(o.chip_label(), "Claude Code · Opus 5.5 · 높음");
        assert_eq!(o.row_title(), "Opus 5.5 (높음)");
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
