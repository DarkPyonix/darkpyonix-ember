//! The 에이전트 screen (FR-L20, FR-L17, `docs/DESIGN.md` section 6.7): harnesses, models,
//! profiles, network and usage.
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer. The components in the second half only hand them to dioxus-compose.
//!
//! Only data the client holds is shown as fact: the detected agents (installed, version),
//! the accounts, the ChatGPT accounts' tokens today and the computers' reachability. The server
//! has no settings, profile, egress or usage API yet, so every setting here lives in a local
//! store ([`LOCAL_STORE_NOTE`]) and every missing figure says "준비 중입니다". No number or
//! account is invented.

use ember_client::state::Reachability;
use ember_client::wire::DetectedAgent;

use super::components::PillTone;
use super::home::{self, Harness, PermMode, UNAVAILABLE};
use crate::server::ChatGptAccount;

/// Said on every screen part whose settings are kept on this device only.
pub const LOCAL_STORE_NOTE: &str =
    "서버에 저장하는 API가 없어 이 기기에만 보관합니다. 서버가 준비되면 옮겨집니다.";

// ---------------------------------------------------------------------------------------------
// Sections (FR-L20)
// ---------------------------------------------------------------------------------------------

/// The five sections, in nav order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Harness,
    Models,
    Profiles,
    Network,
    Usage,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Harness,
        Section::Models,
        Section::Profiles,
        Section::Network,
        Section::Usage,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Harness => "하네스",
            Section::Models => "모델",
            Section::Profiles => "프로필",
            Section::Network => "네트워크",
            Section::Usage => "사용량",
        }
    }

    /// The key a link or a stored choice uses (the mockup's `#harness|models|profiles|...`).
    pub fn key(self) -> &'static str {
        match self {
            Section::Harness => "harness",
            Section::Models => "models",
            Section::Profiles => "profiles",
            Section::Network => "network",
            Section::Usage => "usage",
        }
    }
}

/// The section a key opens. An unknown key opens the first section; the old `routing` key opens
/// the profiles (the mockup renamed it).
pub fn resolve_section(key: &str) -> Section {
    if key == "routing" {
        return Section::Profiles;
    }
    Section::ALL
        .into_iter()
        .find(|x| x.key() == key)
        .unwrap_or(Section::Harness)
}

/// How the section nav is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavShape {
    /// A column on the left.
    Side,
    /// A row of pills above the content.
    Pills,
}

/// TODO(GAPS: 창 폭 레이아웃): the design switches at 900 px; the toolkit reports three size
/// classes, so only the compact class gets pills.
pub fn nav_shape(compact: bool) -> NavShape {
    if compact {
        NavShape::Pills
    } else {
        NavShape::Side
    }
}

// ---------------------------------------------------------------------------------------------
// Harnesses (FR-L20, FR-A2, FR-A5e, FR-S8)
// ---------------------------------------------------------------------------------------------

/// The harness cards. Wider than [`Harness`] (the pickers' three): Antigravity and ACP agents
/// are cards here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentHarness {
    ClaudeCode,
    Codex,
    Gemini,
    Antigravity,
    Acp,
}

impl AgentHarness {
    pub const ALL: [AgentHarness; 5] = [
        AgentHarness::ClaudeCode,
        AgentHarness::Codex,
        AgentHarness::Gemini,
        AgentHarness::Antigravity,
        AgentHarness::Acp,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AgentHarness::ClaudeCode => "Claude Code",
            AgentHarness::Codex => "Codex",
            AgentHarness::Gemini => "Gemini CLI",
            AgentHarness::Antigravity => "Antigravity",
            AgentHarness::Acp => "ACP 에이전트",
        }
    }

    /// The agent kind the server reports in `GET /agents` and in an account's `agent`. Gemini
    /// has none yet (it would be a configured ACP agent); ACP has the configured names, see
    /// [`AgentHarness::of_kind`].
    pub fn kind(self) -> Option<&'static str> {
        match self {
            AgentHarness::ClaudeCode => Some("claude-code"),
            AgentHarness::Codex => Some("codex"),
            AgentHarness::Antigravity => Some("antigravity"),
            AgentHarness::Gemini | AgentHarness::Acp => None,
        }
    }

    /// The card a server kind belongs to: the three built-in kinds, else an ACP agent.
    /// `scripted` is the test agent and has no card.
    pub fn of_kind(kind: &str) -> Option<AgentHarness> {
        match kind {
            "claude-code" => Some(AgentHarness::ClaudeCode),
            "codex" => Some(AgentHarness::Codex),
            "antigravity" => Some(AgentHarness::Antigravity),
            "scripted" => None,
            _ => Some(AgentHarness::Acp),
        }
    }

    /// How the harness is driven (FR-A2).
    pub fn run_mode(self) -> &'static str {
        match self {
            AgentHarness::ClaudeCode => "stream-json",
            AgentHarness::Codex => "app-server",
            AgentHarness::Antigravity => "턴마다 실행",
            AgentHarness::Gemini | AgentHarness::Acp => "ACP",
        }
    }

    /// The sandbox switch: `Some(default)` for Antigravity (off, FR-A5e), `None` where the
    /// harness has no such switch.
    pub fn sandbox_default(self) -> Option<bool> {
        (self == AgentHarness::Antigravity).then_some(false)
    }

    /// Whether the harness can run under the resident shim (FR-S8).
    pub fn shim_supported(self) -> bool {
        true
    }

    /// The picker harness whose model catalog applies, if any.
    pub fn picker_harness(self) -> Option<Harness> {
        match self {
            AgentHarness::ClaudeCode => Some(Harness::ClaudeCode),
            AgentHarness::Codex => Some(Harness::Codex),
            AgentHarness::Gemini => Some(Harness::Gemini),
            AgentHarness::Antigravity | AgentHarness::Acp => None,
        }
    }
}

/// The settings of one harness. TODO(server): none of this has an endpoint; see
/// [`LOCAL_STORE_NOTE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSettings {
    /// `None` where the harness has no sandbox switch.
    pub sandbox: Option<bool>,
    pub shim: bool,
    /// Per-session memory limit in MB; `None` is no limit (FR-S8).
    pub mem_limit_mb: Option<u32>,
    /// The default profile's id.
    pub default_profile: Option<String>,
    /// The default model id and effort; `None` where the harness has no model list yet.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: PermMode,
}

pub fn default_settings(h: AgentHarness) -> HarnessSettings {
    let option = h.picker_harness().map(home::default_option);
    HarnessSettings {
        sandbox: h.sandbox_default(),
        shim: h.shim_supported(),
        mem_limit_mb: None,
        default_profile: None,
        model: option.map(|o| o.model.to_string()),
        effort: option.and_then(|o| o.effort).map(String::from),
        mode: h
            .picker_harness()
            .map(home::default_mode)
            .unwrap_or(PermMode::Ask),
    }
}

/// The memory limits the card steps through, `None` first (no limit).
pub const MEM_STEPS_MB: [Option<u32>; 5] = [None, Some(512), Some(1024), Some(2048), Some(4096)];

/// The next limit after `current`, wrapping to no limit.
pub fn next_mem_limit(current: Option<u32>) -> Option<u32> {
    let at = MEM_STEPS_MB.iter().position(|x| *x == current);
    match at {
        Some(i) => MEM_STEPS_MB[(i + 1) % MEM_STEPS_MB.len()],
        None => None,
    }
}

pub fn mem_limit_label(limit: Option<u32>) -> String {
    match limit {
        Some(mb) => format!("{mb} MB"),
        None => "제한 없음".to_string(),
    }
}

/// The next value after `current` in `options`, wrapping; the first when `current` is absent.
/// A chip that cycles through a short list uses it.
pub fn cycle<T: Clone + PartialEq>(options: &[T], current: Option<&T>) -> Option<T> {
    let first = options.first().cloned();
    let at = current.and_then(|c| options.iter().position(|o| o == c));
    match at {
        Some(i) => options.get((i + 1) % options.len()).cloned(),
        None => first,
    }
}

// ---------------------------------------------------------------------------------------------
// Profiles (FR-L17, D30)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountMode {
    /// One account only.
    Fixed,
    /// An ordered list, switched by the rules.
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// Top to bottom.
    Order,
    /// The account with the most usage left first.
    MostLeft,
}

impl Routing {
    pub fn label(self) -> &'static str {
        match self {
            Routing::Order => "순서대로",
            Routing::MostLeft => "많이 남은 계정",
        }
    }
}

/// What to do when an account hits a rate limit or an authentication error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFail {
    NextAccount,
    Wait,
    Stop,
}

impl OnFail {
    pub fn label(self) -> &'static str {
        match self {
            OnFail::NextAccount => "다음 계정으로",
            OnFail::Wait => "리셋까지 기다림",
            OnFail::Stop => "멈추고 알림",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub harness: AgentHarness,
    pub mode: AccountMode,
    /// Account ids: one when fixed, the switching order when automatic.
    pub accounts: Vec<String>,
    /// Switch away from an account once its usage reaches this percent.
    pub threshold: u8,
    pub policy: Routing,
    pub on_fail: OnFail,
    /// The optional model proxy (FR-U6): a free-text endpoint label. `None` is the harness's
    /// own provider.
    pub proxy: Option<String>,
}

/// What the client knows of one account for routing: remaining usage is `None` because the
/// server reports none yet (FR-U3), `limited` comes from `GET /accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub id: String,
    pub label: String,
    pub harness: AgentHarness,
    pub left_percent: Option<u8>,
    pub limited: bool,
}

/// Move the account at `index` one place up (`-1`) or down (`1`). Returns whether anything
/// moved; the ends stay put. Reordering changes which account the next session starts on.
pub fn move_account(accounts: &mut [String], index: usize, direction: i32) -> bool {
    let target = index as i64 + direction as i64;
    if direction == 0 || index >= accounts.len() || target < 0 || target as usize >= accounts.len()
    {
        return false;
    }
    accounts.swap(index, target as usize);
    true
}

/// Add an account to the end unless it is already there.
pub fn add_account(accounts: &mut Vec<String>, id: &str) -> bool {
    if accounts.iter().any(|a| a == id) {
        return false;
    }
    accounts.push(id.to_string());
    true
}

/// Remove the account at `index`; the last one stays (a profile always has an account).
pub fn remove_account(accounts: &mut Vec<String>, index: usize) -> bool {
    if accounts.len() < 2 || index >= accounts.len() {
        return false;
    }
    accounts.remove(index);
    true
}

pub const THRESHOLD_MIN: u8 = 10;
pub const THRESHOLD_MAX: u8 = 100;
pub const THRESHOLD_STEP: u8 = 5;

/// Step the threshold by `steps` times [`THRESHOLD_STEP`], clamped to 10..=100.
pub fn step_threshold(current: u8, steps: i32) -> u8 {
    let next = current as i32 + steps * THRESHOLD_STEP as i32;
    next.clamp(THRESHOLD_MIN as i32, THRESHOLD_MAX as i32) as u8
}

