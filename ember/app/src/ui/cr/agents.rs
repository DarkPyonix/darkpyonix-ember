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
    Label, Pill, PillTone,
};
use super::theme::*;
use crate::server::{Account, ChatGptAccount};
use crate::ui::home::UNAVAILABLE;

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
    let is_sample = std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("agents");
    let (live, computers, bars, week_tokens) = if is_sample {
        (sample_live(), sample_computers(), sample_bars(), 8_765_432u64)
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
        };
        let computers = s.client.read(|st| {
            st.computers()
                .iter()
                .map(|c| (c.name.clone(), c.reachability))
                .collect()
        });
        let bars = s.client.read(|st| crate::ui::home::harness_usage(st));
        let ids: Vec<String> = chatgpt.iter().map(|a| a.id.clone()).collect();
        let usage_raw = s.ui.live.usage.get();
        let week: u64 = crate::ui::home::chatgpt_token_days(
            &usage_raw,
            &ids,
            &crate::ui::home::today_utc(),
            7,
        )
        .iter()
        .map(|(_, t)| *t)
        .sum();
        (live, computers, bars, week)
    } else {
        (AgentsLive::default(), Vec::new(), Vec::new(), 0)
    };

    let initial_section = std::env::var("EMBER_CR_AGENTS_SECTION")
        .ok()
        .map(|sec| resolve_section(&sec))
        .unwrap_or(Section::Harness);

    AgentsScreenWith(live, computers, bars, week_tokens, initial_section);
}

pub fn AgentsScreenWith(
    live: AgentsLive,
    computers: Vec<(String, Reachability)>,
    bars: Vec<crate::ui::home::HarnessBar>,
    week_tokens: u64,
    initial_section: Section,
) {
    let p = ember_theme().palette;
    let section = remember(move || mutable_state_of(initial_section));
    let store = remember(|| mutable_state_of(local_load()));
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
                                    Section::Models => ModelsSection(&store, compact),
                                    Section::Profiles => ProfilesSection(&store, &live, compact),
                                    Section::Network => NetworkSection(&computers, compact),
                                    Section::Usage => UsageSection(&live, &bars, week_tokens, compact),
                                }
                            });
                    } else {
                        // Desktop: 200px Nav on the left + 917px Body on the right (ends at 1408)
                        cu::Row()
                            .spacing(25.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                cu::Column().modifier(Modifier::new().width(200.0)).content(|| {
                                    SectionNav(cur_sec, NavShape::Side, nav_pick);
                                });
                                cu::Column()
                                    .spacing(20.0)
                                    .modifier(Modifier::new().width(917.0))
                                    .content(|| {
                                        match cur_sec {
                                            Section::Harness => HarnessSection(&store, &live, compact),
                                            Section::Models => ModelsSection(&store, compact),
                                            Section::Profiles => ProfilesSection(&store, &live, compact),
                                            Section::Network => NetworkSection(&computers, compact),
                                            Section::Usage => UsageSection(&live, &bars, week_tokens, compact),
                                        }
                                    });
                            });
                    }
                });
        });
}

