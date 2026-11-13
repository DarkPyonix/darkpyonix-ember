//! compose-rust version of the Agents (에이전트) screen (#220 phase 7; FR-L20, FR-L17,
//! `docs/DESIGN.md` section 6.7): harnesses, models, profiles, network and usage.
//!
//! The decisions (section routing, defaults, account picking, threshold stepping,
//! local store) are `ui::agents`' plain functions, re-exported as they are.
//! Drawing and orchestration use compose-rust MutableState.
//!
//! Only data the client holds is shown as fact: the detected agents (installed, version),
//! the accounts, the ChatGPT accounts' tokens today and the computers' reachability.
//! The server has no settings, profile, egress or usage API yet, so every setting here
//! lives in a local store ([`LOCAL_STORE_NOTE`]) and every missing figure says "준비 중입니다".

#![allow(non_snake_case)]

use std::sync::Mutex;

use compose_rust::drawing::DrawList;
use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, Color, IconRole, ShapeRole};
use ember_client::state::Reachability;
use ember_client::wire::DetectedAgent;

pub use crate::ui::agents::{
    add_account, remove_account, can_delete_profile, chatgpt_tokens_today, computer_status, cycle, default_settings,
    effective_default, fail_options, format_tokens, harness_status, mem_limit_label, move_account,
    nav_shape, new_profile, next_mem_limit, next_profile_id, pick_account, preview_text,
    proxy_supported, resolve_profile, resolve_section, seed_profiles,
    set_account_mode, step_threshold, threshold_label, AccountInfo, AccountMode, AgentHarness,
    HarnessSettings, LocalStore, NavShape, OnFail, Pick, Profile, Routing, Section, SkipReason,
    LOCAL_STORE_NOTE, MEM_STEPS_MB, THRESHOLD_MAX, THRESHOLD_MIN, THRESHOLD_STEP,
};
use super::components::{
    AppButton, ButtonKind, EmberCard, EmberField, FieldOptions, InsetPanel,
    Label, OptionSheet, Pill, PillTone,
};
use super::theme::*;
use crate::server::{Account, ChatGptAccount};
use crate::ui::home::UNAVAILABLE;
use crate::ui::usage::{self, Load};
use ember_client::api::UsageSummary;

const ICON_USER_SVG: &str = include_str!("../../../assets/nav/user.svg");
const ICON_GLOBE_SVG: &str = include_str!("../../../assets/nav/globe.svg");
const ICON_CHART_SVG: &str = include_str!("../../../assets/nav/chart.svg");
const ICON_REPEAT_SVG: &str = include_str!("../../../assets/nav/repeat.svg");
const ICON_ARROW_UP_RIGHT_SVG: &str = include_str!("../../../assets/nav/arrow-up-right.svg");
const ICON_CODE_SVG: &str = include_str!("../../../assets/nav/code.svg");
const ICON_SHIELD_SVG: &str = include_str!("../../../assets/nav/shield.svg");
const ICON_PC_SVG: &str = include_str!("../../../assets/nav/pc.svg");
const ICON_SPARK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.5"><path stroke-linecap="round" stroke-linejoin="round" d="M9.813 15.904 9 18.75l-.813-2.846a4.5 4.5 0 0 0-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 0 0 3.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 0 0 3.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 0 0-3.09 3.09ZM18.259 8.715 18 9.75l-.259-1.035a3.375 3.375 0 0 0-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 0 0 2.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 0 0 2.456 2.456L21.75 6l-1.035.259a3.375 3.375 0 0 0-2.456 2.456ZM16.894 20.567 16.5 21.75l-.394-1.183a2.25 2.25 0 0 0-1.423-1.423L13.5 18.75l1.183-.394a2.25 2.25 0 0 0 1.423-1.423l.394-1.183.394 1.183a2.25 2.25 0 0 0 1.423 1.423l1.183.394-1.183.394a2.25 2.25 0 0 0-1.423 1.423Z"/></svg>"#;

// ---------------------------------------------------------------------------------------------
// The local store (FR-L20, LOCAL_STORE_NOTE)
// ---------------------------------------------------------------------------------------------

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

/// Apply the server's stored `AgentDefaults` and `AgentSettingsResponse` into the local store
/// and wake any mounted `AgentsScreen` (FR-L20, #481, #670).
pub fn sync_from_server(
    defaults: Option<&crate::server::AgentDefaults>,
    settings: Option<&crate::server::AgentSettingsResponse>,
) {
    let mut cur = local_load();
    let before = cur.clone();
    if let Some(d) = defaults {
        cur.apply_server_defaults(d);
    }
    if let Some(s) = settings {
        cur.apply_server_settings(s);
    }
    if cur != before {
        local_save(cur);
        if let Some(s) = crate::services::try_services() {
            crate::ui::coexist::SharedLive::bump(&s.ui.live.agent_store_rev);
        }
    }
}

/// Persist the current default harness, model, effort and mode to `PUT /api/agents/defaults`.
fn save_server_defaults(defaults: crate::server::AgentDefaults) {
    if crate::services::try_services().is_none() {
        return;
    }
    crate::services::run_shared(
        async move {
            let s = crate::services::services();
            if s.demo.is_some() {
                return None;
            }
            match s.server.put_agent_defaults(&defaults).await {
                Ok(saved) => {
                    s.ui.live.agent_defaults.set(saved);
                    None
                }
                Err(e) => Some(crate::ui::agents::server_error_status(&e)),
            }
        },
        |failed| {
            if let Some(status) = failed {
                post_notice(&format!(
                    "기본값을 저장하지 못했습니다. {}",
                    crate::ui::agents::account_error_ko(status)
                ));
            }
        },
    );
}

/// Persist a harness's memory limit to `PUT /api/agents/{agent}/settings` (FR-L20, FR-S8).
fn save_server_mem_limit(h: AgentHarness, mem_limit_mb: Option<u32>) {
    let Some(agent) = h.kind().map(str::to_string) else { return };
    if crate::services::try_services().is_none() {
        return;
    }
    crate::services::run_shared(
        async move {
            let s = crate::services::services();
            if s.demo.is_some() {
                return None;
            }
            let mb = mem_limit_mb.map(u64::from);
            s.server.put_agent_mem_limit(&agent, mb).await.err().map(|e| crate::ui::agents::server_error_status(&e))
        },
        |failed| {
            if let Some(status) = failed {
                post_notice(&format!(
                    "메모리 한도를 저장하지 못했습니다. {}",
                    crate::ui::agents::account_error_ko(status)
                ));
            }
        },
    );
}

/// Change the store and keep it for the next time the screen opens.
fn edit(store: &MutableState<LocalStore>, f: impl FnOnce(&mut LocalStore)) {
    let mut cur = store.get_untracked();
    f(&mut cur);
    local_save(cur.clone());
    store.set(cur);
}

/// Change the profile list, materializing it from `seed` on the first edit.
fn edit_profiles(
    store: &MutableState<LocalStore>,
    seed: Vec<Profile>,
    f: impl FnOnce(&mut Vec<Profile>),
) {
    edit(store, |s| {
        let list = s.profiles.get_or_insert(seed);
        f(list);
    });
}

/// Edits one profile in the local store.
#[derive(Clone)]
struct ProfileEditor {
    store: MutableState<LocalStore>,
    seed: Vec<Profile>,
    id: String,
}

impl ProfileEditor {
    fn apply(&self, f: impl FnOnce(&mut Profile) + 'static) {
        let id = self.id.clone();
        edit_profiles(&self.store, self.seed.clone(), move |l| {
            if let Some(p) = l.iter_mut().find(|p| p.id == id) {
                f(p);
            }
        });
    }
}

fn post_notice(text: &str) {
    if crate::services::try_services().is_some() {
        super::notice::post(text);
    }
}

// ---------------------------------------------------------------------------------------------
// Live state and helper adapters
// ---------------------------------------------------------------------------------------------

/// What the screen reads from the live client once per frame.
#[derive(Clone, PartialEq, Default)]
pub struct AgentsLive {
    pub detected: Vec<DetectedAgent>,
    pub accounts: Vec<AccountInfo>,
    pub chatgpt: Vec<ChatGptAccount>,
    pub default_ids: Vec<String>,
    /// Review-capture sample data (`EMBER_CR_ROUTE=agents`): numbers are flagged 예시.
    pub sample: bool,
    /// Demo mode (`--demo`): agents, versions and counts are canned, so they carry 예시.
    pub demo: bool,
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

fn account_label(accounts: &[AccountInfo], id: &str) -> String {
    accounts
        .iter()
        .find(|a| a.id == id)
        .map_or_else(|| id.to_string(), |a| a.label.clone())
}

fn effective_profiles(store: &LocalStore, live: &AgentsLive) -> Vec<Profile> {
    store
        .profiles
        .clone()
        .unwrap_or_else(|| seed_profiles(&live.accounts, &live.default_ids))
}

// ---------------------------------------------------------------------------------------------
// Screen entries (FR-L20)
// ---------------------------------------------------------------------------------------------

/// The main Agents screen, called from the shell.
pub fn AgentsScreen() {
    // `EMBER_CR_AGENTS_LIVE=1` (debug builds, with `EMBER_CR_ROUTE=agents`) opens the screen on
    // the real data path, so a capture can show the honest empty state with no server.
    let live_path = cfg!(debug_assertions) && std::env::var("EMBER_CR_AGENTS_LIVE").as_deref() == Ok("1");
    let is_sample = std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("agents") && !live_path;
    let (live, computers, bars, usage_load) = if is_sample {
        (sample_live(), sample_computers(), sample_bars(), Load::Ready(crate::demo::usage_summary(crate::demo::now_ms())))
    } else if let Some(s) = crate::services::try_services() {
        let _ = s.ui.live.launcher.get();
        let accounts_raw = s.ui.live.accounts.get();
        let detected = s.ui.live.agents.get();
        let chatgpt = s.ui.live.chatgpt.get();
        let default_ids: Vec<String> = accounts_raw
            .iter()
            .filter(|a| a.is_default)
            .map(|a| a.id.clone())
            .collect();
        let accounts = account_infos(&accounts_raw);
        let live = AgentsLive {
            detected,
            accounts,
            chatgpt: chatgpt.clone(),
            default_ids,
            sample: false,
            demo: s.demo.is_some(),
        };
        let computers = s.client.read(|st| {
            st.computers()
                .iter()
                .map(|c| (c.name.clone(), c.reachability))
                .collect()
        });
        let bars = s.client.read(|st| crate::ui::home::harness_usage(st));
        let week = s.ui.live.usage_summary.get();
        (live, computers, bars, week)
    } else {
        (AgentsLive::default(), Vec::new(), Vec::new(), Load::Loading)
    };

    let initial_section = std::env::var("EMBER_CR_AGENTS_SECTION")
        .ok()
        .map(|sec| resolve_section(&sec))
        .unwrap_or_else(|| match crate::ui::agents::sheet_hook() {
            Some(crate::ui::agents::SheetHook::Harness | crate::ui::agents::SheetHook::Account) => Section::Models,
            Some(crate::ui::agents::SheetHook::AddAccount | crate::ui::agents::SheetHook::AddAccountLogin) => Section::Profiles,
            None => Section::Harness,
        });

    let _requested = remember(|| {
        if !is_sample {
            crate::bridge::request_usage_summary();
        }
        0u8
    });
    AgentsScreenWith(live, computers, bars, usage_load, initial_section);
}

pub fn AgentsScreenWith(
    live: AgentsLive,
    computers: Vec<(String, Reachability)>,
    bars: Vec<crate::ui::home::HarnessBar>,
    usage_load: Load<UsageSummary>,
    initial_section: Section,
) {
    let p = ember_theme().palette;
    let section = remember(move || mutable_state_of(initial_section));
    let store = remember(|| mutable_state_of(local_load()));
    if let Some(s) = crate::services::try_services() {
        let rev = s.ui.live.agent_store_rev.get();
        let seen = remember(move || mutable_state_of(rev));
        if seen.get_untracked() != rev {
            seen.set(rev);
            store.set(local_load());
        }
    }
    let window = compose_rust::runtime::current_window_size();
    let compact = window.is_compact() || window.width_dp <= 900.0;
    let pad = if compact { SP[3] } else { SP[6] };
    let cur_sec = section.get();
    let ui = crate::services::try_services().map(|s| s.ui.clone());

    let nav_pick = {
        let section = section.clone();
        move |s: Section| section.set(s)
    };

    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(|| {
            cu::Column()
                .modifier(Modifier::new().fill_max_width().padding(pad))
                .content(|| {
                    // Page Header spanning full width
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .modifier(Modifier::new().fill_max_width())
                        .content(|| {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                Label(
                                    "에이전트",
                                    if compact { Fs::Xl2 } else { Fs::Xl3 },
                                    Weight::Semibold,
                                    None,
                                    Some(1),
                                );
                                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                                Label(
                                    "하네스와 모델, 프로필, 네트워크, 사용량을 설정합니다",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    None,
                                );
                            });
                            if let (false, Some(ui)) = (compact, ui.as_ref()) {
                                super::project::ThemeButton(ui);
                            }
                        });

                    cu::Spacer().modifier(Modifier::new().height(if compact { SP[2] } else { SP[4] }));

                    if compact {
                        // Compact: horizontal scrolling pills below the header
                        SectionNav(cur_sec, NavShape::Pills, nav_pick);
                        cu::Spacer().modifier(Modifier::new().height(SP[3]));
                        cu::Column()
                            .spacing(20.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                match cur_sec {
                                    Section::Harness => HarnessSection(&store, &live, compact),
                                    Section::Models => ModelsSection(&store, &live, compact),
                                    Section::Profiles => ProfilesSection(&store, &live, compact),
                                    Section::Network => NetworkSection(&computers, live.sample, compact),
                                    Section::Usage => UsageSection(&live, &bars, &usage_load, compact),
                                }
                            });
                    } else {
                        // Desktop: navigation on the left and the remaining width for the body
                        cu::Row()
                            .spacing(25.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                cu::Column().modifier(Modifier::new().width(200.0)).content(|| {
                                    SectionNav(cur_sec, NavShape::Side, nav_pick);
                                });
                                cu::Column()
                                    .spacing(20.0)
                                    .modifier(Modifier::new().weight(1.0))
                                    .content(|| {
                                        match cur_sec {
                                            Section::Harness => HarnessSection(&store, &live, compact),
                                            Section::Models => ModelsSection(&store, &live, compact),
                                            Section::Profiles => ProfilesSection(&store, &live, compact),
                                            Section::Network => NetworkSection(&computers, live.sample, compact),
                                            Section::Usage => UsageSection(&live, &bars, &usage_load, compact),
                                        }
                                    });
                            });
                    }
                });
        });
}