/// "사용률 90%에서 다음 계정으로 넘어갑니다".
pub fn threshold_label(threshold: u8) -> String {
    format!("사용률 {threshold}%에서 다음 계정으로 넘어갑니다")
}

/// Why an account was passed over in the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Limited,
    OverThreshold,
}

/// The account a new session would start on, and the ones skipped on the way.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pick {
    pub account: Option<String>,
    pub skipped: Vec<(String, SkipReason)>,
}

/// Which account a new session of the profile starts on (FR-L17, FR-U3).
///
/// A fixed profile always uses its one account. An automatic one walks the accounts in order
/// (or most usage left first for [`Routing::MostLeft`]; an account with unknown usage sorts
/// after the known ones) and takes the first that is not rate-limited and whose usage has not
/// reached the threshold. An account with unknown usage is never skipped for the threshold.
pub fn pick_account(profile: &Profile, accounts: &[AccountInfo]) -> Pick {
    let info_of = |id: &String| accounts.iter().find(|a| a.id == *id);
    let mut order: Vec<&AccountInfo> = profile.accounts.iter().filter_map(info_of).collect();
    if profile.mode == AccountMode::Fixed {
        return Pick {
            account: order.first().map(|a| a.id.clone()),
            skipped: Vec::new(),
        };
    }
    if profile.policy == Routing::MostLeft {
        // Stable sort: ties keep the stored order; unknown usage sorts after known.
        order.sort_by_key(|a| std::cmp::Reverse(a.left_percent.map_or(-1, i32::from)));
    }
    let mut pick = Pick::default();
    for a in order {
        if a.limited {
            pick.skipped.push((a.id.clone(), SkipReason::Limited));
        } else if a
            .left_percent
            .is_some_and(|left| 100 - i32::from(left) >= i32::from(profile.threshold))
        {
            pick.skipped.push((a.id.clone(), SkipReason::OverThreshold));
        } else {
            pick.account = Some(a.id.clone());
            break;
        }
    }
    pick
}

/// The preview sentence under a profile.
pub fn preview_text(pick: &Pick, label_of: &dyn Fn(&str) -> String) -> String {
    let Some(account) = &pick.account else {
        return "쓸 수 있는 계정이 없습니다".to_string();
    };
    let mut text = format!("지금 새 세션을 열면 {}에서 시작됩니다", label_of(account));
    for (id, why) in &pick.skipped {
        let why = match why {
            SkipReason::Limited => "사용 한도에 도달해",
            SkipReason::OverThreshold => "전환 사용률을 넘어",
        };
        text.push_str(&format!(". {}은(는) {} 건너뜁니다", label_of(id), why));
    }
    text
}

/// The failure handlings a profile offers: "다음 계정으로" only when it switches automatically.
pub fn fail_options(mode: AccountMode) -> Vec<OnFail> {
    match mode {
        AccountMode::Auto => vec![OnFail::NextAccount, OnFail::Wait, OnFail::Stop],
        AccountMode::Fixed => vec![OnFail::Wait, OnFail::Stop],
    }
}

/// Change a profile's account mode. Fixed keeps the first account; automatic takes every
/// account of its harness (`same_harness`, in order) and defaults the failure handling to the
/// next account. A failure handling the mode no longer offers falls back to waiting.
pub fn set_account_mode(p: &mut Profile, mode: AccountMode, same_harness: &[String]) {
    p.mode = mode;
    match mode {
        AccountMode::Fixed => p.accounts.truncate(1),
        AccountMode::Auto => {
            for id in same_harness {
                add_account(&mut p.accounts, id);
            }
            p.on_fail = OnFail::NextAccount;
        }
    }
    if !fail_options(mode).contains(&p.on_fail) {
        p.on_fail = OnFail::Wait;
    }
}

/// The profile a new session of `harness` uses: the project's override when it names a profile
/// of that harness, else the harness's default profile, else none (FR-L17).
pub fn resolve_profile<'a>(
    harness: AgentHarness,
    project_override: Option<&str>,
    harness_default: Option<&str>,
    profiles: &'a [Profile],
) -> Option<&'a Profile> {
    let usable = |id: Option<&str>| {
        id.and_then(|id| profiles.iter().find(|p| p.id == id && p.harness == harness))
    };
    usable(project_override).or_else(|| usable(harness_default))
}

/// A profile used as a harness default cannot be deleted (FR-L17). `defaults` is every
/// harness's default profile id.
pub fn can_delete_profile(profile_id: &str, defaults: &[Option<String>]) -> bool {
    !defaults.iter().any(|d| d.as_deref() == Some(profile_id))
}

/// One profile per harness that has accounts, named after the harness, fixed on the account
/// the server marks default (else the first). Gives a first run something real to show without
/// inventing an account.
pub fn seed_profiles(accounts: &[AccountInfo], default_ids: &[String]) -> Vec<Profile> {
    let mut out = Vec::new();
    for h in AgentHarness::ALL {
        let of_h: Vec<&AccountInfo> = accounts.iter().filter(|a| a.harness == h).collect();
        let chosen = of_h
            .iter()
            .find(|a| default_ids.contains(&a.id))
            .or(of_h.first());
        let Some(chosen) = chosen else { continue };
        out.push(Profile {
            id: format!("seed-{}", h.kind().unwrap_or("acp")),
            name: format!("{} 기본", h.label()),
            harness: h,
            mode: AccountMode::Fixed,
            accounts: vec![chosen.id.clone()],
            threshold: 90,
            policy: Routing::Order,
            on_fail: OnFail::Wait,
            proxy: None,
        });
    }
    out
}

/// Whether a harness can run through a model proxy (FR-U6): only Claude Code, until the other
/// CLIs' proxy settings are verified one by one.
pub fn proxy_supported(h: AgentHarness) -> bool {
    h == AgentHarness::ClaudeCode
}

/// The default profile shown for a harness: the one set, else the first profile of that
/// harness (each harness has exactly one default, FR-L17), else none.
pub fn effective_default(
    h: AgentHarness,
    set: Option<&str>,
    profiles: &[Profile],
) -> Option<String> {
    set.and_then(|id| profiles.iter().find(|p| p.id == id && p.harness == h))
        .or_else(|| profiles.iter().find(|p| p.harness == h))
        .map(|p| p.id.clone())
}

/// A fresh profile id and name: one past the highest "pf-N" in use.
pub fn next_profile_id(profiles: &[Profile]) -> (String, String) {
    let n = profiles
        .iter()
        .filter_map(|p| p.id.strip_prefix("pf-").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .map_or(1, |m| m + 1);
    (format!("pf-{n}"), format!("새 프로필 {n}"))
}

/// A new profile: fixed on `account`, waiting on failure, 90% threshold.
pub fn new_profile(profiles: &[Profile], harness: AgentHarness, account: &str) -> Profile {
    let (id, name) = next_profile_id(profiles);
    Profile {
        id,
        name,
        harness,
        mode: AccountMode::Fixed,
        accounts: vec![account.to_string()],
        threshold: 90,
        policy: Routing::Order,
        on_fail: OnFail::Wait,
        proxy: None,
    }
}

/// What a harness card says about the harness: `(state, tone, version)` from the detected
/// agents (`GET /agents`, FR-A6). Gemini has no server kind yet; an empty answer means the
/// server has not been asked yet.
pub fn harness_status(
    h: AgentHarness,
    detected: &[DetectedAgent],
) -> (&'static str, PillTone, Option<String>) {
    if h == AgentHarness::Gemini {
        return (UNAVAILABLE, PillTone::Grey, None);
    }
    if detected.is_empty() {
        return ("확인 중입니다", PillTone::Grey, None);
    }
    let mine: Vec<&DetectedAgent> = detected
        .iter()
        .filter(|d| AgentHarness::of_kind(&d.kind) == Some(h))
        .collect();
    match mine.iter().find(|d| d.installed) {
        Some(d) => ("설치되어 있습니다", PillTone::Green, d.version.clone()),
        None => ("설치되어 있지 않습니다", PillTone::Grey, None),
    }
}

// ---------------------------------------------------------------------------------------------
// Harness KPI row, version and re-detection (FR-L20, FR-A6)
// ---------------------------------------------------------------------------------------------

/// Said where a value has no source. Never a made-up number.
pub const NO_INFO: &str = "아직 정보가 없습니다";

/// Harnesses the server reported as installed; `None` until the server answered (an empty
/// list is "not asked yet", as in [`harness_status`]).
pub fn kpi_installed(detected: &[DetectedAgent]) -> Option<usize> {
    if detected.is_empty() {
        None
    } else {
        Some(detected.iter().filter(|d| d.installed).count())
    }
}

/// Running shims. TODO(server): the client holds no shim or session-process count, so there is
/// never a source yet; the argument is where one will come from.
pub fn kpi_shim(running: Option<usize>) -> Option<usize> {
    running
}

/// The sum of the memory limits set on the installed harnesses, in MB. `None` when no
/// installed harness has a limit (the limits are this device's settings, `LOCAL_STORE_NOTE`).
pub fn kpi_mem_total_mb(limits_of_installed: &[Option<u32>]) -> Option<u64> {
    let set: Vec<u64> = limits_of_installed.iter().flatten().map(|m| u64::from(*m)).collect();
    if set.is_empty() {
        None
    } else {
        Some(set.iter().sum())
    }
}

/// `"4 GB"` or `"512 MB"`; whole gigabytes drop the fraction, others keep one decimal.
pub fn mem_total_label(mb: u64) -> String {
    if mb >= 1024 {
        if mb % 1024 == 0 {
            format!("{} GB", mb / 1024)
        } else {
            format!("{:.1} GB", mb as f64 / 1024.0)
        }
    } else {
        format!("{mb} MB")
    }
}

/// A KPI value: the number, or [`NO_INFO`] when there is no source.
pub fn kpi_text(value: Option<String>) -> String {
    value.unwrap_or_else(|| NO_INFO.to_string())
}

/// When the detection last ran, relative to `now_ms`. `None` (never ran in this run of the
/// app) says [`NO_INFO`].
pub fn last_detect_label(now_ms: u64, at_ms: Option<u64>) -> String {
    let Some(at) = at_ms else { return NO_INFO.to_string() };
    let secs = now_ms.saturating_sub(at) / 1000;
    if secs < 10 {
        "방금".to_string()
    } else if secs < 60 {
        format!("{secs}초 전")
    } else if secs < 3600 {
        format!("{}분 전", secs / 60)
    } else if secs < 86_400 {
        format!("{}시간 전", secs / 3600)
    } else {
        format!("{}일 전", secs / 86_400)
    }
}

/// The version pill of a card: only a version the server reported.
pub fn version_pill(version: Option<&str>) -> Option<String> {
    version.map(str::trim).filter(|v| !v.is_empty()).map(|v| format!("v{v}"))
}

/// The line under a harness name: the run mode, plus the version when the server reported one.
pub fn harness_subtitle(run_mode: &str, version: Option<&str>) -> String {
    match version_pill(version) {
        Some(v) => format!("{run_mode} · {v}"),
        None => run_mode.to_string(),
    }
}

/// The state of the 다시 찾기 buttons.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Rescan {
    #[default]
    Idle,
    Running,
    /// The sentence the last run ended with.
    Done(String),
}