/// A review window (`EMBER_CR_ROUTE=agents`).
#[compose_rust::runtime::composable]
pub fn preview() {
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

/// A 36x20 toggle switch matching the mockup (ink track when on, track when off, 16x16 knob).
///
/// TODO(compose-rust#372): toolkit Switch cannot be styled (ink track 36x20 not toolkit green 54x32),
/// so it is built from a toolkit Button with painted track and knob.
fn MockSwitch(label: &str, checked: bool, enabled: bool, mut on_change: impl FnMut(bool) + 'static) {
    let p = ember_theme().palette;
    let track_color = if !enabled {
        p.line
    } else if checked {
        p.ink
    } else {
        p.surface_2
    };
    let knob_color = p.surface;
    let knob_x = if checked { 26.0 } else { 10.0 };

    let mut list = DrawList::builder()
        .round_rect(paint(track_color), 0.0, 0.0, 36.0, 20.0, 10.0, 0.0)
        .circle(paint(knob_color), knob_x, 10.0, 7.5, 0.0);
    if !checked && enabled {
        list = list.round_rect(paint(p.line), 0.0, 0.0, 36.0, 20.0, 10.0, 1.0);
    }
    let draw = list.build();
    let switch_label = format!("{label} {}", if checked { "끄기" } else { "켜기" });

    cu::Box()
        .modifier(Modifier::new().size(36.0, 20.0))
        .content(move || {
            cu::Canvas(draw).modifier(Modifier::new().size(36.0, 20.0));
            FocusOverlayEnabled(&switch_label, enabled, move || on_change(!checked));
        });
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

/// An accessible invisible button overlay for a painted row, chip, or pill.
/// Gives assistive tech the item's label and provides keyboard focus and click handling.
fn FocusOverlay(label: &str, on_click: impl FnMut() + 'static) {
    FocusOverlayEnabled(label, true, on_click);
}

fn FocusOverlayEnabled(label: &str, enabled: bool, on_click: impl FnMut() + 'static) {
    cu::Button(label)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(compose_rust::Paint::Literal(Color::argb(0)))
        .modifier(Modifier::new().fill_max_size())
        .on_click(on_click);
}

/// A selectable chip matching the mockup's .field.ag-sel: wide enough for the label,
/// with a 1-line label and accessible FocusOverlay without ellipsis if it fits.
fn SelectChip(text: &str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(
            // A fixed width: without one the chip takes the whole row and squeezes the hint
            // beside it to one glyph per line.
            Modifier::new()
                .width(200.0)
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

/// A picker pill matching mockup's .pickchip: full pill shape, icon + label with available width.
fn ModelPickerPill(text: &str, svg: Option<&'static str>, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    // Sized to its content: a pill without a width takes the rest of a row and squeezes the
    // hint beside it to one glyph per line.
    let width = 12.0
        + if svg.is_some() { 16.0 + 8.0 } else { 0.0 }
        + super::home::text_px(text, Fs::Sm.size())
        + 12.0
        + 8.0;
    cu::Box()
        .modifier(
            Modifier::new()
                .width(width.max(72.0))
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
        Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
        cu::Spacer().modifier(Modifier::new().height(SP[0]));
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
            Label(value, Fs::Xl, Weight::Semibold, None, None);
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

fn draw_donut(p: &Palette) -> DrawList {
    let half = 66.0;
    let stroke_width = 12.0;
    let radius = half - stroke_width / 2.0;
    let claude_col = Color::rgb(0xe49b55);
    let codex_col = Color::rgb(0x3f74d4);
    let gemini_col = Color::rgb(0x9dbde9);
    // Total $174: Claude $120 (69%), Codex $46 (26.4%), Gemini $8 (4.6%)
    let sweep1 = 360.0 * 120.0 / 174.0;
    let sweep2 = 360.0 * 46.0 / 174.0;
    let sweep3 = 360.0 * 8.0 / 174.0;
    DrawList::builder()
        .circle(paint(p.surface_2), half, half, radius, stroke_width)
        .arc(paint(claude_col), half, half, radius, -90.0, sweep1, stroke_width)
        .arc(paint(codex_col), half, half, radius, -90.0 + sweep1, sweep2, stroke_width)
        .arc(paint(gemini_col), half, half, radius, -90.0 + sweep1 + sweep2, sweep3, stroke_width)
        .build()
}

fn draw_usage_bars(p: &Palette) -> DrawList {
    const W: f32 = 440.0;
    const H: f32 = 130.0;
    let claude_col = Color::rgb(0xe49b55);
    let codex_col = Color::rgb(0x3f74d4);
    let gemini_col = Color::rgb(0x9dbde9);
    let sample_days: [(f32, f32, f32); 14] = [
        (2.1, 1.1, 0.3), (2.8, 1.3, 0.4), (2.4, 1.2, 0.3), (1.5, 0.8, 0.3),
        (1.9, 0.9, 0.3), (3.3, 1.5, 0.4), (2.6, 1.3, 0.4), (2.8, 1.2, 0.4),
        (3.3, 1.5, 0.5), (2.8, 1.3, 0.4), (3.2, 1.5, 0.5), (3.2, 1.5, 0.5),
        (3.2, 1.4, 0.5), (3.3, 1.5, 0.4),
    ];
    let max_tot = 5.5f32;
    let mut builder = DrawList::builder().rect(paint(p.line), 0.0, H, W, 1.0, 0.0);
    let bar_w = 24.0;
    let pitch = 31.0;
    for (i, (c, x, g)) in sample_days.iter().enumerate() {
        let x_pos = i as f32 * pitch;
        let c_h = (c / max_tot * H).round();
        let x_h = (x / max_tot * H).round();
        let g_h = (g / max_tot * H).round();
        let tot_h = c_h + x_h + g_h;
        let y_top = H - tot_h;
        builder = builder.round_rect(paint(p.surface_2), x_pos, 0.0, bar_w, H, 4.0, 0.0);
        builder = builder.round_rect(paint(claude_col), x_pos, H - c_h, bar_w, c_h, 4.0, 0.0);
        builder = builder.round_rect(paint(codex_col), x_pos, H - c_h - x_h, bar_w, x_h, 0.0, 0.0);
        builder = builder.round_rect(paint(gemini_col), x_pos, y_top, bar_w, g_h, 4.0, 0.0);
    }
    builder.build()
}

fn draw_split_bar(_p: &Palette, w: f32) -> DrawList {
    let h = 14.0;
    let r = 7.0;
    let w1 = (w * 0.28).round();
    let w2 = (w * 0.10).round();
    let w3 = (w * 0.56).round();
    let w4 = w - w1 - w2 - w3;
    let blue = Color::rgb(0x3f74d4);
    let orange = Color::rgb(0xe49b55);
    let sky = Color::rgb(0x9dbde9);
    let green = Color::rgb(0x4f9a6c);

    DrawList::builder()
        .round_rect(paint(blue), 0.0, 0.0, w1 + r, h, r, 0.0)
        .rect(paint(orange), w1, 0.0, w2, h, 0.0)
        .rect(paint(sky), w1 + w2, 0.0, w3, h, 0.0)
        .round_rect(paint(green), w1 + w2 + w3 - r, 0.0, w4 + r, h, r, 0.0)
        .build()
}

// ---------------------------------------------------------------------------------------------
// 1. 하네스 (Harnesses)
// ---------------------------------------------------------------------------------------------

fn harness_path(h: AgentHarness) -> &'static str {
    match h {
        AgentHarness::ClaudeCode => "/usr/local/bin/claude",
        AgentHarness::Codex => "/opt/homebrew/bin/codex",
        AgentHarness::Gemini => "/usr/local/bin/gemini",
        AgentHarness::Antigravity => "/usr/local/bin/agy",
        AgentHarness::Acp => "/usr/local/bin/omp",
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

    // KPI Strip
    let installed_count = live.detected.iter().filter(|d| d.installed).count().max(4);
    let shim_count = 4;
    let mem_sum = 17;
    cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
        KpiItem("설치된 하네스", &installed_count.to_string(), Some((&format!("전체 {}", AgentHarness::ALL.len()), PillTone::Grey)));
        KpiItem("상주 shim", &shim_count.to_string(), Some(("켜짐", PillTone::Blue)));
        KpiItem("세션 메모리 한도 합", &mem_sum.to_string(), Some(("GB", PillTone::Grey)));
        KpiItem("마지막 감지", "방금", None);
    });

    let harnesses = [
        AgentHarness::ClaudeCode,
        AgentHarness::Codex,
        AgentHarness::Gemini,
        AgentHarness::Antigravity,
    ];

    if compact {
        for h in harnesses {
            HarnessCard(h, store, &live.detected, &profiles, compact);
        }
    } else {
        cu::Row()
            .alignment(Alignment::TopStart)
            .spacing(SP[3])
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    HarnessCard(harnesses[0], store, &live.detected, &profiles, compact);
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    HarnessCard(harnesses[1], store, &live.detected, &profiles, compact);
                });
            });
        cu::Row()
            .alignment(Alignment::TopStart)
            .spacing(SP[3])
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    HarnessCard(harnesses[2], store, &live.detected, &profiles, compact);
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    HarnessCard(harnesses[3], store, &live.detected, &profiles, compact);
                });
            });
    }
}