/// A review window (`EMBER_CR_ROUTE=agents`).
#[compose_rust::runtime::Composable]
pub fn preview_agents() {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(Modifier::new().fill_max_size().background(paint(p.bg)))
        .content(AgentsScreen);
}

// ---------------------------------------------------------------------------------------------
// Shared section components and helpers
// ---------------------------------------------------------------------------------------------

fn section_svg(s: Section) -> &'static str {
    match s {
        Section::Harness => super::shell::ICON_CPU_SVG,
        Section::Models => ICON_SPARK_SVG,
        Section::Profiles => ICON_USER_SVG,
        Section::Network => ICON_GLOBE_SVG,
        Section::Usage => ICON_CHART_SVG,
    }
}

fn SectionNav(current: Section, shape: NavShape, on_pick: impl FnMut(Section) + Clone + 'static) {
    let p = ember_theme().palette;
    match shape {
        NavShape::Side => {
            cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
                for x in Section::ALL {
                    let selected = x == current;
                    let (bg, fg) = if selected {
                        (p.sel, p.text)
                    } else {
                        (Color::argb(0), p.text_2)
                    };
                    let mut on_pick = on_pick.clone();
                    cu::Box()
                        .modifier(
                            Modifier::new()
                                .width(200.0)
                                .height(H_BTN)
                                .background(paint(bg))
                                .corner_radius(CORNER_SM),
                        )
                        .alignment(Alignment::CenterStart)
                        .content(move || {
                            cu::Row().alignment(Alignment::CenterStart).content(|| {
                                cu::Spacer().modifier(Modifier::new().width(12.0));
                                super::shell::NavIcon(section_svg(x), fg.to_argb(), 18.0);
                                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                Label(x.label(), Fs::Md, Weight::Medium, Some(paint(fg)), Some(1));
                                cu::Spacer().modifier(Modifier::new().weight(1.0));
                            });
                            FocusOverlay(x.label(), move || on_pick(x));
                        });
                }
            });
        }
        NavShape::Pills => {
            cu::ScrollRow().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Row().spacing(SP[1]).content(|| {
                    for x in Section::ALL {
                        let selected = x == current;
                        let (bg, fg, border) = if selected {
                            (p.ink, p.ink_text, p.ink)
                        } else {
                            (p.surface, p.text_2, p.line)
                        };
                        let mut on_pick = on_pick.clone();
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .height(H_CTL)
                                    .background(paint(bg))
                                    .border(1.0, paint(border))
                                    .shape_role(ShapeRole::Full),
                            )
                            .alignment(Alignment::Center)
                            .content(move || {
                                cu::Row().alignment(Alignment::CenterStart).content(|| {
                                    cu::Spacer().modifier(Modifier::new().width(12.0));
                                    super::shell::NavIcon(section_svg(x), fg.to_argb(), 16.0);
                                    cu::Spacer().modifier(Modifier::new().width(6.0));
                                    Label(x.label(), Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                                    cu::Spacer().modifier(Modifier::new().width(12.0));
                                });
                                FocusOverlay(x.label(), move || on_pick(x));
                            });
                    }
                });
            });
        }
    }
}

/// A 36x20 toggle switch matching the mockup (ink track when on, `surface-2` when off, `line`
/// when disabled, a `surface` thumb). The toolkit `Switch` takes the size, the colours and the
/// accessible name since compose-rust v0.0.7 (#426, #372); the switch announces its own on or
/// off state, so its name is `label` alone. The mockup's 1 px `line` ring on an enabled, off
/// switch has no builder and is not drawn.
fn MockSwitch(label: &str, checked: bool, enabled: bool, on_change: impl FnMut(bool) + 'static) {
    let p = ember_theme().palette;
    cu::Switch(checked)
        .enabled(enabled)
        .size(36.0, 20.0)
        .track_color(paint(switch_track(checked, enabled, p)))
        .thumb_color(paint(p.surface))
        .accessible_name(label)
        .on_change(on_change);
}

/// The track colour of [`MockSwitch`]: `line` when disabled, `ink` when on, `surface-2` when off.
fn switch_track(checked: bool, enabled: bool, p: &Palette) -> Color {
    if !enabled {
        p.line
    } else if checked {
        p.ink
    } else {
        p.surface_2
    }
}

/// A 20x20 radio button for model selection matching the mockup.
fn ModelRadio(label: &str, selected: bool, mut on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let mut list = DrawList::builder()
        .circle(paint(p.surface), 10.0, 10.0, 9.0, 0.0);
    if selected {
        list = list
            .circle(paint(p.ink), 10.0, 10.0, 9.0, 1.5)
            .circle(paint(p.ink), 10.0, 10.0, 5.0, 0.0);
    } else {
        list = list.circle(paint(p.line), 10.0, 10.0, 9.0, 1.5);
    }
    let draw = list.build();
    let radio_label = label.to_string();

    cu::Box()
        .modifier(Modifier::new().size(20.0, 20.0))
        .content(move || {
            cu::Canvas(draw).modifier(Modifier::new().size(20.0, 20.0));
            FocusOverlay(&radio_label, move || on_click());
        });
}

// v0.0.7: #425 and #429 fixed (`Button::content`, `Modifier::label`); workaround kept until
// checked on a display (moving each painted row into a `Button` content slot changes its press
// feedback and padding). The overlay names itself with `accessible_name` (owned), never
// `Modifier::label`, so a name built from data (a profile, a model) never fills the toolkit's
// label table (#455).
/// An accessible invisible button overlay for a painted row, chip, or pill.
/// Gives assistive tech the item's label and provides keyboard focus and click handling.
fn FocusOverlay(label: &str, on_click: impl FnMut() + 'static) {
    FocusOverlayEnabled(label, true, on_click);
}

fn FocusOverlayEnabled(label: &str, enabled: bool, on_click: impl FnMut() + 'static) {
    cu::Button(label)
        .accessible_name(label)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(compose_rust::Paint::Literal(Color::argb(0)))
        .modifier(Modifier::new().fill_max_size())
        .on_click(on_click);
}

/// A selectable chip matching the mockup's .field.ag-sel: sized to its label, with a 1-line
/// label and an accessible FocusOverlay. A `Box` in a `Row` is content sized since compose-rust
/// v0.0.7 (#427), so the chip needs no fixed width to leave its sibling room.
fn SelectChip(text: &str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(
            Modifier::new()
                .height(H_CTL)
                .background(paint(p.surface_2))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(move || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(14.0));
                Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(14.0));
            });
            FocusOverlay(text, on_click);
        });
}

/// A picker pill matching mockup's .pickchip: full pill shape, icon + label, sized to its
/// content (a `Box` in a `Row` is content sized since compose-rust v0.0.7, #427).
fn ModelPickerPill(text: &str, svg: Option<&'static str>, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(
            Modifier::new()
                .height(H_CTL)
                .background(paint(p.surface_2))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(move || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(12.0));
                if let Some(s) = svg {
                    super::shell::NavIcon(s, p.text_2.to_argb(), 16.0);
                    cu::Spacer().modifier(Modifier::new().width(8.0));
                }
                Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(12.0));
            });
            FocusOverlay(text, on_click);
        });
}

/// A segmented control matching the mockup (.seg button height 32, selected = ink fill, white label).
fn SegGroup<T: Copy + PartialEq + 'static>(
    options: &'static [(T, &'static str)],
    current: T,
    mut on_pick: impl FnMut(T) + Clone + 'static,
) {
    let p = ember_theme().palette;
    cu::Row().spacing(SP[0]).content(|| {
        for (val, label) in options {
            let val = *val;
            let selected = val == current;
            let (bg, fg, border) = if selected {
                (p.ink, p.ink_text, p.ink)
            } else {
                (p.surface, p.text_2, p.line)
            };
            let mut on_pick = on_pick.clone();
            cu::Button(*label)
                .variant(ButtonVariant::Text)
                .color(paint(fg))
                .modifier(
                    Modifier::new()
                        .height(H_CTL)
                        .background(paint(bg))
                        .border(1.0, paint(border))
                        .shape_role(ShapeRole::Full),
                )
                .on_click(move || on_pick(val));
        }
    });
}

fn BotAvatar(h: AgentHarness, size: f32) {
    let bg = match h {
        AgentHarness::ClaudeCode => Color::rgb(0xe49b55),
        AgentHarness::Codex => Color::rgb(0x3f74d4),
        AgentHarness::Gemini => Color::rgb(0x9dbde9),
        AgentHarness::Antigravity => Color::rgb(0x7052bc),
        AgentHarness::Acp => Color::rgb(0x8f8b83),
    };
    let initial = match h {
        AgentHarness::ClaudeCode => "C",
        AgentHarness::Codex => "X",
        AgentHarness::Gemini => "G",
        AgentHarness::Antigravity => "A",
        AgentHarness::Acp => "P",
    };
    cu::Box()
        .modifier(
            Modifier::new()
                .size(size, size)
                .background(paint(bg))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(|| {
            Label(initial, Fs::Sm, Weight::Semibold, Some(paint(Color::rgb(0xffffff))), None);
        });
}

fn VendorDot(h: AgentHarness) {
    let c = match h {
        AgentHarness::ClaudeCode => Color::rgb(0xe49b55),
        AgentHarness::Codex => Color::rgb(0x3f74d4),
        AgentHarness::Gemini => Color::rgb(0x9dbde9),
        AgentHarness::Antigravity => Color::rgb(0x7052bc),
        AgentHarness::Acp => Color::rgb(0x8f8b83),
    };
    cu::Spacer().modifier(
        Modifier::new()
            .size(8.0, 8.0)
            .background(paint(c))
            .shape_role(ShapeRole::Full),
    );
}

fn KpiItem(label: &str, value: &str, pill: Option<(&str, PillTone)>) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
        Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
        cu::Spacer().modifier(Modifier::new().height(SP[0]));
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
            Label(value, Fs::Xl, Weight::Semibold, None, Some(1));
            if let Some((p_text, tone)) = pill {
                Pill(p_text, tone, true);
            }
        });
    });
}

/// A decorative icon glyph for card headers matching mockup's .card-h .ico (30x30 bordered square).
fn CardHeaderIcon(svg: &'static str) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(
            Modifier::new()
                .size(30.0, 30.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_SM),
        )
        .alignment(Alignment::Center)
        .content(|| {
            super::shell::NavIcon(svg, p.text_2.to_argb(), 18.0);
        });
}

fn FormulaChip(svg: Option<&'static str>, text: &str, dashed: bool) {
    let p = ember_theme().palette;
    let border_color = if dashed { p.muted } else { p.line };
    cu::Box()
        .modifier(
            Modifier::new()
                .height(28.0)
                .background(paint(p.surface))
                .border(1.0, paint(border_color))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(|| {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(10.0));
                if let Some(s) = svg {
                    super::shell::NavIcon(s, p.text_2.to_argb(), 16.0);
                    cu::Spacer().modifier(Modifier::new().width(6.0));
                }
                Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(10.0));
            });
        });
}

/// A card with a title row (name on the left, pills on the right) and its rows.
fn Block(title: &str, pills: impl FnOnce(), content: impl FnOnce()) {
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                Label(title, Fs::Lg, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                pills();
            });
            content();
        });
    });
}

