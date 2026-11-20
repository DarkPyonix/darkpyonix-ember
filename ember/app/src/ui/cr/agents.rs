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
use compose_rust::{Alignment, Color, IconRole, ShapeRole};
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
    AppButton, ButtonKind, Choice, ChoiceButton, ChoiceRowFit, EmberCard, EmberField, FieldOptions, InsetPanel,
    DataNamedOverlay, InsetPanelSized, Label, NameThenPillLines, NamedClickable, Pill, PillTone, PopKind, PopMenu, PopRow, PopoverMenu,
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
    let window = crate::ui::window_size::current_window_size();
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
                                // `.head h1`: 32/400, 28 up to 900 px (review AG6).
                                Label(
                                    "에이전트",
                                    if compact { Fs::Xl2 } else { Fs::Xl3 },
                                    PAGE_TITLE_WEIGHT,
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
                        // `.ag-body{gap:var(--gap-card)}`: 16 px between cards (review AG9).
                        cu::Column()
                            .spacing(GAP_CARD)
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
                                    .spacing(GAP_CARD)
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
                    // The painted row is the control (`NamedClickable`, compose-rust#429).
                    NamedClickable(
                        x.label(),
                        Modifier::new()
                            .width(200.0)
                            .height(H_BTN)
                            .background(paint(bg))
                            .corner_radius(CORNER_SM),
                        Alignment::CenterStart,
                        move || on_pick(x),
                        move || {
                            cu::Row().alignment(Alignment::CenterStart).content(|| {
                                cu::Spacer().modifier(Modifier::new().width(12.0));
                                super::shell::NavIcon(section_svg(x), fg.to_argb(), 18.0);
                                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                Label(x.label(), Fs::Md, Weight::Medium, Some(paint(fg)), Some(1));
                                cu::Spacer().modifier(Modifier::new().weight(1.0));
                            });
                        },
                    );
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
                        // The pill is the control and takes the width of its icon and label.
                        NamedClickable(
                            x.label(),
                            Modifier::new()
                                .height(H_CTL)
                                .background(paint(bg))
                                .border(1.0, paint(border))
                                .shape_role(ShapeRole::Full),
                            Alignment::Center,
                            move || on_pick(x),
                            move || {
                                cu::Row().alignment(Alignment::CenterStart).content(|| {
                                    cu::Spacer().modifier(Modifier::new().width(12.0));
                                    super::shell::NavIcon(section_svg(x), fg.to_argb(), 16.0);
                                    cu::Spacer().modifier(Modifier::new().width(6.0));
                                    Label(x.label(), Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                                    cu::Spacer().modifier(Modifier::new().width(12.0));
                                });
                            },
                        );
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
            DataNamedOverlay(&radio_label, true, None, move || on_click());
        });
}

/// A selectable chip matching the mockup's .field.ag-sel: as wide as its one-line label and the
/// 14 px at each side. It had a fixed 200 while the button over it was `fill_max_size`, which made
/// a chip without a width take the rest of the row; the button takes the chip's bounds now
/// (`match_parent_size`, compose-rust#431). Its name can be data (a profile), so that button
/// carries it.
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
            DataNamedOverlay(text, true, None, on_click);
        });
}

/// A picker pill matching mockup's .pickchip: full pill shape, icon + label, sized by its content
/// (12 px, the icon and 8, the label, 12). Its width was a `text_px` estimate for the reason
/// [`SelectChip`] gives, and goes with it (compose-rust#431).
fn ModelPickerPill(text: &str, svg: Option<&'static str>, on_click: impl FnMut() + 'static) {
    MenuPickerPill(text, svg, None, on_click);
}

/// [`ModelPickerPill`] as the control of a [`PopoverMenu`], saying whether the menu is open.
fn MenuPickerPill(text: &str, svg: Option<&'static str>, expanded: Option<bool>, on_click: impl FnMut() + 'static) {
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
            DataNamedOverlay(text, true, expanded, on_click);
        });
}

/// A segmented control matching the mockup (.seg button height 32, selected = ink fill, white label).
fn SegGroup<T: Copy + PartialEq + 'static>(
    options: &'static [(T, &'static str)],
    current: T,
    mut on_pick: impl FnMut(T) + Clone + 'static,
) {
    // `.seg`: 8 px apart and wrapping, each a 32 px `.seg button` with the 13/500 label (#878:
    // the toolkit label drew about 18 px). As wide as its choices: it sits at the end of a
    // SettingRow, beside a title that takes the rest.
    ChoiceRowFit(|| {
        for (val, label) in options {
            let val = *val;
            let selected = val == current;
            let mut on_pick = on_pick.clone();
            ChoiceButton(label, Choice::Radio, selected, true, true, move || on_pick(val));
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

/// `"17 GB"` as `("17", Some("GB"))`: the mockup draws a unit as a small grey pill beside the
/// number (`세션 메모리 한도 합 17 [GB]`); anything else is one value.
pub fn kpi_split_unit(value: &str) -> (&str, Option<&str>) {
    match value.split_once(' ') {
        Some((n, u)) if n.parse::<f64>().is_ok() && matches!(u, "GB" | "MB") => (n, Some(u)),
        _ => (value, None),
    }
}

/// The type of a KPI value: a number is `.kpi .v`, 28/400 with -0.02em (18 at phone width,
/// `style.css` `@media (max-width:560px) .kpi .v`); a word such as 방금 or 아직 정보가
/// 없습니다 is `.v.ag-word`, 18/600 (`agents.css`). Review AG5 measured 18/600 for every value.
pub fn kpi_value_type(value: &str, phone: bool) -> (Fs, Weight) {
    if kpi_split_unit(value).0.parse::<f64>().is_err() {
        (Fs::Xl, Weight::Semibold)
    } else if phone {
        (Fs::Xl, Weight::Regular)
    } else {
        (Fs::Xl2, Weight::Regular)
    }
}

/// `.kpis`: four columns 24 apart; two columns 16 apart up to 1279 px and 12 apart up to 560 px
/// (`style.css`). Returns (columns, gap).
pub fn kpi_grid(width_dp: f32) -> (usize, f32) {
    if width_dp <= 560.0 {
        (2, SP[2])
    } else if width_dp <= 1279.0 {
        (2, SP[3])
    } else {
        (4, SP[5])
    }
}

/// One KPI: the 13 px `text-2` label, then the value and its small pills (a unit, a total, 예시)
/// 8 px apart (`.kpi .v{gap:var(--sp2)}`, `.kpi .v .pill` 20 px).
fn KpiItem(label: &str, value: &str, pill: Option<(&str, PillTone)>, phone: bool) {
    let p = ember_theme().palette;
    let (number, unit) = kpi_split_unit(value);
    let (fs, weight) = kpi_value_type(value, phone);
    cu::Column().content(|| {
        Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
        cu::Spacer().modifier(Modifier::new().height(SP[0]));
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
            Label(number, fs, weight, None, Some(1));
            if let Some(u) = unit {
                Pill(u, PillTone::Grey, true);
            }
            if let Some((p_text, tone)) = pill {
                Pill(p_text, tone, true);
            }
        });
    });
}

/// The KPI strip as the `.kpis` grid for this width; a short last row keeps the cell width.
fn KpiGrid(items: &[(&str, String, Option<(&str, PillTone)>)], width_dp: f32) {
    let (cols, gap) = kpi_grid(width_dp);
    let phone = width_dp <= 560.0;
    FixedGrid(items, cols, gap, |(label, value, pill)| KpiItem(label, value, *pill, phone));
}

/// How many empty cells close a row of `len` items in a `cols`-column grid.
pub fn grid_fillers(len: usize, cols: usize) -> usize {
    cols.saturating_sub(len)
}

/// A grid of `cols` equal columns, `gap` apart both ways. A short last row keeps the cell width:
/// empty weighted cells fill it, where weighted cells alone stretched two tiles to half the
/// width each (review AG8; `agents.css` `.ag-accs`, `.ag-ctiles` are fixed column grids).
fn FixedGrid<T>(items: &[T], cols: usize, gap: f32, cell: impl Fn(&T)) {
    FixedGridRows(items, cols, gap, false, cell);
}

/// Whether a tile grid's rows stretch their tiles to the tallest one: only with more than one
/// column, where tiles sit side by side (a CSS grid's `align-items:stretch`).
pub fn grid_rows_equal_height(cols: usize) -> bool {
    cols > 1
}

/// [`FixedGrid`] whose rows can be `equal_height`: each cell then fills the row's height, so a
/// tile that fills its cell is as tall as the tallest tile beside it (review r7: account tiles
/// 88 / 88 / 98 where one carries 한도 가까움; `Row::equal_height`, compose-rust#389).
fn FixedGridRows<T>(items: &[T], cols: usize, gap: f32, equal_height: bool, cell: impl Fn(&T)) {
    let cols = cols.max(1);
    cu::Column().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
        for chunk in items.chunks(cols) {
            cu::Row().alignment(Alignment::TopStart).spacing(gap).equal_height(equal_height).modifier(Modifier::new().fill_max_width()).content(|| {
                for x in chunk {
                    let m = Modifier::new().weight(1.0);
                    cu::Column().modifier(if equal_height { m.fill_max_height() } else { m }).content(|| cell(x));
                }
                for _ in 0..grid_fillers(chunk.len(), cols) {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                }
            });
        }
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

/// The least a card header's content stands: `.card-h{min-height:56px}` less its 16 px above
/// and below. A 30 px tile or avatar makes the header 62, a two-line title taller.
pub const CARD_HEAD_CONTENT_MIN_H: f32 = 56.0 - 2.0 * PAD_CARD_V;

/// The mockup's `.card` with a `.card-h` over a `.card-b` (`agents.js` `agCard`, `style.css`):
/// the header's children 12 apart and centred inside 16 px above and below and 20 px at the
/// sides, a 1 px `line` rule under it across the whole card, then the body in the same padding.
/// The card has no padding of its own, so the rule reaches both borders (review r7: the cards
/// drew their title as the body's first line with no rule).
fn HeadedCard(head: impl FnOnce(), body: impl FnOnce()) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, Some(0.0), None, || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Spacer().modifier(Modifier::new().height(PAD_CARD_V));
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().width(PAD_CARD_H));
                // compose-rust#778 / #826 (v0.0.11 `Modifier::min_height`): `.card-h{min-height:56px}`
                // less the 16 px padding above and below.
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[2])
                    .modifier(Modifier::new().weight(1.0).min_height(CARD_HEAD_CONTENT_MIN_H))
                    .content(head);
                cu::Spacer().modifier(Modifier::new().width(PAD_CARD_H));
            });
            cu::Spacer().modifier(Modifier::new().height(PAD_CARD_V));
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
            cu::Spacer().modifier(Modifier::new().height(PAD_CARD_V));
            cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().width(PAD_CARD_H));
                cu::Column().modifier(Modifier::new().weight(1.0)).content(body);
                cu::Spacer().modifier(Modifier::new().width(PAD_CARD_H));
            });
            cu::Spacer().modifier(Modifier::new().height(PAD_CARD_V));
        });
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