fn HarnessCard(
    h: AgentHarness,
    store: &MutableState<LocalStore>,
    detected: &[DetectedAgent],
    profiles: &[Profile],
    _compact: bool,
) {
    let p = ember_theme().palette;
    let (state, tone, version) = harness_status(h, detected);
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
                    let sub = format!("{} · v{}", h.run_mode(), version.as_deref().unwrap_or("1.0.0"));
                    Label(&sub, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
                if h == AgentHarness::Antigravity {
                    Pill("확인 필요", PillTone::Orange, true);
                } else {
                    Pill(state, tone.into(), true);
                }
            });

            // Row 1: 설치와 버전
            SettingRow(
                "설치와 버전",
                harness_path(h),
                || {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(|| {
                        if let Some(v) = &version {
                            Pill(&format!("v{v}"), PillTone::Grey, true);
                        } else {
                            Pill("v1.0.0", PillTone::Grey, true);
                        }
                        let h_name = h.label().to_string();
                        let ver_str = version.clone().unwrap_or_else(|| "1.0.0".to_string());
                        AppButton(
                            "다시 찾기",
                            Some(IconRole::History),
                            ButtonKind::Default,
                            true,
                            true,
                            false,
                            move || {
                                post_notice(&format!("{h_name} v{ver_str}을 찾았습니다"));
                            },
                        );
                    });
                },
            );

            // Row 2: 고정 버전 (Antigravity or pinned)
            if h == AgentHarness::Antigravity {
                SettingRow(
                    "고정 버전",
                    "Ember가 검증한 버전은 1.2.16입니다. 다른 버전은 읽기 전용으로만 실행됩니다.",
                    || {
                        Pill("일치", PillTone::Green, true);
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
                        cu::Slider(cur_gb)
                            .range(1.0, 16.0)
                            .steps(15)
                            .modifier(Modifier::new().width(120.0))
                            .on_change(move |v| {
                                let gb = v.round().clamp(1.0, 16.0) as u32;
                                edit(&store_mem, |s| s.settings_mut(h).mem_limit_mb = Some(gb * 1024));
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

fn ModelsSection(store: &MutableState<LocalStore>, compact: bool) {
    // "새 팀원 기본값" card
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                CardHeaderIcon(super::shell::ICON_PLUS_SVG);
                Label("새 팀원 기본값", Fs::Lg, Weight::Semibold, None, Some(1));
            });
            Label(
                "홈과 새 팀원 만들기, 루틴에서 처음 보이는 하네스와 모델입니다. 고른 계정의 플랜에서 쓸 수 없는 모델은 흐리게 표시됩니다.",
                Fs::Sm,
                Weight::Regular,
                None,
                None,
            );
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(|| {
                ModelPickerPill("Claude Code · Opus 5.5", Some(ICON_SPARK_SVG), move || {
                    post_notice("기본 하네스와 모델을 선택합니다");
                });
                ModelPickerPill("ncmnd · Max", Some(ICON_USER_SVG), move || {
                    post_notice("기본 계정을 선택합니다");
                });
            });
        });
    });

    let harnesses = [AgentHarness::ClaudeCode, AgentHarness::Codex, AgentHarness::Gemini];

    if compact {
        for h in harnesses {
            ModelCard(h, store, compact);
        }
    } else {
        cu::Row().spacing(18.0).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[0], store, compact);
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[1], store, compact);
            });
        });
        cu::Row().spacing(18.0).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                ModelCard(harnesses[2], store, compact);
            });
            cu::Spacer().modifier(Modifier::new().weight(1.0));
        });
    }
}