/// Why a re-detection gave no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanFailure {
    /// The computer cannot be probed from here yet (501 `not_implemented`).
    NotImplemented,
    Failed,
}

pub const RESCAN_NOT_IMPLEMENTED: &str = "이 컴퓨터에서는 아직 다시 찾을 수 없습니다";
pub const RESCAN_FAILED: &str = "다시 찾지 못했습니다. 잠시 뒤에 다시 시도해 주세요";
/// The Gemini card: no CLI adapter yet, so nothing is detected or invented.
pub const SOON: &str = "준비 중입니다";
pub const NO_VERSION: &str = "버전을 알 수 없습니다";

/// Map a failed `POST .../harnesses/detect` to what the screen says.
pub fn rescan_failure(status: Option<u16>, code: Option<&str>) -> RescanFailure {
    if status == Some(501) || code == Some("not_implemented") {
        RescanFailure::NotImplemented
    } else {
        RescanFailure::Failed
    }
}

/// The detect answer as agent entries (path and probe time kept as reported).
pub fn detect_to_agents(got: &[ember_client::api::DetectedHarness]) -> Vec<DetectedAgent> {
    got.iter()
        .map(|h| DetectedAgent {
            kind: h.harness.clone(),
            installed: h.installed,
            version: h.version.clone(),
            path: h.path.clone(),
            detected_at: h.detected_at,
        })
        .collect()
}

/// `before` with the entries of `fresh` replacing the same kinds; kinds the probe did not
/// answer (Gemini) keep their entry.
pub fn merge_detected(before: &[DetectedAgent], fresh: &[DetectedAgent]) -> Vec<DetectedAgent> {
    let mut out: Vec<DetectedAgent> =
        before.iter().filter(|b| !fresh.iter().any(|f| f.kind == b.kind)).cloned().collect();
    out.extend(fresh.iter().cloned());
    out
}

/// The newest probe time the server reported (epoch ms).
pub fn latest_detected_at(list: &[DetectedAgent]) -> Option<u64> {
    list.iter().filter_map(|d| d.detected_at).filter(|t| *t > 0).max().map(|t| t as u64)
}

/// The sentence a finished re-detection ends with. `after` is the failure when the server could
/// not be asked or answered nothing.
pub fn rescan_sentence(before: &[DetectedAgent], after: Result<&[DetectedAgent], RescanFailure>) -> String {
    let after = match after {
        Ok(a) => a,
        Err(RescanFailure::NotImplemented) => return RESCAN_NOT_IMPLEMENTED.to_string(),
        Err(RescanFailure::Failed) => return RESCAN_FAILED.to_string(),
    };
    let found = after.iter().filter(|d| d.installed).count();
    let was = |kind: &str| before.iter().any(|d| d.kind == kind && d.installed);
    let new = after.iter().filter(|d| d.installed && !was(&d.kind)).count();
    if found == 0 {
        "찾지 못했습니다".to_string()
    } else if new == 0 {
        "새로 찾은 하네스가 없습니다".to_string()
    } else {
        format!("찾았습니다: {new}개")
    }
}

/// What the 다시 찾기 button says: always the action, or that it is running. The outcome is
/// shown once beside the button ([`rescan_note`]), not in the label of every card.
pub fn rescan_label(state: &Rescan) -> String {
    match state {
        Rescan::Running => "찾는 중입니다".to_string(),
        Rescan::Idle | Rescan::Done(_) => "다시 찾기".to_string(),
    }
}

/// The outcome sentence to show near the buttons, while there is one.
pub fn rescan_note(state: &Rescan) -> Option<String> {
    match state {
        Rescan::Done(s) => Some(s.clone()),
        _ => None,
    }
}

/// Whether a harness card's 다시 찾기 can be pressed, and the reason when it cannot.
/// Gemini has no adapter yet (owner decision), so its probe would answer nothing.
pub fn rescan_enabled(h: AgentHarness, state: &Rescan) -> (bool, Option<&'static str>) {
    if h == AgentHarness::Gemini {
        (false, Some(SOON))
    } else {
        (!matches!(state, Rescan::Running), None)
    }
}

/// The path line of a harness card: the path the server reported for an installed harness,
/// [`SOON`] for Gemini, otherwise [`NO_INFO`]. `example` marks sample or demo data.
pub fn harness_path_text(h: AgentHarness, detected: &[DetectedAgent], example: bool) -> String {
    if h == AgentHarness::Gemini {
        return SOON.to_string();
    }
    let path = detected
        .iter()
        .find(|d| d.installed && AgentHarness::of_kind(&d.kind) == Some(h))
        .and_then(|d| d.path.as_deref())
        .filter(|p| !p.is_empty());
    match path {
        Some(p) if example => format!("{p} (예시)"),
        Some(p) => p.to_string(),
        None => NO_INFO.to_string(),
    }
}

/// The version pill text of an installed harness: the version, or [`NO_VERSION`] when the
/// server reported none; a harness that is not installed has no pill text.
/// The version Ember verified for Antigravity (the 고정 버전 row).
pub const PINNED_ANTIGRAVITY_VERSION: &str = "1.2.16";

/// How the detected version compares with the pinned one. `None` when nothing was detected (no
/// version reported): the row then shows no verdict, never a made-up "일치".
pub fn pin_verdict(detected: Option<&str>, pinned: &str) -> Option<&'static str> {
    let v = detected?.trim().trim_start_matches('v');
    if v.is_empty() {
        None
    } else if v == pinned.trim_start_matches('v') {
        Some("일치")
    } else {
        Some("다름")
    }
}

pub fn version_text(installed: bool, version: Option<&str>, example: bool) -> Option<String> {
    if !installed {
        return None;
    }
    let v = version_pill(version).unwrap_or_else(|| NO_VERSION.to_string());
    Some(if example && version.is_some() { format!("{v} (예시)") } else { v })
}

// ---------------------------------------------------------------------------------------------
// Network (FR-X6) and usage (FR-U7)
// ---------------------------------------------------------------------------------------------

/// A computer row of the network section.
pub fn computer_status(r: Reachability) -> (&'static str, PillTone) {
    match r {
        Reachability::Online => ("온라인", PillTone::Green),
        Reachability::Offline => ("오프라인", PillTone::Grey),
        Reachability::Unknown => ("알 수 없음", PillTone::Grey),
    }
}

/// Tokens consumed today by the ChatGPT plan accounts (FR-U4), the only token count the client
/// holds. `None` when there is no such account: never zero by default.
pub fn chatgpt_tokens_today(accounts: &[ChatGptAccount]) -> Option<u64> {
    if accounts.is_empty() {
        None
    } else {
        Some(accounts.iter().map(|a| a.tokens_today).sum())
    }
}

/// "12,345" with thousands separators.
pub fn format_tokens(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The local store
// ---------------------------------------------------------------------------------------------

/// Everything the screen lets the user change. TODO(server): there is no settings or profile
/// API, so it is kept in memory for this run of the app ([`LOCAL_STORE_NOTE`]); the server's
/// store replaces it and then every client sees the same values (PR-1).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalStore {
    /// One entry per [`AgentHarness::ALL`].
    pub harness: Vec<HarnessSettings>,
    /// `None` until the user edits: the profiles are then derived from the real accounts
    /// ([`seed_profiles`]).
    pub profiles: Option<Vec<Profile>>,
    /// The harness the owner chose as the default for new team members; `None` until chosen
    /// (FR-L20). Never seeded from the Home catalog.
    pub default_harness: Option<AgentHarness>,
    /// The model id the owner chose with it; `None` until chosen.
    pub default_model: Option<String>,
}

impl LocalStore {
    pub fn new() -> LocalStore {
        LocalStore {
            harness: AgentHarness::ALL
                .iter()
                .map(|h| default_settings(*h))
                .collect(),
            profiles: None,
            default_harness: None,
            default_model: None,
        }
    }

    fn index(h: AgentHarness) -> usize {
        AgentHarness::ALL.iter().position(|x| *x == h).unwrap_or(0)
    }

    pub fn settings(&self, h: AgentHarness) -> &HarnessSettings {
        &self.harness[Self::index(h)]
    }

    pub fn settings_mut(&mut self, h: AgentHarness) -> &mut HarnessSettings {
        &mut self.harness[Self::index(h)]
    }
}

impl Default for LocalStore {
    fn default() -> Self {
        LocalStore::new()
    }
}

/// What the client knows about one computer beyond its name and reachability. The client holds
/// no OS, IP or per-computer session counts (`ember_client::state::Computer`), so a live
/// computer has every field `None` (FR-L20); only a sample capture fills them, marked 예시.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputerFacts {
    pub os: Option<String>,
    pub ip: Option<String>,
    pub sessions: Option<u32>,
    pub connections: Option<u32>,
    /// The computer the sessions move to when this one drops (sample only).
    pub fallback: Option<String>,
}

/// The facts for a computer. Live (`sample == false`) is always empty; the name-keyed table
/// is the review window's sample data.
pub fn computer_facts(name: &str, sample: bool) -> ComputerFacts {
    if !sample {
        return ComputerFacts::default();
    }
    let s = |v: &str| Some(v.to_string());
    match name {
        "mac-mini" => ComputerFacts { os: s("macOS 26 · 메인 서버"), ip: s("211.36.142.18"), sessions: Some(7), connections: Some(12), fallback: None },
        "ws-linux-01" => ComputerFacts { os: s("Ubuntu 24.04"), ip: s("121.134.77.205"), sessions: Some(6), connections: Some(9), fallback: None },
        "macbook-pro" => ComputerFacts { os: s("macOS 26"), ip: s("175.223.10.91"), sessions: Some(0), connections: Some(0), fallback: None },
        "win-desktop" => ComputerFacts { os: s("Windows 11"), ip: None, sessions: Some(3), connections: Some(0), fallback: s("mac-mini") },
        _ => ComputerFacts::default(),
    }
}

/// A fact's text: the value, or [`NO_INFO`]. A sample value carries 예시.
pub fn fact_text(value: Option<&str>, sample: bool) -> String {
    match value {
        Some(v) if sample => format!("{v} (예시)"),
        Some(v) => v.to_string(),
        None => NO_INFO.to_string(),
    }
}

