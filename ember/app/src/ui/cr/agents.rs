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

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, IconRole, Paint, SpaceRole};
use ember_client::state::Reachability;
use ember_client::wire::DetectedAgent;

pub use crate::ui::agents::{
    add_account, can_delete_profile, chatgpt_tokens_today, computer_status, cycle, default_settings,
    effective_default, fail_options, format_tokens, harness_status, mem_limit_label, move_account,
    nav_shape, new_profile, next_mem_limit, next_profile_id, pick_account, preview_text,
    proxy_supported, remove_account, resolve_profile, resolve_section, seed_profiles,
    set_account_mode, step_threshold, threshold_label, AccountInfo, AccountMode, AgentHarness,
    HarnessSettings, LocalStore, NavShape, OnFail, Pick, Profile, Routing, Section, SkipReason,
    LOCAL_STORE_NOTE, MEM_STEPS_MB, THRESHOLD_MAX, THRESHOLD_MIN, THRESHOLD_STEP,
};
use super::components::{
    AppButton, ButtonKind, Clickable, EmberCard, EmberField, FieldOptions, FilterChip, InsetPanel,
    Label, PickChip, Pill, PillTone,
};
use super::theme::*;
use crate::server::{Account, ChatGptAccount};
use crate::ui::home::UNAVAILABLE;

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
    let shape = nav_shape(compact);
    let pad = if compact { SP[3] } else { SP[5] };
    let cur_sec = section.get();

    let nav_pick = {
        let section = section.clone();
        move |s: Section| section.set(s)
    };

    match shape {
        NavShape::Pills => {
            cu::Column()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(|| {
                    SectionNav(cur_sec, NavShape::Pills, nav_pick);
                    cu::Spacer()
                        .modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                    cu::ScrollColumn()
                        .modifier(Modifier::new().fill_max_width().weight(1.0).fill_max_height())
                        .content(|| {
                            cu::Column()
                                .spacing(GAP_CARD)
                                .modifier(Modifier::new().fill_max_width().padding(pad))
                                .content(|| {
                                    Label(
                                        "에이전트",
                                        Fs::Xl2,
                                        Weight::Semibold,
                                        None,
                                        Some(1),
                                    );
                                    Label(
                                        "하네스와 모델, 프로필, 네트워크, 사용량을 설정합니다",
                                        Fs::Sm,
                                        Weight::Regular,
                                        Some(paint(p.text_2)),
                                        None,
                                    );
                                    match cur_sec {
                                        Section::Harness => HarnessSection(&store, &live),
                                        Section::Models => ModelsSection(&store),
                                        Section::Profiles => ProfilesSection(&store, &live),
                                        Section::Network => NetworkSection(&computers),
                                        Section::Usage => UsageSection(&live, &bars, week_tokens),
                                    }
                                });
                        });
                });
        }
        NavShape::Side => {
            cu::Row()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(|| {
                    let side_w = if window.width_dp > 1279.0 { 200.0 } else { 180.0 };
                    cu::Column()
                        .modifier(Modifier::new().width(side_w).fill_max_height())
                        .content(|| {
                            SectionNav(cur_sec, NavShape::Side, nav_pick);
                        });
                    cu::Spacer()
                        .modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));
                    cu::ScrollColumn()
                        .modifier(Modifier::new().fill_max_width().weight(1.0).fill_max_height())
                        .content(|| {
                            cu::Column()
                                .spacing(GAP_CARD)
                                .modifier(Modifier::new().fill_max_width().padding(pad))
                                .content(|| {
                                    Label(
                                        "에이전트",
                                        Fs::Xl3,
                                        Weight::Semibold,
                                        None,
                                        Some(1),
                                    );
                                    Label(
                                        "하네스와 모델, 프로필, 네트워크, 사용량을 설정합니다",
                                        Fs::Sm,
                                        Weight::Regular,
                                        Some(paint(p.text_2)),
                                        None,
                                    );
                                    match cur_sec {
                                        Section::Harness => HarnessSection(&store, &live),
                                        Section::Models => ModelsSection(&store),
                                        Section::Profiles => ProfilesSection(&store, &live),
                                        Section::Network => NetworkSection(&computers),
                                        Section::Usage => UsageSection(&live, &bars, week_tokens),
                                    }
                                });
                        });
                });
        }
    }
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
// Shared section components
// ---------------------------------------------------------------------------------------------

