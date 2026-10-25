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

use std::sync::Mutex;

use dioxus_compose::prelude::*;

use ember_client::state::Reachability;

use super::components::*;
use super::home::{self, Harness, PermMode, UNAVAILABLE};
use super::theme::*;
use super::components::ButtonKind;
use crate::server::{Account, ChatGptAccount};
use crate::ui::use_ui;
use ember_client::wire::DetectedAgent;

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

    /// Whether the harness can run under the resident shim (FR-S8). Antigravity runs one
    /// process per turn and has none yet.
    pub fn shim_supported(self) -> bool {
        self != AgentHarness::Antigravity
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
}

impl LocalStore {
    pub fn new() -> LocalStore {
        LocalStore {
            harness: AgentHarness::ALL
                .iter()
                .map(|h| default_settings(*h))
                .collect(),
            profiles: None,
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

static LOCAL: Mutex<Option<LocalStore>> = Mutex::new(None);

fn local_load() -> LocalStore {
    LOCAL
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

fn local_save(store: LocalStore) {
    if let Ok(mut g) = LOCAL.lock() {
        *g = Some(store);
    }
}

/// Change the store and keep it for the next time the screen opens.
fn edit(mut store: Signal<LocalStore>, f: impl FnOnce(&mut LocalStore)) {
    let snapshot = {
        let mut w = store.write();
        f(&mut w);
        w.clone()
    };
    local_save(snapshot);
}

/// Edits one profile in the local store.
#[derive(Clone)]
struct ProfileEditor {
    store: Signal<LocalStore>,
    seed: Vec<Profile>,
    id: String,
}

impl ProfileEditor {
    fn apply(&self, f: impl FnOnce(&mut Profile) + 'static) {
        let id = self.id.clone();
        edit_profiles(self.store, self.seed.clone(), move |l| {
            if let Some(p) = l.iter_mut().find(|p| p.id == id) {
                f(p);
            }
        });
    }
}

/// Change the profile list, materialising it from `seed` on the first edit.
fn edit_profiles(store: Signal<LocalStore>, seed: Vec<Profile>, f: impl FnOnce(&mut Vec<Profile>)) {
    edit(store, |s| {
        let list = s.profiles.get_or_insert(seed);
        f(list);
    });
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// What the screen reads from the live client once per frame.
#[derive(Clone, PartialEq)]
struct AgentsLive {
    detected: Vec<DetectedAgent>,
    accounts: Vec<AccountInfo>,
    chatgpt: Vec<ChatGptAccount>,
    default_ids: Vec<String>,
}

fn account_infos(accounts: &[Account]) -> Vec<AccountInfo> {
    accounts
        .iter()
        .filter_map(|a| {
            Some(AccountInfo {
                id: a.id.clone(),
                label: a.label.clone(),
                harness: AgentHarness::of_kind(&a.agent)?,
                // The server reports no remaining usage yet (FR-U3).
                left_percent: None,
                limited: a.limited,
            })
        })
        .collect()
}

#[component]
pub fn Agents() -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let compact = window.is_compact();
    let mut section = use_signal(|| Section::Harness);
    let store = use_signal(local_load);

    let accounts_raw = ui.live.accounts.read().clone();
    let live = AgentsLive {
        detected: ui.live.agents.read().clone(),
        accounts: account_infos(&accounts_raw),
        chatgpt: ui.live.chatgpt.read().clone(),
        default_ids: accounts_raw
            .iter()
            .filter(|a| a.is_default)
            .map(|a| a.id.clone())
            .collect(),
    };
    let pad = if compact { SP[3] } else { SP[6] };

    let nav = rsx! {
        SectionNav {
            current: section(),
            shape: nav_shape(compact),
            on_pick: move |s: Section| section.set(s),
        }
    };
    let body = rsx! {
        ScrollColumn {
            fill_max_width: true,
            weight: 1.0,
            Column { fill_max_width: true, padding: pad, spacing: GAP_CARD,
                Label { text: "에이전트", fs: Fs::Xl3, weight: Weight::Medium }
                match section() {
                    Section::Harness => rsx! { HarnessSection { store, live: live.clone() } },
                    Section::Models => rsx! { ModelsSection { store } },
                    Section::Profiles => rsx! { ProfilesSection { store, live: live.clone() } },
                    Section::Network => rsx! { NetworkSection {} },
                    Section::Usage => rsx! { UsageSection { live: live.clone() } },
                }
            }
        }
    };

    match nav_shape(compact) {
        NavShape::Pills => rsx! {
            Column { fill_max_width: true, fill_max_height: true, {nav} {body} }
        },
        NavShape::Side => rsx! {
            Row { fill_max_width: true, fill_max_height: true, {nav} {body} }
        },
    }
}

/// The section nav: a column on the left, or a row of pills above the content.
#[component]
fn SectionNav(current: Section, shape: NavShape, on_pick: EventHandler<Section>) -> Element {
    match shape {
        NavShape::Side => rsx! {
            Column { padding: SP[3], spacing: SP[1],
                for x in Section::ALL {
                    PickChip {
                        key: "{x.key()}",
                        text: x.label(),
                        selected: x == current,
                        on_click: move |()| on_pick.call(x),
                    }
                }
            }
        },
        NavShape::Pills => rsx! {
            ScrollRow { fill_max_width: true,
                Row { padding: SP[3], spacing: SP[1],
                    for x in Section::ALL {
                        PickChip {
                            key: "{x.key()}",
                            text: x.label(),
                            selected: x == current,
                            on_click: move |()| on_pick.call(x),
                        }
                    }
                }
            }
        },
    }
}

/// A card with a title row (name on the left, pills on the right) and its rows.
#[component]
fn Block(#[props(into)] title: String, pills: Element, children: Element) -> Element {
    rsx! {
        EmberCard { fill_max_width: true,
            Column { fill_max_width: true, padding: PAD_CARD_H, spacing: SP[2],
                Row { fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[1],
                    Label { text: title, fs: Fs::Lg, weight: Weight::Semibold }
                    Spacer { weight: 1.0 }
                    {pills}
                }
                {children}
            }
        }
    }
}

/// One setting: title and hint on the left, the control on the right.
#[component]
fn SettingRow(
    #[props(into)] title: String,
    #[props(into)] hint: String,
    control: Element,
) -> Element {
    let theme = use_ember_theme();
    rsx! {
        Row { fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[2],
            Column { weight: 1.0,
                Label { text: title, fs: Fs::Md, weight: Weight::Medium }
                if !hint.is_empty() {
                    Label { text: hint, fs: Fs::Xs, color: paint(theme.palette.muted) }
                }
            }
            {control}
        }
    }
}

/// A line telling that a part has no server data yet.
#[component]
fn Pending(#[props(into)] text: String) -> Element {
    let theme = use_ember_theme();
    rsx! {
        InsetPanel { fill_max_width: true,
            Label { text: format!("{text} {UNAVAILABLE}"), fs: Fs::Sm, color: paint(theme.palette.muted) }
        }
    }
}

#[component]
fn LocalNote() -> Element {
    let theme = use_ember_theme();
    rsx! {
        // TODO(server): remove when the settings and profiles live on the main server (FR-L20).
        Label { text: LOCAL_STORE_NOTE, fs: Fs::Xs, color: paint(theme.palette.muted) }
    }
}

// ---- 하네스 ---------------------------------------------------------------------------------

#[component]
fn HarnessSection(store: Signal<LocalStore>, live: AgentsLive) -> Element {
    let profiles = effective_profiles(&store.read(), &live);
    rsx! {
        LocalNote {}
        for h in AgentHarness::ALL {
            HarnessCard {
                key: "{h.label()}",
                h,
                store,
                detected: live.detected.clone(),
                profiles: profiles.clone(),
            }
        }
    }
}

fn effective_profiles(store: &LocalStore, live: &AgentsLive) -> Vec<Profile> {
    store
        .profiles
        .clone()
        .unwrap_or_else(|| seed_profiles(&live.accounts, &live.default_ids))
}

#[component]
fn HarnessCard(
    h: AgentHarness,
    store: Signal<LocalStore>,
    detected: Vec<DetectedAgent>,
    profiles: Vec<Profile>,
) -> Element {
    let (state, tone, version) = harness_status(h, &detected);
    let settings = store.read().settings(h).clone();
    let of_h: Vec<Profile> = profiles
        .iter()
        .filter(|p| p.harness == h)
        .cloned()
        .collect();
    let current = effective_default(h, settings.default_profile.as_deref(), &profiles);
    let profile_name = current
        .as_ref()
        .and_then(|id| of_h.iter().find(|p| p.id == *id))
        .map_or("프로필 없음".to_string(), |p| p.name.clone());
    let options: Vec<Option<String>> = std::iter::once(None)
        .chain(of_h.iter().map(|p| Some(p.id.clone())))
        .collect();

    rsx! {
        Block {
            title: h.label(),
            pills: rsx! {
                if let Some(v) = version { Pill { text: v, tone: PillTone::Grey, small: true } }
                Pill { text: state, tone, small: true }
            },
            SettingRow {
                title: "실행 방식",
                hint: "하네스마다 정해진 프로토콜로 실행합니다.",
                control: rsx! { Pill { text: h.run_mode(), tone: PillTone::Sky } },
            }
            if let Some(on) = settings.sandbox {
                SettingRow {
                    title: "샌드박스",
                    hint: if on {
                        "켜져 있습니다. 샌드박스 안에서는 셸 쓰기가 실패합니다."
                    } else {
                        "기본으로 꺼져 있습니다. 승인 훅이 모든 도구 호출을 확인합니다."
                    },
                    control: rsx! {
                        Switch {
                            checked: on,
                            on_change: move |v: bool| edit(store, |s| s.settings_mut(h).sandbox = Some(v)),
                        }
                    },
                }
            }
            SettingRow {
                title: "상주 shim",
                hint: if h.shim_supported() {
                    "서버가 멈춰도 에이전트가 계속 실행되고, 다시 시작되면 이어 붙습니다."
                } else {
                    "턴마다 프로세스를 새로 띄우므로 아직 shim 없이 직접 실행합니다."
                },
                control: rsx! {
                    if h.shim_supported() {
                        Switch {
                            checked: settings.shim,
                            on_change: move |v: bool| edit(store, |s| s.settings_mut(h).shim = v),
                        }
                    } else {
                        Pill { text: UNAVAILABLE, tone: PillTone::Grey }
                    }
                },
            }
            SettingRow {
                title: "세션당 메모리 한도",
                hint: "넘기면 그 세션만 끝납니다. 누르면 값이 바뀝니다.",
                control: rsx! {
                    PickChip {
                        text: mem_limit_label(settings.mem_limit_mb),
                        on_click: move |()| edit(store, |s| {
                            let m = s.settings_mut(h);
                            m.mem_limit_mb = next_mem_limit(m.mem_limit_mb);
                        }),
                    }
                },
            }
            SettingRow {
                title: "기본 프로필",
                hint: "이 하네스의 새 세션이 쓰는 프로필입니다. 누르면 다음 프로필로 바뀝니다.",
                control: rsx! {
                    PickChip {
                        text: profile_name,
                        on_click: move |()| {
                            let next = cycle(&options, Some(&current)).unwrap_or(None);
                            edit(store, |s| s.settings_mut(h).default_profile = next);
                        },
                    }
                },
            }
        }
    }
}

// ---- 모델 -----------------------------------------------------------------------------------

#[component]
fn ModelsSection(store: Signal<LocalStore>) -> Element {
    rsx! {
        LocalNote {}
        for h in AgentHarness::ALL {
            ModelCard { key: "{h.label()}", h, store }
        }
    }
}

#[component]
fn ModelCard(h: AgentHarness, store: Signal<LocalStore>) -> Element {
    let settings = store.read().settings(h).clone();
    let Some(ph) = h.picker_harness() else {
        // TODO(server): the server exposes no model list, and only the three picker harnesses
        // have a local one (home.rs).
        return rsx! {
            Block { title: h.label(), pills: rsx! {},
                Pending { text: "모델 목록은" }
            }
        };
    };
    let mut models: Vec<(&'static str, &'static str)> = Vec::new();
    for o in home::model_catalog()
        .into_iter()
        .filter(|o| o.harness == ph)
    {
        if !models.iter().any(|(id, _)| *id == o.model) {
            models.push((o.model, o.name));
        }
    }
    let modes = home::modes_for(ph);
    rsx! {
        Block {
            title: h.label(),
            pills: rsx! {
                if h == AgentHarness::Gemini { Pill { text: UNAVAILABLE, tone: PillTone::Grey, small: true } }
            },
            SettingRow { title: "기본 모델", hint: "새 세션에서 처음 고르는 모델입니다.", control: rsx! {} }
            ScrollRow { fill_max_width: true,
                Row { spacing: SP[1],
                    for (id, name) in models {
                        PickChip {
                            key: "{id}",
                            text: name,
                            selected: settings.model.as_deref() == Some(id),
                            on_click: move |()| edit(store, |s| s.settings_mut(h).model = Some(id.to_string())),
                        }
                    }
                }
            }
            if ph.has_effort() {
                SettingRow { title: "기본 강도", hint: "", control: rsx! {} }
                Row { spacing: SP[1],
                    for e in home::EFFORTS {
                        PickChip {
                            key: "{e}",
                            text: e,
                            selected: settings.effort.as_deref() == Some(e),
                            on_click: move |()| edit(store, |s| s.settings_mut(h).effort = Some(e.to_string())),
                        }
                    }
                }
            }
            SettingRow {
                title: "기본 권한 모드",
                hint: "바이패스는 모든 명령을 묻지 않고 실행합니다.",
                control: rsx! {
                    if settings.mode.is_bypass() { Pill { text: "주의", tone: PillTone::Orange, small: true } }
                },
            }
            ScrollRow { fill_max_width: true,
                Row { spacing: SP[1],
                    for m in modes {
                        PickChip {
                            key: "{m.label()}",
                            text: m.label(),
                            selected: settings.mode == *m,
                            on_click: {
                                let m = *m;
                                move |()| edit(store, |s| s.settings_mut(h).mode = m)
                            },
                        }
                    }
                }
            }
            Pending { text: "플랜별 사용 가능 여부는" }
        }
    }
}

// ---- 프로필 ---------------------------------------------------------------------------------

#[component]
fn ProfilesSection(store: Signal<LocalStore>, live: AgentsLive) -> Element {
    let profiles = effective_profiles(&store.read(), &live);
    let defaults: Vec<Option<String>> = AgentHarness::ALL
        .iter()
        .map(|h| {
            effective_default(
                *h,
                store.read().settings(*h).default_profile.as_deref(),
                &profiles,
            )
        })
        .collect();
    let first = live.accounts.first().cloned();
    let seed = profiles.clone();
    let for_new = profiles.clone();
    rsx! {
        LocalNote {}
        Row { fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[2],
            Label { text: "프로필은 계정과 라우팅을 묶어 하네스에 할당합니다.", fs: Fs::Sm }
            Spacer { weight: 1.0 }
            AppButton {
                text: "새 프로필",
                icon: IconRole::Add,
                enabled: first.is_some(),
                on_click: move |()| {
                    if let Some(a) = &first {
                        let p = new_profile(&for_new, a.harness, &a.id);
                        edit_profiles(store, seed.clone(), |l| l.push(p));
                    }
                },
            }
        }
        if live.accounts.is_empty() {
            Pending { text: "계정 목록은" }
        }
        for p in profiles.clone() {
            ProfileCard {
                key: "{p.id}",
                can_delete: can_delete_profile(&p.id, &defaults),
                p: p.clone(),
                all: profiles.clone(),
                live: live.clone(),
                store,
            }
        }
    }
}

fn account_label(accounts: &[AccountInfo], id: &str) -> String {
    accounts
        .iter()
        .find(|a| a.id == id)
        .map_or_else(|| id.to_string(), |a| a.label.clone())
}

#[component]
fn ProfileCard(
    p: Profile,
    all: Vec<Profile>,
    can_delete: bool,
    live: AgentsLive,
    store: Signal<LocalStore>,
) -> Element {
    let theme = use_ember_theme();
    let id = p.id.clone();
    let same: Vec<AccountInfo> = live
        .accounts
        .iter()
        .filter(|a| a.harness == p.harness)
        .cloned()
        .collect();
    let same_ids: Vec<String> = same.iter().map(|a| a.id.clone()).collect();
    let rest: Vec<AccountInfo> = same
        .iter()
        .filter(|a| !p.accounts.contains(&a.id))
        .cloned()
        .collect();
    let pick = pick_account(&p, &live.accounts);
    let label_of = |aid: &str| account_label(&live.accounts, aid);
    let preview = preview_text(&pick, &label_of);
    let fails = fail_options(p.mode);
    let proxy_hint = p
        .proxy
        .clone()
        .unwrap_or_else(|| "http://localhost:8080".to_string());

    // Every edit is on this profile.
    let ed = ProfileEditor {
        store,
        seed: all.clone(),
        id,
    };

    rsx! {
        Block {
            title: p.name.clone(),
            pills: rsx! {
                Pill { text: p.harness.label(), tone: PillTone::Blue, small: true }
                Pill { text: if p.mode == AccountMode::Fixed { "고정" } else { "자동 전환" }, tone: PillTone::Grey, small: true }
            },
            SettingRow {
                title: "이름",
                hint: "비워 두면 이름이 바뀌지 않습니다.",
                control: rsx! {
                    // TODO(dioxus-compose): TextField is uncontrolled and has no initial value,
                    // so the current name is the placeholder.
                    TextField {
                        placeholder: p.name.clone(),
                        on_value_change: {
                            let ed = ed.clone();
                            move |v: String| {
                                let v = v.trim().to_string();
                                if !v.is_empty() { ed.apply(move |p| p.name = v); }
                            }
                        },
                    }
                },
            }
            SettingRow { title: "계정 방식", hint: "", control: rsx! {} }
            Row { spacing: SP[1],
                for (mode, text) in [(AccountMode::Fixed, "계정 고정"), (AccountMode::Auto, "자동 전환")] {
                    PickChip {
                        key: "{text}",
                        text,
                        selected: p.mode == mode,
                        on_click: {
                            let ed = ed.clone();
                            let same_ids = same_ids.clone();
                            move |()| {
                                let same_ids = same_ids.clone();
                                ed.apply(move |p| set_account_mode(p, mode, &same_ids));
                            }
                        },
                    }
                }
            }
            if p.mode == AccountMode::Fixed {
                SettingRow { title: "계정", hint: "이 계정 하나로만 실행됩니다.", control: rsx! {} }
                ScrollRow { fill_max_width: true,
                    Row { spacing: SP[1],
                        for a in same.clone() {
                            PickChip {
                                key: "{a.id}",
                                text: a.label.clone(),
                                selected: p.accounts.first() == Some(&a.id),
                                on_click: {
                                    let ed = ed.clone();
                                    let aid = a.id.clone();
                                    move |()| {
                                        let aid = aid.clone();
                                        ed.apply(move |p| p.accounts = vec![aid]);
                                    }
                                },
                            }
                        }
                    }
                }
                if same.is_empty() { Pending { text: "이 하네스의 계정은" } }
            } else {
                SettingRow { title: "전환 순서", hint: "위에서부터 씁니다. 끌기 대신 위로와 아래로 버튼을 씁니다.", control: rsx! {} }
                // TODO(GAPS: 끌기): drag to reorder needs pointer capture; buttons for now.
                for (i, aid) in p.accounts.clone().into_iter().enumerate() {
                    Row { key: "{aid}", fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[1],
                        Label { text: format!("{}", i + 1), fs: Fs::Sm, color: paint(theme.palette.muted) }
                        Label { text: account_label(&live.accounts, &aid), fs: Fs::Md }
                        if live.accounts.iter().any(|a| a.id == aid && a.limited) {
                            Pill { text: "한도 도달", tone: PillTone::Orange, small: true }
                        }
                        Spacer { weight: 1.0 }
                        AppButton {
                            text: "위로",
                            small: true,
                            enabled: i > 0,
                            on_click: {
                                let ed = ed.clone();
                                move |()| ed.apply(move |p| { move_account(&mut p.accounts, i, -1); })
                            },
                        }
                        AppButton {
                            text: "아래로",
                            small: true,
                            enabled: i + 1 < p.accounts.len(),
                            on_click: {
                                let ed = ed.clone();
                                move |()| ed.apply(move |p| { move_account(&mut p.accounts, i, 1); })
                            },
                        }
                        AppButton {
                            text: "빼기",
                            small: true,
                            enabled: p.accounts.len() > 1,
                            on_click: {
                                let ed = ed.clone();
                                move |()| ed.apply(move |p| { remove_account(&mut p.accounts, i); })
                            },
                        }
                    }
                }
                if !rest.is_empty() {
                    ScrollRow { fill_max_width: true,
                        Row { spacing: SP[1],
                            for a in rest {
                                PickChip {
                                    key: "{a.id}",
                                    text: format!("+ {}", a.label),
                                    on_click: {
                                        let ed = ed.clone();
                                        let aid = a.id.clone();
                                        move |()| {
                                            let aid = aid.clone();
                                            ed.apply(move |p| { add_account(&mut p.accounts, &aid); });
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
                SettingRow {
                    title: "전환 사용률",
                    hint: threshold_label(p.threshold),
                    control: rsx! {
                        AppButton {
                            text: "낮추기",
                            small: true,
                            on_click: {
                                let ed = ed.clone();
                                move |()| ed.apply(|p| p.threshold = step_threshold(p.threshold, -1))
                            },
                        }
                        AppButton {
                            text: "높이기",
                            small: true,
                            on_click: {
                                let ed = ed.clone();
                                move |()| ed.apply(|p| p.threshold = step_threshold(p.threshold, 1))
                            },
                        }
                    },
                }
                SettingRow { title: "라우팅 정책", hint: "새 세션의 계정을 고르는 방법입니다.", control: rsx! {} }
                Row { spacing: SP[1],
                    for r in [Routing::Order, Routing::MostLeft] {
                        PickChip {
                            key: "{r.label()}",
                            text: r.label(),
                            selected: p.policy == r,
                            on_click: {
                                let ed = ed.clone();
                                move |()| ed.apply(move |p| p.policy = r)
                            },
                        }
                    }
                }
                if p.policy == Routing::MostLeft {
                    Pending { text: "계정별 남은 사용량은" }
                }
            }
            SettingRow { title: "실패할 때", hint: "속도 제한이나 인증 오류가 나면 이렇게 처리합니다.", control: rsx! {} }
            Row { spacing: SP[1],
                for f in fails {
                    PickChip {
                        key: "{f.label()}",
                        text: f.label(),
                        selected: p.on_fail == f,
                        on_click: {
                            let ed = ed.clone();
                            move |()| ed.apply(move |p| p.on_fail = f)
                        },
                    }
                }
            }
            SettingRow {
                title: "모델 프록시",
                hint: if proxy_supported(p.harness) {
                    "비워 두면 하네스의 공급자를 그대로 씁니다."
                } else {
                    "이 하네스의 프록시 설정은 확인 전입니다."
                },
                control: rsx! {
                    if proxy_supported(p.harness) {
                        TextField {
                            placeholder: proxy_hint,
                            on_value_change: {
                                let ed = ed.clone();
                                move |v: String| {
                                    let v = v.trim().to_string();
                                    ed.apply(move |p| p.proxy = (!v.is_empty()).then_some(v));
                                }
                            },
                        }
                    } else {
                        Pill { text: UNAVAILABLE, tone: PillTone::Grey }
                    }
                },
            }
            InsetPanel { fill_max_width: true,
                Label { text: preview, fs: Fs::Sm }
            }
            Row { fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[2],
                if !can_delete {
                    Label { text: "기본 프로필로 쓰고 있어 지울 수 없습니다.", fs: Fs::Xs, color: paint(theme.palette.muted) }
                }
                Spacer { weight: 1.0 }
                AppButton {
                    text: "프로필 지우기",
                    kind: ButtonKind::Destructive,
                    small: true,
                    enabled: can_delete,
                    on_click: {
                        let seed = all.clone();
                        let pid = p.id.clone();
                        move |()| edit_profiles(store, seed.clone(), |l| l.retain(|x| x.id != pid))
                    },
                }
            }
        }
    }
}

// ---- 네트워크 -------------------------------------------------------------------------------

#[component]
fn NetworkSection() -> Element {
    let ui = use_ui();
    let _ = *ui.live.launcher.read();
    let computers: Vec<(String, Reachability)> = crate::services::services().client.read(|s| {
        s.computers()
            .iter()
            .map(|c| (c.name.clone(), c.reachability))
            .collect()
    });
    rsx! {
        Block {
            title: "에이전트 API 트래픽",
            pills: rsx! { Pill { text: "검증 전 (#130)", tone: PillTone::Orange, small: true } },
            SettingRow {
                title: "현재 컴퓨터로 내보내기",
                hint: "에이전트의 API 요청이 세션이 있는 컴퓨터를 거쳐 나가, 모델 공급자에게 그 컴퓨터의 주소가 보입니다.",
                control: rsx! { Pill { text: UNAVAILABLE, tone: PillTone::Grey } },
            }
            // TODO(server): no endpoint reports or sets egress (EMBER_AGENT_EGRESS, FR-X6).
            Pending { text: "켜기와 끄기는" }
        }
        Block { title: "컴퓨터", pills: rsx! {},
            if computers.is_empty() {
                Pending { text: "컴퓨터 목록은" }
            }
            for (name, r) in computers {
                Row { key: "{name}", fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[2],
                    Label { text: name.clone(), fs: Fs::Md }
                    Spacer { weight: 1.0 }
                    Label { text: format!("주소와 연결 수 {UNAVAILABLE}"), fs: Fs::Xs }
                    Pill { text: computer_status(r).0, tone: computer_status(r).1, small: true }
                }
            }
        }
    }
}

// ---- 사용량 ---------------------------------------------------------------------------------

#[component]
fn UsageSection(live: AgentsLive) -> Element {
    let ui = use_ui();
    let _ = *ui.live.launcher.read();
    let bars = crate::services::services()
        .client
        .read(|s| home::harness_usage(s));
    let tokens = chatgpt_tokens_today(&live.chatgpt);
    let ids: Vec<String> = live.chatgpt.iter().map(|a| a.id.clone()).collect();
    let week: u64 = home::chatgpt_token_days(&ui.live.usage.read(), &ids, &home::today_utc(), 7)
        .iter()
        .map(|(_, t)| *t)
        .sum();
    rsx! {
        Block { title: "오늘 토큰", pills: rsx! {},
            match tokens {
                Some(n) => rsx! {
                    Label { text: format_tokens(n), fs: Fs::Xl3, weight: Weight::Medium }
                    Label { text: format!("최근 7일 {}", format_tokens(week)), fs: Fs::Sm }
                    Label { text: "ChatGPT 플랜 계정이 보고한 토큰만 합친 값입니다. 다른 하네스는 준비 중입니다.", fs: Fs::Xs }
                },
                None => rsx! { Pending { text: "토큰 사용량은" } },
            }
            // TODO(server): other harnesses and token kinds (cache) are not reported by /api/usage yet.
        }
        Block { title: "하네스별", pills: rsx! {},
            for b in bars {
                Row { key: "{b.label}", fill_max_width: true, alignment: Alignment::CenterStart,
                    Label { text: b.label.clone(), fs: Fs::Md }
                    Spacer { weight: 1.0 }
                    Label { text: format!("세션 {}개", b.sessions), fs: Fs::Sm }
                }
            }
            Pending { text: "하네스별 비용과 모델별 토큰은" }
        }
        Block { title: "계정", pills: rsx! {},
            if live.accounts.is_empty() {
                Pending { text: "계정 목록은" }
            }
            for a in live.accounts.clone() {
                Row { key: "{a.id}", fill_max_width: true, alignment: Alignment::CenterStart, spacing: SP[1],
                    Label { text: a.label.clone(), fs: Fs::Md }
                    Pill { text: a.harness.label(), tone: PillTone::Grey, small: true }
                    Spacer { weight: 1.0 }
                    if a.limited {
                        Pill { text: "한도 도달", tone: PillTone::Orange, small: true }
                    } else {
                        Pill { text: "사용 가능", tone: PillTone::Green, small: true }
                    }
                }
            }
            Pending { text: "계정별 남은 사용량 링은" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fr_s8_antigravity_has_no_shim_yet_the_others_default_to_one() {
        assert!(!AgentHarness::Antigravity.shim_supported());
        assert!(!default_settings(AgentHarness::Antigravity).shim);
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
}