/// One setting: title and hint on the left, the control on the right.
fn SettingRow(title: &str, hint: &str, control: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
        cu::Spacer().modifier(Modifier::new().height(SP[1]));
        cu::Row()
            .alignment(Alignment::CenterStart)
            .spacing(SP[3])
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label(title, Fs::Md, Weight::Medium, None, Some(1));
                    if !hint.is_empty() {
                        cu::Spacer().modifier(Modifier::new().height(SP[0]));
                        Label(hint, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    }
                });
                control();
            });
        cu::Spacer().modifier(Modifier::new().height(SP[1]));
    });
}

/// A line telling that a part has no server data yet.
fn Pending(text: &str) {
    let p = ember_theme().palette;
    InsetPanel(true, || {
        Label(
            &format!("{text} {UNAVAILABLE}"),
            Fs::Sm,
            Weight::Regular,
            Some(paint(p.muted)),
            None,
        );
    });
}

fn LocalNote() {
    let p = ember_theme().palette;
    // TODO(server): remove when the settings and profiles live on the main server (FR-L20).
    Label(
        LOCAL_STORE_NOTE,
        Fs::Xs,
        Weight::Regular,
        Some(paint(p.muted)),
        None,
    );
}

// ---------------------------------------------------------------------------------------------
// Drawing helpers for charts, rings and donuts
// ---------------------------------------------------------------------------------------------

fn draw_ring(percent: Option<u8>, size: f32, stroke_width: f32, p: &Palette) -> DrawList {
    let half = size / 2.0;
    let radius = half - stroke_width / 2.0;
    let track_color = p.surface_2;
    let mut builder = DrawList::builder().circle(paint(track_color), half, half, radius, stroke_width);
    if let Some(pct) = percent {
        let fill_color = if pct < 15 {
            p.orange.strong
        } else {
            p.green.strong
        };
        let sweep = 360.0 * (pct as f32) / 100.0;
        builder = builder.arc(paint(fill_color), half, half, radius, -90.0, sweep, stroke_width);
    }
    builder.build()
}

fn draw_usage_bars(p: &Palette, bars: &[usage::DayBar]) -> DrawList {
    const W: f32 = 440.0;
    const H: f32 = 130.0;
    let claude_col = Color::rgb(0xe49b55);
    let codex_col = Color::rgb(0x3f74d4);
    let other_col = Color::rgb(0x9dbde9);
    let max_tot = bars.iter().map(|b| b.total()).max().unwrap_or(0).max(1) as f32;
    let mut builder = DrawList::builder().rect(paint(p.line), 0.0, H, W, 1.0, 0.0);
    let pitch = W / bars.len().max(1) as f32;
    let bar_w = (pitch - 7.0).max(2.0);
    for (i, b) in bars.iter().enumerate() {
        let x_pos = i as f32 * pitch;
        let h = |n: u64| (n as f32 / max_tot * H).round();
        let (c_h, x_h, g_h) = (h(b.claude), h(b.codex), h(b.other));
        builder = builder.round_rect(paint(p.surface_2), x_pos, 0.0, bar_w, H, 4.0, 0.0);
        builder = builder.round_rect(paint(claude_col), x_pos, H - c_h, bar_w, c_h, 4.0, 0.0);
        builder = builder.round_rect(paint(codex_col), x_pos, H - c_h - x_h, bar_w, x_h, 0.0, 0.0);
        builder = builder.round_rect(paint(other_col), x_pos, H - c_h - x_h - g_h, bar_w, g_h, 4.0, 0.0);
    }
    builder.build()
}

/// A bar split into input and output by their counts; a grey track when there are none.
fn draw_split_bar(p: &Palette, w: f32, input: u64, output: u64) -> DrawList {
    let h = 14.0;
    let r = 7.0;
    let total = (input + output).max(1) as f32;
    let w1 = (w * input as f32 / total).round();
    DrawList::builder()
        .round_rect(paint(p.surface_2), 0.0, 0.0, w, h, r, 0.0)
        .round_rect(paint(Color::rgb(0x3f74d4)), 0.0, 0.0, w1.max(0.0), h, r, 0.0)
        .round_rect(paint(Color::rgb(0xe49b55)), w1, 0.0, if input + output == 0 { 0.0 } else { w - w1 }, h, r, 0.0)
        .build()
}

// ---------------------------------------------------------------------------------------------
// 1. 하네스 (Harnesses)
// ---------------------------------------------------------------------------------------------

/// Sample-capture paths (`EMBER_CR_ROUTE=agents`), shown marked 예시. Real screens show only
/// the path the server reported (FR-L20); Gemini has no adapter, so no path.
fn sample_path(kind: &str) -> Option<String> {
    match kind {
        "claude-code" => Some("/usr/local/bin/claude".into()),
        "codex" => Some("/opt/homebrew/bin/codex".into()),
        "antigravity" => Some("/usr/local/bin/agy".into()),
        _ => None,
    }
}

fn harness_mode_desc(h: AgentHarness) -> &'static str {
    match h {
        AgentHarness::ClaudeCode => "Claude Code의 --print 모드를 stream-json 입출력으로 구동합니다.",
        AgentHarness::Codex => "codex app-server를 JSON-RPC로 구동합니다.",
        AgentHarness::Gemini | AgentHarness::Acp => "Agent Client Protocol 어댑터로 구동합니다. 다른 방식과 비교 중인 후보입니다.",
        AgentHarness::Antigravity => "턴 하나마다 프로세스를 새로 띄우고 stream-json 출력을 읽습니다.",
    }
}

fn harness_shim_desc(h: AgentHarness) -> &'static str {
    match h {
        AgentHarness::ClaudeCode | AgentHarness::Gemini | AgentHarness::Acp => {
            "서버가 다시 시작되거나 업데이트되어도 에이전트가 계속 돌고, 새 서버가 이어 붙습니다."
        }
        AgentHarness::Codex => {
            "shim은 살아 있지만 서버가 다시 시작되면 다음 메시지에서 에이전트 자체 resume으로 이어집니다."
        }
        AgentHarness::Antigravity => {
            "턴마다 프로세스를 새로 띄우므로 아직 shim 없이 직접 실행합니다."
        }
    }
}

fn HarnessSection(store: &MutableState<LocalStore>, live: &AgentsLive, compact: bool) {
    let profiles = effective_profiles(&store.get(), live);

    // KPI strip: every value comes from the detected agents, this device's limits or the
    // detection clock; a value with no source says 아직 정보가 없습니다 (FR-L20).
    let installed_count = crate::ui::agents::kpi_installed(&live.detected);
    let installed_h: Vec<AgentHarness> = AgentHarness::ALL
        .iter()
        .copied()
        .filter(|h| live.detected.iter().any(|d| d.installed && AgentHarness::of_kind(&d.kind) == Some(*h)))
        .collect();
    let limits: Vec<Option<u32>> = installed_h.iter().map(|h| store.get().settings(*h).mem_limit_mb).collect();
    let mem_total = crate::ui::agents::kpi_mem_total_mb(&limits).map(crate::ui::agents::mem_total_label);
    // TODO(server): no shim or session-process count reaches the client; samples count the
    // harnesses that support a shim, flagged 예시.
    let shim_count = if live.sample {
        Some(installed_h.iter().filter(|h| h.shim_supported()).count())
    } else {
        crate::ui::agents::kpi_shim(None)
    };
    let checked = crate::services::try_services().and_then(|s| s.ui.live.agents_checked_ms.get());
    let last_detect = if live.sample {
        "방금".to_string()
    } else {
        crate::ui::agents::last_detect_label(crate::demo::now_ms().max(0) as u64, checked)
    };
    let example = live.sample || live.demo;
    let sample_pill = |on: bool| on.then_some(("예시", PillTone::Orange));
    let total = format!("전체 {}", AgentHarness::ALL.len());
    cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
        KpiItem(
            "설치된 하네스",
            &crate::ui::agents::kpi_text(installed_count.map(|n| n.to_string())),
            installed_count.map(|_| if example { ("예시", PillTone::Orange) } else { (total.as_str(), PillTone::Grey) }),
        );
        KpiItem(
            "상주 shim",
            &crate::ui::agents::kpi_text(shim_count.map(|n| n.to_string())),
            sample_pill(example && shim_count.is_some()),
        );
        KpiItem(
            "세션 메모리 한도 합",
            &crate::ui::agents::kpi_text(mem_total),
            None,
        );
        KpiItem("마지막 감지", &last_detect, sample_pill(example && checked.is_some() || live.sample));
    });

    // The outcome of 다시 찾기, once, near the buttons; it goes back to nothing after a few seconds.
    if let Some(note) = crate::services::try_services()
        .and_then(|s| crate::ui::agents::rescan_note(&s.ui.live.agents_rescan.get()))
    {
        let p = ember_theme().palette;
        Label(&note, Fs::Xs, Weight::Medium, Some(paint(p.muted)), None);
    }

    let harnesses = [
        AgentHarness::ClaudeCode,
        AgentHarness::Codex,
        AgentHarness::Gemini,
        AgentHarness::Antigravity,
    ];

    let cols = harness_columns(compact_window_width(compact));
    if cols < 2 {
        for h in harnesses {
            HarnessCard(h, store, &live.detected, &profiles, compact, example);
        }
    } else {
        for pair in harnesses.chunks(cols) {
            cu::Row()
                .alignment(Alignment::TopStart)
                .spacing(SP[3])
                .modifier(Modifier::new().fill_max_width())
                .content(|| {
                    for h in pair {
                        let h = *h;
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                            HarnessCard(h, store, &live.detected, &profiles, compact, example);
                        });
                    }
                });
        }
    }
}

/// The window width the harness grid decides from; a compact layout is always one column.
fn compact_window_width(compact: bool) -> f32 {
    if compact { 0.0 } else { compose_rust::runtime::current_window_size().width_dp }
}

/// Columns of the harness cards (FR-L20): two from 1280 dp, one below. At 1024 dp two columns
/// left each card about 320 dp, so the hints wrapped per character; the mockup keeps one column
/// there.
pub fn harness_columns(window_dp: f32) -> usize {
    if window_dp >= 1280.0 { 2 } else { 1 }
}

/// Width of one harness card at `window_dp` with `nav` dp of navigation: the body is the window
/// minus the navigation, the page padding and (on a desktop layout) the 200 dp section column
/// with its 25 dp gap; `columns` cards share it with `SP[3]` between them.
pub fn harness_card_width(window_dp: f32, nav: f32, columns: usize) -> f32 {
    let n = columns.max(1) as f32;
    let body = if window_dp <= 900.0 {
        window_dp - nav - 2.0 * SP[3]
    } else {
        window_dp - nav - 2.0 * SP[6] - 200.0 - 25.0
    };
    ((body - SP[3] * (n - 1.0)) / n).max(0.0)
}