fn SectionNav(current: Section, shape: NavShape, on_pick: impl FnMut(Section) + Clone + 'static) {
    let p = ember_theme().palette;
    match shape {
        NavShape::Side => {
            cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                for x in Section::ALL {
                    let selected = x == current;
                    let bg = if selected {
                        paint(p.sel)
                    } else {
                        Paint::Literal(compose_rust::Color::argb(0))
                    };
                    let fg = if selected { p.text } else { p.text_2 };
                    let mut on_pick = on_pick.clone();
                    // TODO(compose-rust#372): Clickable container for nav row until Modifier.clickable lands
                    Clickable(
                        Modifier::new()
                            .fill_max_width()
                            .height(H_BTN)
                            .background(bg)
                            .corner_radius(CORNER_SM),
                        Alignment::CenterStart,
                        move || on_pick(x),
                        || {
                            cu::Row().alignment(Alignment::CenterStart).content(|| {
                                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                                Label(x.label(), Fs::Md, Weight::Medium, Some(paint(fg)), Some(1));
                            });
                        },
                    );
                    cu::Spacer().modifier(Modifier::new().height(SP[0]));
                }
            });
        }
        NavShape::Pills => {
            cu::ScrollRow().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Row().spacing(SP[1]).modifier(Modifier::new().padding(SP[2])).content(|| {
                    for x in Section::ALL {
                        let selected = x == current;
                        let mut on_pick = on_pick.clone();
                        FilterChip(x.label(), selected, move || on_pick(x));
                    }
                });
            });
        }
    }
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
    cu::Row()
        .alignment(Alignment::CenterStart)
        .spacing(SP[3])
        .modifier(Modifier::new().fill_max_width().padding(SP[1]))
        .content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(title, Fs::Md, Weight::Medium, None, Some(1));
                if !hint.is_empty() {
                    Label(hint, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                }
            });
            control();
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
// 1. 하네스 (Harnesses)
// ---------------------------------------------------------------------------------------------

fn HarnessSection(store: &MutableState<LocalStore>, live: &AgentsLive) {
    let profiles = effective_profiles(&store.get(), live);
    LocalNote();
    for h in AgentHarness::ALL {
        HarnessCard(h, store, &live.detected, &profiles);
    }
}

fn HarnessCard(
    h: AgentHarness,
    store: &MutableState<LocalStore>,
    detected: &[DetectedAgent],
    profiles: &[Profile],
) {
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

    Block(
        h.label(),
        || {
            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                if let Some(v) = version {
                    Pill(&v, PillTone::Grey, true);
                }
                Pill(state, tone.into(), true);
            });
        },
        || {
            SettingRow(
                "실행 방식",
                "하네스마다 정해진 프로토콜로 실행합니다.",
                || {
                    Pill(h.run_mode(), PillTone::Sky, false);
                },
            );
            if let Some(on) = settings.sandbox {
                let hint = if on {
                    "켜져 있습니다. 샌드박스 안에서는 셸 쓰기가 실패합니다."
                } else {
                    "기본으로 꺼져 있습니다. 승인 훅이 모든 도구 호출을 확인합니다."
                };
                let store = store.clone();
                SettingRow(
                    "샌드박스",
                    hint,
                    move || {
                        drop(
                            cu::Switch(on).on_change(move |v: bool| {
                                edit(&store, |s| s.settings_mut(h).sandbox = Some(v));
                                post_notice(if v {
                                    "샌드박스를 켰습니다. 셸 쓰기가 실패합니다."
                                } else {
                                    "샌드박스를 껐습니다."
                                });
                            }),
                        );
                    },
                );
            }
            let shim_hint = if h.shim_supported() {
                "서버가 멈춰도 에이전트가 계속 실행되고, 다시 시작되면 이어 붙습니다."
            } else {
                "턴마다 프로세스를 새로 띄우므로 아직 shim 없이 직접 실행합니다."
            };
            let store_1 = store.clone();
            SettingRow(
                "상주 shim",
                shim_hint,
                move || {
                    if h.shim_supported() {
                        drop(
                            cu::Switch(settings.shim).on_change(move |v: bool| {
                                edit(&store_1, |s| s.settings_mut(h).shim = v);
                                post_notice(if v {
                                    "상주 shim을 켰습니다."
                                } else {
                                    "상주 shim을 껐습니다."
                                });
                            }),
                        );
                    } else {
                        Pill(UNAVAILABLE, PillTone::Grey, false);
                    }
                },
            );
            let store_2 = store.clone();
            SettingRow(
                "세션당 메모리 한도",
                "넘기면 그 세션만 끝납니다. 누르면 값이 바뀝니다.",
                move || {
                    PickChip(
                        &mem_limit_label(settings.mem_limit_mb),
                        None,
                        false,
                        false,
                        move || {
                            edit(&store_2, |s| {
                                let m = s.settings_mut(h);
                                m.mem_limit_mb = next_mem_limit(m.mem_limit_mb);
                            });
                        },
                    );
                },
            );
            let store_3 = store.clone();
            SettingRow(
                "기본 프로필",
                "이 하네스의 새 세션이 쓰는 프로필입니다. 누르면 다음 프로필로 바뀝니다.",
                move || {
                    let options = options.clone();
                    let current = current.clone();
                    PickChip(&profile_name, None, false, false, move || {
                        let next = cycle(&options, Some(&current)).unwrap_or(None);
                        edit(&store_3, |s| s.settings_mut(h).default_profile = next);
                    });
                },
            );
        },
    );
}