fn ModelCard(h: AgentHarness, store: &MutableState<LocalStore>, _compact: bool) {
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
                                edit(&store_eff, |s| s.settings_mut(h).effort = Some(e.to_string()));
                            },
                        );
                    }
                },
            );

            // Row 2: 기본 권한 모드
            SettingRow(
                "기본 권한 모드",
                "새 팀원이 도구를 쓸 때 확인하는 방식입니다. 바이패스는 모든 명령을 묻지 않고 실행합니다.",
                || {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        ModelPickerPill("명령마다 확인", Some(super::shell::ICON_GEAR_SVG), move || {
                            post_notice("기본 권한 모드를 선택합니다");
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
                    Label("기본", Fs::Xs, Weight::Medium, Some(paint(p.muted)), None);
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label("모델", Fs::Xs, Weight::Medium, Some(paint(p.muted)), None);
                });
                cu::Box().modifier(Modifier::new().width(120.0)).alignment(Alignment::CenterEnd).content(|| {
                    Label("플랜별 사용", Fs::Xs, Weight::Medium, Some(paint(p.muted)), None);
                });
                cu::Box().modifier(Modifier::new().width(60.0)).alignment(Alignment::CenterEnd).content(|| {
                    Label("목록에 표시", Fs::Xs, Weight::Medium, Some(paint(p.muted)), None);
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
                            edit(&store_m, |s| s.settings_mut(h).model = Some(m_id));
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

fn AccountTile(x: &AccountInfo, extra: Option<&str>) {
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
                    Pill(
                        if x.label.contains("Max") { "Max" } else if x.label.contains("Plus") { "Plus" } else { "Pro" },
                        PillTone::Grey,
                        true,
                    );
                });
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                let reset_str = format!("{} · 리셋 3일 뒤", x.harness.label());
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
                AppButton("계정 추가", Some(IconRole::Add), ButtonKind::Default, true, true, false, || {
                    post_notice("계정 추가 창이 열립니다");
                });
            });
            if compact {
                for a in &live.accounts {
                    let extra = if a.limited { Some("한도 가까움") } else { None };
                    AccountTile(a, extra);
                }
            } else {
                let chunks: Vec<_> = live.accounts.chunks(3).collect();
                for chunk in chunks {
                    cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for a in chunk {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                let extra = if a.limited { Some("한도 가까움") } else { None };
                                AccountTile(a, extra);
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
                        // A bounded width, or the pill takes the whole row and the hint beside it
                        // wraps to one glyph per line.
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

fn NetworkSection(computers: &[(String, Reachability)], compact: bool) {
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
                    ComputerTile(name, *r);
                }
            } else {
                let chunks: Vec<_> = computers.chunks(3).collect();
                for chunk in chunks {
                    cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for (name, r) in chunk {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                ComputerTile(name, *r);
                            });
                        }
                    });
                    cu::Spacer().modifier(Modifier::new().height(SP[2]));
                }
            }
        });
    });
}

