//! The "기기 권한" section of the settings sheet (FR-N8, #817): each admitted device with its
//! scope and a picker (전체 / 읽기 전용 / 채팅만). Narrowing the device the app runs on asks
//! first. Logic in `ui/device_scope.rs`.

#![allow(non_snake_case)]

use compose_rust::runtime::{launched_effect, mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, TypeRole};

use ember_client::api::{DeviceRecord, DeviceScope};

use super::components::{AppButton, ButtonKind, FilterChip, InsetPanel, Label, Pill, PillTone, Txt};
use super::theme::*;
use crate::services::{run_shared, try_services};
use crate::ui::device_scope::*;

#[derive(Debug, Clone, PartialEq)]
enum ListState {
    Loading,
    Ready(Vec<DeviceRecord>),
    Failed(String),
}

/// A pending narrowing of this device, waiting for yes.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    peer_id: String,
    scope: DeviceScope,
}

fn load(state: MutableState<ListState>) {
    if try_services().is_none() {
        state.set(ListState::Ready(demo_devices()));
        return;
    }
    run_shared(async move { crate::services::services().devices().await }, move |r| {
        state.set(match r {
            Ok(v) => ListState::Ready(v),
            Err(e) => ListState::Failed(failure_text(&e.to_string())),
        })
    });
}

fn set(state: MutableState<ListState>, error: MutableState<Option<String>>, peer_id: String, scope: DeviceScope) {
    error.set(None);
    if try_services().is_none() {
        return;
    }
    run_shared(async move { crate::services::services().set_device_scope(&peer_id, scope).await }, move |r| match r {
        Ok(answer) => {
            if let ListState::Ready(mut v) = state.get_untracked() {
                apply_answer(&mut v, answer);
                state.set(ListState::Ready(v));
            }
        }
        Err(e) => error.set(Some(set_error_text(&e).to_string())),
    });
}

pub fn DeviceScopeSection() {
    let p = ember_theme().palette;
    let state = remember(|| mutable_state_of(ListState::Loading));
    let error = remember(|| mutable_state_of(None::<String>));
    let pending = remember(|| mutable_state_of(None::<Pending>));
    // TODO(whoami): the self row is decided here, from `Services::self_peer_id` (`Api::whoami`).
    let self_peer = try_services().and_then(|s| s.self_peer_id());

    let st = state.clone();
    launched_effect((), move || {
        let st = st.clone();
        async move { load(st) }
    });

    InsetPanel(true, || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| match state.get() {
            ListState::Loading => Label("기기를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1)),
            ListState::Failed(t) => Label(&t, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(2)),
            ListState::Ready(list) if list.is_empty() => {
                Label(NO_DEVICES, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1))
            }
            ListState::Ready(list) => {
                for row in scope_rows(&list, self_peer.as_deref()) {
                    cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                                Label(&row.name, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                                if row.is_self {
                                    Pill("이 기기", PillTone::Grey, true);
                                }
                            });
                            Txt(&row.short_peer, TypeRole::Mono, Some(paint(p.muted)), Some(1), None, None, None);
                        });
                        for chip in &row.chips {
                            let (st, er, pe) = (state.clone(), error.clone(), pending.clone());
                            let (peer, from, to) = (row.peer_id.clone(), row.scope, chip.scope);
                            FilterChip(chip.label, chip.selected, move || {
                                match pick_action(&peer, self_peer_of(), from, to) {
                                    PickAction::Nothing => {}
                                    PickAction::Set => set(st.clone(), er.clone(), peer.clone(), to),
                                    PickAction::Confirm => pe.set(Some(Pending { peer_id: peer.clone(), scope: to })),
                                }
                            });
                        }
                    });
                    if pending.get().is_some_and(|q| q.peer_id == row.peer_id) {
                        Label(CONFIRM_SELF, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(3));
                        cu::Row().spacing(SP[1]).content(|| {
                            let (st, er, pe) = (state.clone(), error.clone(), pending.clone());
                            AppButton("권한 좁히기", None, ButtonKind::Destructive, true, true, false, move || {
                                if let Some(q) = pe.get_untracked() {
                                    pe.set(None);
                                    set(st.clone(), er.clone(), q.peer_id, q.scope);
                                }
                            });
                            let pe = pending.clone();
                            AppButton("취소", None, ButtonKind::Default, true, true, false, move || pe.set(None));
                        });
                    }
                }
            }
        });
    });
    if let Some(t) = error.get() {
        Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(2));
    }
}

fn self_peer_of() -> Option<&'static str> {
    use std::sync::OnceLock;
    static SELF: OnceLock<Option<String>> = OnceLock::new();
    SELF.get_or_init(|| try_services().and_then(|s| s.self_peer_id())).as_deref()
}