/// `.ag-formula`: chips with an operator after each but the last, 8 apart, wrapping to the next
/// line (`flex-wrap:wrap`). A chip and its operator stay together, so an operator never starts a
/// line. A plain row of all of them was wider than the section body below about 1100 dp and
/// pushed the whole column over the section nav (review r7, agents-profiles-1024).
fn FormulaFlow(steps: &[(Option<&'static str>, &str, bool)], ops: &[&str], op_fs: Fs) {
    let p = ember_theme().palette;
    cu::FlowRow()
        .horizontal_spacing(SP[1])
        .vertical_spacing(SP[1])
        .modifier(Modifier::new().fill_max_width())
        .content(|| {
            for (i, (svg, text, dashed)) in steps.iter().enumerate() {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                    FormulaChip(*svg, text, *dashed);
                    if let Some(op) = ops.get(i) {
                        Label(op, op_fs, Weight::Regular, Some(paint(p.muted)), None);
                    }
                });
            }
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

/// Whether a setting row puts its control under its title and hint: below 560 px the mockup's
/// `.ag-row` wraps and `.ag-rc` takes the whole line (`flex:1 1 100%`, start aligned), where a
/// control beside the text left the hints 2 to 4 words a line (review AG13, r6 item 1d).
pub fn setting_row_stacked(width_dp: f32) -> bool {
    width_dp <= 560.0
}

/// One setting: title and hint on the left, the control on the right; below 560 px the control
/// sits under the text.
fn SettingRow(title: &str, hint: &str, control: impl FnOnce()) {
    SettingRowAt(setting_row_stacked(crate::ui::window_size::current_window_size().width_dp), title, hint, control);
}

fn SettingRowAt(stacked: bool, title: &str, hint: &str, control: impl FnOnce()) {
    SettingRowIn(stacked, false, title, hint, control);
}

/// The first row of a card body: `.ag-rows>.ag-row:first-child{border-top:0;padding-top:0}`, so
/// no rule doubles the one under the card header.
fn SettingRowFirst(title: &str, hint: &str, control: impl FnOnce()) {
    SettingRowIn(setting_row_stacked(crate::ui::window_size::current_window_size().width_dp), true, title, hint, control);
}

fn SettingRowIn(stacked: bool, first: bool, title: &str, hint: &str, control: impl FnOnce()) {
    let p = ember_theme().palette;
    let text = |m: Modifier| {
        cu::Column().modifier(m).content(|| {
            Label(title, Fs::Md, Weight::Medium, None, Some(1));
            if !hint.is_empty() {
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                Label(hint, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
            }
        });
    };
    cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
        if !first {
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
        }
        if stacked {
            // `.ag-row{gap:var(--sp2)}` once wrapped: 8 px between the text and the control.
            cu::Column()
                .alignment(Alignment::TopStart)
                .spacing(SP[1])
                .modifier(Modifier::new().fill_max_width())
                .content(|| {
                    text(Modifier::new().fill_max_width());
                    control();
                });
        } else {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .spacing(SP[3])
                .modifier(Modifier::new().fill_max_width())
                .content(|| {
                    text(Modifier::new().weight(1.0));
                    control();
                });
        }
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

/// An account ring: the full `--track` circle (`#ecebe6`, dark `#2f2e2b`) under the arc. The
/// track was `surface-2`, the tile's own fill, so only the arc showed (review AG7).
fn draw_ring(percent: Option<u8>, size: f32, stroke_width: f32, p: &Palette) -> DrawList {
    let half = size / 2.0;
    let radius = half - stroke_width / 2.0;
    let track_color = p.track;
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
    let kpis = [
        (
            "설치된 하네스",
            crate::ui::agents::kpi_text(installed_count.map(|n| n.to_string())),
            installed_count.map(|_| if example { ("예시", PillTone::Orange) } else { (total.as_str(), PillTone::Grey) }),
        ),
        (
            "상주 shim",
            crate::ui::agents::kpi_text(shim_count.map(|n| n.to_string())),
            sample_pill(example && shim_count.is_some()),
        ),
        ("세션 메모리 한도 합", crate::ui::agents::kpi_text(mem_total), None),
        ("마지막 감지", last_detect, sample_pill(example && checked.is_some() || live.sample)),
    ];
    KpiGrid(&kpis, crate::ui::window_size::current_window_size().width_dp);

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
    if compact { 0.0 } else { crate::ui::window_size::current_window_size().width_dp }
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

/// The width of one of `columns` cards side by side in the section body at `window_dp`, with
/// the shell navigation this width gets (sidebar, rail or none). A collapsed sidebar only makes
/// the body wider, so a grid planned from this width still fits.
pub fn body_card_width(window_dp: f32, columns: usize) -> f32 {
    use crate::ui::shell::{nav_mode, ShellMode};
    let nav = match nav_mode(window_dp, false) {
        ShellMode::BottomTabs => 0.0,
        ShellMode::Rail => RAIL_W,
        ShellMode::Sidebar => SIDE_W,
    };
    harness_card_width(window_dp, nav, columns)
}

/// The content width inside a card of [`body_card_width`]: less its 20 dp sides (`--pad-card`).
pub fn card_inner_width(window_dp: f32, columns: usize) -> f32 {
    (body_card_width(window_dp, columns) - 2.0 * PAD_CARD_H).max(0.0)
}

/// `.ag-grid2`: two cards a row from 1280 dp, one below (`agents.css` `@media (max-width:1279px)`):
/// the harness, model and profile cards and the usage pair.
pub fn grid2_columns(window_dp: f32) -> usize {
    harness_columns(window_dp)
}

/// `repeat(auto-fill, minmax(<min>, 1fr))` with `gap` between: as many columns of at least
/// `min` as `avail` holds, never fewer than one.
pub fn auto_fill_columns(avail: f32, min: f32, gap: f32) -> usize {
    (((avail + gap) / (min + gap)).floor() as usize).max(1)
}

/// `.ag-ctiles{minmax(240px,1fr)}`: the narrowest computer tile.
pub const COMPUTER_TILE_MIN: f32 = 240.0;
/// The narrowest account tile. The mockup's `.ag-accs{minmax(260px,1fr)}` leaves the name about
/// 100 dp beside the 72 dp ring and the plan pill, enough for its short ids (`ncmnd`) but not for
/// the account labels Ember shows: at 1440 three 290 dp tiles cut "Claude Max (work)" to
/// "Claude Max (..." (review r7). 320 keeps two tiles a row at 1440 and 1280 (as before r7),
/// one at 1024, and gives the name and the pill their whole text.
pub const ACCOUNT_TILE_MIN: f32 = 320.0;

/// Columns of a tile grid (accounts, computers) in a full-width card at `window_dp`: one up to
/// 560 dp (`@media (max-width:560px) .ag-accs,.ag-ctiles{minmax(0,1fr)}`), then as many tiles of
/// at least `min` as the card holds, 12 apart.
pub fn tile_columns(window_dp: f32, min: f32) -> usize {
    if window_dp <= 560.0 {
        1
    } else {
        auto_fill_columns(card_inner_width(window_dp, 1), min, SP[2])
    }
}

/// The content width of one tile in a `cols`-column grid of a full-width card: its cell less the
/// panel's 16 dp sides (`InsetPanel`: 12 and a 4 dp spacer).
pub fn tile_inner_width(window_dp: f32, cols: usize) -> f32 {
    let n = cols.max(1) as f32;
    ((card_inner_width(window_dp, 1) - SP[2] * (n - 1.0)) / n - 2.0 * (PAD_ROW_V + SP[0])).max(0.0)
}

/// The room IP 확인 takes at the end of a computer tile's count line: the small button (66 dp in
/// the r7 captures, 68 kept) and the 8 dp before it.
pub const IP_CHECK_ROOM: f32 = 68.0 + 8.0;

/// Whether every count of a computer tile fits on a line beside IP 확인 at this window width
/// (`.ag-cstat`). A number always does; a count that reads 아직 정보가 없습니다 does not in a
/// narrow tile (review r7 and #886: at 900 gpu-box's second count ended in an ellipsis). Widths
/// are the layout estimate of [`super::home::text_px`]. A window that has not reported its width
/// counts as wide.
pub fn counts_beside_ip_check(window_dp: f32, counts: &[(&str, &str)]) -> bool {
    if window_dp <= 0.0 {
        return true;
    }
    let room = tile_inner_width(window_dp, tile_columns(window_dp, COMPUTER_TILE_MIN)) - IP_CHECK_ROOM;
    counts
        .iter()
        .all(|(num, unit)| super::home::text_px(num, Fs::Sm.size()) + super::home::text_px(unit, Fs::Xs.size()) <= room)
}

/// The usage hero puts its figures (320 dp) and the 440 dp day bars side by side only when the
/// card holds both 20 apart (`.ag-hero{flex-wrap:wrap}`); otherwise the bars go under them.
pub fn usage_hero_side_by_side(window_dp: f32) -> bool {
    card_inner_width(window_dp, 1) >= USAGE_FIGURES_W + SP[4] + USAGE_BARS_W
}
const USAGE_FIGURES_W: f32 = 320.0;
const USAGE_BARS_W: f32 = 440.0;

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

    // The header owns its copy of the version: the rows below read the original.
    let head_version = version.clone();
    HeadedCard(
        move || {
            BotAvatar(h, 30.0);
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(h.label(), Fs::Md, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().height(2.0));
                let mut sub = crate::ui::agents::harness_subtitle(h.run_mode(), head_version.as_deref());
                if example && head_version.is_some() {
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
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                // Row 1: 설치와 버전
                SettingRowFirst(
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
        },
    );
}

// ---------------------------------------------------------------------------------------------
// 2. 모델 (Models)
// ---------------------------------------------------------------------------------------------

/// One model of Ember's reference catalog. `plans` run from the smaller plan to the larger, as
/// the mockup's `plans` (`Pro`, `Max`; `Plus`, `Pro`).
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
        plans: &[("Pro", true), ("Max", true)],
    },
    ModelSpec {
        id: "sonnet",
        name: "Sonnet 5.5",
        note: "속도와 품질의 균형",
        plans: &[("Pro", true), ("Max", true)],
    },
    ModelSpec {
        id: "haiku",
        name: "Haiku 5.5",
        note: "가장 빠르고 저렴합니다",
        plans: &[("Pro", true), ("Max", true)],
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
    // The demo's account pick stays on this screen (no server call), like its harness pick.
    let local_account = remember(|| mutable_state_of(None::<String>));
    let chips = crate::ui::agents::default_chips(&live.accounts, &live.default_ids, &store.get());
    let chips = if example { crate::ui::agents::with_local_account(chips, local_account.get()) } else { chips };
    let harness_pill = crate::ui::agents::harness_chip_text(&chips, model_name, example);
    let account_pill = crate::ui::agents::account_chip_text(&chips, live.accounts.len(), example);
    let hook = crate::ui::agents::sheet_hook();
    let pick_open = remember(move || mutable_state_of(hook == Some(crate::ui::agents::SheetHook::Harness)));
    let acct_open = remember(move || mutable_state_of(hook == Some(crate::ui::agents::SheetHook::Account)));
    let stacked = crate::ui::agents::chips_stacked(crate::ui::window_size::current_window_size().width_dp);
    let rows = crate::ui::agents::pick_rows(&crate::ui::agents::harness_options(&live.detected), model_pairs);
    let row_chosen = rows
        .iter()
        .position(|r| r.enabled && Some(r.harness) == chips.harness && r.model == chips.model);
    let options = crate::ui::agents::account_options(&live.accounts);
    let acct_rows = crate::ui::agents::account_sheet_rows(&options);
    // "새 팀원 기본값" card
    HeadedCard(
        || {
            CardHeaderIcon(super::shell::ICON_PLUS_SVG);
            Label("새 팀원 기본값", Fs::Lg, Weight::Semibold, None, Some(1));
            if example {
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Pill(crate::ui::agents::EXAMPLE_MARK, PillTone::Grey, true);
            }
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label(
                    "홈과 새 팀원 만들기, 루틴에서 처음 보이는 하네스와 모델입니다. 고른 계정의 플랜에서 쓸 수 없는 모델은 흐리게 표시됩니다.",
                    Fs::Sm,
                    Weight::Regular,
                    None,
                    None,
                );
                // Each chip opens its list as a popover under it (FR-L10.4, #888, review AG14), the
                // mockup's `.mp-pop` (a bottom sheet at 560 and below). Not built: the harness rail,
                // search and shortcuts of the shared model picker (FR-L15) in this card.
                let chips_body = || {
                    let pick_rows: Vec<PopRow> = rows
                        .iter()
                        .enumerate()
                        .map(|(i, r)| PopRow {
                            icon: Some(ICON_SPARK_SVG),
                            title: r.label.clone(),
                            tint: (!r.enabled).then_some(ember_theme().palette.muted),
                            chosen: row_chosen == Some(i),
                            ..Default::default()
                        })
                        .collect();
                    let close = {
                        let o = pick_open.clone();
                        move || o.set(false)
                    };
                    let (st, list) = (store.clone(), rows.clone());
                    let po = pick_open.clone();
                    let pick_is_open = pick_open.get();
                    PopoverMenu(
                        pick_is_open,
                        PopMenu { name: super::a11y::MODEL_PICKER, kind: PopKind::Mode, head: "", sub: None, rows: &pick_rows, align: compose_rust::PopoverAlign::Start },
                        close,
                        move |i| {
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
                        },
                        || MenuPickerPill(&harness_pill, Some(ICON_SPARK_SVG), Some(pick_is_open), move || po.set(true)),
                        None,
                    );
                    let acct_pop: Vec<PopRow> = acct_rows
                        .iter()
                        .enumerate()
                        .map(|(i, label)| PopRow {
                            icon: Some(ICON_USER_SVG),
                            title: label.clone(),
                            chosen: live.accounts.get(i).is_some_and(|a| Some(&a.label) == chips.account.as_ref()),
                            ..Default::default()
                        })
                        .collect();
                    let close = {
                        let o = acct_open.clone();
                        move || o.set(false)
                    };
                    let list = options.clone();
                    let labels: Vec<String> = live.accounts.iter().map(|a| a.label.clone()).collect();
                    let picked = local_account.clone();
                    let ao = acct_open.clone();
                    let acct_is_open = acct_open.get();
                    PopoverMenu(
                        acct_is_open,
                        PopMenu { name: super::a11y::ACCOUNT_PICKER, kind: PopKind::Mode, head: "", sub: None, rows: &acct_pop, align: compose_rust::PopoverAlign::Start },
                        close,
                        move |i| {
                            let Some(o) = list.get(i) else { return };
                            if example {
                                picked.set(labels.get(i).cloned());
                                post_notice(crate::ui::agents::DEMO_CHOICE_NOTE);
                            } else {
                                set_default_account(o.id.clone());
                            }
                        },
                        || MenuPickerPill(&account_pill, Some(ICON_USER_SVG), Some(acct_is_open), move || ao.set(true)),
                        None,
                    );
                };
                // Below 560 dp the chips stack so each keeps its full text.
                if stacked {
                    cu::Column().spacing(SP[2]).content(chips_body);
                } else {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(chips_body);
                }
                Label(crate::ui::agents::DEFAULTS_STORAGE_NOTE, Fs::Xs, Weight::Regular, Some(paint(ember_theme().palette.muted)), None);
            });
        },
    );

    let harnesses = [AgentHarness::ClaudeCode, AgentHarness::Codex, AgentHarness::Gemini];

    // `.ag-grid2`: two cards a row from 1280 dp, one below. At 1024 two columns left the model
    // name about 75 dp, so the 기본 badge drew as an empty pill (review r7).
    if grid2_columns(compact_window_width(compact)) < 2 {
        for h in harnesses {
            ModelCard(h, store, compact, example);
        }
    } else {
        cu::Row().spacing(GAP_CARD).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[0], store, compact, example);
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[1], store, compact, example);
            });
        });
        cu::Row().spacing(GAP_CARD).modifier(Modifier::new().fill_max_width()).content(|| {
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
pub(super) fn AddAccountSheet(add: &MutableState<crate::ui::agents::AddAccount>, example: bool) {
    use crate::ui::agents::{add_account_kinds, AddAccount, AddPhase};
    let theme = ember_theme();
    let p = theme.palette;
    let cur = add.get();
    let close = {
        let a = add.clone();
        move || a.set(AddAccount::closed())
    };
    let busy = cur.phase == AddPhase::Busy;
    let dismiss = {
        let close = close.clone();
        move || {
            if !busy {
                close();
            }
        }
    };
    cu::Dialog(cur.open)
        .modifier(
            with_shadows(Modifier::new(), theme.shadow_lg())
                .width(420.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_MD)
                .padding(SP[3]),
        )
        .on_dismiss(dismiss)
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
                        AppButton("닫기", None, ButtonKind::Default, true, !busy, false, close.clone());
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

    HeadedCard(
        move || {
            VendorDot(h);
            Label(h.label(), Fs::Md, Weight::Semibold, None, Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Pill(&format!("모델 {}", specs.len()), PillTone::Grey, true);
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                // Row 1: 기본 추론 강도 (SegGroup with 낮음/보통/높음)
                SettingRowFirst(
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

                // Model table (`.ag-mhead`, `.ag-mrow`): columns 28 | name | plans | switch, 12
                // apart. The plans column is as wide as its pills and the header reads right-aligned
                // over it on one line; below 560 dp the header hides and the plans go under the note
                // (`agents.css` `@media (max-width:560px)`).
                let phone = model_table_phone(crate::ui::window_size::current_window_size().width_dp);
                cu::Spacer().modifier(Modifier::new().height(SP[1]));
                if !phone {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        cu::Box().modifier(Modifier::new().width(28.0)).content(|| {
                            Label("기본", Fs::Xs2, Weight::Medium, Some(paint(p.muted)), Some(1));
                        });
                        cu::Column().modifier(Modifier::new().weight(1.0).width(0.0)).content(|| {
                            Label("모델", Fs::Xs2, Weight::Medium, Some(paint(p.muted)), Some(1));
                        });
                        Label(MODEL_PLANS_HEADER, Fs::Xs2, Weight::Medium, Some(paint(p.muted)), Some(1));
                        // The mockup's header span wraps in its 36 px cell: 목록에 over 표시, left aligned.
                        cu::Box().modifier(Modifier::new().width(MODEL_SWITCH_COL)).alignment(Alignment::CenterStart).content(|| {
                            Label(MODEL_SWITCH_HEADER, Fs::Xs2, Weight::Medium, Some(paint(p.muted)), Some(2));
                        });
                    });
                }

                for m in specs {
                    let is_def = settings.model.as_deref() == Some(m.id);
                    let m_id = m.id.to_string();
                    let store_m = store.clone();
                    let plans = || {
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[0]).content(|| {
                            for (p_name, ok) in m.plans {
                                Pill(
                                    &format!("{}{p_name}", if *ok { "✓ " } else { "✕ " }),
                                    if *ok { PillTone::Green } else { PillTone::Grey },
                                    true,
                                );
                            }
                        });
                    };
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[1])).content(|| {
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
                        cu::Column().modifier(Modifier::new().weight(1.0).width(0.0)).content(|| {
                            // `.ag-mn b`: the name, then the blue 기본 badge right after it. The
                            // mockup's name and note wrap in their `minmax(0,1fr)` column instead of
                            // ending in "..." (#886: at 1280 `GPT-5 Cod...`, `가장 깊게 생각...`), and
                            // the badge keeps its own width (it was squeezed to a blob beside a name
                            // measured first).
                            if is_def {
                                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                                    NameThenPillLines(m.name, Fs::Md, Weight::Medium, None, false, MODEL_TEXT_LINES, SP[1], || Pill("기본", PillTone::Blue, true));
                                });
                            } else {
                                Label(m.name, Fs::Md, Weight::Medium, None, Some(MODEL_TEXT_LINES));
                            }
                            cu::Spacer().modifier(Modifier::new().height(2.0));
                            Label(m.note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(MODEL_TEXT_LINES));
                            if phone {
                                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                                plans();
                            }
                        });
                        if !phone {
                            plans();
                        }
                        cu::Box().modifier(Modifier::new().width(MODEL_SWITCH_COL)).alignment(Alignment::CenterEnd).content(|| {
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
        },
    );
}

/// The plans column header. The marks are Ember's own reference list, not server data, and the
/// column says so (SPEC FR-L20); the mockup's shorter 플랜별 사용 does not.
pub const MODEL_PLANS_HEADER: &str = "플랜별 사용 (Ember 참고 목록)";
/// The lines a model's name and note may take in the table: two, as the mockup's wrapping
/// `.ag-mn` column shows them at 1280, where two model cards share a row (#886).
pub const MODEL_TEXT_LINES: u32 = 2;
/// The switch column: `.ag-mhead`/`.ag-mrow` `grid-template-columns: 28px minmax(0,1fr) auto 36px`,
/// as wide as the 36 px switch (#886). Its header wraps to two lines, as the mockup's does.
const MODEL_SWITCH_COL: f32 = 36.0;
/// The switch column's header (`agents.js` `.ag-mhead`).
const MODEL_SWITCH_HEADER: &str = "목록에 표시";

/// Below 560 dp the model table hides its header and puts the plans under the note.
pub fn model_table_phone(width_dp: f32) -> bool {
    width_dp <= 560.0
}

// ---------------------------------------------------------------------------------------------
// 3. 프로필 (Profiles)
// ---------------------------------------------------------------------------------------------

/// The plan an account's own label names (`Max`, `Plus`, `Pro`); a guessed "Pro" would be an
/// invented fact, so a label without one gets no pill.
pub fn account_plan(label: &str) -> Option<&'static str> {
    ["Max", "Plus", "Pro"].into_iter().find(|n| label.contains(*n))
}

/// An account tile's first line (`.ag-acc-t .flex`): the harness dot, the name on one line with
/// an ellipsis, then the plan pill whole. The name sits in a zero-width weighted column: the
/// desktop renderer gives a one-line label its whole width first, which squeezed the pill after
/// it to its two 4 px insets, an empty grey circle on "Claude Max (work)" (review AG16, r6).
fn AccountNameLine(x: &AccountInfo) {
    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        VendorDot(x.harness);
        cu::Column().modifier(Modifier::new().weight(1.0).width(0.0)).content(|| {
            Label(&x.label, Fs::Md, Weight::Semibold, None, Some(1));
        });
        if let Some(plan) = account_plan(&x.label) {
            Pill(plan, PillTone::Grey, true);
        }
    });
}