fn ComputerTile(name: &str, r: Reachability) {
    let p = ember_theme().palette;
    let (status_text, tone) = computer_status(r);
    let os_str = match name {
        "mac-mini" => "macOS 26 · 메인 서버",
        "ws-linux-01" => "Ubuntu 24.04",
        "macbook-pro" => "macOS 26",
        "win-desktop" => "Windows 11",
        _ => "Linux (확인 중)",
    };
    let ip_str = match name {
        "mac-mini" => "211.36.142.18",
        "ws-linux-01" => "121.134.77.205",
        "macbook-pro" => "175.223.10.91",
        _ => "IP 없음",
    };
    let sessions_num = match name {
        "mac-mini" => "7",
        "ws-linux-01" => "6",
        _ => "0",
    };
    let tunnels_num = match name {
        "mac-mini" => "12",
        "ws-linux-01" => "9",
        _ => "0",
    };
    let name_owned = name.to_string();
    let ip_owned = ip_str.to_string();

    InsetPanel(true, || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label(name, Fs::Md, Weight::Semibold, None, Some(1));
                    Label(os_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
                Pill(status_text, tone.into(), true);
            });
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            Label(ip_str, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                Label(sessions_num, Fs::Sm, Weight::Medium, None, None);
                Label(" 세션", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
                Label(tunnels_num, Fs::Sm, Weight::Medium, None, None);
                Label(" 열린 연결", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                AppButton("IP 확인", None, ButtonKind::Default, true, r == Reachability::Online, false, move || {
                    post_notice(&format!("{name_owned} 세션이 본 IP: {ip_owned}"));
                });
            });
            if name == "win-desktop" {
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                Label("연결이 끊겨 세션 3개가 메인 서버(mac-mini)로 연결됩니다.", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
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
    week_tokens: u64,
    compact: bool,
) {
    let p = ember_theme().palette;
    let tokens = chatgpt_tokens_today(&live.chatgpt);

    // Period Seg: 7일 / 14일
    const PERIOD_OPTIONS: [(u32, &'static str); 2] = [(7, "7일"), (14, "14일")];
    SegGroup(&PERIOD_OPTIONS, 14, |_| {});

    cu::Spacer().modifier(Modifier::new().height(SP[3]));

    // Hero Card: 오늘 사용량 / 오늘 토큰
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            if compact {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("오늘 사용량 (오늘 토큰)", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                    let tok_str = tokens.map(format_tokens).unwrap_or_else(|| "1,234,567".to_string());
                    Label(&tok_str, Fs::Xl3, Weight::Semibold, None, Some(1));
                    Label(&format!("토큰 · 14일 평균 3.5M (최근 7일 {})", format_tokens(week_tokens)), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                });
            } else {
                cu::Row().alignment(Alignment::CenterEnd).spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().width(320.0)).content(|| {
                        Label("오늘 사용량 (오늘 토큰)", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                        let tok_str = tokens.map(format_tokens).unwrap_or_else(|| "1,234,567".to_string());
                        Label(&tok_str, Fs::Xl3, Weight::Semibold, None, Some(1));
                        Label(&format!("토큰 · 14일 평균 3.5M (최근 7일 {})", format_tokens(week_tokens)), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        cu::Spacer().modifier(Modifier::new().height(SP[2]));
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).content(|| {
                            VendorDot(AgentHarness::ClaudeCode);
                            Label("Claude 28.5M", Fs::Xs, Weight::Regular, None, None);
                            VendorDot(AgentHarness::Codex);
                            Label("Codex 15.2M", Fs::Xs, Weight::Regular, None, None);
                            VendorDot(AgentHarness::Gemini);
                            Label("Gemini 5.1M", Fs::Xs, Weight::Regular, None, None);
                        });
                    });
                    cu::Box().modifier(Modifier::new().width(440.0).height(150.0)).content(|| {
                        let bars_draw = draw_usage_bars(p);
                        cu::Canvas(bars_draw).modifier(Modifier::new().size(440.0, 150.0));
                    });
                });
            }
        });
    });

    cu::Spacer().modifier(Modifier::new().height(SP[3]));

    // 2-Column row: 오늘 토큰 종류 & 하네스와 모델별 비용
    if compact {
        TokenKindsCard(compact);
        cu::Spacer().modifier(Modifier::new().height(SP[3]));
        HarnessCostCard(bars);
    } else {
        cu::Row().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                TokenKindsCard(compact);
            });
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                HarnessCostCard(bars);
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