fn HarnessCard(
    h: AgentHarness,
    store: &MutableState<LocalStore>,
    detected: &[DetectedAgent],
    profiles: &[Profile],
    _compact: bool,
    example: bool,
) {
    let p = ember_theme().palette;
    let (state, tone, version) = harness_status(h, detected);
    let installed = h != AgentHarness::Gemini && state == "설치되어 있습니다";
    let path_text = crate::ui::agents::harness_path_text(h, detected, example);
    let rescan = crate::services::try_services().map(|s| s.ui.live.agents_rescan.get());
    let settings = store.get().settings(h).clone();
    let of_h: Vec<Profile> = profiles
        .iter()
        .filter(|p| p.harness == h)
        .cloned()
        .collect();
    let current = effective_default(h, settings.default_profile.as_deref(), profiles);
    let profile_name = current
        .as_ref()
        .and_then(|id| of_h.iter().find(|p| &p.id == id))
        .map_or("프로필 없음".to_string(), |p| p.name.clone());
    let options: Vec<Option<String>> = std::iter::once(None)
        .chain(of_h.iter().map(|p| Some(p.id.clone())))
        .collect();

    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            // Header
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                BotAvatar(h, 30.0);
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label(h.label(), Fs::Md, Weight::Semibold, None, Some(1));
                    cu::Spacer().modifier(Modifier::new().height(2.0));
                    let mut sub = crate::ui::agents::harness_subtitle(h.run_mode(), version.as_deref());
                    if example && version.is_some() {
                        sub.push_str(" (예시)");
                    }
                    Label(&sub, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
                if h == AgentHarness::Antigravity {
                    Pill("확인 필요", PillTone::Orange, true);
                } else if h == AgentHarness::Gemini {
                    Pill(crate::ui::agents::SOON, PillTone::Grey, true);
                } else {
                    Pill(state, tone.into(), true);
                }
            });

            // Row 1: 설치와 버전
            SettingRow(
                "설치와 버전",
                &path_text,
                || {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(|| {
                        match crate::ui::agents::version_text(installed, version.as_deref(), example) {
                            Some(v) => Pill(&v, PillTone::Grey, true),
                            None if h == AgentHarness::Gemini => Pill(crate::ui::agents::SOON, PillTone::Grey, true),
                            None => Pill(crate::ui::agents::NO_INFO, PillTone::Grey, true),
                        }
                        let rstate = rescan.clone().unwrap_or_default();
                        let (can, reason) = crate::ui::agents::rescan_enabled(h, &rstate);
                        let label = crate::ui::agents::rescan_label(&rstate);
                        if let Some(r) = reason {
                            Label(r, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }
                        AppButton(
                            &label,
                            Some(IconRole::History),
                            ButtonKind::Default,
                            true,
                            can,
                            false,
                            || {
                                // Runs the detection again (`POST .../harnesses/detect`) and reports its real
                                // result; without services (a capture) there is nothing to ask.
                                if crate::services::try_services().is_some() {
                                    crate::bridge::rescan_agents();
                                }
                            },
                        );
                    });
                },
            );

            // Row 2: 고정 버전 (Antigravity or pinned)
            if h == AgentHarness::Antigravity {
                SettingRow(
                    "고정 버전",
                    &format!("Ember가 검증한 버전은 {}입니다. 다른 버전은 읽기 전용으로만 실행됩니다.", crate::ui::agents::PINNED_ANTIGRAVITY_VERSION),
                    || {
                        // A verdict only for a version the server reported; nothing is shown
                        // for an undetected harness.
                        match crate::ui::agents::pin_verdict(version.as_deref(), crate::ui::agents::PINNED_ANTIGRAVITY_VERSION) {
                            Some("일치") => Pill("일치", PillTone::Green, true),
                            Some(_) => Pill("다름", PillTone::Orange, true),
                            None => {}
                        }
                    },
                );
            }

            // Row 3: 기본 프로필 (프로필)
            let store_pf = store.clone();
            let options_pf = options.clone();
            let current_pf = current.clone();
            SettingRow(
                "기본 프로필",
                "이 하네스로 만든 팀원은 모두 이 프로필의 계정과 규칙을 따릅니다.",
                move || {
                    SelectChip(&profile_name, move || {
                        let next = cycle(&options_pf, Some(&current_pf)).unwrap_or(None);
                        edit(&store_pf, |s| s.settings_mut(h).default_profile = next);
                    });
                },
            );

            // Row 4: 실행 방식
            SettingRow(
                "실행 방식",
                harness_mode_desc(h),
                || {
                    Pill(h.run_mode(), PillTone::Sky, false);
                },
            );

            // Row 5: 샌드박스
            if let Some(on) = settings.sandbox {
                let hint = if on {
                    "켜져 있습니다(EMBER_AGY_SANDBOX=1). 샌드박스 안에서는 셸 쓰기가 실패합니다."
                } else {
                    "기본으로 꺼져 있습니다. 승인 훅이 모든 도구 호출을 확인합니다."
                };
                let store_sb = store.clone();
                SettingRow(
                    "샌드박스 (--sandbox)",
                    hint,
                    move || {
                        MockSwitch("샌드박스", on, true, move |v| {
                            edit(&store_sb, |s| s.settings_mut(h).sandbox = Some(v));
                            post_notice(if v {
                                "샌드박스를 켰습니다. 셸 쓰기가 실패합니다."
                            } else {
                                "샌드박스를 껐습니다."
                            });
                        });
                    },
                );
            }

            // Row 6: 상주 shim
            let store_shim = store.clone();
            SettingRow(
                "상주 shim",
                harness_shim_desc(h),
                move || {
                    MockSwitch("상주 shim", settings.shim && h.shim_supported(), h.shim_supported(), move |v| {
                        edit(&store_shim, |s| s.settings_mut(h).shim = v);
                        post_notice(if v {
                            "상주 shim을 켰습니다."
                        } else {
                            "상주 shim을 껐습니다."
                        });
                    });
                },
            );

            // Row 7: 세션당 메모리 한도
            let store_mem = store.clone();
            let cur_mb = settings.mem_limit_mb.unwrap_or(4096);
            let cur_gb = (cur_mb / 1024).clamp(1, 16) as f32;
            SettingRow(
                "세션당 메모리 한도",
                "넘으면 그 세션만 종료되고 다른 세션은 계속됩니다.",
                move || {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(|| {
                        // The Slider takes the mockup's colours and its name (compose-rust v0.0.7, #428).
                        cu::Slider(cur_gb)
                            .range(1.0, 16.0)
                            .steps(15)
                            .track_color(paint(p.line))
                            .active_track_color(paint(p.ink))
                            .thumb_color(paint(p.ink))
                            .accessible_name("세션당 메모리 한도")
                            .modifier(Modifier::new().width(120.0))
                            .on_change(move |v| {
                                let gb = v.round().clamp(1.0, 16.0) as u32;
                                let mb = gb * 1024;
                                edit(&store_mem, |s| s.settings_mut(h).mem_limit_mb = Some(mb));
                                if !example {
                                    save_server_mem_limit(h, Some(mb));
                                }
                            });
                        Label(&format!("{} GB", cur_gb as u32), Fs::Sm, Weight::Medium, None, Some(1));
                    });
                },
            );
        });
    });
}

// ---------------------------------------------------------------------------------------------
// 2. 모델 (Models)
// ---------------------------------------------------------------------------------------------

struct ModelSpec {
    id: &'static str,
    name: &'static str,
    note: &'static str,
    plans: &'static [(&'static str, bool)],
}

const CLAUDE_SPECS: [ModelSpec; 3] = [
    ModelSpec {
        id: "opus",
        name: "Opus 5.5",
        note: "가장 깊게 생각합니다",
        plans: &[("Max", true), ("Pro", true)],
    },
    ModelSpec {
        id: "sonnet",
        name: "Sonnet 5.5",
        note: "속도와 품질의 균형",
        plans: &[("Max", true), ("Pro", true)],
    },
    ModelSpec {
        id: "haiku",
        name: "Haiku 5.5",
        note: "가장 빠르고 저렴합니다",
        plans: &[("Max", true), ("Pro", true)],
    },
];

const CODEX_SPECS: [ModelSpec; 3] = [
    ModelSpec {
        id: "gpt-5",
        name: "GPT-5",
        note: "기본 코딩 모델",
        plans: &[("Plus", true), ("Pro", true)],
    },
    ModelSpec {
        id: "gpt-5-codex",
        name: "GPT-5 Codex",
        note: "긴 코딩 작업에 맞춥니다",
        plans: &[("Plus", false), ("Pro", true)],
    },
    ModelSpec {
        id: "gpt-5-mini",
        name: "GPT-5 mini",
        note: "빠른 수정용",
        plans: &[("Plus", true), ("Pro", true)],
    },
];

const GEMINI_SPECS: [ModelSpec; 2] = [
    ModelSpec {
        id: "gemini-2.5-pro",
        name: "Gemini Pro",
        note: "기본 모델",
        plans: &[("Pro", true), ("Flash", true)],
    },
    ModelSpec {
        id: "gemini-2.5-flash",
        name: "Gemini Flash",
        note: "빠른 응답용",
        plans: &[("Pro", true), ("Flash", true)],
    },
];

fn specs_of(h: AgentHarness) -> &'static [ModelSpec] {
    match h {
        AgentHarness::ClaudeCode => &CLAUDE_SPECS,
        AgentHarness::Codex => &CODEX_SPECS,
        AgentHarness::Gemini => &GEMINI_SPECS,
        _ => &[],
    }
}

/// `(id, name)` of a harness's catalog models, for the default picker.
fn model_pairs(h: AgentHarness) -> Vec<(String, String)> {
    specs_of(h).iter().map(|m| (m.id.to_string(), m.name.to_string())).collect()
}

/// `POST /api/accounts/{id}/default`, then re-read the account list so the chip shows the
/// server's answer. A failure is said, not hidden.
fn set_default_account(id: String) {
    crate::services::run_shared(
        async move {
            let s = crate::services::services();
            match s.server.set_default_account(&id).await {
                Ok(_) => {
                    reload_accounts().await;
                    None
                }
                Err(e) => Some(crate::ui::agents::server_error_status(&e)),
            }
        },
        |failed| {
            if let Some(status) = failed {
                post_notice(&format!("기본 계정을 바꾸지 못했습니다. {}", crate::ui::agents::account_error_ko(status)));
            }
        },
    );
}

async fn reload_accounts() {
    let s = crate::services::services();
    if let Ok(a) = s.server.accounts().await {
        s.ui.live.accounts.set(a);
    }
}

/// The catalog name for a stored model id, else the id itself.
fn model_name(h: AgentHarness, id: &str) -> String {
    let specs: &[ModelSpec] = specs_of(h);
    specs.iter().find(|m| m.id == id).map_or_else(|| id.to_string(), |m| m.name.to_string())
}

fn ModelsSection(store: &MutableState<LocalStore>, live: &AgentsLive, compact: bool) {
    // The chips show what is stored: the harness and model the owner chose on this device
    // and the account the server marks default; with none, they say so (FR-L20).
    let example = live.sample || live.demo;
    let chips = crate::ui::agents::default_chips(&live.accounts, &live.default_ids, &store.get());
    let harness_pill = crate::ui::agents::harness_chip_text(&chips, model_name, example);
    let account_pill = crate::ui::agents::account_chip_text(&chips, live.accounts.len(), example);
    let hook = crate::ui::agents::sheet_hook();
    let pick_open = remember(move || mutable_state_of(hook == Some(crate::ui::agents::SheetHook::Harness)));
    let acct_open = remember(move || mutable_state_of(hook == Some(crate::ui::agents::SheetHook::Account)));
    let stacked = crate::ui::agents::chips_stacked(compose_rust::runtime::current_window_size().width_dp);
    let rows = crate::ui::agents::pick_rows(&crate::ui::agents::harness_options(&live.detected), model_pairs);
    let row_labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
    let row_chosen = rows
        .iter()
        .position(|r| r.enabled && Some(r.harness) == chips.harness && r.model == chips.model);
    let options = crate::ui::agents::account_options(&live.accounts);
    let acct_rows = crate::ui::agents::account_sheet_rows(&options);
    // "새 팀원 기본값" card
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                CardHeaderIcon(super::shell::ICON_PLUS_SVG);
                Label("새 팀원 기본값", Fs::Lg, Weight::Semibold, None, Some(1));
                if example {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Pill(crate::ui::agents::EXAMPLE_MARK, PillTone::Grey, true);
                }
            });
            Label(
                "홈과 새 팀원 만들기, 루틴에서 처음 보이는 하네스와 모델입니다. 고른 계정의 플랜에서 쓸 수 없는 모델은 흐리게 표시됩니다.",
                Fs::Sm,
                Weight::Regular,
                None,
                None,
            );
            let chips_body = || {
                let po = pick_open.clone();
                ModelPickerPill(&harness_pill, Some(ICON_SPARK_SVG), move || po.set(true));
                let ao = acct_open.clone();
                ModelPickerPill(&account_pill, Some(ICON_USER_SVG), move || ao.set(true));
                // One node holds both sheets, so the closed dialogs add no gap.
                cu::Box().content(|| {
                    let close = {
                        let o = pick_open.clone();
                        move || o.set(false)
                    };
                    let (st, list) = (store.clone(), rows.clone());
                    OptionSheet(pick_open.get(), &row_labels, row_chosen, 320.0, close, move |i| {
                        if let Some(r) = list.get(i) {
                            let mut payload = None;
                            edit(&st, |s| {
                                if crate::ui::agents::apply_pick(s, r) {
                                    if let Some(m) = r.model.as_ref() {
                                        s.settings_mut(r.harness).model = Some(m.clone());
                                    }
                                    payload = Some(s.to_server_defaults());
                                }
                            });
                            if let Some(d) = payload {
                                if example {
                                    post_notice(crate::ui::agents::DEMO_CHOICE_NOTE);
                                } else {
                                    save_server_defaults(d);
                                }
                            }
                        }
                    });
                    let close = {
                        let o = acct_open.clone();
                        move || o.set(false)
                    };
                    let list = options.clone();
                    OptionSheet(acct_open.get(), &acct_rows, None, 320.0, close, move |i| {
                        let Some(o) = list.get(i) else { return };
                        if example {
                            post_notice(crate::ui::agents::DEMO_CHOICE_NOTE);
                        } else {
                            set_default_account(o.id.clone());
                        }
                    });
                });
            };
            // Below 560 dp the chips stack so each keeps its full text.
            if stacked {
                cu::Column().spacing(SP[2]).content(chips_body);
            } else {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(chips_body);
            }
            Label(crate::ui::agents::DEFAULTS_STORAGE_NOTE, Fs::Xs, Weight::Regular, Some(paint(ember_theme().palette.muted)), None);
        });
    });

    let harnesses = [AgentHarness::ClaudeCode, AgentHarness::Codex, AgentHarness::Gemini];

    if compact {
        for h in harnesses {
            ModelCard(h, store, compact, example);
        }
    } else {
        cu::Row().spacing(18.0).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[0], store, compact, example);
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[1], store, compact, example);
            });
        });
        cu::Row().spacing(18.0).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[2], store, compact, example);
            });
            cu::Spacer().modifier(Modifier::new().weight(1.0));
        });
    }
}

// ---------------------------------------------------------------------------------------------
// 계정 추가 (FR-U1): the honest flow
// ---------------------------------------------------------------------------------------------