/// A count's number, or [`NO_INFO`]; never zero by default.
pub fn count_text(value: Option<u32>) -> String {
    value.map_or_else(|| NO_INFO.to_string(), |n| n.to_string())
}

/// The sentence under an unreachable computer: only when it is really offline and its session
/// count and fallback are known.
pub fn disconnect_note(r: Reachability, facts: &ComputerFacts) -> Option<String> {
    if r != Reachability::Offline {
        return None;
    }
    let n = facts.sessions.filter(|n| *n > 0)?;
    let to = facts.fallback.as_deref()?;
    Some(format!("연결이 끊겨 세션 {n}개가 {to}(으)로 연결됩니다. (예시)"))
}

/// The account tile's second line: the harness, and a reset time only when one is known. The
/// accounts API carries none, so only a sample shows one, marked 예시.
pub fn account_reset_line(harness_label: &str, sample: bool) -> String {
    if sample {
        format!("{harness_label} · 리셋 3일 뒤 (예시)")
    } else {
        format!("{harness_label} · 리셋 시각 {NO_INFO}")
    }
}

// ---------------------------------------------------------------------------------------------
// 새 팀원 기본값 (FR-L20, FR-L17) and 계정 추가 (FR-U1)
// ---------------------------------------------------------------------------------------------

/// A default the owner has not chosen yet.
pub const NOT_CHOSEN: &str = "아직 정하지 않았습니다";
/// The account chip when the server lists no account.
pub const NO_ACCOUNTS: &str = "계정이 없습니다";
/// The only row of the account sheet when there is no account.
pub const ADD_ACCOUNT_FIRST: &str = "계정 추가에서 먼저 추가하세요";
/// Under the default chips: what is stored where (the server keeps only the default account).
pub const DEFAULTS_STORAGE_NOTE: &str =
    "고른 하네스와 모델은 이 기기에만 저장됩니다. 기본 계정만 서버에 저장됩니다.";
/// Why a harness cannot be picked: the server found no binary.
pub const NOT_INSTALLED_REASON: &str = "감지되지 않았습니다";
/// Why Gemini cannot be picked: the server has no adapter yet (D40).
pub const HARNESS_SOON_REASON: &str = "준비 중입니다";
/// The mark on a sample or demo value.
pub const EXAMPLE_MARK: &str = "예시";
/// Said in the 계정 추가 sheet: the app opens no login window; the server's user logs in.
pub const NO_LOGIN_WINDOW_NOTE: &str =
    "앱 안에서 로그인 창을 여는 기능은 아직 없습니다. 아래 명령을 서버를 실행하는 사용자로 터미널에서 실행한 뒤 로그인 확인을 누르세요.";
/// The demo sheet calls nothing.
pub const DEMO_ADD_NOTE: &str = "예시입니다. 서버를 호출하지 않았고 계정을 만들지 않았습니다.";
/// A demo choice changes nothing.
pub const DEMO_CHOICE_NOTE: &str = "예시입니다. 서버를 호출하지 않았습니다.";
/// The server refused to create the account.
pub const ADD_FAILED: &str = "계정을 만들지 못했습니다";
/// The check call failed.
pub const CHECK_FAILED: &str = "로그인을 확인하지 못했습니다";

/// What the two chips of the 새 팀원 기본값 card show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultChips {
    pub harness: Option<AgentHarness>,
    /// The model id the owner chose; never the catalog default.
    pub model: Option<String>,
    /// The label of the default account of the chosen harness.
    pub account: Option<String>,
}

/// The chips come from what is stored: the harness and model this device keeps once the owner
/// chose them, and the account the server marks default for that harness. With no harness
/// chosen the account shows only when exactly one account is default (no guess among several).
pub fn default_chips(accounts: &[AccountInfo], default_ids: &[String], store: &LocalStore) -> DefaultChips {
    let defaults: Vec<&AccountInfo> = default_ids.iter().filter_map(|id| accounts.iter().find(|a| &a.id == id)).collect();
    let harness = store.default_harness;
    let account = match harness {
        Some(h) => defaults.iter().find(|a| a.harness == h).map(|a| a.label.clone()),
        None if defaults.len() == 1 => Some(defaults[0].label.clone()),
        None => None,
    };
    DefaultChips { harness, model: harness.and(store.default_model.clone()), account }
}

fn mark(text: String, example: bool) -> String {
    if example { format!("{text} ({EXAMPLE_MARK})") } else { text }
}

/// The harness chip: [`NOT_CHOSEN`] until the owner chose; the model only when chosen too.
pub fn harness_chip_text(chips: &DefaultChips, model_name: impl Fn(AgentHarness, &str) -> String, example: bool) -> String {
    match (chips.harness, chips.model.as_deref()) {
        (None, _) => NOT_CHOSEN.to_string(),
        (Some(h), Some(m)) => mark(format!("{} · {}", h.label(), model_name(h, m)), example),
        (Some(h), None) => mark(h.label().to_string(), example),
    }
}

/// The account chip: [`NO_ACCOUNTS`] when the server lists none, [`NOT_CHOSEN`] when none is default.
pub fn account_chip_text(chips: &DefaultChips, account_count: usize, example: bool) -> String {
    if account_count == 0 {
        return NO_ACCOUNTS.to_string();
    }
    match &chips.account {
        Some(a) => mark(a.clone(), example),
        None => NOT_CHOSEN.to_string(),
    }
}

/// One real account the sheet can choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOption {
    pub id: String,
    pub label: String,
}

/// The sheet's accounts: exactly the ones the server lists, `한도 가까움` on a limited one.
pub fn account_options(accounts: &[AccountInfo]) -> Vec<AccountOption> {
    accounts
        .iter()
        .map(|a| AccountOption {
            id: a.id.clone(),
            label: if a.limited {
                format!("{} · {} · 한도 가까움", a.harness.label(), a.label)
            } else {
                format!("{} · {}", a.harness.label(), a.label)
            },
        })
        .collect()
}

/// The sheet's rows: the accounts, or the single sentence that says to add one first.
pub fn account_sheet_rows(options: &[AccountOption]) -> Vec<String> {
    if options.is_empty() {
        vec![ADD_ACCOUNT_FIRST.to_string()]
    } else {
        options.iter().map(|o| o.label.clone()).collect()
    }
}

/// A harness the default picker lists, and why it cannot be chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessOption {
    pub harness: AgentHarness,
    pub installed: bool,
    pub reason: Option<&'static str>,
}

impl HarnessOption {
    pub fn enabled(&self) -> bool {
        self.reason.is_none()
    }
}

/// The harnesses with a model catalog; only a detected, installed one is enabled.
pub fn harness_options(detected: &[DetectedAgent]) -> Vec<HarnessOption> {
    [AgentHarness::ClaudeCode, AgentHarness::Codex, AgentHarness::Gemini]
        .into_iter()
        .map(|h| {
            if h == AgentHarness::Gemini {
                return HarnessOption { harness: h, installed: false, reason: Some(HARNESS_SOON_REASON) };
            }
            let installed = detected.iter().any(|d| d.installed && AgentHarness::of_kind(&d.kind) == Some(h));
            HarnessOption { harness: h, installed, reason: (!installed).then_some(NOT_INSTALLED_REASON) }
        })
        .collect()
}

/// One row of the harness and model sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRow {
    pub harness: AgentHarness,
    pub model: Option<String>,
    pub label: String,
    pub enabled: bool,
}

/// The rows: one per model of an enabled harness, one disabled row with the reason otherwise.
/// `models` gives `(id, name)` pairs of a harness.
pub fn pick_rows(options: &[HarnessOption], models: impl Fn(AgentHarness) -> Vec<(String, String)>) -> Vec<PickRow> {
    let mut rows = Vec::new();
    for o in options {
        match o.reason {
            None => {
                for (id, name) in models(o.harness) {
                    rows.push(PickRow {
                        harness: o.harness,
                        model: Some(id),
                        label: format!("{} · {}", o.harness.label(), name),
                        enabled: true,
                    });
                }
            }
            Some(reason) => rows.push(PickRow {
                harness: o.harness,
                model: None,
                label: format!("{} · {}", o.harness.label(), reason),
                enabled: false,
            }),
        }
    }
    rows
}

/// Keeps a chosen row on this device. A disabled row changes nothing; returns whether it did.
pub fn apply_pick(store: &mut LocalStore, row: &PickRow) -> bool {
    if !row.enabled {
        return false;
    }
    store.default_harness = Some(row.harness);
    store.default_model = row.model.clone();
    true
}

/// A harness kind the 계정 추가 sheet lists. `note` is the server's own sentence for a kind it
/// refuses (`login_instructions`); `None` means the server creates accounts of the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddKind {
    pub kind: &'static str,
    pub label: &'static str,
    pub note: Option<&'static str>,
}

impl AddKind {
    pub fn enabled(&self) -> bool {
        self.note.is_none()
    }
}

pub const ANTIGRAVITY_NOTE: &str = "Antigravity accounts are not supported yet.";
pub const ACP_NOTE: &str = "ACP agent: log in with the agent's own CLI on the server. Ember does not isolate accounts for ACP agents yet.";

pub fn add_account_kinds() -> [AddKind; 4] {
    [
        AddKind { kind: "claude-code", label: "Claude Code", note: None },
        AddKind { kind: "codex", label: "Codex", note: None },
        AddKind { kind: "antigravity", label: "Antigravity", note: Some(ANTIGRAVITY_NOTE) },
        AddKind { kind: "acp", label: "ACP", note: Some(ACP_NOTE) },
    ]
}

/// The Korean sentence for a disabled harness row. The UI is Korean only (NFR-L4), so the
/// server's English note is never drawn; unknown notes get the generic sentence.
pub fn account_note_ko(kind: &str, server_note: &str) -> String {
    match (kind, server_note) {
        ("antigravity", ANTIGRAVITY_NOTE) => "지원 준비 중입니다. 지금은 계정을 만들 수 없습니다".to_string(),
        ("acp", ACP_NOTE) => "이 에이전트는 자신의 명령줄 도구로 로그인합니다. 서버가 계정을 만들지 않습니다".to_string(),
        _ => "지금은 만들 수 없습니다".to_string(),
    }
}

/// The note line of the login block: the sample note once (the sheet header draws it for the
/// example already, so the block draws it only when the header did not); a server note is
/// English, so it is not drawn.
pub fn login_block_note(c: &CreatedLogin, header_has_sample_note: bool) -> Option<String> {
    (c.note == DEMO_ADD_NOTE && !header_has_sample_note).then(|| c.note.clone())
}

/// What the server returned for a new account: the command to run, never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct CreatedLogin {
    pub id: String,
    pub command: String,
    pub note: String,
    /// The answer of the last 로그인 확인; `None` before any.
    pub status: Option<String>,
    pub checking: bool,
}