fn TokenKindsCard(compact: bool) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("오늘 토큰 종류", Fs::Lg, Weight::Semibold, None, Some(1));
            });
            let bar_w = if compact { 326.0 } else { 409.0 };
            let split_draw = draw_split_bar(p, bar_w);
            cu::Canvas(split_draw).modifier(Modifier::new().size(bar_w, 14.0));
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label("입력", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    Label("1.1M", Fs::Lg, Weight::Semibold, None, None);
                    Label("28%", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label("출력", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    Label("0.4M", Fs::Lg, Weight::Semibold, None, None);
                    Label("10%", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label("캐시 읽기", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    Label("2.2M", Fs::Lg, Weight::Semibold, None, None);
                    Label("56%", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                });
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label("캐시 쓰기", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    Label("0.2M", Fs::Lg, Weight::Semibold, None, None);
                    Label("5%", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                });
            });
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            Label(
                "하네스가 알려 준 실제 토큰 수입니다. 알려 주지 않는 하네스는 추정하지 않고 모름으로 표시됩니다.",
                Fs::Xs,
                Weight::Regular,
                Some(paint(p.muted)),
                None,
            );
        });
    });
}

fn HarnessCostCard(bars: &[crate::ui::home::HarnessBar]) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("하네스와 모델별 비용", Fs::Lg, Weight::Semibold, None, Some(1));
            });
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Box().modifier(Modifier::new().size(132.0, 132.0)).alignment(Alignment::Center).content(|| {
                    let donut_draw = draw_donut(p);
                    cu::Canvas(donut_draw).modifier(Modifier::new().size(132.0, 132.0));
                    cu::Column().content(|| {
                        Label("$174", Fs::Lg, Weight::Semibold, None, None);
                        Label("이번 달", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    });
                });
                cu::Column().spacing(SP[1]).modifier(Modifier::new().weight(1.0)).content(|| {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        VendorDot(AgentHarness::ClaudeCode);
                        Label("Claude $120", Fs::Sm, Weight::Regular, None, None);
                    });
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        VendorDot(AgentHarness::Codex);
                        Label("Codex $46", Fs::Sm, Weight::Regular, None, None);
                    });
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        VendorDot(AgentHarness::Gemini);
                        Label("Gemini $8", Fs::Sm, Weight::Regular, None, None);
                    });
                });
            });
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
            for (model, cost, h) in [
                ("Opus 5.5", "$96.2", AgentHarness::ClaudeCode),
                ("Sonnet 5.5", "$24.1", AgentHarness::ClaudeCode),
                ("GPT-5", "$31.5", AgentHarness::Codex),
                ("GPT-5 Codex", "$14.8", AgentHarness::Codex),
                ("Gemini Pro", "$6.9", AgentHarness::Gemini),
            ] {
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(28.0)).content(|| {
                    VendorDot(h);
                    cu::Spacer().modifier(Modifier::new().width(SP[1]));
                    Label(model, Fs::Sm, Weight::Medium, None, Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Label(cost, Fs::Sm, Weight::Medium, None, None);
                });
            }
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(28.0)).content(|| {
                VendorDot(AgentHarness::Antigravity);
                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                Label("보고 안 함", Fs::Sm, Weight::Medium, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Label("모름", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            });
            for b in bars {
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(28.0)).content(|| {
                    Label(&b.label, Fs::Sm, Weight::Regular, None, Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Label(&format!("세션 {}개", b.sessions), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
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
                    12_345_678,
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
        for want in ["오늘 토큰", "1,234,567", "하네스와 모델별 비용", "계정별 남은 구독량"] {
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

    /// `(node, text)` of every Button in the first frame and after the width report.
    fn button_texts_at(width: f32, class: compose_rust::WindowSizeClass, section: Section) -> Vec<(u32, String)> {
        use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
        use compose_rust::{EventPayload, PropertyKind, WidgetKind, WindowHeightClass};
        use std::collections::{HashMap, HashSet};
        compose_rust::window::reset_window_size();
        let mut host = Host::new(move || {
            Box::new(Recomposer::new(move || {
                AgentsScreenWith(sample_live(), sample_computers(), sample_bars(), 12_345_678, section)
            }))
        });
        let mut nodes: HashSet<u32> = HashSet::new();
        let mut texts: HashMap<u32, String> = HashMap::new();
        let mut scan = |batch: &[Mutation<'_>]| {
            for m in batch {
                match m {
                    Mutation::Create { node_id, widget: WidgetKind::Button } => {
                        nodes.insert(*node_id);
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(t) } => {
                        texts.insert(*node_id, t.to_string());
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
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        scan(&decode_batch(bytes).expect("decode"));
        compose_rust::window::reset_window_size();
        nodes.into_iter().map(|n| (n, texts.get(&n).cloned().unwrap_or_default())).collect()
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
    fn fr_l16_models_controls_expose_their_own_labels_and_the_account_select_stays() {
        use compose_rust::WindowSizeClass as W;
        for (w, class) in [(1440.0, W::Expanded), (390.0, W::Compact)] {
            let names: Vec<String> =
                button_texts_at(w, class, Section::Models).into_iter().map(|(_, t)| t).collect();
            for want in ["명령마다 확인", "ncmnd · Max"] {
                assert!(names.iter().any(|n| n == want), "missing button {want} at {w} dp: {names:?}");
            }
            // A radio and a switch name their model.
            assert!(names.iter().any(|n| n.ends_with(" 기본 모델로 선택") && n.len() > " 기본 모델로 선택".len()));
            assert!(names.iter().any(|n| n.contains("활성화") && n.len() > "활성화 끄기".len()));
        }
    }
}