/// Pick a harness and a label, create the account on the server, show the login command it
/// returns and let the owner ask whether the login happened. There is no login window: the
/// login runs on the server's machine (INTENT E3). Demo mode shows a sample and calls nothing.
fn AddAccountSheet(add: &MutableState<crate::ui::agents::AddAccount>, example: bool) {
    use crate::ui::agents::{add_account_kinds, AddAccount, AddPhase};
    let theme = ember_theme();
    let p = theme.palette;
    let cur = add.get();
    let close = {
        let a = add.clone();
        move || a.set(AddAccount::closed())
    };
    let busy = cur.phase == AddPhase::Busy;
    cu::Dialog(cur.open)
        .modifier(
            with_shadows(Modifier::new(), theme.shadow_lg())
                .width(420.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_MD)
                .padding(SP[3]),
        )
        .on_dismiss(close.clone())
        .content(|| {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("계정 추가", Fs::Lg, Weight::Semibold, None, Some(1));
                Label(crate::ui::agents::NO_LOGIN_WINDOW_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                if example {
                    Label(crate::ui::agents::DEMO_ADD_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                }
                if let AddPhase::Created(c) = &cur.phase {
                    if let Some(note) = crate::ui::agents::login_block_note(c, example) {
                        Label(&note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    }
                    // Selectable plain text; edits are dropped. Never logged.
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        text: Some(c.command.clone()),
                        multiline: true,
                        on_value_change: Some(Box::new(|_: String| {})),
                        ..Default::default()
                    });
                    if let Some(line) = &c.status {
                        Label(line, Fs::Sm, Weight::Medium, None, None);
                    }
                    cu::Row().spacing(SP[2]).content(|| {
                        let a = add.clone();
                        let id = c.id.clone();
                        AppButton("로그인 확인", None, ButtonKind::Primary, true, !c.checking && !id.is_empty() && !example, false, move || {
                            check_login(&a, id.clone());
                        });
                        AppButton("닫기", None, ButtonKind::Default, true, true, false, close.clone());
                    });
                } else {
                    Label("하네스", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    for k in add_account_kinds() {
                        cu::Column().content(|| {
                            let a = add.clone();
                            let kind = if cur.kind == Some(k.kind) { ButtonKind::Primary } else { ButtonKind::Default };
                            AppButton(k.label, None, kind, true, k.enabled() && !busy, false, move || {
                                let mut n = a.get_untracked();
                                n.kind = Some(k.kind);
                                a.set(n);
                            });
                            if let Some(note) = k.note {
                                let ko = crate::ui::agents::account_note_ko(k.kind, note);
                                Label(&ko, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                            }
                        });
                    }
                    Label("이름", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    let a = add.clone();
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: "예: 회사 계정".to_string(),
                        enabled: Some(!busy),
                        on_value_change: Some(Box::new(move |v: String| {
                            let mut n = a.get_untracked();
                            n.label = v;
                            a.set(n);
                        })),
                        ..Default::default()
                    });
                    match &cur.phase {
                        AddPhase::Busy => Label("만드는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None),
                        AddPhase::Failed(e) => Label(e, Fs::Sm, Weight::Regular, Some(paint(p.text)), None),
                        _ => {}
                    }
                    cu::Row().spacing(SP[2]).content(|| {
                        let a = add.clone();
                        AppButton("만들기", None, ButtonKind::Primary, true, cur.can_create(), false, move || create_account(&a, example));
                        AppButton("닫기", None, ButtonKind::Default, true, true, false, close.clone());
                    });
                }
            });
        });
}

fn create_account(add: &MutableState<crate::ui::agents::AddAccount>, example: bool) {
    use crate::ui::agents::{created_phase, sample_created, AddPhase, ADD_FAILED};
    let cur = add.get_untracked();
    let Some(kind) = cur.kind.filter(|_| cur.can_create()) else { return };
    let mut next = cur.clone();
    if example {
        next.phase = sample_created();
        add.set(next);
        return;
    }
    next.phase = AddPhase::Busy;
    add.set(next);
    let label = cur.label.trim().to_string();
    let st = add.clone();
    crate::services::run_shared(
        async move {
            let s = crate::services::services();
            let r = s.server.create_account(kind, &label).await.map_err(|e| crate::ui::agents::server_error_status(&e));
            if r.is_ok() {
                reload_accounts().await;
            }
            r
        },
        move |r| {
            let mut n = st.get_untracked();
            n.phase = match r {
                Ok(a) => created_phase(&a),
                Err(status) => AddPhase::Failed(format!("{ADD_FAILED}. {}", crate::ui::agents::account_error_ko(status))),
            };
            st.set(n);
        },
    );
}

fn check_login(add: &MutableState<crate::ui::agents::AddAccount>, id: String) {
    use crate::ui::agents::{check_line, AddPhase, CHECK_FAILED};
    fn set(add: &MutableState<crate::ui::agents::AddAccount>, checking: bool, line: Option<String>) {
        let mut n = add.get_untracked();
        if let AddPhase::Created(c) = &mut n.phase {
            c.checking = checking;
            if line.is_some() {
                c.status = line;
            }
        }
        add.set(n);
    }
    set(add, true, None);
    let st = add.clone();
    crate::services::run_shared(
        async move {
            let s = crate::services::services();
            let r = s.server.check_account(&id).await.map(|a| a.account.status).map_err(|e| crate::ui::agents::server_error_status(&e));
            if r.is_ok() {
                reload_accounts().await;
            }
            r
        },
        move |r| {
            let line = match r {
                Ok(status) => check_line(&status),
                Err(status) => format!("{CHECK_FAILED}. {}", crate::ui::agents::account_error_ko(status)),
            };
            set(&st, false, Some(line));
        },
    );
}

fn ModelCard(h: AgentHarness, store: &MutableState<LocalStore>, _compact: bool, example: bool) {
    let p = ember_theme().palette;
    let settings = store.get().settings(h).clone();
    let specs: &'static [ModelSpec] = match h {
        AgentHarness::ClaudeCode => &CLAUDE_SPECS,
        AgentHarness::Codex => &CODEX_SPECS,
        AgentHarness::Gemini => &GEMINI_SPECS,
        _ => &[],
    };

    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            // Header
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                VendorDot(h);
                Label(h.label(), Fs::Md, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Pill(&format!("모델 {}", specs.len()), PillTone::Grey, true);
            });

            // Row 1: 기본 추론 강도 (SegGroup with 낮음/보통/높음)
            SettingRow(
                "기본 추론 강도",
                "새 팀원을 만들 때 처음 고른 값입니다.",
                || {
                    if h == AgentHarness::Gemini {
                        Label("추론 강도를 고르지 않습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    } else {
                        const EFFORT_OPTIONS: [(&'static str, &'static str); 3] = [
                            ("Low", "낮음"),
                            ("Medium", "보통"),
                            ("High", "높음"),
                        ];
                        let cur_effort_owned = settings.effort.clone().unwrap_or_else(|| "Medium".to_string());
                        let cur_effort: &'static str = EFFORT_OPTIONS.iter().find(|(k, _)| *k == cur_effort_owned.as_str()).map(|(k, _)| *k).unwrap_or("Medium");
                        let store_eff = store.clone();
                        SegGroup(
                            &EFFORT_OPTIONS,
                            cur_effort,
                            move |e| {
                                let mut payload = None;
                                edit(&store_eff, |s| {
                                    s.settings_mut(h).effort = Some(e.to_string());
                                    if s.default_harness == Some(h) {
                                        payload = Some(s.to_server_defaults());
                                    }
                                });
                                if let Some(d) = payload {
                                    if !example {
                                        save_server_defaults(d);
                                    }
                                }
                            },
                        );
                    }
                },
            );

            // Row 2: 기본 권한 모드
            let store_mode = store.clone();
            SettingRow(
                "기본 권한 모드",
                "새 팀원이 도구를 쓸 때 확인하는 방식입니다. 바이패스는 모든 명령을 묻지 않고 실행합니다.",
                move || {
                    let modes: Vec<crate::ui::home::PermMode> = h
                        .picker_harness()
                        .map(crate::ui::home::modes_for)
                        .unwrap_or(&[crate::ui::home::PermMode::Ask])
                        .to_vec();
                    let cur_mode = settings.mode;
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        let st = store_mode.clone();
                        ModelPickerPill(cur_mode.label(), Some(super::shell::ICON_GEAR_SVG), move || {
                            let next = cycle(&modes, Some(&cur_mode)).unwrap_or(crate::ui::home::PermMode::Ask);
                            let mut payload = None;
                            edit(&st, |s| {
                                s.settings_mut(h).mode = next;
                                if s.default_harness == Some(h) {
                                    payload = Some(s.to_server_defaults());
                                }
                            });
                            if let Some(d) = payload {
                                if !example {
                                    save_server_defaults(d);
                                }
                            }
                        });
                        if settings.mode.is_bypass() {
                            Pill("주의", PillTone::Orange, true);
                        }
                    });
                },
            );

            // Model Radio Table
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().padding(SP[1])).content(|| {
                cu::Box().modifier(Modifier::new().width(28.0)).content(|| {
                    Label("기본", Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(1));
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label("모델", Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(1));
                });
                cu::Box().modifier(Modifier::new().width(120.0)).alignment(Alignment::CenterEnd).content(|| {
                    Label("플랜별 사용 (Ember 참고 목록)", Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(2));
                });
                cu::Box().modifier(Modifier::new().width(60.0)).alignment(Alignment::CenterEnd).content(|| {
                    Label("목록에 표시", Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(1));
                });
            });

            for m in specs {
                let is_def = settings.model.as_deref() == Some(m.id);
                let m_id = m.id.to_string();
                let store_m = store.clone();
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().padding(SP[1])).content(|| {
                    cu::Box().modifier(Modifier::new().width(28.0)).content(|| {
                        let m_id = m_id.clone();
                        ModelRadio(&format!("{} 기본 모델로 선택", m.name), is_def, move || {
                            let m_id = m_id.clone();
                            let mut payload = None;
                            edit(&store_m, |s| {
                                s.settings_mut(h).model = Some(m_id.clone());
                                if s.default_harness == Some(h) {
                                    s.default_model = Some(m_id);
                                    payload = Some(s.to_server_defaults());
                                }
                            });
                            if let Some(d) = payload {
                                if !example {
                                    save_server_defaults(d);
                                }
                            }
                            post_notice("기본 모델을 바꿨습니다");
                        });
                    });
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                            Label(m.name, Fs::Md, Weight::Medium, None, Some(1));
                            if is_def {
                                Pill("기본", PillTone::Blue, true);
                            }
                        });
                        cu::Spacer().modifier(Modifier::new().height(2.0));
                        Label(m.note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                    cu::Box().modifier(Modifier::new().width(120.0)).alignment(Alignment::CenterEnd).content(|| {
                        cu::Row().spacing(SP[0]).content(|| {
                            for (p_name, ok) in m.plans {
                                Pill(
                                    &format!("{}{p_name}", if *ok { "✓ " } else { "✕ " }),
                                    if *ok { PillTone::Green } else { PillTone::Grey },
                                    true,
                                );
                            }
                        });
                    });
                    cu::Box().modifier(Modifier::new().width(60.0)).alignment(Alignment::CenterEnd).content(|| {
                        let m_name = m.name.to_string();
                        MockSwitch(&format!("{m_name} 활성화"), true, true, move |v| {
                            let msg = if v {
                                format!("{m_name} 모델을 활성화했습니다")
                            } else {
                                format!("{m_name} 모델을 비활성화했습니다")
                            };
                            post_notice(&msg);
                        });
                    });
                });
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------
// 3. 프로필 (Profiles)
// ---------------------------------------------------------------------------------------------

fn AccountTile(x: &AccountInfo, extra: Option<&str>, sample: bool) {
    let p = ember_theme().palette;
    InsetPanel(true, || {
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Box().modifier(Modifier::new().size(72.0, 72.0)).alignment(Alignment::Center).content(|| {
                let ring = draw_ring(x.left_percent, 72.0, 6.0, p);
                cu::Canvas(ring).modifier(Modifier::new().size(72.0, 72.0));
                match x.left_percent {
                    Some(pct) => Label(&format!("{pct}%"), Fs::Sm, Weight::Semibold, None, None),
                    None => Label("모름", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None),
                }
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                    VendorDot(x.harness);
                    Label(&x.label, Fs::Md, Weight::Semibold, None, Some(1));
                    // A plan pill only when the account's own label names the plan; a guessed
                    // "Pro" would be an invented fact.
                    if let Some(plan) = ["Max", "Plus", "Pro"].into_iter().find(|n| x.label.contains(*n)) {
                        Pill(plan, PillTone::Grey, true);
                    }
                });
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                let reset_str = crate::ui::agents::account_reset_line(x.harness.label(), sample);
                Label(&reset_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                if let Some(left) = x.left_percent {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        Label("5h", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        let bar_color = if left < 20 { p.orange.strong } else { p.green.strong };
                        let bar_draw = DrawList::builder()
                            .round_rect(paint(p.surface_2), 0.0, 0.0, 115.0, 6.0, 3.0, 0.0)
                            .round_rect(paint(bar_color), 0.0, 0.0, 115.0 * (left as f32) / 100.0, 6.0, 3.0, 0.0)
                            .build();
                        cu::Canvas(bar_draw).modifier(Modifier::new().size(115.0, 6.0));
                        Label(&format!("{left}%"), Fs::Xs, Weight::Medium, None, None);
                    });
                } else {
                    Label("5시간 한도를 알려 주지 않습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                }
                if let Some(pill_txt) = extra {
                    cu::Spacer().modifier(Modifier::new().height(SP[0]));
                    Pill(pill_txt, PillTone::Orange, true);
                }
            });
        });
    });
}