impl std::fmt::Debug for CreatedLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedLogin").field("id", &self.id).field("command", &"<hidden>").finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddPhase {
    Form,
    Busy,
    Created(CreatedLogin),
    Failed(String),
}

/// The state of the 계정 추가 sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddAccount {
    pub open: bool,
    pub kind: Option<&'static str>,
    pub label: String,
    pub phase: AddPhase,
}

impl AddAccount {
    pub fn closed() -> AddAccount {
        AddAccount { open: false, kind: None, label: String::new(), phase: AddPhase::Form }
    }

    /// A kind the server accepts and a label are needed; not while a call runs or after creation.
    pub fn can_create(&self) -> bool {
        let kind_ok = self.kind.is_some_and(|k| add_account_kinds().iter().any(|a| a.kind == k && a.enabled()));
        kind_ok && !self.label.trim().is_empty() && matches!(self.phase, AddPhase::Form | AddPhase::Failed(_))
    }
}

/// The created phase from the server's answer.
pub fn created_phase(a: &crate::server::AccountWithLogin) -> AddPhase {
    AddPhase::Created(CreatedLogin {
        id: a.account.id.clone(),
        command: a.login.login.clone(),
        note: a.login.note.clone(),
        status: None,
        checking: false,
    })
}

/// The sentence for the status a 로그인 확인 returned.
pub fn check_line(status: &str) -> String {
    match status {
        "logged_in" => "로그인되어 있습니다".to_string(),
        "logged_out" => "아직 로그인하지 않았습니다".to_string(),
        _ => "로그인 상태를 알 수 없습니다".to_string(),
    }
}

/// Demo mode's sample result: marked 예시, nothing was created.
pub fn sample_created() -> AddPhase {
    AddPhase::Created(CreatedLogin {
        id: String::new(),
        command: "CLAUDE_CONFIG_DIR=/example/claude claude auth login (예시)".to_string(),
        note: DEMO_ADD_NOTE.to_string(),
        status: None,
        checking: false,
    })
}

/// A sheet a capture can open at start (`EMBER_CR_AGENTS_SHEET`, debug builds only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetHook {
    Harness,
    Account,
    AddAccount,
    AddAccountLogin,
}

/// Pure parse of the hook value. A release build (`debug == false`) never opens a sheet.
pub fn parse_sheet_hook(value: Option<&str>, debug: bool) -> Option<SheetHook> {
    if !debug {
        return None;
    }
    match value? {
        "harness" => Some(SheetHook::Harness),
        "account" => Some(SheetHook::Account),
        "add-account" => Some(SheetHook::AddAccount),
        "add-account-login" => Some(SheetHook::AddAccountLogin),
        _ => None,
    }
}

/// The hook read from `EMBER_CR_AGENTS_SHEET`.
pub fn sheet_hook() -> Option<SheetHook> {
    parse_sheet_hook(std::env::var("EMBER_CR_AGENTS_SHEET").ok().as_deref(), cfg!(debug_assertions))
}

/// The sample login command of the capture hook: it carries no secret.
pub const SAMPLE_LOGIN_COMMAND: &str = "claude login (예시)";

/// The 계정 추가 state a capture opens: the form, or the sample login block.
pub fn hook_add_account(hook: Option<SheetHook>) -> AddAccount {
    match hook {
        Some(SheetHook::AddAccount) => AddAccount { open: true, ..AddAccount::closed() },
        Some(SheetHook::AddAccountLogin) => AddAccount {
            open: true,
            kind: Some("claude-code"),
            label: EXAMPLE_MARK.to_string(),
            phase: AddPhase::Created(CreatedLogin {
                id: String::new(),
                command: SAMPLE_LOGIN_COMMAND.to_string(),
                note: DEMO_ADD_NOTE.to_string(),
                status: None,
                checking: false,
            }),
        },
        _ => AddAccount::closed(),
    }
}

/// Below this width the two default chips stack so each shows its full text.
pub const CHIPS_STACK_BELOW_DP: f32 = 560.0;