// ---------------------------------------------------------------------------------------------
// 2. 모델 (Models)
// ---------------------------------------------------------------------------------------------

fn ModelsSection(store: &MutableState<LocalStore>) {
    LocalNote();
    for h in AgentHarness::ALL {
        ModelCard(h, store);
    }
}

fn ModelCard(h: AgentHarness, store: &MutableState<LocalStore>) {
    let settings = store.get().settings(h).clone();
    let Some(ph) = h.picker_harness() else {
        // TODO(server): the server exposes no model list, and only the three picker harnesses
        // have a local one (home.rs).
        Block(
            h.label(),
            || {},
            || {
                Pending("모델 목록은");
            },
        );
        return;
    };

    let mut models: Vec<(&'static str, &'static str)> = Vec::new();
    for o in crate::ui::home::model_catalog()
        .into_iter()
        .filter(|o| o.harness == ph)
    {
        if !models.iter().any(|(id, _)| *id == o.model) {
            models.push((o.model, o.name));
        }
    }
    let modes = crate::ui::home::modes_for(ph);

    Block(
        h.label(),
        || {
            if h == AgentHarness::Gemini {
                Pill(UNAVAILABLE, PillTone::Grey, true);
            }
        },
        || {
            SettingRow(
                "기본 모델",
                "새 세션에서 처음 고르는 모델입니다.",
                || {},
            );
            cu::ScrollRow().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Row().spacing(SP[0]).content(|| {
                    for (id, name) in models {
                        let selected = settings.model.as_deref() == Some(id);
                        let store = store.clone();
                        let id_str = id.to_string();
                        PickChip(name, None, selected, false, move || {
                            let id_str = id_str.clone();
                            edit(&store, move |s| s.settings_mut(h).model = Some(id_str));
                        });
                    }
                });
            });
            if ph.has_effort() {
                SettingRow("기본 강도", "", || {});
                cu::Row().spacing(SP[0]).content(|| {
                    for e in crate::ui::home::EFFORTS {
                        let selected = settings.effort.as_deref() == Some(e);
                        let store = store.clone();
                        let e_str = e.to_string();
                        PickChip(e, None, selected, false, move || {
                            let e_str = e_str.clone();
                            edit(&store, move |s| s.settings_mut(h).effort = Some(e_str));
                        });
                    }
                });
            }
            SettingRow(
                "기본 권한 모드",
                "바이패스는 모든 명령을 묻지 않고 실행합니다.",
                || {
                    if settings.mode.is_bypass() {
                        Pill("주의", PillTone::Orange, true);
                    }
                },
            );
            cu::ScrollRow().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Row().spacing(SP[0]).content(|| {
                    for m in modes {
                        let selected = settings.mode == *m;
                        let store = store.clone();
                        let m_val = *m;
                        PickChip(m.label(), None, selected, false, move || {
                            edit(&store, move |s| s.settings_mut(h).mode = m_val);
                        });
                    }
                });
            });
            Pending("플랜별 사용 가능 여부는");
        },
    );
}