fn ProfilesSection(store: &MutableState<LocalStore>, live: &AgentsLive, compact: bool) {
    let p = ember_theme().palette;
    let profiles = effective_profiles(&store.get(), live);
    let defaults: Vec<Option<String>> = AgentHarness::ALL
        .iter()
        .map(|h| {
            effective_default(
                *h,
                store.get().settings(*h).default_profile.as_deref(),
                &profiles,
            )
        })
        .collect();
    let first = live.accounts.first().cloned();
    let add = remember(|| mutable_state_of(crate::ui::agents::hook_add_account(crate::ui::agents::sheet_hook())));
    let seed = profiles.clone();
    let for_new = profiles.clone();

    // 1. Pipeline formula row
    if compact {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                FormulaChip(Some(ICON_USER_SVG), "계정 묶음", false);
                Label("+", Fs::Lg, Weight::Regular, Some(paint(p.muted)), None);
                FormulaChip(Some(ICON_REPEAT_SVG), "전환 규칙", false);
                Label("+", Fs::Lg, Weight::Regular, Some(paint(p.muted)), None);
            });
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                FormulaChip(Some(ICON_ARROW_UP_RIGHT_SVG), "모델 프록시 (선택)", true);
                Label("=", Fs::Lg, Weight::Regular, Some(paint(p.muted)), None);
                FormulaChip(Some(ICON_SPARK_SVG), "프로필", false);
            });
        });
    } else {
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            FormulaChip(Some(ICON_USER_SVG), "계정 묶음", false);
            Label("+", Fs::Lg, Weight::Regular, Some(paint(p.muted)), None);
            FormulaChip(Some(ICON_REPEAT_SVG), "전환 규칙", false);
            Label("+", Fs::Lg, Weight::Regular, Some(paint(p.muted)), None);
            FormulaChip(Some(ICON_ARROW_UP_RIGHT_SVG), "모델 프록시 (선택)", true);
            Label("=", Fs::Lg, Weight::Regular, Some(paint(p.muted)), None);
            FormulaChip(Some(ICON_SPARK_SVG), "프로필", false);
        });
    }
    cu::Spacer().modifier(Modifier::new().height(SP[1]));
    Label(
        "하네스마다 프로필 하나를 정하면 그 하네스로 만든 팀원이 모두 그 프로필을 따릅니다. 팀원마다 따로 설정하지 않습니다.",
        Fs::Sm,
        Weight::Regular,
        Some(paint(p.text_2)),
        None,
    );

    // 2. "계정" Card
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                CardHeaderIcon(ICON_USER_SVG);
                Label("계정", Fs::Lg, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                let open_add = add.clone();
                AppButton("계정 추가", Some(IconRole::Add), ButtonKind::Default, true, true, false, move || {
                    open_add.set(crate::ui::agents::AddAccount { open: true, ..crate::ui::agents::AddAccount::closed() });
                });
                AddAccountSheet(&add, live.sample || live.demo);
            });
            if compact {
                for a in &live.accounts {
                    let extra = if a.limited { Some("한도 가까움") } else { None };
                    AccountTile(a, extra, live.sample);
                }
            } else {
                let chunks: Vec<_> = live.accounts.chunks(3).collect();
                for chunk in chunks {
                    cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for a in chunk {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                let extra = if a.limited { Some("한도 가까움") } else { None };
                                AccountTile(a, extra, live.sample);
                            });
                        }
                    });
                    cu::Spacer().modifier(Modifier::new().height(SP[2]));
                }
            }
        });
    });

    // 3. "하네스별 기본 프로필" Card
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                CardHeaderIcon(super::shell::ICON_CPU_SVG);
                Label("하네스별 기본 프로필", Fs::Lg, Weight::Semibold, None, Some(1));
            });
            for h in [AgentHarness::ClaudeCode, AgentHarness::Codex, AgentHarness::Gemini] {
                let p_id = effective_default(h, store.get().settings(h).default_profile.as_deref(), &profiles);
                let p_name = p_id.as_ref()
                    .and_then(|id| profiles.iter().find(|p| &p.id == id))
                    .map_or("프로필 없음".to_string(), |p| p.name.clone());
                SettingRow(
                    h.label(),
                    "이 하네스로 만든 팀원은 모두 이 프로필을 따릅니다.",
                    || {
                        SelectChip(&p_name, move || {
                            post_notice("기본 프로필을 선택합니다");
                        });
                    },
                );
            }
        });
    });

    // 4. Section Title & Add Profile Button
    cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
        Label(&format!("프로필 {}개", profiles.len()), Fs::Lg, Weight::Semibold, None, Some(1));
        cu::Spacer().modifier(Modifier::new().weight(1.0));
        let store = store.clone();
        let first = first.clone();
        let seed = seed.clone();
        let for_new = for_new.clone();
        AppButton(
            "새 프로필",
            Some(IconRole::Add),
            ButtonKind::Default,
            true,
            first.is_some(),
            false,
            move || {
                if let Some(a) = &first {
                    let p = new_profile(&for_new, a.harness, &a.id);
                    edit_profiles(&store, seed.clone(), |l| l.push(p));
                    post_notice("새 프로필을 만들었습니다.");
                }
            },
        );
    });

    // 5. Grid of Profile Cards
    if compact {
        for p in profiles.clone() {
            let can_delete = can_delete_profile(&p.id, &defaults);
            ProfileCard(p, profiles.clone(), can_delete, live, store, compact);
        }
    } else {
        let chunks: Vec<_> = profiles.chunks(2).map(|c| c.to_vec()).collect();
        for chunk in chunks {
            cu::Row().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
                for p in chunk {
                    let can_delete = can_delete_profile(&p.id, &defaults);
                    let all = profiles.clone();
                    let live_c = live.clone();
                    let store_c = store.clone();
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                        ProfileCard(p, all, can_delete, &live_c, &store_c, compact);
                    });
                }
            });
        }
    }
}

fn ProfileCard(
    p: Profile,
    all: Vec<Profile>,
    can_delete: bool,
    live: &AgentsLive,
    store: &MutableState<LocalStore>,
    compact: bool,
) {
    let theme = ember_theme();
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

    let ed = ProfileEditor {
        store: store.clone(),
        seed: all.clone(),
        id,
    };

    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            // Header
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                VendorDot(p.harness);
                Label(&p.name, Fs::Lg, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Pill(p.harness.label(), PillTone::Blue, true);
                Pill(if p.mode == AccountMode::Fixed { "고정" } else { "자동 전환" }, PillTone::Grey, true);
                let seed = all.clone();
                let pid = p.id.clone();
                let p_name = p.name.clone();
                let store = store.clone();
                AppButton("지우기", Some(IconRole::Close), ButtonKind::Default, true, can_delete, false, move || {
                    edit_profiles(&store, seed.clone(), |l| l.retain(|x| x.id != pid));
                    post_notice(&format!("{p_name} 프로필을 지웠습니다."));
                });
            });

            // Name Field: stacked on compact width (solves review finding 5)
            let ed_name = ed.clone();
            let name_ph = p.name.clone();
            if compact {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("이름", Fs::Md, Weight::Medium, None, Some(1));
                    Label("비워 두면 이름이 바뀌지 않습니다.", Fs::Xs, Weight::Regular, Some(paint(theme.palette.muted)), None);
                    cu::Spacer().modifier(Modifier::new().height(SP[1]));
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: name_ph,
                        on_value_change: Some(Box::new(move |v: String| {
                            let v = v.trim().to_string();
                            if !v.is_empty() {
                                ed_name.apply(move |p| p.name = v);
                            }
                        })),
                        ..Default::default()
                    });
                });
            } else {
                SettingRow(
                    "이름",
                    "비워 두면 이름이 바뀌지 않습니다.",
                    move || {
                        cu::Box().modifier(Modifier::new().width(240.0)).content(|| {
                            EmberField(FieldOptions {
                                fill_max_width: true,
                                placeholder: name_ph,
                                on_value_change: Some(Box::new(move |v: String| {
                                    let v = v.trim().to_string();
                                    if !v.is_empty() {
                                        ed_name.apply(move |p| p.name = v);
                                    }
                                })),
                                ..Default::default()
                            });
                        });
                    },
                );
            }

            // Target Harness
            SettingRow(
                "대상 하네스",
                "",
                || {
                    SelectChip(p.harness.label(), move || {
                        post_notice("프로필 하네스를 선택합니다");
                    });
                },
            );

            // Account Mode
            const MODE_OPTIONS: [(AccountMode, &'static str); 2] = [
                (AccountMode::Fixed, "고정"),
                (AccountMode::Auto, "자동 전환"),
            ];
            let ed_mode = ed.clone();
            let same_ids_clone = same_ids.clone();
            SettingRow(
                "계정 방식",
                "",
                move || {
                    SegGroup(
                        &MODE_OPTIONS,
                        p.mode,
                        move |m| {
                            let ids = same_ids_clone.clone();
                            ed_mode.apply(move |p| set_account_mode(p, m, &ids));
                        },
                    );
                },
            );

            if p.mode == AccountMode::Fixed {
                SettingRow(
                    "계정",
                    "이 계정 하나로만 실행됩니다.",
                    || {
                        let cur_acc = p.accounts.first().map(|a| account_label(&live.accounts, a)).unwrap_or_else(|| "계정 없음".to_string());
                        ModelPickerPill(&cur_acc, Some(ICON_USER_SVG), move || {
                            post_notice("고정 계정을 선택합니다");
                        });
                    },
                );
            } else {
                SettingRow(
                    "전환 순서",
                    "위에서부터 씁니다. 끌기 대신 위로와 아래로 버튼을 씁니다.",
                    || {},
                );
                let len = p.accounts.len();
                for (i, aid) in p.accounts.clone().into_iter().enumerate() {
                    let ed_up = ed.clone();
                    let ed_down = ed.clone();
                    let ed_rm = ed.clone();
                    let is_limited = live.accounts.iter().any(|a| a.id == aid && a.limited);
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .spacing(SP[1])
                        .modifier(Modifier::new().fill_max_width().height(H_CTL))
                        .content(|| {
                            Label(&(i + 1).to_string(), Fs::Sm, Weight::Medium, Some(paint(theme.palette.muted)), Some(1));
                            cu::Spacer().modifier(Modifier::new().width(SP[0]));
                            let acct_info = live.accounts.iter().find(|a| a.id == aid);
                            let left_pct = acct_info.and_then(|a| a.left_percent);
                            let ring = draw_ring(left_pct, 32.0, 4.0, theme.palette);
                            cu::Canvas(ring).modifier(Modifier::new().size(32.0, 32.0));
                            cu::Spacer().modifier(Modifier::new().width(SP[0]));
                            Label(&account_label(&live.accounts, &aid), Fs::Md, Weight::Regular, None, Some(1));
                            if is_limited {
                                cu::Spacer().modifier(Modifier::new().width(SP[0]));
                                Pill("한도 도달", PillTone::Orange, true);
                            }
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            AppButton("위로", None, ButtonKind::Default, true, i > 0, false, move || {
                                ed_up.apply(move |p| { move_account(&mut p.accounts, i, -1); });
                            });
                            AppButton("아래로", None, ButtonKind::Default, true, i + 1 < len, false, move || {
                                ed_down.apply(move |p| { move_account(&mut p.accounts, i, 1); });
                            });
                            AppButton("빼기", None, ButtonKind::Default, true, len > 1, false, move || {
                                ed_rm.apply(move |p| { remove_account(&mut p.accounts, i); });
                            });
                        });
                }
                if !rest.is_empty() {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        for a in rest {
                            let ed = ed.clone();
                            let aid = a.id.clone();
                            ModelPickerPill(&format!("+ {}", a.label), None, move || {
                                let aid = aid.clone();
                                ed.apply(move |p| { add_account(&mut p.accounts, &aid); });
                            });
                        }
                    });
                }

                // Threshold stepping
                let ed_dec = ed.clone();
                let ed_inc = ed.clone();
                SettingRow(
                    "전환 사용률",
                    &threshold_label(p.threshold),
                    move || {
                        cu::Row().spacing(SP[1]).content(|| {
                            AppButton("낮추기", None, ButtonKind::Default, true, true, false, move || {
                                ed_dec.apply(|p| p.threshold = step_threshold(p.threshold, -1));
                            });
                            AppButton("높이기", None, ButtonKind::Default, true, true, false, move || {
                                ed_inc.apply(|p| p.threshold = step_threshold(p.threshold, 1));
                            });
                        });
                    },
                );

                // Routing policy
                const ROUTING_OPTIONS: [(Routing, &'static str); 2] = [
                    (Routing::Order, "순서대로"),
                    (Routing::MostLeft, "많이 남은 계정"),
                ];
                let ed_policy = ed.clone();
                SettingRow(
                    "라우팅 정책",
                    "새 세션의 계정을 고르는 방법입니다.",
                    move || {
                        SegGroup(&ROUTING_OPTIONS, p.policy, move |r| {
                            ed_policy.apply(move |p| p.policy = r);
                        });
                    },
                );
            }

            // Failure handling
            SettingRow(
                "실패할 때",
                "속도 제한이나 인증 오류가 나면 이렇게 처리합니다.",
                || {
                    let cur_fail = p.on_fail.label();
                    SelectChip(cur_fail, move || {
                        post_notice("실패 시 동작을 선택합니다");
                    });
                },
            );

            // Model proxy
            let ed_proxy = ed.clone();
            let is_proxy = proxy_supported(p.harness);
            let has_proxy = p.proxy.is_some();
            SettingRow(
                "모델 프록시",
                "하네스의 API 주소를 프록시로 돌려 다른 출처의 모델을 씁니다. 프록시 서버는 Ember에 들어 있지 않습니다.",
                move || {
                    if is_proxy {
                        MockSwitch("모델 프록시", has_proxy, true, move |on| {
                            ed_proxy.apply(move |p| {
                                p.proxy = if on {
                                    Some("http://127.0.0.1:4000".to_string())
                                } else {
                                    None
                                };
                            });
                        });
                    } else {
                        Pill(UNAVAILABLE, PillTone::Grey, false);
                    }
                },
            );

            if has_proxy {
                let ed_purl = ed.clone();
                cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                        Label("프록시 주소", Fs::Xs, Weight::Regular, Some(paint(theme.palette.muted)), None);
                        cu::Spacer().modifier(Modifier::new().height(SP[0]));
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: proxy_hint,
                            on_value_change: Some(Box::new(move |v: String| {
                                let v = v.trim().to_string();
                                ed_purl.apply(move |p| p.proxy = (!v.is_empty()).then_some(v));
                            })),
                            ..Default::default()
                        });
                    });
                });
            }

            // Inset preview
            InsetPanel(true, || {
                Label(&preview, Fs::Sm, Weight::Regular, None, None);
            });
        });
    });
}