/// `.inset.ag-acc`; with `fill_height` it takes its cell's height (an `equal_height` grid row).
fn AccountTile(x: &AccountInfo, extra: Option<&str>, sample: bool, fill_height: bool) {
    let p = ember_theme().palette;
    InsetPanelSized(true, fill_height, || {
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
                AccountNameLine(x);
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                let reset_str = crate::ui::agents::account_reset_line(x.harness.label(), sample);
                Label(&reset_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                if let Some(left) = x.left_percent {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        Label("5h", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        let bar_color = if left < 20 { p.orange.strong } else { p.green.strong };
                        let bar_draw = DrawList::builder()
                            // `.track`: the 6 px bar on `--track`, not the tile's fill.
                            .round_rect(paint(p.track), 0.0, 0.0, 115.0, 6.0, 3.0, 0.0)
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

    // 1. Pipeline formula row (`.ag-formula{flex-wrap:wrap}`): a step that does not fit starts
    // the next line instead of pushing the column over the section nav (review r7, 1024).
    FormulaFlow(
        &[
            (Some(ICON_USER_SVG), "계정 묶음", false),
            (Some(ICON_REPEAT_SVG), "전환 규칙", false),
            (Some(ICON_ARROW_UP_RIGHT_SVG), "모델 프록시 (선택)", true),
            (Some(ICON_SPARK_SVG), "프로필", false),
        ],
        &["+", "+", "="],
        Fs::Lg,
    );
    Label(
        "하네스마다 프로필 하나를 정하면 그 하네스로 만든 팀원이 모두 그 프로필을 따릅니다. 팀원마다 따로 설정하지 않습니다.",
        Fs::Sm,
        Weight::Regular,
        Some(paint(p.text_2)),
        None,
    );

    // 2. "계정" Card
    HeadedCard(
        || {
            CardHeaderIcon(ICON_USER_SVG);
            Label("계정", Fs::Lg, Weight::Semibold, None, Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            let open_add = add.clone();
            AppButton("계정 추가", Some(IconRole::Add), ButtonKind::Default, true, true, false, move || {
                open_add.set(crate::ui::agents::AddAccount { open: true, ..crate::ui::agents::AddAccount::closed() });
            });
            AddAccountSheet(&add, live.sample || live.demo);
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                // `.ag-accs`: tiles of at least `ACCOUNT_TILE_MIN`, 12 apart, one column up to 560 dp;
                // a short last row keeps the cell width.
                let cols = tile_columns(crate::ui::window_size::current_window_size().width_dp, ACCOUNT_TILE_MIN);
                // Tiles in a row are one height, with or without 한도 가까움 (a grid stretches).
                let equal = grid_rows_equal_height(cols);
                FixedGridRows(&live.accounts, cols, SP[2], equal, |a| {
                    let extra = if a.limited { Some("한도 가까움") } else { None };
                    AccountTile(a, extra, live.sample, equal);
                });
            });
        },
    );

    // 3. "하네스별 기본 프로필" Card
    HeadedCard(
        || {
            CardHeaderIcon(super::shell::ICON_CPU_SVG);
            Label("하네스별 기본 프로필", Fs::Lg, Weight::Semibold, None, Some(1));
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                for h in [AgentHarness::ClaudeCode, AgentHarness::Codex, AgentHarness::Gemini] {
                    let p_id = effective_default(h, store.get().settings(h).default_profile.as_deref(), &profiles);
                    let p_name = p_id.as_ref()
                        .and_then(|id| profiles.iter().find(|p| &p.id == id))
                        .map_or("프로필 없음".to_string(), |p| p.name.clone());
                    SettingRowIn(
                        setting_row_stacked(crate::ui::window_size::current_window_size().width_dp),
                        h == AgentHarness::ClaudeCode,
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
        },
    );

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

    // 5. Grid of Profile Cards (`.ag-grid2`: two a row from 1280 dp, one below)
    if grid2_columns(compact_window_width(compact)) < 2 {
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

    // The header owns copies of what it shows: the rows below move parts of the profile into
    // their handlers.
    let (head_name, head_harness, head_fixed, head_id) = (p.name.clone(), p.harness, p.mode == AccountMode::Fixed, p.id.clone());
    let (head_seed, head_store) = (all.clone(), store.clone());
    HeadedCard(
        move || {
            VendorDot(head_harness);
            Label(&head_name, Fs::Lg, Weight::Semibold, None, Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Pill(head_harness.label(), PillTone::Blue, true);
            Pill(if head_fixed { "고정" } else { "자동 전환" }, PillTone::Grey, true);
            let seed = head_seed;
            let pid = head_id;
            let p_name = head_name.clone();
            let store = head_store;
            AppButton("지우기", Some(IconRole::Close), ButtonKind::Default, true, can_delete, false, move || {
                edit_profiles(&store, seed.clone(), |l| l.retain(|x| x.id != pid));
                post_notice(&format!("{p_name} 프로필을 지웠습니다."));
            });
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
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
        },
    );
}

// ---------------------------------------------------------------------------------------------
// 4. 네트워크 (Network)
// ---------------------------------------------------------------------------------------------

fn NetworkSection(computers: &[(String, Reachability)], sample: bool, _compact: bool) {
    let p = ember_theme().palette;

    // Card 1: API 트래픽 내보내기 
    HeadedCard(
        || {
            CardHeaderIcon(ICON_GLOBE_SVG);
            Label("API 트래픽 내보내기", Fs::Lg, Weight::Semibold, None, Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Pill("검증 전 (#130)", PillTone::Orange, true);
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                SettingRowFirst(
                    "작업 중인 컴퓨터로 내보내기",
                    "에이전트의 API 요청이 그 세션이 일하는 컴퓨터에서 나가므로 공급자는 그 컴퓨터의 IP를 봅니다.",
                    || {
                        MockSwitch("작업 중인 컴퓨터로 내보내기", true, true, |_| post_notice("아직 검증 전이라 켜 둔 채로만 보입니다"));
                    },
                );

                cu::Spacer().modifier(Modifier::new().height(SP[1]));

                // 4-step chain of formula chips; it wraps like `.ag-formula`.
                FormulaFlow(
                    &[
                        (None, "에이전트 CLI", false),
                        (None, "세션 프록시 (루프백)", false),
                        (None, "작업 중인 컴퓨터", false),
                        (None, "모델 공급자", false),
                    ],
                    &["→", "→", "→"],
                    Fs::Md,
                );

                cu::Spacer().modifier(Modifier::new().height(SP[1]));
                Label(
                    "컴퓨터를 바꾸면 새 연결부터 새 컴퓨터를 따르고, 이미 열린 연결은 원래 컴퓨터를 씁니다.",
                    Fs::Sm,
                    Weight::Regular,
                    Some(paint(p.text_2)),
                    None,
                );
            });
        },
    );

    // Card 2: 컴퓨터별 상태 (컴퓨터), 16 below the first (the section column's gap).
    HeadedCard(
        || {
            CardHeaderIcon(ICON_PC_SVG);
            Label("컴퓨터별 상태", Fs::Lg, Weight::Semibold, None, Some(1));
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                if computers.is_empty() {
                    Pending("컴퓨터 목록은");
                } else {
                    // `.ag-ctiles`: tiles of at least 240 dp, 12 apart, one column up to 560 dp; a
                    // short last row keeps the cell width.
                    let cols = tile_columns(crate::ui::window_size::current_window_size().width_dp, COMPUTER_TILE_MIN);
                    FixedGrid(computers, cols, SP[2], |(name, r)| ComputerTile(name, *r, sample));
                }
            });
        },
    );
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
            // `.ag-cstat`: the two counts, then IP 확인 at the end. The counts sit in a zero-width
            // weighted column and wrap there, so the button keeps its own width: a long value
            // (아직 정보가 없습니다 twice) pushed the button out of a 290 dp tile (review r7).
            let pairs = [(sessions_num.as_str(), " 세션"), (tunnels_num.as_str(), " 열린 연결")];
            let counts = || {
                cu::FlowRow()
                    .horizontal_spacing(SP[3])
                    .vertical_spacing(SP[0])
                    .modifier(Modifier::new().fill_max_width())
                    .content(|| {
                        for (num, unit) in pairs {
                            cu::Row().alignment(Alignment::CenterStart).content(|| {
                                Label(num, Fs::Sm, Weight::Medium, None, Some(1));
                                Label(unit, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            });
                        }
                    });
            };
            let ip_check = move || {
                AppButton("IP 확인", None, ButtonKind::Default, true, r == Reachability::Online && ip_known.is_some(), false, move || {
                    if let Some(ip) = &ip_known {
                        post_notice(&format!("{name_owned} 세션이 본 IP: {ip}"));
                    }
                });
            };
            if counts_beside_ip_check(crate::ui::window_size::current_window_size().width_dp, &pairs) {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().weight(1.0).width(0.0)).content(counts);
                    ip_check();
                });
            } else {
                // A count that is a sentence (아직 정보가 없습니다 열린 연결) is wider than the room
                // beside the button in a narrow tile, and a one-line label cannot wrap: the
                // counts take the tile's width and the button the line under them, at the end.
                counts();
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    ip_check();
                });
            }
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
    let width_dp = crate::ui::window_size::current_window_size().width_dp;
    let window = remember(|| mutable_state_of(14u32));

    // Period Seg: 7일 / 14일
    const PERIOD_OPTIONS: [(u32, &'static str); 2] = [(7, "7일"), (14, "14일")];
    let pick = window.clone();
    SegGroup(&PERIOD_OPTIONS, window.get(), move |v| pick.set(v));
    let m = usage::section_model(load, &crate::ui::home::today_utc(), window.get() as usize);
    // Cards follow 16 apart: the section column's gap, no spacer on top (review AG9).

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
            let day_bars = || {
                cu::Box().modifier(Modifier::new().width(USAGE_BARS_W).height(150.0)).content(|| {
                    let bars_draw = draw_usage_bars(p, &m.bars);
                    cu::Canvas(bars_draw).modifier(Modifier::new().size(USAGE_BARS_W, 150.0));
                });
            };
            if compact {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(figures);
            } else if usage_hero_side_by_side(width_dp) {
                cu::Row().alignment(Alignment::CenterEnd).spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().width(USAGE_FIGURES_W)).content(figures);
                    day_bars();
                });
            } else {
                // `.ag-hero{flex-wrap:wrap}`: the bars go under the figures when both do not fit;
                // side by side they were 780 dp in a 623 dp card at 1024 (review r7).
                cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().fill_max_width()).content(figures);
                    day_bars();
                });
            }
        });
    });

    // `.ag-grid2`: 토큰 종류 and 하네스별 토큰과 비용 side by side from 1280 dp, stacked below.
    let pair_cols = grid2_columns(compact_window_width(compact));
    let split_w = card_inner_width(width_dp, pair_cols);
    if pair_cols < 2 {
        TokenKindsCard(&m, split_w);
        HarnessCostCard(&m, bars);
    } else {
        cu::Row().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                TokenKindsCard(&m, split_w);
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                HarnessCostCard(&m, bars);
            });
        });
    }

    // Section Title & Accounts subscription cards: `.ag-h2` (15/600, 8 px margin on top), then
    // the card 16 below it.
    cu::Column().content(|| {
        cu::Spacer().modifier(Modifier::new().height(SP[1]));
        Label("계정별 남은 구독량", Fs::Lg, Weight::Semibold, None, Some(1));
    });

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