// ---------------------------------------------------------------------------------------------
// 3. 프로필 (Profiles)
// ---------------------------------------------------------------------------------------------

fn ProfilesSection(store: &MutableState<LocalStore>, live: &AgentsLive) {
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

    LocalNote();
    cu::Row()
        .alignment(Alignment::CenterStart)
        .spacing(SP[2])
        .modifier(Modifier::new().fill_max_width())
        .content(|| {
            Label(
                "프로필은 계정과 라우팅을 묶어 하네스에 할당합니다.",
                Fs::Sm,
                Weight::Regular,
                None,
                None,
            );
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

    if live.accounts.is_empty() {
        Pending("계정 목록은");
    }

    for p in profiles.clone() {
        let can_delete = can_delete_profile(&p.id, &defaults);
        ProfileCard(p, profiles.clone(), can_delete, live, store);
    }
}

fn ProfileCard(
    p: Profile,
    all: Vec<Profile>,
    can_delete: bool,
    live: &AgentsLive,
    store: &MutableState<LocalStore>,
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

    let block_title = p.name.clone();
    let name_hint = p.name.clone();
    Block(
        &block_title,
        || {
            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                Pill(p.harness.label(), PillTone::Blue, true);
                Pill(
                    if p.mode == AccountMode::Fixed {
                        "고정"
                    } else {
                        "자동 전환"
                    },
                    PillTone::Grey,
                    true,
                );
            });
        },
        || {
            let ed_clone = ed.clone();
            SettingRow(
                "이름",
                "비워 두면 이름이 바뀌지 않습니다.",
                move || {
                    cu::Box().modifier(Modifier::new().width(240.0)).content(|| {
                        // TODO(compose-rust#374): TextField is uncontrolled and has no initial value,
                        // so the current name is the placeholder.
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: name_hint.clone(),
                            on_value_change: Some(Box::new(move |v: String| {
                                let v = v.trim().to_string();
                                if !v.is_empty() {
                                    ed_clone.apply(move |p| p.name = v);
                                }
                            })),
                            ..Default::default()
                        });
                    });
                },
            );
            SettingRow("계정 방식", "", || {});
            cu::Row().spacing(SP[1]).content(|| {
                for (mode, text) in [
                    (AccountMode::Fixed, "계정 고정"),
                    (AccountMode::Auto, "자동 전환"),
                ] {
                    let ed = ed.clone();
                    let same_ids = same_ids.clone();
                    PickChip(text, None, p.mode == mode, false, move || {
                        let same_ids = same_ids.clone();
                        ed.apply(move |p| set_account_mode(p, mode, &same_ids));
                    });
                }
            });
            if p.mode == AccountMode::Fixed {
                SettingRow("계정", "이 계정 하나로만 실행됩니다.", || {});
                cu::ScrollRow().modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Row().spacing(SP[1]).content(|| {
                        for a in same.clone() {
                            let ed = ed.clone();
                            let aid = a.id.clone();
                            PickChip(
                                &a.label,
                                None,
                                p.accounts.first() == Some(&a.id),
                                false,
                                move || {
                                    let aid = aid.clone();
                                    ed.apply(move |p| p.accounts = vec![aid]);
                                },
                            );
                        }
                    });
                });
                if same.is_empty() {
                    Pending("이 하네스의 계정은");
                }
            } else {
                SettingRow(
                    "전환 순서",
                    "위에서부터 씁니다. 끌기 대신 위로와 아래로 버튼을 씁니다.",
                    || {},
                );
                // TODO(GAPS: 끌기): drag to reorder needs pointer capture; buttons for now.
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
                            Label(
                                &(i + 1).to_string(),
                                Fs::Sm,
                                Weight::Medium,
                                Some(paint(theme.palette.muted)),
                                Some(1),
                            );
                            cu::Spacer().modifier(Modifier::new().width(SP[0]));
                            Label(
                                &account_label(&live.accounts, &aid),
                                Fs::Md,
                                Weight::Regular,
                                None,
                                Some(1),
                            );
                            if is_limited {
                                cu::Spacer().modifier(Modifier::new().width(SP[0]));
                                Pill("한도 도달", PillTone::Orange, true);
                            }
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            AppButton("위로", None, ButtonKind::Default, true, i > 0, false, move || {
                                ed_up.apply(move |p| {
                                    move_account(&mut p.accounts, i, -1);
                                });
                            });
                            AppButton(
                                "아래로",
                                None,
                                ButtonKind::Default,
                                true,
                                i + 1 < len,
                                false,
                                move || {
                                    ed_down.apply(move |p| {
                                        move_account(&mut p.accounts, i, 1);
                                    });
                                },
                            );
                            AppButton("빼기", None, ButtonKind::Default, true, len > 1, false, move || {
                                ed_rm.apply(move |p| {
                                    remove_account(&mut p.accounts, i);
                                });
                            });
                        });
                }
                if !rest.is_empty() {
                    cu::ScrollRow().modifier(Modifier::new().fill_max_width()).content(|| {
                        cu::Row().spacing(SP[1]).content(|| {
                            for a in rest {
                                let ed = ed.clone();
                                let aid = a.id.clone();
                                PickChip(&format!("+ {}", a.label), None, false, false, move || {
                                    let aid = aid.clone();
                                    ed.apply(move |p| {
                                        add_account(&mut p.accounts, &aid);
                                    });
                                });
                            }
                        });
                    });
                }
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
                SettingRow(
                    "라우팅 정책",
                    "새 세션의 계정을 고르는 방법입니다.",
                    || {},
                );
                cu::Row().spacing(SP[1]).content(|| {
                    for r in [Routing::Order, Routing::MostLeft] {
                        let ed = ed.clone();
                        PickChip(r.label(), None, p.policy == r, false, move || {
                            ed.apply(move |p| p.policy = r);
                        });
                    }
                });
                if p.policy == Routing::MostLeft {
                    Pending("계정별 남은 사용량은");
                }
            }
            SettingRow(
                "실패할 때",
                "속도 제한이나 인증 오류가 나면 이렇게 처리합니다.",
                || {},
            );
            cu::Row().spacing(SP[1]).content(|| {
                for f in fails {
                    let ed = ed.clone();
                    PickChip(f.label(), None, p.on_fail == f, false, move || {
                        ed.apply(move |p| p.on_fail = f);
                    });
                }
            });
            let ed_proxy = ed.clone();
            let is_proxy = proxy_supported(p.harness);
            let hint = if is_proxy {
                "비워 두면 하네스의 공급자를 그대로 씁니다."
            } else {
                "이 하네스의 프록시 설정은 확인 전입니다."
            };
            SettingRow(
                "모델 프록시",
                hint,
                move || {
                    if is_proxy {
                        cu::Box().modifier(Modifier::new().width(240.0)).content(|| {
                            EmberField(FieldOptions {
                                fill_max_width: true,
                                placeholder: proxy_hint,
                                on_value_change: Some(Box::new(move |v: String| {
                                    let v = v.trim().to_string();
                                    ed_proxy.apply(move |p| p.proxy = (!v.is_empty()).then_some(v));
                                })),
                                ..Default::default()
                            });
                        });
                    } else {
                        Pill(UNAVAILABLE, PillTone::Grey, false);
                    }
                },
            );
            InsetPanel(true, || {
                Label(&preview, Fs::Sm, Weight::Regular, None, None);
            });
            cu::Row()
                .alignment(Alignment::CenterStart)
                .spacing(SP[2])
                .modifier(Modifier::new().fill_max_width())
                .content(|| {
                    if !can_delete {
                        Label(
                            "기본 프로필로 쓰고 있어 지울 수 없습니다.",
                            Fs::Xs,
                            Weight::Regular,
                            Some(paint(theme.palette.muted)),
                            None,
                        );
                    }
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    let seed = all.clone();
                    let pid = p.id.clone();
                    let p_name = p.name.clone();
                    let store = store.clone();
                    AppButton(
                        "프로필 지우기",
                        None,
                        ButtonKind::Destructive,
                        true,
                        can_delete,
                        false,
                        move || {
                            edit_profiles(&store, seed.clone(), |l| l.retain(|x| x.id != pid));
                            post_notice(&format!("{p_name} 프로필을 지웠습니다."));
                        },
                    );
                });
        },
    );
}