// ---------------------------------------------------------------------------------------------
// 4. 네트워크 (Network)
// ---------------------------------------------------------------------------------------------

fn NetworkSection(computers: &[(String, Reachability)], sample: bool, compact: bool) {
    let p = ember_theme().palette;

    // Card 1: API 트래픽 내보내기 
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("API 트래픽 내보내기", Fs::Lg, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Pill("검증 전 (#130)", PillTone::Orange, true);
            });

            SettingRow(
                "작업 중인 컴퓨터로 내보내기",
                "에이전트의 API 요청이 그 세션이 일하는 컴퓨터에서 나가므로 공급자는 그 컴퓨터의 IP를 봅니다.",
                || {
                    MockSwitch("작업 중인 컴퓨터로 내보내기", true, true, |_| post_notice("아직 검증 전이라 켜 둔 채로만 보입니다"));
                },
            );

            cu::Spacer().modifier(Modifier::new().height(SP[1]));

            // 4-step chain of formula chips
            if compact {
                cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        FormulaChip(None, "에이전트 CLI", false);
                        Label("→", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                        FormulaChip(None, "세션 프록시 (루프백)", false);
                        Label("→", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                    });
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        FormulaChip(None, "작업 중인 컴퓨터", false);
                        Label("→", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                        FormulaChip(None, "모델 공급자", false);
                    });
                });
            } else {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    FormulaChip(None, "에이전트 CLI", false);
                    Label("→", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                    FormulaChip(None, "세션 프록시 (루프백)", false);
                    Label("→", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                    FormulaChip(None, "작업 중인 컴퓨터", false);
                    Label("→", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                    FormulaChip(None, "모델 공급자", false);
                });
            }

            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            Label(
                "컴퓨터를 바꾸면 새 연결부터 새 컴퓨터를 따르고, 이미 열린 연결은 원래 컴퓨터를 씁니다.",
                Fs::Sm,
                Weight::Regular,
                Some(paint(p.text_2)),
                None,
            );
        });
    });

    cu::Spacer().modifier(Modifier::new().height(SP[3]));

    // Card 2: 컴퓨터별 상태 (컴퓨터)
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("컴퓨터별 상태", Fs::Lg, Weight::Semibold, None, Some(1));
            });

            if computers.is_empty() {
                Pending("컴퓨터 목록은");
            } else if compact {
                for (name, r) in computers {
                    ComputerTile(name, *r, sample);
                }
            } else {
                let chunks: Vec<_> = computers.chunks(3).collect();
                for chunk in chunks {
                    cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for (name, r) in chunk {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                ComputerTile(name, *r, sample);
                            });
                        }
                    });
                    cu::Spacer().modifier(Modifier::new().height(SP[2]));
                }
            }
        });
    });
}

fn ComputerTile(name: &str, r: Reachability, sample: bool) {
    let p = ember_theme().palette;
    let (status_text, tone) = computer_status(r);
    // The client holds only a name and reachability; the rest reads 아직 정보가 없습니다 on the
    // live path and comes from the sample table (marked 예시) in a review window.
    let facts = crate::ui::agents::computer_facts(name, sample);
    let os_str = crate::ui::agents::fact_text(facts.os.as_deref(), sample);
    let ip_str = crate::ui::agents::fact_text(facts.ip.as_deref(), sample);
    let sessions_num = crate::ui::agents::count_text(facts.sessions);
    let tunnels_num = crate::ui::agents::count_text(facts.connections);
    let note = crate::ui::agents::disconnect_note(r, &facts);
    let name_owned = name.to_string();
    let ip_known = facts.ip.clone();

    InsetPanel(true, || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label(name, Fs::Md, Weight::Semibold, None, Some(1));
                    Label(&os_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
                Pill(status_text, tone.into(), true);
            });
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            Label(&ip_str, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                Label(&sessions_num, Fs::Sm, Weight::Medium, None, Some(1));
                Label(" 세션", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
                Label(&tunnels_num, Fs::Sm, Weight::Medium, None, Some(1));
                Label(" 열린 연결", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                AppButton("IP 확인", None, ButtonKind::Default, true, r == Reachability::Online && ip_known.is_some(), false, move || {
                    if let Some(ip) = &ip_known {
                        post_notice(&format!("{name_owned} 세션이 본 IP: {ip}"));
                    }
                });
            });
            if let Some(note) = &note {
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                Label(note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------
// 5. 사용량 (Usage)
// ---------------------------------------------------------------------------------------------

fn UsageSection(
    live: &AgentsLive,
    bars: &[crate::ui::home::HarnessBar],
    load: &Load<UsageSummary>,
    compact: bool,
) {
    let p = ember_theme().palette;
    let window = remember(|| mutable_state_of(14u32));

    // Period Seg: 7일 / 14일
    const PERIOD_OPTIONS: [(u32, &'static str); 2] = [(7, "7일"), (14, "14일")];
    let pick = window.clone();
    SegGroup(&PERIOD_OPTIONS, window.get(), move |v| pick.set(v));
    let m = usage::section_model(load, &crate::ui::home::today_utc(), window.get() as usize);

    cu::Spacer().modifier(Modifier::new().height(SP[3]));

    // Hero Card: 오늘 토큰, the harnesses and the daily bars
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            let figures = || {
                Label("오늘 토큰", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                Label(&m.today, Fs::Xl3, Weight::Semibold, None, Some(1));
                Label(&m.today_sub, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                if let Some(n) = &m.notice {
                    Label(n, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                }
                cu::Spacer().modifier(Modifier::new().height(SP[2]));
                for (kind, value) in &m.harness {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(|| {
                        VendorDot(AgentHarness::of_kind(kind).unwrap_or(AgentHarness::Acp));
                        Label(&format!("{} {}", usage::agent_label(kind), value), Fs::Xs, Weight::Regular, None, Some(1));
                    });
                }
            };
            if compact {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(figures);
            } else {
                cu::Row().alignment(Alignment::CenterEnd).spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().width(320.0)).content(figures);
                    cu::Box().modifier(Modifier::new().width(440.0).height(150.0)).content(|| {
                        let bars_draw = draw_usage_bars(p, &m.bars);
                        cu::Canvas(bars_draw).modifier(Modifier::new().size(440.0, 150.0));
                    });
                });
            }
        });
    });

    cu::Spacer().modifier(Modifier::new().height(SP[3]));

    // 2-Column row: 토큰 종류 & 하네스별 토큰과 비용
    if compact {
        TokenKindsCard(&m, compact);
        cu::Spacer().modifier(Modifier::new().height(SP[3]));
        HarnessCostCard(&m, bars);
    } else {
        cu::Row().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                TokenKindsCard(&m, compact);
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                HarnessCostCard(&m, bars);
            });
        });
    }

    cu::Spacer().modifier(Modifier::new().height(SP[3]));

    // Section Title & Accounts subscription cards
    Label("계정별 남은 구독량", Fs::Lg, Weight::Semibold, None, Some(1));
    cu::Spacer().modifier(Modifier::new().height(SP[2]));

    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            for a in &live.accounts {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width().height(H_CTL)).content(|| {
                    VendorDot(a.harness);
                    Label(&a.label, Fs::Md, Weight::Regular, None, Some(1));
                    Pill(a.harness.label(), PillTone::Grey, true);
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    if a.limited {
                        Pill("한도 도달", PillTone::Orange, true);
                    } else {
                        Pill("사용 가능", PillTone::Green, true);
                    }
                });
            }
        });
    });
}

fn TokenKindsCard(m: &usage::SectionModel, compact: bool) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("토큰 종류", Fs::Lg, Weight::Semibold, None, Some(1));
            });
            let bar_w = if compact { 326.0 } else { 409.0 };
            let split_draw = draw_split_bar(p, bar_w, m.input_n, m.output_n);
            cu::Canvas(split_draw).modifier(Modifier::new().size(bar_w, 14.0));
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                for (name, value, share) in [
                    ("입력", m.input.as_str(), m.input_share.as_str()),
                    ("출력", m.output.as_str(), m.output_share.as_str()),
                    ("캐시 읽기", usage::UNKNOWN, ""),
                    ("캐시 쓰기", usage::UNKNOWN, ""),
                ] {
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                        Label(name, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        Label(value, Fs::Lg, Weight::Semibold, None, Some(1));
                        Label(share, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                }
            });
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            Label(
                "하네스가 알려 준 실제 토큰 수입니다. 알려 주지 않은 값은 추정하지 않고 알 수 없음으로 표시합니다. 캐시 토큰은 서버가 아직 모으지 않습니다.",
                Fs::Xs,
                Weight::Regular,
                Some(paint(p.muted)),
                None,
            );
        });
    });
}