pub fn chips_stacked(window_dp: f32) -> bool {
    window_dp < CHIPS_STACK_BELOW_DP
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(kind: &str, installed: bool, version: Option<&str>) -> DetectedAgent {
        DetectedAgent { kind: kind.into(), installed, version: version.map(String::from), ..Default::default() }
    }

    #[test]
    fn fr_l20_kpi_installed_counts_only_reported_installs() {
        assert_eq!(kpi_installed(&[]), None);
        let d = [det("claude-code", true, Some("2.1.4")), det("codex", false, None)];
        assert_eq!(kpi_installed(&d), Some(1));
        assert_eq!(kpi_installed(&[det("codex", false, None)]), Some(0));
    }

    #[test]
    fn fr_l20_kpi_without_source_says_no_info() {
        assert_eq!(kpi_text(kpi_shim(None).map(|n| n.to_string())), "아직 정보가 없습니다");
        assert_eq!(kpi_text(kpi_installed(&[]).map(|n| n.to_string())), NO_INFO);
        assert_eq!(kpi_mem_total_mb(&[None, None]), None);
        assert_eq!(kpi_mem_total_mb(&[]), None);
        assert_eq!(kpi_text(kpi_installed(&[det("codex", true, None)]).map(|n| n.to_string())), "1");
    }

    #[test]
    fn fr_l20_kpi_memory_total_sums_set_limits() {
        assert_eq!(kpi_mem_total_mb(&[Some(2048), None, Some(4096)]), Some(6144));
        assert_eq!(mem_total_label(6144), "6 GB");
        assert_eq!(mem_total_label(1536), "1.5 GB");
        assert_eq!(mem_total_label(512), "512 MB");
    }

    #[test]
    fn fr_l20_last_detect_label_is_relative_or_no_info() {
        assert_eq!(last_detect_label(1_000_000, None), NO_INFO);
        assert_eq!(last_detect_label(1_005_000, Some(1_000_000)), "방금");
        assert_eq!(last_detect_label(1_030_000, Some(1_000_000)), "30초 전");
        assert_eq!(last_detect_label(1_000_000 + 5 * 60_000, Some(1_000_000)), "5분 전");
        assert_eq!(last_detect_label(1_000_000 + 3 * 3_600_000, Some(1_000_000)), "3시간 전");
        assert_eq!(last_detect_label(1_000_000, Some(2_000_000)), "방금");
    }

    #[test]
    fn fr_l20_version_shown_only_when_reported() {
        assert_eq!(version_pill(None), None);
        assert_eq!(version_pill(Some("  ")), None);
        assert_eq!(version_pill(Some("2.1.4")).as_deref(), Some("v2.1.4"));
        assert_eq!(harness_subtitle("stream-json", None), "stream-json");
        assert_eq!(harness_subtitle("stream-json", Some("2.1.4")), "stream-json · v2.1.4");
    }

    #[test]
    fn fr_l20_not_detected_harness_has_no_version() {
        let d = [det("codex", false, Some("0.1.0"))];
        let (_, _, v) = harness_status(AgentHarness::Codex, &d);
        assert_eq!(version_pill(v.as_deref()), None);
    }

    #[test]
    fn fr_l21_rescan_sentence_per_outcome() {
        let before = [det("claude-code", true, None), det("codex", false, None)];
        let same = [det("claude-code", true, None), det("codex", false, None)];
        assert_eq!(rescan_sentence(&before, Ok(&same)), "새로 찾은 하네스가 없습니다");
        let more = [det("claude-code", true, None), det("codex", true, Some("0.48.0"))];
        assert_eq!(rescan_sentence(&before, Ok(&more)), "찾았습니다: 1개");
        let none = [det("claude-code", false, None)];
        assert_eq!(rescan_sentence(&before, Ok(&none)), "찾지 못했습니다");
        assert_eq!(rescan_sentence(&before, Err(RescanFailure::Failed)), RESCAN_FAILED);
        assert_eq!(rescan_sentence(&[], Ok(&more)), "찾았습니다: 2개");
    }

    #[test]
    fn fr_l21_rescan_label_follows_state() {
        assert_eq!(rescan_label(&Rescan::Idle), "다시 찾기");
        assert_eq!(rescan_label(&Rescan::Running), "찾는 중입니다");
        assert_eq!(rescan_label(&Rescan::Done("찾았습니다: 1개".into())), "다시 찾기");
        assert_eq!(rescan_note(&Rescan::Done("x".into())).as_deref(), Some("x"));
        assert_eq!(rescan_note(&Rescan::Idle), None);
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn info(id: &str, left: Option<u8>, limited: bool) -> AccountInfo {
        AccountInfo {
            id: id.into(),
            label: id.into(),
            harness: AgentHarness::ClaudeCode,
            left_percent: left,
            limited,
        }
    }

    fn auto_profile(accounts: &[&str]) -> Profile {
        Profile {
            id: "p1".into(),
            name: "자동".into(),
            harness: AgentHarness::ClaudeCode,
            mode: AccountMode::Auto,
            accounts: s(accounts),
            threshold: 90,
            policy: Routing::Order,
            on_fail: OnFail::NextAccount,
            proxy: None,
        }
    }

    // ---- section routing -----------------------------------------------------------------

    #[test]
    fn fr_l20_five_sections_in_order() {
        let keys: Vec<_> = Section::ALL.iter().map(|x| x.label()).collect();
        assert_eq!(keys, ["하네스", "모델", "프로필", "네트워크", "사용량"]);
    }

    #[test]
    fn fr_l20_section_keys_round_trip_and_unknown_opens_the_first() {
        for x in Section::ALL {
            assert_eq!(resolve_section(x.key()), x);
        }
        assert_eq!(resolve_section("nope"), Section::Harness);
        assert_eq!(resolve_section(""), Section::Harness);
        assert_eq!(resolve_section("routing"), Section::Profiles);
    }

    #[test]
    fn fr_l20_nav_is_pills_on_compact_and_a_side_column_otherwise() {
        assert_eq!(nav_shape(true), NavShape::Pills);
        assert_eq!(nav_shape(false), NavShape::Side);
    }

    // ---- harness defaults ----------------------------------------------------------------

    #[test]
    fn fr_a5e_only_antigravity_has_a_sandbox_and_it_is_off_by_default() {
        for h in AgentHarness::ALL {
            let want = (h == AgentHarness::Antigravity).then_some(false);
            assert_eq!(h.sandbox_default(), want, "{h:?}");
            assert_eq!(default_settings(h).sandbox, want, "{h:?}");
        }
    }

    #[test]
    fn fr_s8_antigravity_and_other_harnesses_default_to_shim() {
        assert!(AgentHarness::Antigravity.shim_supported());
        assert!(default_settings(AgentHarness::Antigravity).shim);
        assert!(default_settings(AgentHarness::ClaudeCode).shim);
        assert!(default_settings(AgentHarness::Codex).shim);
    }

    #[test]
    fn fr_s8_memory_limit_steps_wrap_back_to_no_limit() {
        assert_eq!(default_settings(AgentHarness::Codex).mem_limit_mb, None);
        assert_eq!(next_mem_limit(None), Some(512));
        assert_eq!(next_mem_limit(Some(512)), Some(1024));
        assert_eq!(next_mem_limit(Some(4096)), None);
        assert_eq!(next_mem_limit(Some(777)), None);
        assert_eq!(mem_limit_label(None), "제한 없음");
        assert_eq!(mem_limit_label(Some(1024)), "1024 MB");
    }

    #[test]
    fn fr_l20_server_kinds_map_to_cards() {
        assert_eq!(
            AgentHarness::of_kind("claude-code"),
            Some(AgentHarness::ClaudeCode)
        );
        assert_eq!(AgentHarness::of_kind("codex"), Some(AgentHarness::Codex));
        assert_eq!(
            AgentHarness::of_kind("antigravity"),
            Some(AgentHarness::Antigravity)
        );
        assert_eq!(AgentHarness::of_kind("omp"), Some(AgentHarness::Acp));
        assert_eq!(AgentHarness::of_kind("scripted"), None);
        assert_eq!(AgentHarness::Gemini.kind(), None);
    }

    #[test]
    fn fr_l16_default_model_and_mode_come_from_the_home_catalog() {
        let d = default_settings(AgentHarness::ClaudeCode);
        let o = home::default_option(Harness::ClaudeCode);
        assert_eq!(d.model.as_deref(), Some(o.model));
        assert_eq!(d.effort.as_deref(), o.effort);
        assert_eq!(d.mode, home::default_mode(Harness::ClaudeCode));
        // No model list yet for these.
        assert_eq!(default_settings(AgentHarness::Antigravity).model, None);
        assert_eq!(default_settings(AgentHarness::Acp).model, None);
    }

    #[test]
    fn cycle_wraps_and_starts_at_the_first() {
        let o = [1, 2, 3];
        assert_eq!(cycle(&o, Some(&1)), Some(2));
        assert_eq!(cycle(&o, Some(&3)), Some(1));
        assert_eq!(cycle(&o, None), Some(1));
        assert_eq!(cycle(&o, Some(&9)), Some(1));
        assert_eq!(cycle::<i32>(&[], None), None);
    }

    // ---- default resolution: harness to profile ------------------------------------------

    fn profile_of(id: &str, h: AgentHarness) -> Profile {
        Profile {
            id: id.into(),
            harness: h,
            ..auto_profile(&["a"])
        }
    }

    #[test]
    fn fr_l17_a_session_uses_the_project_override_then_the_harness_default() {
        let ps = vec![
            profile_of("c1", AgentHarness::ClaudeCode),
            profile_of("c2", AgentHarness::ClaudeCode),
            profile_of("x1", AgentHarness::Codex),
        ];
        let id = |p: Option<&Profile>| p.map(|p| p.id.clone());
        let h = AgentHarness::ClaudeCode;
        assert_eq!(
            id(resolve_profile(h, Some("c2"), Some("c1"), &ps)),
            Some("c2".into())
        );
        assert_eq!(
            id(resolve_profile(h, None, Some("c1"), &ps)),
            Some("c1".into())
        );
        assert_eq!(id(resolve_profile(h, None, None, &ps)), None);
    }

    #[test]
    fn fr_l17_an_override_or_default_of_another_harness_or_a_missing_profile_is_ignored() {
        let ps = vec![
            profile_of("c1", AgentHarness::ClaudeCode),
            profile_of("x1", AgentHarness::Codex),
        ];
        let h = AgentHarness::ClaudeCode;
        // The override names a Codex profile: fall through to the harness default.
        assert_eq!(
            resolve_profile(h, Some("x1"), Some("c1"), &ps).map(|p| p.id.as_str()),
            Some("c1")
        );
        // Both dangling.
        assert!(resolve_profile(h, Some("gone"), Some("x1"), &ps).is_none());
    }

    #[test]
    fn fr_l17_a_profile_used_as_a_default_cannot_be_deleted() {
        let defaults = vec![Some("c1".to_string()), None, Some("x1".to_string())];
        assert!(!can_delete_profile("c1", &defaults));
        assert!(!can_delete_profile("x1", &defaults));
        assert!(can_delete_profile("c2", &defaults));
    }

    #[test]
    fn fr_l17_seed_makes_one_fixed_profile_per_harness_from_real_accounts() {
        let mut a = info("a1", None, false);
        let mut b = info("a2", None, false);
        let mut c = info("c1", None, false);
        a.harness = AgentHarness::ClaudeCode;
        b.harness = AgentHarness::ClaudeCode;
        c.harness = AgentHarness::Codex;
        let ps = seed_profiles(&[a, b, c], &s(&["a2"]));
        assert_eq!(ps.len(), 2);
        let claude = ps
            .iter()
            .find(|p| p.harness == AgentHarness::ClaudeCode)
            .unwrap();
        assert_eq!(claude.mode, AccountMode::Fixed);
        assert_eq!(claude.accounts, s(&["a2"]));
        let codex = ps
            .iter()
            .find(|p| p.harness == AgentHarness::Codex)
            .unwrap();
        assert_eq!(codex.accounts, s(&["c1"]));
        assert!(seed_profiles(&[], &[]).is_empty());
    }

    // ---- account ordering ----------------------------------------------------------------

    #[test]
    fn fr_l17_move_up_and_down_swaps_neighbours() {
        let mut v = s(&["a", "b", "c"]);
        assert!(move_account(&mut v, 1, -1));
        assert_eq!(v, s(&["b", "a", "c"]));
        assert!(move_account(&mut v, 1, 1));
        assert_eq!(v, s(&["b", "c", "a"]));
    }

    #[test]
    fn fr_l17_the_ends_stay_put() {
        let mut v = s(&["a", "b"]);
        assert!(!move_account(&mut v, 0, -1));
        assert!(!move_account(&mut v, 1, 1));
        assert!(!move_account(&mut v, 5, 1));
        assert_eq!(v, s(&["a", "b"]));
    }

    #[test]
    fn fr_l17_reordering_changes_the_account_the_next_session_starts_on() {
        let accts = vec![info("a", Some(80), false), info("b", Some(80), false)];
        let mut p = auto_profile(&["a", "b"]);
        assert_eq!(pick_account(&p, &accts).account.as_deref(), Some("a"));
        move_account(&mut p.accounts, 1, -1);
        assert_eq!(pick_account(&p, &accts).account.as_deref(), Some("b"));
    }

    #[test]
    fn fr_l17_add_skips_duplicates_and_remove_keeps_the_last_account() {
        let mut v = s(&["a"]);
        assert!(!add_account(&mut v, "a"));
        assert!(add_account(&mut v, "b"));
        assert_eq!(v, s(&["a", "b"]));
        assert!(remove_account(&mut v, 0));
        assert!(!remove_account(&mut v, 0));
        assert_eq!(v, s(&["b"]));
        assert!(!remove_account(&mut v, 9));
    }

    // ---- threshold and routing preview ---------------------------------------------------

    #[test]
    fn fr_l17_threshold_steps_by_five_and_clamps() {
        assert_eq!(step_threshold(90, 1), 95);
        assert_eq!(step_threshold(90, -1), 85);
        assert_eq!(step_threshold(95, 3), 100);
        assert_eq!(step_threshold(15, -5), 10);
        assert_eq!(
            threshold_label(90),
            "사용률 90%에서 다음 계정으로 넘어갑니다"
        );
    }

    #[test]
    fn fr_l17_an_account_over_the_threshold_is_skipped() {
        // 5% left is 95% used, over a 90% threshold.
        let accts = vec![info("a", Some(5), false), info("b", Some(60), false)];
        let p = auto_profile(&["a", "b"]);
        let pick = pick_account(&p, &accts);
        assert_eq!(pick.account.as_deref(), Some("b"));
        assert_eq!(
            pick.skipped,
            vec![("a".to_string(), SkipReason::OverThreshold)]
        );
    }

    #[test]
    fn fr_l17_exactly_at_the_threshold_switches() {
        // 10% left is 90% used: reached, so skipped.
        let accts = vec![info("a", Some(10), false), info("b", Some(50), false)];
        let pick = pick_account(&auto_profile(&["a", "b"]), &accts);
        assert_eq!(pick.account.as_deref(), Some("b"));
        let accts = vec![info("a", Some(11), false), info("b", Some(50), false)];
        assert_eq!(
            pick_account(&auto_profile(&["a", "b"]), &accts)
                .account
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn fr_u3_a_rate_limited_account_is_skipped_with_its_reason() {
        let accts = vec![info("a", None, true), info("b", None, false)];
        let pick = pick_account(&auto_profile(&["a", "b"]), &accts);
        assert_eq!(pick.account.as_deref(), Some("b"));
        assert_eq!(pick.skipped, vec![("a".to_string(), SkipReason::Limited)]);
    }

    #[test]
    fn fr_u3_unknown_usage_is_never_skipped_for_the_threshold() {
        let accts = vec![info("a", None, false), info("b", Some(90), false)];
        assert_eq!(
            pick_account(&auto_profile(&["a", "b"]), &accts)
                .account
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn fr_u3_most_left_routing_prefers_the_account_with_more_left_and_unknown_last() {
        let accts = vec![
            info("a", Some(30), false),
            info("b", Some(80), false),
            info("c", None, false),
        ];
        let mut p = auto_profile(&["a", "b", "c"]);
        p.policy = Routing::MostLeft;
        assert_eq!(pick_account(&p, &accts).account.as_deref(), Some("b"));
        // Ties keep the stored order.
        let tie = vec![info("a", Some(50), false), info("b", Some(50), false)];
        let mut p2 = auto_profile(&["b", "a"]);
        p2.policy = Routing::MostLeft;
        assert_eq!(pick_account(&p2, &tie).account.as_deref(), Some("b"));
    }

    #[test]
    fn fr_l17_a_fixed_profile_always_uses_its_account() {
        let accts = vec![info("a", Some(1), true), info("b", Some(90), false)];
        let mut p = auto_profile(&["a", "b"]);
        p.mode = AccountMode::Fixed;
        let pick = pick_account(&p, &accts);
        assert_eq!(pick.account.as_deref(), Some("a"));
        assert!(pick.skipped.is_empty());
    }

    #[test]
    fn fr_l17_nothing_usable_gives_no_account() {
        let accts = vec![info("a", None, true)];
        let pick = pick_account(&auto_profile(&["a"]), &accts);
        assert_eq!(pick.account, None);
        assert_eq!(pick_account(&auto_profile(&["zz"]), &accts).account, None);
        let label = |id: &str| id.to_string();
        assert_eq!(preview_text(&pick, &label), "쓸 수 있는 계정이 없습니다");
    }

    #[test]
    fn fr_l17_preview_names_the_account_and_the_skipped_ones() {
        let pick = Pick {
            account: Some("b".into()),
            skipped: vec![("a".into(), SkipReason::Limited)],
        };
        let label = |id: &str| format!("<{id}>");
        let t = preview_text(&pick, &label);
        assert!(t.contains("<b>"));
        assert!(t.contains("<a>"));
        assert!(t.contains("한도"));
    }

    #[test]
    fn fr_l17_failure_handling_offers_next_account_only_when_automatic() {
        assert_eq!(
            fail_options(AccountMode::Auto),
            vec![OnFail::NextAccount, OnFail::Wait, OnFail::Stop]
        );
        assert_eq!(
            fail_options(AccountMode::Fixed),
            vec![OnFail::Wait, OnFail::Stop]
        );
    }

    #[test]
    fn fr_l17_switching_the_mode_fixes_the_account_list_and_failure_handling() {
        let mut p = auto_profile(&["a", "b"]);
        set_account_mode(&mut p, AccountMode::Fixed, &s(&["a", "b", "c"]));
        assert_eq!(p.mode, AccountMode::Fixed);
        assert_eq!(p.accounts, s(&["a"]));
        assert_eq!(p.on_fail, OnFail::Wait);
        set_account_mode(&mut p, AccountMode::Auto, &s(&["a", "b", "c"]));
        assert_eq!(p.mode, AccountMode::Auto);
        assert_eq!(p.accounts, s(&["a", "b", "c"]));
        assert_eq!(p.on_fail, OnFail::NextAccount);
    }

    // ---- network and usage ---------------------------------------------------------------

    #[test]
    fn fr_x6_computer_rows_follow_reachability() {
        assert_eq!(computer_status(Reachability::Online).0, "온라인");
        assert_eq!(computer_status(Reachability::Offline).0, "오프라인");
        assert_eq!(computer_status(Reachability::Unknown).0, "알 수 없음");
    }

    #[test]
    fn fr_u7_tokens_today_sum_real_accounts_and_are_unknown_without_any() {
        let acct = |t: u64| ChatGptAccount {
            id: "x".into(),
            label: "x".into(),
            email: None,
            status: "signed_in".into(),
            limited: false,
            limited_until: None,
            limit_reason: None,
            tokens_today: t,
            weekly_cap: None,
        };
        assert_eq!(chatgpt_tokens_today(&[]), None);
        assert_eq!(chatgpt_tokens_today(&[acct(1200), acct(34)]), Some(1234));
        assert_eq!(format_tokens(1234567), "1,234,567");
        assert_eq!(format_tokens(12), "12");
        assert_eq!(format_tokens(0), "0");
    }

    // ---- additions -----------------------------------------------------------------------

    #[test]
    fn fr_u6_only_claude_code_takes_a_proxy_for_now() {
        assert!(proxy_supported(AgentHarness::ClaudeCode));
        assert!(!proxy_supported(AgentHarness::Codex));
        assert!(!proxy_supported(AgentHarness::Antigravity));
    }

    #[test]
    fn fr_l17_the_shown_default_falls_back_to_the_harness_first_profile() {
        let ps = vec![
            profile_of("c1", AgentHarness::ClaudeCode),
            profile_of("c2", AgentHarness::ClaudeCode),
            profile_of("x1", AgentHarness::Codex),
        ];
        let h = AgentHarness::ClaudeCode;
        assert_eq!(effective_default(h, Some("c2"), &ps).as_deref(), Some("c2"));
        assert_eq!(effective_default(h, None, &ps).as_deref(), Some("c1"));
        assert_eq!(effective_default(h, Some("x1"), &ps).as_deref(), Some("c1"));
        assert_eq!(
            effective_default(AgentHarness::Antigravity, None, &ps),
            None
        );
    }

    #[test]
    fn fr_l17_new_profiles_get_the_next_free_number() {
        assert_eq!(
            next_profile_id(&[]),
            ("pf-1".to_string(), "새 프로필 1".to_string())
        );
        let ps = vec![
            profile_of("pf-3", AgentHarness::Codex),
            profile_of("seed-codex", AgentHarness::Codex),
        ];
        assert_eq!(next_profile_id(&ps).0, "pf-4");
        let p = new_profile(&ps, AgentHarness::Codex, "c1");
        assert_eq!(p.accounts, s(&["c1"]));
        assert_eq!(p.mode, AccountMode::Fixed);
        assert_eq!(p.on_fail, OnFail::Wait);
    }

    #[test]
    fn fr_a6_harness_status_reads_the_detected_agents() {
        let d = |k: &str, i: bool, v: Option<&str>| DetectedAgent {
            kind: k.into(),
            installed: i,
            version: v.map(String::from),
            ..Default::default()
        };
        let list = vec![
            d("claude-code", true, Some("2.1.0")),
            d("codex", false, None),
            d("omp", true, Some("1")),
        ];
        assert_eq!(
            harness_status(AgentHarness::ClaudeCode, &list).2.as_deref(),
            Some("2.1.0")
        );
        assert_eq!(
            harness_status(AgentHarness::ClaudeCode, &list).1,
            PillTone::Green
        );
        assert_eq!(harness_status(AgentHarness::Codex, &list).1, PillTone::Grey);
        assert_eq!(harness_status(AgentHarness::Acp, &list).1, PillTone::Green);
        assert_eq!(
            harness_status(AgentHarness::Antigravity, &list).0,
            "설치되어 있지 않습니다"
        );
        assert_eq!(
            harness_status(AgentHarness::ClaudeCode, &[]).0,
            "확인 중입니다"
        );
        assert_eq!(harness_status(AgentHarness::Gemini, &list).0, UNAVAILABLE);
    }

    #[test]
    fn fr_l20_path_shown_only_when_reported() {
        let mut c = det("claude-code", true, Some("2.1.4"));
        assert_eq!(harness_path_text(AgentHarness::ClaudeCode, &[c.clone()], false), NO_INFO);
        c.path = Some("/opt/x/claude".into());
        assert_eq!(harness_path_text(AgentHarness::ClaudeCode, &[c.clone()], false), "/opt/x/claude");
        assert_eq!(harness_path_text(AgentHarness::ClaudeCode, &[c.clone()], true), "/opt/x/claude (예시)");
        // Not installed: a stale path is not shown. Other harness: nothing.
        let mut n = det("codex", false, None);
        n.path = Some("/old/codex".into());
        assert_eq!(harness_path_text(AgentHarness::Codex, &[n], false), NO_INFO);
        assert_eq!(harness_path_text(AgentHarness::Codex, &[c], false), NO_INFO);
        assert_eq!(harness_path_text(AgentHarness::Gemini, &[], false), SOON);
    }

    #[test]
    fn fr_l20_version_text_says_unknown_for_null() {
        assert_eq!(version_text(true, None, false).as_deref(), Some(NO_VERSION));
        assert_eq!(version_text(false, Some("1.0"), false), None);
        assert!(version_text(true, Some("2.1.0"), true).unwrap().ends_with("(예시)"));
    }

    #[test]
    fn fr_l20_rescan_outcome_maps_failures_incl_501() {
        assert_eq!(rescan_failure(Some(501), Some("not_implemented")), RescanFailure::NotImplemented);
        assert_eq!(rescan_failure(Some(501), None), RescanFailure::NotImplemented);
        assert_eq!(rescan_failure(Some(404), None), RescanFailure::Failed);
        assert_eq!(rescan_failure(None, None), RescanFailure::Failed);
        assert_eq!(rescan_sentence(&[], Err(RescanFailure::NotImplemented)), "이 컴퓨터에서는 아직 다시 찾을 수 없습니다");
        assert_eq!(rescan_sentence(&[], Err(RescanFailure::Failed)), RESCAN_FAILED);
    }

    #[test]
    fn fr_l20_detect_answer_replaces_entries_and_keeps_gemini() {
        let before = vec![det("gemini", false, None), det("codex", false, None)];
        let got = vec![ember_client::api::DetectedHarness {
            harness: "codex".into(),
            installed: true,
            version: None,
            path: Some("/p/codex".into()),
            detected_at: Some(1_761_900_000_000),
        }];
        let fresh = detect_to_agents(&got);
        let merged = merge_detected(&before, &fresh);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|d| d.kind == "gemini"));
        let c = merged.iter().find(|d| d.kind == "codex").unwrap();
        assert!(c.installed && c.path.as_deref() == Some("/p/codex"));
        assert_eq!(latest_detected_at(&merged), Some(1_761_900_000_000));
        assert_eq!(latest_detected_at(&before), None);
    }

    #[test]
    fn fr_l20_gemini_rescan_is_disabled_with_a_reason() {
        assert_eq!(rescan_enabled(AgentHarness::Gemini, &Rescan::Idle), (false, Some("준비 중입니다")));
        assert_eq!(rescan_enabled(AgentHarness::Codex, &Rescan::Idle), (true, None));
        assert_eq!(rescan_enabled(AgentHarness::Codex, &Rescan::Running), (false, None));
    }

    #[test]
    fn fr_l20_the_pin_verdict_needs_a_detected_version() {
        assert_eq!(pin_verdict(None, PINNED_ANTIGRAVITY_VERSION), None);
        assert_eq!(pin_verdict(Some(" "), PINNED_ANTIGRAVITY_VERSION), None);
        assert_eq!(pin_verdict(Some("1.2.16"), PINNED_ANTIGRAVITY_VERSION), Some("일치"));
        assert_eq!(pin_verdict(Some("v1.2.16"), PINNED_ANTIGRAVITY_VERSION), Some("일치"));
        assert_eq!(pin_verdict(Some("1.3.0"), PINNED_ANTIGRAVITY_VERSION), Some("다름"));
    }

    #[test]
    fn fr_l20_live_computer_has_no_invented_facts() {
        assert_eq!(computer_facts("mac-mini", false), ComputerFacts::default());
        assert_eq!(computer_facts("win-desktop", false), ComputerFacts::default());
        assert_eq!(fact_text(None, false), NO_INFO);
        assert_eq!(count_text(None), NO_INFO);
    }

    #[test]
    fn fr_l20_sample_computer_facts_are_marked_example() {
        let f = computer_facts("mac-mini", true);
        assert_eq!(fact_text(f.ip.as_deref(), true), "211.36.142.18 (예시)");
        assert_eq!(count_text(f.sessions), "7");
        assert_eq!(computer_facts("gpu-box", true), ComputerFacts::default());
    }

    #[test]
    fn fr_l20_disconnect_note_needs_an_offline_computer_with_sessions() {
        let w = computer_facts("win-desktop", true);
        assert!(disconnect_note(Reachability::Offline, &w).unwrap().contains("세션 3개"));
        assert_eq!(disconnect_note(Reachability::Online, &w), None);
        assert_eq!(disconnect_note(Reachability::Unknown, &w), None);
        assert_eq!(disconnect_note(Reachability::Offline, &ComputerFacts::default()), None);
        let zero = ComputerFacts { sessions: Some(0), fallback: Some("x".into()), ..Default::default() };
        assert_eq!(disconnect_note(Reachability::Offline, &zero), None);
    }

    #[test]
    fn fr_x6_reset_text_only_when_a_source_exists() {
        assert!(!account_reset_line("Codex", false).contains("3일"));
        assert!(account_reset_line("Codex", false).contains(NO_INFO));
        assert!(account_reset_line("Codex", true).contains("(예시)"));
    }

    fn acct(id: &str, label: &str, h: AgentHarness, limited: bool) -> AccountInfo {
        AccountInfo { id: id.into(), label: label.into(), harness: h, left_percent: None, limited }
    }

    fn catalog(h: AgentHarness) -> Vec<(String, String)> {
        match h {
            AgentHarness::ClaudeCode => vec![("opus".into(), "Opus".into()), ("sonnet".into(), "Sonnet".into())],
            AgentHarness::Codex => vec![("gpt-5".into(), "GPT-5".into())],
            _ => vec![],
        }
    }

    #[test]
    fn fr_l20_default_chips_say_not_chosen_when_nothing_is_stored() {
        let chips = default_chips(&[], &[], &LocalStore::new());
        assert_eq!(chips, DefaultChips { harness: None, model: None, account: None });
        assert_eq!(harness_chip_text(&chips, |_, m| m.to_string(), false), NOT_CHOSEN);
        assert_eq!(account_chip_text(&chips, 0, false), NO_ACCOUNTS);
        assert_eq!(account_chip_text(&chips, 2, false), NOT_CHOSEN);
        assert!(!NO_ACCOUNTS.contains("ncmnd"));
    }

    #[test]
    fn fr_l17_agents_sheet_hook_is_debug_only() {
        assert_eq!(parse_sheet_hook(Some("harness"), true), Some(SheetHook::Harness));
        assert_eq!(parse_sheet_hook(Some("account"), true), Some(SheetHook::Account));
        assert_eq!(parse_sheet_hook(Some("add-account"), true), Some(SheetHook::AddAccount));
        assert_eq!(parse_sheet_hook(Some("add-account-login"), true), Some(SheetHook::AddAccountLogin));
        assert_eq!(parse_sheet_hook(Some("nope"), true), None);
        assert_eq!(parse_sheet_hook(None, true), None);
        for v in ["harness", "account", "add-account", "add-account-login"] {
            assert_eq!(parse_sheet_hook(Some(v), false), None);
        }
        let login = hook_add_account(Some(SheetHook::AddAccountLogin));
        match login.phase {
            AddPhase::Created(c) => assert!(c.command.contains(EXAMPLE_MARK) && !c.command.contains("CONFIG_DIR")),
            other => panic!("{other:?}"),
        }
        assert!(!hook_add_account(None).open);
    }

    #[test]
    fn fr_l17_default_chips_stack_below_560_dp() {
        assert!(chips_stacked(390.0) && chips_stacked(559.0));
        assert!(!chips_stacked(560.0) && !chips_stacked(1440.0));
    }

    #[test]
    fn fr_l17_disabled_rows_always_carry_a_reason() {
        let models = |_h: AgentHarness| vec![("m".to_string(), "M".to_string())];
        for detected in [vec![], vec![det("claude-code", true, None)], vec![det("claude-code", true, None), det("codex", true, None)]] {
            let rows = pick_rows(&harness_options(&detected), models);
            assert!(!rows.is_empty());
            for r in rows.iter().filter(|r| !r.enabled) {
                let reason = [NOT_INSTALLED_REASON, HARNESS_SOON_REASON].into_iter().find(|x| r.label.contains(x));
                assert!(reason.is_some(), "disabled row without reason: {}", r.label);
                assert!(r.label.ends_with(reason.unwrap()));
            }
        }
    }

    #[test]
    fn fr_l20_default_chip_model_is_not_seeded_from_the_home_catalog() {
        let store = LocalStore::new();
        // The per-harness card still holds the seeded value; the chip never reads it.
        assert!(store.settings(AgentHarness::ClaudeCode).model.is_some());
        assert_eq!(default_chips(&[], &[], &store).model, None);
        let mut chosen = LocalStore::new();
        let row = PickRow { harness: AgentHarness::Codex, model: Some("gpt-5".into()), label: String::new(), enabled: true };
        assert!(apply_pick(&mut chosen, &row));
        let chips = default_chips(&[], &[], &chosen);
        assert_eq!(harness_chip_text(&chips, |_, m| m.to_uppercase(), false), "Codex · GPT-5");
        assert_eq!(harness_chip_text(&chips, |_, m| m.to_uppercase(), true), "Codex · GPT-5 (예시)");
    }

    #[test]
    fn fr_l20_default_account_chip_shows_the_default_of_the_chosen_harness() {
        let accounts = [acct("c1", "work", AgentHarness::ClaudeCode, false), acct("x1", "plus", AgentHarness::Codex, false)];
        let ids = vec!["c1".to_string(), "x1".to_string()];
        let mut store = LocalStore::new();
        // Two defaults and no chosen harness: no guess.
        assert_eq!(default_chips(&accounts, &ids, &store).account, None);
        store.default_harness = Some(AgentHarness::Codex);
        assert_eq!(default_chips(&accounts, &ids, &store).account.as_deref(), Some("plus"));
        store.default_harness = Some(AgentHarness::ClaudeCode);
        assert_eq!(default_chips(&accounts, &ids, &store).account.as_deref(), Some("work"));
        // One default and no chosen harness: that one.
        assert_eq!(default_chips(&accounts, &ids[..1], &LocalStore::new()).account.as_deref(), Some("work"));
    }

    #[test]
    fn fr_l20_account_options_list_only_real_accounts_and_mark_limited() {
        let accounts = [acct("c1", "work", AgentHarness::ClaudeCode, true), acct("x1", "plus", AgentHarness::Codex, false)];
        let o = account_options(&accounts);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0], AccountOption { id: "c1".into(), label: "Claude Code · work · 한도 가까움".into() });
        assert_eq!(o[1].label, "Codex · plus");
    }

    #[test]
    fn fr_l20_account_options_empty_says_to_add_one() {
        let o = account_options(&[]);
        assert!(o.is_empty());
        assert_eq!(account_sheet_rows(&o), vec![ADD_ACCOUNT_FIRST.to_string()]);
    }

    #[test]
    fn fr_l20_harness_options_disable_undetected_harnesses() {
        let d = [det("claude-code", true, Some("2.1.4")), det("codex", false, None)];
        let o = harness_options(&d);
        assert!(o[0].enabled());
        assert_eq!(o[1].reason, Some(NOT_INSTALLED_REASON));
        assert_eq!(o[2].reason, Some(HARNESS_SOON_REASON));
        let rows = pick_rows(&o, catalog);
        assert_eq!(rows.iter().filter(|r| r.enabled).count(), 2);
        assert_eq!(rows[0].label, "Claude Code · Opus");
        let disabled = rows.iter().find(|r| r.harness == AgentHarness::Codex).unwrap();
        assert!(!disabled.enabled && disabled.label.contains(NOT_INSTALLED_REASON));
        let mut store = LocalStore::new();
        assert!(!apply_pick(&mut store, disabled));
        assert_eq!(store.default_harness, None);
    }

    #[test]
    fn fr_l17_account_notes_are_korean_only() {
        for k in add_account_kinds() {
            if let Some(n) = k.note {
                let ko = account_note_ko(k.kind, n);
                assert!(ko.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)));
                assert!(!ko.chars().any(|c| c.is_ascii_alphabetic()), "{ko}");
            }
        }
        assert_eq!(account_note_ko("antigravity", ANTIGRAVITY_NOTE), "지원 준비 중입니다. 지금은 계정을 만들 수 없습니다");
        assert_eq!(account_note_ko("acp", ACP_NOTE), "이 에이전트는 자신의 명령줄 도구로 로그인합니다. 서버가 계정을 만들지 않습니다");
        assert_eq!(account_note_ko("acp", "Something new."), "지금은 만들 수 없습니다");
    }

    #[test]
    fn fr_l17_login_block_sample_note_once() {
        let AddPhase::Created(c) = sample_created() else { panic!() };
        // the header draws the sample note in example mode, so the block draws none
        let drawn = usize::from(true) + login_block_note(&c, true).iter().count();
        assert_eq!(drawn, 1);
        assert_eq!(login_block_note(&c, false).as_deref(), Some(DEMO_ADD_NOTE));
    }

    #[test]
    fn fr_u1_add_account_sheet_disables_antigravity_and_acp_with_the_servers_note() {
        let kinds = add_account_kinds();
        assert!(kinds[0].enabled() && kinds[1].enabled());
        assert_eq!(kinds[2].note, Some("Antigravity accounts are not supported yet."));
        assert!(!kinds[2].enabled() && !kinds[3].enabled());
        assert!(kinds[3].note.unwrap().starts_with("ACP agent: log in with the agent's own CLI"));
        let mut a = AddAccount::closed();
        a.label = "work".into();
        a.kind = Some("antigravity");
        assert!(!a.can_create());
        a.kind = Some("codex");
        assert!(a.can_create());
        a.label = "  ".into();
        assert!(!a.can_create());
    }

    #[test]
    fn fr_u1_add_account_shows_the_returned_login_command_and_check_status() {
        let made: crate::server::AccountWithLogin = serde_json::from_value(serde_json::json!({
            "id": "a1", "agent": "claude-code", "label": "work", "status": "logged_out",
            "login": { "login": "CLAUDE_CONFIG_DIR=/d claude auth login", "note": "Run on the server." }
        }))
        .unwrap();
        let AddPhase::Created(c) = created_phase(&made) else { panic!("not created") };
        assert_eq!(c.id, "a1");
        assert_eq!(c.command, "CLAUDE_CONFIG_DIR=/d claude auth login");
        assert_eq!(c.note, "Run on the server.");
        assert_eq!(c.status, None);
        assert!(!format!("{c:?}").contains("claude auth login"));
        assert_eq!(check_line("logged_in"), "로그인되어 있습니다");
        assert_eq!(check_line("logged_out"), "아직 로그인하지 않았습니다");
        assert_eq!(check_line("unknown"), "로그인 상태를 알 수 없습니다");
    }

    #[test]
    fn fr_l20_add_account_never_claims_a_login_window() {
        let source = include_str!("cr/agents.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        let old = ["계정 추가 창이", " 열립니다"].concat();
        assert!(!production.contains(&old));
        assert!(NO_LOGIN_WINDOW_NOTE.contains("로그인 창을 여는 기능은 아직 없습니다"));
        assert!(DEFAULTS_STORAGE_NOTE.contains("이 기기에만 저장됩니다"));
    }
}