// ---------------------------------------------------------------------------------------------
// 4. 네트워크 (Network)
// ---------------------------------------------------------------------------------------------

fn NetworkSection(computers: &[(String, Reachability)]) {
    LocalNote();
    Block(
        "에이전트 API 트래픽",
        || {
            Pill("검증 전 (#130)", PillTone::Orange, true);
        },
        || {
            SettingRow(
                "현재 컴퓨터로 내보내기",
                "에이전트의 API 요청이 세션이 있는 컴퓨터를 거쳐 나가, 모델 공급자에게 그 컴퓨터의 주소가 보입니다.",
                || {
                    Pill(UNAVAILABLE, PillTone::Grey, false);
                },
            );
            // TODO(server): no endpoint reports or sets egress (EMBER_AGENT_EGRESS, FR-X6).
            Pending("켜기와 끄기는");
        },
    );

    Block(
        "컴퓨터",
        || {},
        || {
            if computers.is_empty() {
                Pending("컴퓨터 목록은");
            }
            for (name, r) in computers {
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[2])
                    .modifier(Modifier::new().fill_max_width().height(H_CTL))
                    .content(|| {
                        Label(name, Fs::Md, Weight::Regular, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        Label(
                            &format!("주소와 연결 수 {UNAVAILABLE}"),
                            Fs::Xs,
                            Weight::Regular,
                            None,
                            None,
                        );
                        let (text, tone) = computer_status(*r);
                        Pill(text, tone.into(), true);
                    });
            }
        },
    );
}