fn HarnessCostCard(m: &usage::SectionModel, bars: &[crate::ui::home::HarnessBar]) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("하네스별 토큰과 비용", Fs::Lg, Weight::Semibold, None, Some(1));
            });
            for (kind, value) in &m.harness {
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(28.0)).content(|| {
                    VendorDot(AgentHarness::of_kind(kind).unwrap_or(AgentHarness::Acp));
                    cu::Spacer().modifier(Modifier::new().width(SP[1]));
                    Label(&usage::agent_label(kind), Fs::Sm, Weight::Medium, None, Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Label(value, Fs::Sm, Weight::Medium, None, Some(1));
                });
            }
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(28.0)).content(|| {
                Label("비용", Fs::Sm, Weight::Medium, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Label(usage::UNKNOWN, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
            Label("가격표가 정해지지 않아 비용은 계산하지 않습니다.", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
            for b in bars {
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(28.0)).content(|| {
                    Label(&b.label, Fs::Sm, Weight::Regular, None, Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Label(&format!("세션 {}개", b.sessions), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Sample data for review captures (`EMBER_CR_ROUTE=agents`)
// ---------------------------------------------------------------------------------------------

pub fn sample_live() -> AgentsLive {
    let d = |k: &str, i: bool, v: Option<&str>| DetectedAgent {
        kind: k.into(),
        installed: i,
        version: v.map(String::from),
        path: if i { sample_path(k) } else { None },
        detected_at: None,
    };
    let detected = vec![
        d("claude-code", true, Some("2.1.4")),
        d("codex", true, Some("0.48.0")),
        d("gemini", true, Some("0.9.2")),
        d("antigravity", true, Some("1.2.16")),
        d("omp", false, None),
    ];
    let accounts = vec![
        AccountInfo {
            id: "claude:ncmnd".into(),
            label: "Claude Max".into(),
            harness: AgentHarness::ClaudeCode,
            left_percent: Some(72),
            limited: false,
        },
        AccountInfo {
            id: "claude:work".into(),
            label: "Claude Max (work)".into(),
            harness: AgentHarness::ClaudeCode,
            left_percent: Some(38),
            limited: false,
        },
        AccountInfo {
            id: "codex:ncmnd".into(),
            label: "Codex Pro".into(),
            harness: AgentHarness::Codex,
            left_percent: Some(9),
            limited: true,
        },
        AccountInfo {
            id: "codex:work".into(),
            label: "Codex Plus".into(),
            harness: AgentHarness::Codex,
            left_percent: Some(64),
            limited: false,
        },
        AccountInfo {
            id: "gemini:ncmnd".into(),
            label: "Gemini Pro".into(),
            harness: AgentHarness::Gemini,
            left_percent: Some(80),
            limited: false,
        },
    ];
    let chatgpt = vec![ChatGptAccount {
        id: "gpt-1".into(),
        label: "ChatGPT Max".into(),
        email: Some("user@example.com".into()),
        status: "signed_in".into(),
        limited: false,
        limited_until: None,
        limit_reason: None,
        tokens_today: 1_234_567,
        weekly_cap: None,
    }];
    let default_ids = vec!["claude:ncmnd".into(), "codex:ncmnd".into()];
    AgentsLive {
        detected,
        accounts,
        chatgpt,
        default_ids,
        sample: true,
        demo: false,
    }
}

pub fn sample_computers() -> Vec<(String, Reachability)> {
    vec![
        ("mac-mini".into(), Reachability::Online),
        ("ws-linux-01".into(), Reachability::Online),
        ("macbook-pro".into(), Reachability::Online),
        ("win-desktop".into(), Reachability::Offline),
        ("gpu-box".into(), Reachability::Unknown),
    ]
}

pub fn sample_bars() -> Vec<crate::ui::home::HarnessBar> {
    vec![
        crate::ui::home::HarnessBar {
            label: "Claude Code".into(),
            sessions: 7,
        },
        crate::ui::home::HarnessBar {
            label: "Codex".into(),
            sessions: 4,
        },
        crate::ui::home::HarnessBar {
            label: "Gemini CLI".into(),
            sessions: 2,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::{Host, Recomposer};

    fn agent_card_width(window_width: f32) -> f32 {
        use crate::ui::shell::{nav_mode, ShellMode};
        let nav = match nav_mode(window_width, false) {
            ShellMode::BottomTabs => 0.0,
            ShellMode::Rail => RAIL_W,
            ShellMode::Sidebar => SIDE_W,
        };
        harness_card_width(window_width, nav, harness_columns(window_width))
    }

    #[test]
    fn fr_l20a_agent_card_width_fills_available_content() {
        assert_eq!(agent_card_width(390.0), 358.0);
        assert_eq!(agent_card_width(768.0), 664.0);
        assert_eq!(agent_card_width(1440.0), 450.5);
    }

    #[test]
    fn fr_l20_the_harness_grid_is_one_column_until_1280_so_text_never_wraps_per_character() {
        assert_eq!(
            (harness_columns(1440.0), harness_columns(1280.0), harness_columns(1024.0), harness_columns(768.0), harness_columns(390.0)),
            (2, 2, 1, 1, 1)
        );
        // 1024 (rail): one card of 663 dp, where two columns were about 320 dp each.
        assert_eq!(agent_card_width(1024.0), 663.0);
        assert!(agent_card_width(1024.0) > 600.0);
        // No width makes a card narrower than 340 dp (a hint beside its control stays readable).
        for w in [390.0, 560.0, 768.0, 900.0, 1024.0, 1280.0, 1440.0, 1920.0] {
            assert!(agent_card_width(w) >= 340.0, "{w}");
        }
        assert_eq!(compact_window_width(true), 0.0);
        assert_eq!(harness_columns(compact_window_width(true)), 1);
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

    fn profile_of(id: &str, h: AgentHarness) -> Profile {
        Profile {
            id: id.into(),
            harness: h,
            ..auto_profile(&["a"])
        }
    }

    fn text_of(content: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    fn screen_text_at(width: f32, class: compose_rust::WindowSizeClass, section: Section) -> String {
        use compose_rust::protocol::{decode_batch, HostEvent};
        use compose_rust::{EventPayload, WindowHeightClass};
        compose_rust::window::reset_window_size();
        let mut host = Host::new(move || {
            Box::new(Recomposer::new(move || {
                AgentsScreenWith(
                    sample_live(),
                    sample_computers(),
                    sample_bars(),
                    Load::Ready(crate::demo::usage_summary(crate::demo::now_ms())),
                    section,
                )
            }))
        });
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: width,
                height_dp: 900.0,
                class,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        compose_rust::window::reset_window_size();
        text
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
        let o = crate::ui::home::default_option(crate::ui::home::Harness::ClaudeCode);
        assert_eq!(d.model.as_deref(), Some(o.model));
        assert_eq!(d.effort.as_deref(), o.effort);
        assert_eq!(d.mode, crate::ui::home::default_mode(crate::ui::home::Harness::ClaudeCode));
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
        assert_eq!(
            resolve_profile(h, Some("x1"), Some("c1"), &ps).map(|p| p.id.as_str()),
            Some("c1")
        );
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
            crate::ui::components::PillTone::Green
        );
        assert_eq!(harness_status(AgentHarness::Codex, &list).1, crate::ui::components::PillTone::Grey);
        assert_eq!(harness_status(AgentHarness::Acp, &list).1, crate::ui::components::PillTone::Green);
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

    // ---- compose-rust Host rendering tests ------------------------------------------------

    #[test]
    fn fr_l20_without_services_the_screen_renders_safely() {
        let text = text_of(AgentsScreen);
        assert!(text.contains("에이전트"));
        assert!(text.contains("하네스"));
    }

    #[test]
    fn fr_l20_sample_data_renders_all_sections() {
        let text = screen_text_at(1440.0, compose_rust::WindowSizeClass::Expanded, Section::Harness);
        for want in ["에이전트", "하네스", "모델", "프로필", "네트워크", "사용량"] {
            assert!(text.contains(want), "missing section tab {want}");
        }
        for want in ["Claude Code", "실행 방식", "상주 shim", "세션당 메모리 한도", "기본 프로필"] {
            assert!(text.contains(want), "missing harness content {want}");
        }
    }

    #[test]
    fn fr_l16_models_section_renders_model_catalog() {
        let text = screen_text_at(1440.0, compose_rust::WindowSizeClass::Expanded, Section::Models);
        for want in ["Claude Code", "기본 모델", "기본 권한 모드"] {
            assert!(text.contains(want), "missing models content {want}");
        }
    }

    #[test]
    fn fr_l17_profiles_section_renders_controls() {
        let text = screen_text_at(1440.0, compose_rust::WindowSizeClass::Expanded, Section::Profiles);
        for want in ["새 프로필", "계정 방식", "실패할 때", "모델 프록시"] {
            assert!(text.contains(want), "missing profiles content {want}");
        }
    }

    #[test]
    fn fr_x6_network_section_renders_computers() {
        let text = screen_text_at(1440.0, compose_rust::WindowSizeClass::Expanded, Section::Network);
        for want in ["API 트래픽 내보내기", "작업 중인 컴퓨터로 내보내기", "컴퓨터별 상태", "mac-mini"] {
            assert!(text.contains(want), "missing network content {want}");
        }
    }

    #[test]
    fn fr_u7_usage_section_renders_tokens_and_bars() {
        let text = screen_text_at(1440.0, compose_rust::WindowSizeClass::Expanded, Section::Usage);
        for want in ["오늘 토큰", "UTC 기준", "토큰 종류", "하네스별 토큰과 비용", "알 수 없음", "계정별 남은 구독량"] {
            assert!(text.contains(want), "missing usage content {want}");
        }
    }

    #[test]
    fn fr_l20_compact_window_renders_pills_nav() {
        let text = screen_text_at(390.0, compose_rust::WindowSizeClass::Compact, Section::Harness);
        for want in ["에이전트", "하네스", "모델"] {
            assert!(text.contains(want), "missing compact nav item {want}");
        }
    }

    #[test]
    fn fr_l20_local_store_save_and_load() {
        let mut store = local_load();
        store.settings_mut(AgentHarness::Codex).mem_limit_mb = Some(2048);
        local_save(store.clone());
        let loaded = local_load();
        assert_eq!(loaded.settings(AgentHarness::Codex).mem_limit_mb, Some(2048));
    }

    #[test]
    fn no_empty_button_in_agents_file() {
        let src = include_str!("agents.rs");
        let needle = ["cu::Button(", "\"\")"].concat();
        let in_tests = src.split("mod tests").nth(1).unwrap_or("");
        assert!(!src.replace(in_tests, "").contains(&needle), "an empty Button has no screen reader name");
    }

    #[test]
    fn fr_l20_switch_track_follows_the_mockup() {
        // compose-rust#426 (v0.0.7): the toolkit Switch takes the mockup's track colours.
        let p = LIGHT;
        assert_eq!(switch_track(true, true, &p), p.ink);
        assert_eq!(switch_track(false, true, &p), p.surface_2);
        assert_eq!(switch_track(true, false, &p), p.line);
        let src = include_str!("agents.rs").split("mod tests").next().unwrap();
        let switch = src.split("fn MockSwitch(").nth(1).unwrap().split("fn switch_track(").next().unwrap();
        assert!(switch.contains("cu::Switch(checked)") && switch.contains(".size(36.0, 20.0)"));
        assert!(switch.contains(".accessible_name(label)"));
        assert!(!switch.contains("Canvas"), "the switch is the toolkit's, not a painted stand-in");
    }

    /// `(node, name)` of every Button, Switch and Slider in the first frame and after the width
    /// report. The name is the `accessible_name` when one is set (compose-rust v0.0.7), else the
    /// visible text.
    fn button_texts_at(width: f32, class: compose_rust::WindowSizeClass, section: Section) -> Vec<(u32, String)> {
        use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
        use compose_rust::{EventPayload, PropertyKind, WidgetKind, WindowHeightClass};
        use std::collections::{HashMap, HashSet};
        compose_rust::window::reset_window_size();
        let mut host = Host::new(move || {
            Box::new(Recomposer::new(move || {
                AgentsScreenWith(sample_live(), sample_computers(), sample_bars(), Load::Ready(crate::demo::usage_summary(crate::demo::now_ms())), section)
            }))
        });
        let mut nodes: HashSet<u32> = HashSet::new();
        let mut texts: HashMap<u32, String> = HashMap::new();
        let mut names: HashMap<u32, String> = HashMap::new();
        let mut scan = |batch: &[Mutation<'_>]| {
            for m in batch {
                match m {
                    Mutation::Create { node_id, widget: WidgetKind::Button | WidgetKind::Switch | WidgetKind::Slider } => {
                        nodes.insert(*node_id);
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(t) } => {
                        texts.insert(*node_id, t.to_string());
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::AccessibleName, value: PropertyValue::String(t) } => {
                        names.insert(*node_id, t.to_string());
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: width,
                height_dp: 900.0,
                class,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        scan(&decode_batch(bytes).expect("decode"));
        compose_rust::window::reset_window_size();
        nodes
            .into_iter()
            .map(|n| (n, names.get(&n).or_else(|| texts.get(&n)).cloned().unwrap_or_default()))
            .collect()
    }

    #[test]
    fn fr_l20_no_button_has_an_empty_name_in_any_section_at_1440_and_390() {
        use compose_rust::WindowSizeClass as W;
        for (w, class) in [(1440.0, W::Expanded), (390.0, W::Compact)] {
            for section in Section::ALL {
                let buttons = button_texts_at(w, class, section);
                for (node, text) in &buttons {
                    assert!(
                        !text.trim().is_empty(),
                        "Button {node} has no name in {section:?} at {w} dp"
                    );
                }
            }
        }
    }

    #[test]
    fn fr_l16_models_controls_expose_their_own_labels_and_the_default_chips_say_not_chosen() {
        use compose_rust::WindowSizeClass as W;
        for (w, class) in [(1440.0, W::Expanded), (390.0, W::Compact)] {
            let names: Vec<String> =
                button_texts_at(w, class, Section::Models).into_iter().map(|(_, t)| t).collect();
            for want in ["명령마다 확인", "아직 정하지 않았습니다"] {
                assert!(names.iter().any(|n| n == want), "missing button {want} at {w} dp: {names:?}");
            }
            // A radio and a switch name their model.
            assert!(names.iter().any(|n| n.ends_with(" 기본 모델로 선택") && n.len() > " 기본 모델로 선택".len()));
            assert!(names.iter().any(|n| n.ends_with(" 활성화") && n.len() > " 활성화".len()));
        }
    }

    #[test]
    fn fr_l20_sample_paths_are_marked_and_gemini_has_none() {
        let live = sample_live();
        let claude = AgentHarness::ClaudeCode;
        assert!(crate::ui::agents::harness_path_text(claude, &live.detected, true).ends_with("(예시)"));
        assert_eq!(crate::ui::agents::harness_path_text(AgentHarness::Gemini, &live.detected, true), "준비 중입니다");
        // Real (non-sample) data without a reported path shows no path at all.
        let bare = vec![DetectedAgent { kind: "claude-code".into(), installed: true, ..Default::default() }];
        assert_eq!(crate::ui::agents::harness_path_text(claude, &bare, false), crate::ui::agents::NO_INFO);
    }
}