/// `split_w` is the card's content width: the split bar spans it (`.ag-split`), where a fixed
/// 409 dp bar overflowed a 1024 dp window's card (review r7).
fn TokenKindsCard(m: &usage::SectionModel, split_w: f32) {
    let p = ember_theme().palette;
    HeadedCard(
        move || {
            CardHeaderIcon(ICON_CHART_SVG);
            Label("토큰 종류", Fs::Lg, Weight::Semibold, None, Some(1));
        },
        move || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                let bar_w = split_w;
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
        },
    );
}

fn HarnessCostCard(m: &usage::SectionModel, bars: &[crate::ui::home::HarnessBar]) {
    let p = ember_theme().palette;
    HeadedCard(
        || {
            CardHeaderIcon(ICON_CHART_SVG);
            Label("하네스별 토큰과 비용", Fs::Lg, Weight::Semibold, None, Some(1));
        },
        || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
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
        },
    );
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

    /// One node of a composition: widget, parent, modifiers (`"Weight(1.0)"`) and props
    /// (`"FontSize=Float(28.0)"`).
    struct Node {
        id: u32,
        widget: String,
        parent: Option<u32>,
        mods: Vec<String>,
        props: Vec<String>,
    }

    fn nodes_of(content: fn()) -> Vec<Node> {
        use compose_rust::protocol::{decode_batch, Mutation};
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut nodes: Vec<Node> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, widget } => nodes.push(Node {
                    id: *node_id,
                    widget: format!("{widget:?}"),
                    parent: None,
                    mods: Vec::new(),
                    props: Vec::new(),
                }),
                Mutation::Insert { parent_id, node_id, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.parent = Some(*parent_id);
                    }
                }
                Mutation::SetModifier { node_id, modifier, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.mods.push(format!("{modifier:?}"));
                    }
                }
                Mutation::SetProp { node_id, property, value } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.props.push(format!("{property:?}={value:?}"));
                    }
                }
                _ => {}
            }
        }
        nodes
    }

    fn text_node<'a>(nodes: &'a [Node], text: &str) -> &'a Node {
        let want = format!("Text=String({text:?})");
        nodes.iter().find(|n| n.props.contains(&want)).unwrap_or_else(|| panic!("no text {text}"))
    }

    fn children<'a>(nodes: &'a [Node], parent: &Node) -> Vec<&'a Node> {
        nodes.iter().filter(|n| n.parent == Some(parent.id)).collect()
    }

    #[test]
    fn fr_l20_kpi_numbers_are_28_regular_and_words_18_semibold() {
        // Review AG5: every value drew about 18/600. `.kpi .v` is 28/400, 18 at phone width;
        // `.v.ag-word` (방금) is 18/600; a unit is a small grey pill.
        assert_eq!(kpi_value_type("4", false), (Fs::Xl2, Weight::Regular));
        assert_eq!(kpi_value_type("17 GB", false), (Fs::Xl2, Weight::Regular));
        assert_eq!(kpi_value_type("4", true), (Fs::Xl, Weight::Regular));
        assert_eq!(kpi_value_type("방금", false), (Fs::Xl, Weight::Semibold));
        assert_eq!(kpi_value_type("3분 전", false), (Fs::Xl, Weight::Semibold));
        assert_eq!(kpi_value_type(crate::ui::agents::NO_INFO, false), (Fs::Xl, Weight::Semibold));
        assert_eq!(kpi_split_unit("17 GB"), ("17", Some("GB")));
        assert_eq!(kpi_split_unit("512 MB"), ("512", Some("MB")));
        assert_eq!(kpi_split_unit("3분 전"), ("3분 전", None));
        assert_eq!(kpi_split_unit("4"), ("4", None));
        assert_eq!(Fs::Xl2.size(), 28.0);
        assert!((Fs::Xl2.letter_spacing() - (-0.56)).abs() < 1e-6, "-0.02em");

        let nodes = nodes_of(|| KpiItem("세션 메모리 한도 합", "17 GB", None, false));
        let value = text_node(&nodes, "17");
        assert!(value.props.iter().any(|p| p == "FontSize=Float(28.0)"), "{:?}", value.props);
        assert!(value.props.iter().any(|p| p == "FontWeight=Integer(400)"), "{:?}", value.props);
        let unit = text_node(&nodes, "GB");
        let pill = nodes.iter().find(|n| Some(n.id) == unit.parent).expect("the unit pill");
        assert!(pill.mods.iter().any(|m| m == "Height(20.0)"), "a small pill: {:?}", pill.mods);
        let word = nodes_of(|| KpiItem("마지막 감지", "방금", Some(("예시", PillTone::Orange)), false));
        let w = text_node(&word, "방금");
        assert!(w.props.iter().any(|p| p == "FontSize=Float(18.0)"), "{:?}", w.props);
        assert!(w.props.iter().any(|p| p == "FontWeight=Integer(600)"), "{:?}", w.props);
    }

    #[test]
    fn fr_l20_kpis_are_four_columns_two_up_to_1279_and_a_short_row_keeps_its_cells() {
        assert_eq!(kpi_grid(1440.0), (4, 24.0));
        assert_eq!(kpi_grid(1280.0), (4, 24.0));
        assert_eq!(kpi_grid(1279.0), (2, 16.0));
        assert_eq!(kpi_grid(768.0), (2, 16.0));
        assert_eq!(kpi_grid(390.0), (2, 12.0));
        assert_eq!((grid_fillers(3, 3), grid_fillers(2, 3), grid_fillers(1, 3), grid_fillers(4, 3)), (0, 1, 2, 0));
        let nodes = nodes_of(|| {
            let items = ["a", "b", "c", "d", "e"];
            FixedGrid(&items, 3, 12.0, |x| Label(x, Fs::Sm, Weight::Regular, None, None));
        });
        let rows: Vec<&Node> = nodes.iter().filter(|n| n.widget == "Row").collect();
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let kids = children(&nodes, row);
            assert_eq!(kids.len(), 3, "three cells in every row, the last one padded");
            assert!(kids.iter().all(|k| k.mods.iter().any(|m| m == "Weight(1.0)")), "equal cells");
        }
        let last = children(&nodes, rows[1]);
        assert_eq!(last.iter().filter(|k| k.widget == "Spacer").count(), 1);
        let grid = nodes.iter().find(|n| n.widget == "Column" && Some(n.id) == rows[0].parent).expect("grid column");
        assert!(grid.props.iter().any(|p| p.starts_with("Spacing=") && p.contains("12.0")), "{:?}", grid.props);
    }

    #[test]
    fn fr_l20_account_ring_and_bar_tracks_are_the_track_token() {
        // Review AG7: the ring's track was `surface-2` on a `surface-2` tile, so it vanished.
        for p in [&LIGHT, &DARK] {
            let want = DrawList::builder()
                .circle(paint(p.track), 36.0, 36.0, 33.0, 6.0)
                .arc(paint(p.green.strong), 36.0, 36.0, 33.0, -90.0, 360.0 * 38.0 / 100.0, 6.0)
                .build();
            assert!(draw_ring(Some(38), 72.0, 6.0, p) == want, "the ring draws the track circle, then the arc");
            let unknown = DrawList::builder().circle(paint(p.track), 36.0, 36.0, 33.0, 6.0).build();
            assert!(draw_ring(None, 72.0, 6.0, p) == unknown, "an unknown share is the bare track");
            assert_ne!(p.track, p.surface_2);
        }
        assert_eq!((LIGHT.track, DARK.track), (Color::rgb(0xecebe6), Color::rgb(0x2f2e2b)));
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        let tile = production.split("fn AccountTile(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(tile.contains(".round_rect(paint(p.track), 0.0, 0.0, 115.0, 6.0, 3.0, 0.0)"), "{tile}");
    }

    #[test]
    fn fr_l20_cards_are_16_apart_and_tile_grids_keep_their_cells() {
        // Review AG8, AG9: weighted tiles in `chunks(3)` rows stretched a short last row, and
        // spacers on top of the 20 px column gap left 58 px between cards (mockup 16-18).
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        let screen = production.split("pub fn AgentsScreenWith(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert_eq!(screen.matches(".spacing(GAP_CARD)").count(), 2, "both layouts: {screen}");
        assert!(!screen.contains(".spacing(20.0)"));
        assert_eq!(GAP_CARD, 16.0);
        for f in ["fn ProfilesSection(", "fn NetworkSection(", "fn UsageSection("] {
            let body = production.split(f).nth(1).unwrap().split("\nfn ").next().unwrap();
            assert!(!body.contains(".chunks(3)"), "{f}");
            assert!(!body.contains("cu::Spacer().modifier(Modifier::new().height(SP[3]));"), "{f}: no spacer between cards");
        }
        for f in ["fn ProfilesSection(", "fn NetworkSection("] {
            let body = production.split(f).nth(1).unwrap().split("\nfn ").next().unwrap();
            // The account tiles moved to `FixedGridRows` (one height a row, #886).
            assert!(body.contains("FixedGrid(") || body.contains("FixedGridRows("), "{f}");
        }
    }

    fn ancestors(nodes: &[Node], n: &Node) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = n.parent;
        while let Some(id) = cur {
            out.push(id);
            cur = nodes.iter().find(|x| x.id == id).and_then(|x| x.parent);
        }
        out
    }

    #[test]
    fn fr_l20_tile_grids_take_the_mockup_columns_for_the_card_width() {
        // Review r7: three fixed columns left gpu-box's tile 290 dp, IP 확인 fell out, and at
        // 1024 the 3-column grids were wider than the body and pushed it over the section nav.
        assert_eq!(auto_fill_columns(877.0, 240.0, 12.0), 3);
        assert_eq!(auto_fill_columns(100.0, 240.0, 12.0), 1, "never fewer than one");
        assert_eq!(card_inner_width(1440.0, 1), 877.0);
        assert_eq!(card_inner_width(1024.0, 1), 623.0);
        let cols = |min: f32| [1920.0, 1440.0, 1280.0, 1024.0, 901.0, 900.0, 561.0, 560.0, 390.0].map(|w| tile_columns(w, min));
        // `.ag-ctiles{minmax(240px,1fr)}`, one column up to 560.
        assert_eq!(cols(COMPUTER_TILE_MIN), [5, 3, 2, 2, 2, 3, 1, 1, 1]);
        // Accounts: two a row at 1440 and 1280 so the name and its plan pill keep their text.
        assert_eq!(cols(ACCOUNT_TILE_MIN), [4, 2, 2, 1, 1, 2, 1, 1, 1]);
        for w in [1920.0, 1440.0, 1280.0, 1024.0, 901.0, 900.0, 600.0] {
            for min in [COMPUTER_TILE_MIN, ACCOUNT_TILE_MIN] {
                let n = tile_columns(w, min) as f32;
                let cell = (card_inner_width(w, 1) - SP[2] * (n - 1.0)) / n;
                assert!(n == 1.0 || cell >= min, "{w}: {n} cells of {cell}");
            }
        }
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        for (f, min) in [("fn ProfilesSection(", "ACCOUNT_TILE_MIN"), ("fn NetworkSection(", "COMPUTER_TILE_MIN")] {
            let body = production.split(f).nth(1).unwrap().split("\nfn ").next().unwrap();
            assert!(body.contains(&format!("tile_columns(crate::ui::window_size::current_window_size().width_dp, {min})")), "{f}");
            assert!(!body.contains("FixedGrid(&live.accounts, 3") && !body.contains("FixedGrid(computers, 3"), "{f}");
        }
    }

    #[test]
    fn fr_l20_ip_check_keeps_its_width_and_the_counts_wrap_beside_it() {
        // gpu-box on the live path: both counts read 아직 정보가 없습니다.
        let nodes = nodes_of(|| ComputerTile("gpu-box", Reachability::Unknown, false));
        let flow = nodes.iter().find(|n| n.widget == "FlowRow").expect("the counts wrap");
        let holder = nodes.iter().find(|n| Some(n.id) == flow.parent).unwrap();
        assert!(holder.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", holder.mods);
        assert!(holder.mods.iter().any(|m| m == "Width(0.0)"), "{:?}", holder.mods);
        let ip = text_node(&nodes, "IP 확인");
        let ip_up = ancestors(&nodes, ip);
        assert!(!ip_up.contains(&holder.id), "the button is outside the weighted counts");
        let line = holder.parent.unwrap();
        assert!(ip_up.contains(&line), "the button shares the counts' line");
        for unit in [" 세션", " 열린 연결"] {
            assert!(ancestors(&nodes, text_node(&nodes, unit)).contains(&flow.id), "{unit}");
        }
    }

    #[test]
    fn fr_l20_a_narrow_computer_tile_puts_ip_check_under_counts_that_are_sentences() {
        // #886: at 900 gpu-box's second count (아직 정보가 없습니다 열린 연결) ended in an ellipsis
        // beside the button. Three 244 dp tiles leave 136 dp beside IP 확인.
        assert_eq!(tile_columns(900.0, COMPUTER_TILE_MIN), 3);
        assert_eq!(tile_inner_width(900.0, 3), (900.0 - RAIL_W - 2.0 * SP[3] - 2.0 * PAD_CARD_H - 2.0 * SP[2]) / 3.0 - 32.0);
        assert_eq!(IP_CHECK_ROOM, 76.0);
        let unknown = [(crate::ui::agents::NO_INFO, " 세션"), (crate::ui::agents::NO_INFO, " 열린 연결")];
        let numbers = [("7", " 세션"), ("12", " 열린 연결")];
        assert!(!counts_beside_ip_check(900.0, &unknown));
        assert!(counts_beside_ip_check(900.0, &numbers), "numbers stay beside the button");
        assert!(counts_beside_ip_check(390.0, &unknown), "one 286 dp column holds both");
        assert!(counts_beside_ip_check(0.0, &unknown), "an unreported width is wide");
        for w in [1920.0, 1440.0, 1280.0, 1024.0, 900.0, 600.0, 390.0] {
            // Under the button's line the counts have the whole tile: each fits it.
            let widest = crate::ui::cr::home::text_px(crate::ui::agents::NO_INFO, 13.0) + crate::ui::cr::home::text_px(" 열린 연결", 12.0);
            assert!(widest <= tile_inner_width(w, tile_columns(w, COMPUTER_TILE_MIN)), "{w}");
        }
        crate::ui::window_size::set_opened_for_test(Some((900.0, 800.0)));
        let nodes = nodes_of(|| ComputerTile("gpu-box", Reachability::Unknown, false));
        crate::ui::window_size::set_opened_for_test(None);
        let flow = nodes.iter().find(|n| n.widget == "FlowRow").expect("the counts wrap");
        assert!(flow.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", flow.mods);
        let holder = nodes.iter().find(|n| Some(n.id) == flow.parent).unwrap();
        assert!(!holder.mods.iter().any(|m| m == "Width(0.0)"), "the counts take the tile's width: {:?}", holder.mods);
        let ip_up = ancestors(&nodes, text_node(&nodes, "IP 확인"));
        assert!(ip_up.contains(&holder.id), "the button is in the same tile column");
        let line = nodes.iter().find(|n| ip_up.contains(&n.id) && n.parent == Some(holder.id)).expect("the button's line");
        assert_eq!(line.widget, "Row");
        assert_ne!(line.id, flow.id);
        assert!(line.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", line.mods);
        let lead = children(&nodes, line).into_iter().find(|n| n.widget == "Spacer").expect("a spacer pushes the button to the end");
        assert!(lead.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", lead.mods);
        for unit in [" 세션", " 열린 연결"] {
            let unit_node = text_node(&nodes, unit);
            assert!(ancestors(&nodes, unit_node).contains(&flow.id), "{unit}");
            assert!(unit_node.props.iter().any(|p| p == "MaxLines=Integer(1)"), "{:?}", unit_node.props);
        }
    }

    fn headed_probe() {
        HeadedCard(
            || {
                CardHeaderIcon(ICON_USER_SVG);
                Label("HEAD", Fs::Lg, Weight::Semibold, None, Some(1));
            },
            || Label("BODY", Fs::Sm, Weight::Regular, None, None),
        );
    }

    #[test]
    fn fr_l20_a_card_has_a_header_row_with_an_icon_tile_and_a_rule_across_the_card() {
        // Review r7 and #886: the cards drew their title as the first body line, with no rule.
        // `.card-h`: 16 20 padding, at least 56 high, a 1 px line under it; `.ico` 30 px.
        assert_eq!(CARD_HEAD_CONTENT_MIN_H, 24.0);
        let nodes = nodes_of(headed_probe);
        let p = ember_theme().palette;
        let line = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.line)));
        let rules: Vec<&Node> = nodes
            .iter()
            .filter(|n| n.widget == "Spacer" && n.mods.contains(&line) && n.mods.iter().any(|m| m == "Height(1.0)"))
            .collect();
        assert_eq!(rules.len(), 1, "one rule under the header");
        let rule = rules[0];
        assert!(rule.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", rule.mods);
        let stack = nodes.iter().find(|n| Some(n.id) == rule.parent).unwrap();
        assert_eq!(stack.widget, "Column");
        let (head, body) = (text_node(&nodes, "HEAD"), text_node(&nodes, "BODY"));
        let (head_up, body_up) = (ancestors(&nodes, head), ancestors(&nodes, body));
        assert!(head_up.contains(&stack.id) && body_up.contains(&stack.id));
        // The rule is not inside either padded part: it spans the card.
        let part = |up: &[u32]| nodes.iter().find(|n| up.contains(&n.id) && n.parent == Some(stack.id)).map(|n| n.id).unwrap();
        assert_ne!(part(&head_up), part(&body_up), "header and body are separate rows");
        // The card itself has no padding, so the rule reaches both borders.
        let rule_up = ancestors(&nodes, rule);
        let card = nodes
            .iter()
            .find(|n| rule_up.contains(&n.id) && n.widget == "Box" && n.mods.iter().any(|m| m.starts_with("Border")))
            .expect("the card");
        assert!(card.mods.iter().any(|m| m == "Padding(0.0)"), "{:?}", card.mods);
        // The header row: 20 at each side, at least 24 px tall (`SizeIn` `min_h`), then the children 12 apart.
        let head_row = nodes.iter().find(|n| Some(n.id) == head.parent).unwrap();
        assert_eq!(head_row.widget, "Row");
        assert!(head_row.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", head_row.mods);
        assert!(head_row.mods.iter().any(|m| m.contains("min_height: 24.0")), "the minimum height on the header row: {:?}", head_row.mods);
        let outer = nodes.iter().find(|n| Some(n.id) == head_row.parent).unwrap();
        let spacers: Vec<&Node> = children(&nodes, outer).into_iter().filter(|n| n.widget == "Spacer").collect();
        assert_eq!(spacers.iter().filter(|n| n.mods.iter().any(|m| m == "Width(20.0)")).count(), 2, "20 px sides");
        assert!(!spacers.iter().any(|n| n.mods.iter().any(|m| m == "Height(24.0)")), "no spacer strut");
        let tile = children(&nodes, head_row).into_iter().find(|n| n.widget == "Box").expect("the icon tile");
        for want in ["Width(30.0)", "Height(30.0)"] {
            assert!(tile.mods.iter().any(|m| m == want), "{want}: {:?}", tile.mods);
        }
        assert!(tile.mods.iter().any(|m| m.starts_with("Border")), "{:?}", tile.mods);
        // Every card with a title goes through it, and the four that had no tile have one.
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        assert_eq!(production.matches("    HeadedCard(\n").count(), 10, "harness, 새 팀원 기본값, model, 계정, 하네스별 기본 프로필, profile, 2 network, 2 usage");
        for (icon, title) in [
            ("super::shell::ICON_PLUS_SVG", "새 팀원 기본값"),
            ("ICON_USER_SVG", "계정"),
            ("super::shell::ICON_CPU_SVG", "하네스별 기본 프로필"),
            ("ICON_GLOBE_SVG", "API 트래픽 내보내기"),
            ("ICON_PC_SVG", "컴퓨터별 상태"),
            ("ICON_CHART_SVG", "토큰 종류"),
            ("ICON_CHART_SVG", "하네스별 토큰과 비용"),
        ] {
            let want = format!("CardHeaderIcon({icon});\n            Label(\"{title}\", Fs::Lg, Weight::Semibold, None, Some(1));");
            assert!(production.contains(&want), "{title}");
        }
        // The first row under a header has no rule of its own (`.ag-row:first-child`).
        let first = nodes_of(|| SettingRowFirst("제목", "설명", || Label("CONTROL", Fs::Sm, Weight::Regular, None, None)));
        assert!(!first.iter().any(|n| n.widget == "Spacer" && n.mods.contains(&format!("{:?}", compose_rust::schema::Modifier::Background(paint(ember_theme().palette.line))))));
        let later = nodes_of(|| SettingRowAt(false, "제목", "설명", || Label("CONTROL", Fs::Sm, Weight::Regular, None, None)));
        assert!(later.iter().any(|n| n.widget == "Spacer" && n.mods.iter().any(|m| m == "Height(1.0)")));
    }

    fn grid_probe() {
        let sample = sample_live();
        FixedGridRows(&sample.accounts[..2], 2, SP[2], true, |a| AccountTile(a, if a.limited { Some("한도 가까움") } else { None }, true, true));
    }

    fn row_props(equal: bool) -> Vec<String> {
        fn on() {
            cu::Row().alignment(Alignment::TopStart).spacing(12.0).equal_height(true).content(|| {});
        }
        fn off() {
            cu::Row().alignment(Alignment::TopStart).spacing(12.0).equal_height(false).content(|| {});
        }
        let nodes = nodes_of(if equal { on as fn() } else { off as fn() });
        nodes.iter().find(|n| n.widget == "Row").expect("row").props.clone()
    }

    #[test]
    fn fr_l20_account_tiles_in_a_row_are_one_height() {
        // Review r7 and #886: 88 / 88 / 98, the tile with 한도 가까움 taller than its neighbours.
        assert!(!grid_rows_equal_height(1));
        assert!(grid_rows_equal_height(2));
        let (on, off) = (row_props(true), row_props(false));
        let marks: Vec<&String> = on.iter().filter(|p| !off.contains(p)).collect();
        assert!(!marks.is_empty(), "equal_height is sent: {on:?} against {off:?}");
        let nodes = nodes_of(grid_probe);
        let row = nodes
            .iter()
            .find(|n| n.widget == "Row" && marks.iter().all(|p| n.props.contains(p)))
            .expect("the grid row is equal_height");
        let cells = children(&nodes, row);
        assert_eq!(cells.len(), 2);
        for cell in cells {
            assert_eq!(cell.widget, "Column");
            for want in ["Weight(1.0)", "FillMaxHeight"] {
                assert!(cell.mods.iter().any(|m| m == want), "{want}: {:?}", cell.mods);
            }
            let tile = children(&nodes, cell);
            assert_eq!(tile.len(), 1);
            assert_eq!(tile[0].widget, "Box");
            assert!(tile[0].mods.iter().any(|m| m == "FillMaxHeight"), "the tile fills its cell: {:?}", tile[0].mods);
            assert!(tile[0].mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", tile[0].mods);
        }
        // The computers grid keeps its own heights (its counts wrap; see FixedGrid).
        let plain = nodes_of(|| FixedGrid(&[1, 2], 2, 12.0, |_| Label("T", Fs::Sm, Weight::Regular, None, None)));
        assert!(!plain.iter().any(|n| n.mods.iter().any(|m| m == "FillMaxHeight")));
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        let profiles = production.split("fn ProfilesSection(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(profiles.contains("FixedGridRows(&live.accounts, cols, SP[2], equal, |a| {"), "{profiles}");
        assert!(profiles.contains("AccountTile(a, extra, live.sample, equal);"));
    }

    #[test]
    fn fr_l20_card_pairs_are_two_columns_from_1280_and_fit_their_column_below() {
        // `.ag-grid2` is one column up to 1279 px: at 1024 two model cards left the name about
        // 75 dp and the 기본 badge an empty pill; the usage pair and profile cards likewise.
        assert_eq!([1440.0, 1280.0, 1279.0, 1024.0, 900.0].map(grid2_columns), [2, 2, 1, 1, 1]);
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        for f in ["fn ModelsSection(", "fn ProfilesSection(", "fn UsageSection("] {
            let body = production.split(f).nth(1).unwrap().split("\nfn ").next().unwrap();
            assert!(body.contains("grid2_columns(compact_window_width(compact))"), "{f}");
        }
        // The usage hero puts the 440 dp bars under the figures when the card cannot hold both.
        assert!(usage_hero_side_by_side(1440.0));
        assert!(!usage_hero_side_by_side(1280.0));
        assert!(!usage_hero_side_by_side(1024.0));
        // The split bar spans its card: 410.5 dp at 1440 (two cards), 623 at 1024 (one).
        assert_eq!(card_inner_width(1440.0, grid2_columns(1440.0)), 410.5);
        assert_eq!(card_inner_width(1024.0, grid2_columns(1024.0)), 623.0);
        let usage = production.split("fn UsageSection(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(usage.contains("card_inner_width(width_dp, pair_cols)"));
        let kinds = production.split("fn TokenKindsCard(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(!kinds.contains("409.0") && !kinds.contains("326.0"), "no fixed bar width");
    }

    #[test]
    fn fr_l10_4_a_default_chip_says_whether_its_menu_is_open() {
        let _guard = crate::services::test_guard();
        let sem = |nodes: Vec<Node>| -> String {
            let button = nodes.iter().find(|n| n.widget == "Button").expect("the chip's button");
            button.mods.iter().find(|m| m.starts_with("Semantics")).cloned().unwrap_or_default()
        };
        let open = sem(nodes_of(|| MenuPickerPill("아직 정하지 않았습니다", Some(ICON_USER_SVG), Some(true), || {})));
        let closed = sem(nodes_of(|| MenuPickerPill("아직 정하지 않았습니다", Some(ICON_USER_SVG), Some(false), || {})));
        let plain = sem(nodes_of(|| ModelPickerPill("아직 정하지 않았습니다", Some(ICON_USER_SVG), || {})));
        assert!(!open.is_empty() && !closed.is_empty(), "{open} {closed}");
        assert_ne!(open, closed);
        assert_ne!(closed, plain, "a chip that opens no menu says neither");
    }

    #[test]
    fn section_rows_are_the_named_controls_and_data_named_chips_keep_a_matched_button() {
        // compose-rust#429, #431 (#673): the screen's own overlay functions are gone. A section
        // row or pill is the control itself; a chip or radio named from data (a profile, a model)
        // keeps `components::DataNamedOverlay`, which takes its box's bounds and no more.
        let _guard = crate::services::test_guard();
        for shape in [NavShape::Side, NavShape::Pills] {
            let nodes = match shape {
                NavShape::Side => nodes_of(|| SectionNav(Section::Harness, NavShape::Side, |_| {})),
                NavShape::Pills => nodes_of(|| SectionNav(Section::Harness, NavShape::Pills, |_| {})),
            };
            assert!(!nodes.iter().any(|n| n.widget == "Button"), "{shape:?}: no button over a row");
            for x in Section::ALL {
                let want = format!("AccessibleName=String({:?})", x.label());
                let row = nodes.iter().find(|n| n.props.contains(&want)).unwrap_or_else(|| panic!("{shape:?}: {}", x.label()));
                assert_eq!(row.widget, "Box");
                assert!(row.props.iter().any(|p| p.starts_with("OnClick=")), "{:?}", row.props);
                assert!(row.mods.iter().any(|m| m.starts_with("Semantics") && m.contains("Button")), "{:?}", row.mods);
                assert_eq!(row.mods.iter().filter(|m| m.starts_with("PressFeedback")).count(), 1, "{:?}", row.mods);
                assert!(row.mods.iter().any(|m| m.starts_with("Background")), "the painted box takes the press: {:?}", row.mods);
            }
        }
        let radio = nodes_of(|| ModelRadio("Opus 기본 모델로 선택", true, || {}));
        let over = radio.iter().find(|n| n.widget == "Button").expect("the radio's button");
        assert!(over.props.iter().any(|p| p == "AccessibleName=String(\"Opus 기본 모델로 선택\")"), "{:?}", over.props);
        assert!(over.mods.iter().any(|m| m == "MatchParentSize") && !over.mods.iter().any(|m| m.starts_with("FillMax")), "{:?}", over.mods);
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains(&["Focus", "Overlay"].concat()), "no overlay function of its own");
        assert!(!production.contains(&["cu::", "Button("].concat()), "no direct toolkit button");
    }

    #[test]
    fn chips_and_picker_pills_send_no_width_of_their_own() {
        // compose-rust#431 (#673): the fixed 200 and the `text_px` estimate are gone; the box
        // sends a height and takes the width of its row of spacers and label.
        let _guard = crate::services::test_guard();
        for nodes in [
            nodes_of(|| SelectChip("자동", || {})),
            nodes_of(|| ModelPickerPill("아직 정하지 않았습니다", Some(ICON_USER_SVG), || {})),
            nodes_of(|| ModelPickerPill("+ 계정", None, || {})),
        ] {
            let over = nodes.iter().find(|n| n.widget == "Button").expect("the chip's button");
            let chip = nodes.iter().find(|n| Some(n.id) == over.parent).expect("the chip");
            assert!(chip.mods.iter().any(|m| *m == format!("Height({H_CTL:?})")), "{:?}", chip.mods);
            assert!(!chip.mods.iter().any(|m| m.starts_with("Width(") || m.starts_with("Size(") || m.starts_with("FillMax")), "{:?}", chip.mods);
            assert!(over.mods.iter().any(|m| m == "MatchParentSize"), "{:?}", over.mods);
        }
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        for f in ["fn SelectChip(", "fn MenuPickerPill("] {
            let body = production.split(f).nth(1).unwrap().split("\nfn ").next().unwrap();
            assert!(!body.contains(".width(200.0)") && !body.contains("text_px("), "{f}");
        }
    }

    #[test]
    fn fr_l10_4_new_member_default_chips_open_popovers_under_them() {
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        let body = production.split("fn ModelsSection(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert_eq!(body.matches("PopoverMenu(").count(), 2, "the harness and the account chip");
        assert!(body.contains("MODEL_PICKER") && body.contains("ACCOUNT_PICKER"));
        assert!(body.contains("|| MenuPickerPill(&harness_pill, Some(ICON_SPARK_SVG), Some(pick_is_open)") && body.contains("|| MenuPickerPill(&account_pill, Some(ICON_USER_SVG), Some(acct_is_open)"), "each chip is its menu's anchor and says whether it is open");
        assert!(!production.contains("OptionSheet("));
    }

    #[test]
    fn fr_l20_formula_chips_wrap_and_keep_each_operator_with_its_chip() {
        let nodes = nodes_of(|| {
            FormulaFlow(
                &[(None, "계정 묶음", false), (None, "전환 규칙", false), (None, "프로필", false)],
                &["+", "="],
                Fs::Lg,
            )
        });
        let flow = nodes.iter().find(|n| n.widget == "FlowRow").expect("a wrapping row");
        assert!(flow.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", flow.mods);
        let units: Vec<&Node> = nodes.iter().filter(|n| n.parent == Some(flow.id)).collect();
        assert_eq!(units.len(), 3, "one unit a chip");
        let plus = text_node(&nodes, "+");
        let chip = text_node(&nodes, "계정 묶음");
        assert!(ancestors(&nodes, plus).contains(&units[0].id) && ancestors(&nodes, chip).contains(&units[0].id));
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        for f in ["fn ProfilesSection(", "fn NetworkSection("] {
            let body = production.split(f).nth(1).unwrap().split("\nfn ").next().unwrap();
            assert!(body.contains("FormulaFlow("), "{f}");
            assert!(!body.contains("FormulaChip("), "{f}: every chip goes through the wrapping row");
        }
    }

    #[test]
    fn fr_l16_model_table_header_is_one_line_and_plans_run_from_the_smaller_plan() {
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let nodes = nodes_of(|| {
            let st = remember(|| mutable_state_of(LocalStore::default()));
            ModelCard(AgentHarness::ClaudeCode, &st, false, true);
        });
        crate::ui::window_size::set_opened_for_test(None);
        // SPEC FR-L20 keeps the longer wording; it reads on one line over the plan pills.
        let head = text_node(&nodes, MODEL_PLANS_HEADER);
        assert!(head.props.iter().any(|p| p == "MaxLines=Integer(1)"), "{:?}", head.props);
        let holder = nodes.iter().find(|n| Some(n.id) == head.parent).unwrap();
        assert_eq!(holder.widget, "Row", "the header is not squeezed into a fixed box");
        assert!(!holder.mods.iter().any(|m| m == "Width(120.0)"));
        // The mockup's `plans` order: Pro, then Max.
        let order: Vec<&str> = nodes
            .iter()
            .filter_map(|n| n.props.iter().find(|p| p.contains("✓ Pro") || p.contains("✓ Max")))
            .map(|p| if p.contains("Pro") { "Pro" } else { "Max" })
            .take(2)
            .collect();
        assert_eq!(order, ["Pro", "Max"]);
        // The grid's 12 dp column gap (`.ag-mrow{gap:var(--sp3)}`) and the header shown above 560.
        assert!(holder.props.iter().any(|p| p.starts_with("Spacing=") && p.contains("12.0")), "{:?}", holder.props);
        assert!(model_table_phone(560.0) && !model_table_phone(561.0));
    }

    #[test]
    fn fr_l20_model_switch_column_is_the_mockups_36() {
        // #886: `.ag-mrow` ends in a 36 px column; ours was 60. The header wraps in it.
        assert_eq!(MODEL_SWITCH_COL, 36.0);
        let src = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        assert_eq!(src.matches("Modifier::new().width(MODEL_SWITCH_COL)").count(), 2, "header and rows share the column");
        assert!(src.contains("Label(MODEL_SWITCH_HEADER, Fs::Xs2, Weight::Medium, Some(paint(p.muted)), Some(2))"));
        assert!(super::super::home::text_px("목록에", Fs::Xs2.size()) <= MODEL_SWITCH_COL, "the first line fits the column");
    }

    #[test]
    fn fr_l20_model_names_and_notes_wrap_at_1280_instead_of_ending_in_an_ellipsis() {
        // #886, review ba: at 1280 two model cards share a row and the names and notes ended in
        // "..." (`GPT-5 Cod...`); the mockup's `.ag-mn` column wraps them.
        crate::ui::window_size::set_opened_for_test(Some((1280.0, 800.0)));
        let nodes = nodes_of(|| {
            let st = remember(|| mutable_state_of(LocalStore::default()));
            ModelCard(AgentHarness::Codex, &st, false, true);
        });
        crate::ui::window_size::set_opened_for_test(None);
        assert_eq!(MODEL_TEXT_LINES, 2);
        // GPT-5 Codex is not the default, so its name is a plain label.
        for t in ["GPT-5 Codex", "긴 코딩 작업에 맞춥니다"] {
            let n = text_node(&nodes, t);
            assert!(n.props.iter().any(|p| p == "MaxLines=Integer(2)"), "{t}: {:?}", n.props);
        }
        // The default model's name and its 기본 badge go through NameThenPillLines (two lines), so the badge keeps
        // its own width right after the name.
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("NameThenPillLines(m.name, Fs::Md, Weight::Medium, None, false, MODEL_TEXT_LINES, SP[1], || Pill(\"기본\", PillTone::Blue, true));"));
        assert!(production.contains("Label(m.note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(MODEL_TEXT_LINES));"));
    }

    #[test]
    fn fr_l20_the_plan_pill_keeps_its_text_beside_a_long_account_name() {
        // Review AG16 and r6: "Claude Max (work)" drew its Max pill as an empty grey circle.
        assert_eq!(account_plan("Claude Max (work)"), Some("Max"));
        assert_eq!(account_plan("Codex Plus"), Some("Plus"));
        assert_eq!(account_plan("ncmnd"), None);
        let nodes = nodes_of(|| AccountNameLine(&AccountInfo {
            id: "a".into(),
            label: "Claude Max (work) 아주 긴 이름의 계정".into(),
            harness: AgentHarness::ClaudeCode,
            left_percent: Some(38),
            limited: false,
        }));
        let name = text_node(&nodes, "Claude Max (work) 아주 긴 이름의 계정");
        let holder = nodes.iter().find(|n| Some(n.id) == name.parent).unwrap();
        assert_eq!(holder.widget, "Column");
        assert!(holder.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", holder.mods);
        assert!(holder.mods.iter().any(|m| m == "Width(0.0)"), "a zero-width weighted column: {:?}", holder.mods);
        assert!(name.props.iter().any(|p| p == "MaxLines=Integer(1)"), "{:?}", name.props);
        let plan = text_node(&nodes, "Max");
        let pill = nodes.iter().find(|n| Some(n.id) == plan.parent).unwrap();
        assert!(!pill.mods.iter().any(|m| m.starts_with("Weight")), "the pill keeps its own width");
        let line = nodes.iter().find(|n| Some(n.id) == holder.parent).unwrap();
        assert_eq!(Some(line.id), pill.parent, "name and pill share the line");
        assert!(line.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", line.mods);
    }

    #[test]
    fn fr_l20_below_560_a_setting_row_puts_its_control_under_the_text() {
        assert!(setting_row_stacked(390.0));
        assert!(setting_row_stacked(560.0));
        assert!(!setting_row_stacked(561.0));
        assert!(!setting_row_stacked(1440.0));
        let probe = |nodes: &[Node]| {
            let title = text_node(nodes, "추론 강도");
            let text_col = nodes.iter().find(|n| Some(n.id) == title.parent).unwrap();
            let line = nodes.iter().find(|n| Some(n.id) == text_col.parent).unwrap();
            let control = text_node(nodes, "CONTROL");
            (line.widget.clone(), Some(line.id) == control.parent, text_col.mods.clone())
        };
        let stacked = nodes_of(|| SettingRowAt(true, "추론 강도", "다음 세션부터 적용합니다", || Label("CONTROL", Fs::Sm, Weight::Regular, None, None)));
        let (widget, same, mods) = probe(&stacked);
        assert_eq!(widget, "Column", "the control goes under the text");
        assert!(same);
        assert!(mods.iter().any(|m| m == "FillMaxWidth"), "the text takes the whole line: {mods:?}");
        let beside = nodes_of(|| SettingRowAt(false, "추론 강도", "다음 세션부터 적용합니다", || Label("CONTROL", Fs::Sm, Weight::Regular, None, None)));
        let (widget, same, mods) = probe(&beside);
        assert_eq!(widget, "Row");
        assert!(same);
        assert!(mods.iter().any(|m| m == "Weight(1.0)"), "{mods:?}");
    }

    #[test]
    fn fr_l20_page_title_is_the_book_weight() {
        // Review AG6: `.head h1` is 32/400; the screen drew Semibold.
        let production = include_str!("agents.rs").split("#[cfg(test)]").next().unwrap();
        let title = production.split("\"에이전트\",").nth(1).unwrap();
        let title = &title[..title.find(");").unwrap()];
        assert!(title.contains("PAGE_TITLE_WEIGHT"), "{title}");
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
                safe_area_bottom: 0.0,
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
                safe_area_bottom: 0.0,
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