// ---------------------------------------------------------------------------------------------
// 5. 사용량 (Usage)
// ---------------------------------------------------------------------------------------------

fn UsageSection(
    live: &AgentsLive,
    bars: &[crate::ui::home::HarnessBar],
    week_tokens: u64,
) {
    LocalNote();
    let tokens = chatgpt_tokens_today(&live.chatgpt);

    Block(
        "오늘 토큰",
        || {},
        || {
            match tokens {
                Some(n) => {
                    Label(&format_tokens(n), Fs::Xl3, Weight::Medium, None, Some(1));
                    Label(
                        &format!("최근 7일 {}", format_tokens(week_tokens)),
                        Fs::Sm,
                        Weight::Regular,
                        None,
                        None,
                    );
                    Label(
                        "ChatGPT 플랜 계정이 보고한 토큰만 합친 값입니다. 다른 하네스는 준비 중입니다.",
                        Fs::Xs,
                        Weight::Regular,
                        None,
                        None,
                    );
                }
                None => {
                    Pending("토큰 사용량은");
                }
            }
            // TODO(server): other harnesses and token kinds (cache) are not reported by /api/usage yet.
        },
    );

    Block(
        "하네스별",
        || {},
        || {
            for b in bars {
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .modifier(Modifier::new().fill_max_width().height(H_CTL))
                    .content(|| {
                        Label(&b.label, Fs::Md, Weight::Regular, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        Label(&format!("세션 {}개", b.sessions), Fs::Sm, Weight::Regular, None, None);
                    });
            }
            Pending("하네스별 비용과 모델별 토큰은");
        },
    );

    Block(
        "계정",
        || {},
        || {
            if live.accounts.is_empty() {
                Pending("계정 목록은");
            }
            for a in &live.accounts {
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[1])
                    .modifier(Modifier::new().fill_max_width().height(H_CTL))
                    .content(|| {
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
            Pending("계정별 남은 사용량 링은");
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
        for want in ["에이전트 API 트래픽", "현재 컴퓨터로 내보내기", "컴퓨터", "mac-mini"] {
            assert!(text.contains(want), "missing network content {want}");
        }
    }

    #[test]
    fn fr_u7_usage_section_renders_tokens_and_bars() {
        let text = screen_text_at(1440.0, compose_rust::WindowSizeClass::Expanded, Section::Usage);
        for want in ["오늘 토큰", "1,234,567", "하네스별", "계정"] {
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
}
