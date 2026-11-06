//! The add-computer sheet of the Project screen (FR-X7 / FR-N2, #490). It lists the account's
//! hub devices and registers one as a computer with the node's token. The decisions are in
//! `ui::add_computer`. The token lives in this sheet's state only until it closes: it is never
//! logged or saved.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;

use super::components::{AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, Label, Pill, SheetFooter};
use super::notice;
use super::theme::*;
use crate::services::{run_shared, services};
use crate::ui::add_computer::*;
use crate::ui::project::{CloseAction, SettingsGuard, DISCARD_QUESTION};

#[derive(Clone)]
pub struct AddUi {
    pub open: MutableState<bool>,
    devices: MutableState<DevicesState>,
    /// The endpoint id whose token is being asked for.
    target: MutableState<Option<String>>,
    token: MutableState<String>,
    name: MutableState<String>,
    saving: MutableState<bool>,
    failure: MutableState<Option<String>>,
    confirm: MutableState<bool>,
    /// The form, or the confirmation before the POST. Holds no token.
    step: MutableState<AddStep>,
    armed_at: MutableState<i64>,
    /// A debug review capture: sample devices, nothing calls the server.
    review: bool,
}

impl AddUi {
    pub fn new() -> AddUi {
        AddUi {
            open: mutable_state_of(false),
            devices: mutable_state_of(DevicesState::Loading),
            target: mutable_state_of(None),
            token: mutable_state_of(String::new()),
            name: mutable_state_of(String::new()),
            saving: mutable_state_of(false),
            failure: mutable_state_of(None),
            confirm: mutable_state_of(false),
            step: mutable_state_of(AddStep::cleared()),
            armed_at: mutable_state_of(0),
            review: false,
        }
    }

    /// The debug review hook (`EMBER_CR_PROJECT_SHEET=add-computer`, `EMBER_CR_ADD_COMPUTER=confirm`).
    pub fn for_review() -> AddUi {
        let mode = review_mode(
            std::env::var("EMBER_CR_PROJECT_SHEET").ok().as_deref(),
            std::env::var("EMBER_CR_ADD_COMPUTER").ok().as_deref(),
            cfg!(debug_assertions),
        );
        let mut ui = AddUi::new();
        if mode == ReviewMode::Off {
            return ui;
        }
        ui.review = true;
        ui.open.set(true);
        ui.devices.set(DevicesState::Ready(demo_devices()));
        if mode == ReviewMode::Confirm {
            ui.target.set(Some(REVIEW_DEVICE_ID.to_string()));
            ui.token.set(REVIEW_TOKEN.to_string());
            ui.step.set(AddStep::Confirm { device_id: REVIEW_DEVICE_ID.to_string(), name: None });
        }
        ui
    }

    /// The 컴퓨터 추가 chip: open the sheet and read the devices.
    pub fn show(&self) {
        self.open.set(true);
        if self.review {
            return;
        }
        load_devices(self.devices.clone());
    }

    fn leave(&self) {
        self.open.set(false);
        self.target.set(None);
        self.token.set(String::new());
        self.name.set(String::new());
        self.failure.set(None);
        self.confirm.set(false);
        self.step.set(AddStep::cleared());
    }
}

/// Reads the devices; the demo serves samples and calls nothing.
fn load_devices(state: MutableState<DevicesState>) {
    if services().demo.is_some() {
        state.set(DevicesState::Ready(demo_devices()));
        return;
    }
    state.set(DevicesState::Loading);
    run_shared(async move { services().client.api().hub_devices().await }, move |r| state.set(devices_state(r)));
}

/// Sends the token once, then refreshes the computer list and the devices.
fn add(ui: AddUi, endpoint_id: String, token: String, name: Option<String>) {
    if services().demo.is_some() || ui.review {
        ui.failure.set(Some(DEMO_CANNOT_ADD.to_string()));
        return;
    }
    ui.saving.set(true);
    ui.failure.set(None);
    run_shared(
        async move {
            let r = services().client.api().add_hub_computer(&endpoint_id, name.as_deref(), &token).await;
            if r.is_ok() {
                if let Ok(list) = services().server.computers(false).await {
                    let merged = services().client.read(|st| crate::model::merge_computers(st.computers(), &list));
                    services().client.apply(ember_client::state::Input::ComputersLoaded(merged));
                }
            }
            r
        },
        move |r| {
            ui.saving.set(false);
            match r {
                Ok(c) => {
                    ui.step.set(AddStep::cleared());
                    ui.target.set(None);
                    ui.token.set(String::new());
                    ui.name.set(String::new());
                    notice::post(if c.name.is_empty() { "컴퓨터를 추가했습니다".to_string() } else { format!("{}을 추가했습니다", c.name) });
                    load_devices(ui.devices.clone());
                }
                Err(e) => {
                    ui.step.set(AddStep::cleared());
                    ui.failure.set(Some(add_error_text(e.status()).to_string()))
                }
            }
        },
    );
}

pub fn AddComputerSheet(ui: &AddUi) {
    let p = ember_theme().palette;
    let target = ui.target.get();
    let saving = ui.saving.get();
    let blank = is_blank(&ui.token.get(), &ui.name.get());
    let guard = SettingsGuard::new(saving, blank || target.is_none());
    let request_close = {
        let u = ui.clone();
        move || match guard.on_close() {
            CloseAction::Ignore => {}
            CloseAction::Confirm => u.confirm.set(true),
            CloseAction::Close => u.leave(),
        }
    };
    let w = compose_rust::runtime::current_window_size();
    let body_h = super::components::sheet_body_max_height(w.width_dp, w.height_dp, false, 60.0).min(700.0);
    let sample = services().demo.is_some() || ui.review;
    EmberSheet("컴퓨터 추가", ui.open.get(), request_close.clone(), None, || {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(body_h)).content(|| {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                let muted = Some(paint(p.muted));
                if sample {
                    Label("예시 기기입니다. 데모에서는 서버를 쓰지 않습니다", Fs::Xs, Weight::Regular, muted, None);
                }
                match ui.devices.get() {
                    DevicesState::Loading => Label("기기를 불러오는 중입니다", Fs::Md, Weight::Regular, muted, None),
                    DevicesState::HubOff => Label(HUB_OFF, Fs::Md, Weight::Regular, muted, None),
                    DevicesState::Failed(m) => Label(&m, Fs::Md, Weight::Regular, Some(paint(p.red.strong)), None),
                    DevicesState::Ready(list) if list.is_empty() => Label(NO_DEVICES, Fs::Md, Weight::Regular, muted, None),
                    DevicesState::Ready(list) => {
                        for r in device_rows(&list, sample) {
                            DeviceRowView(&r, ui, target.as_deref() == Some(r.endpoint_id.as_str()), saving);
                        }
                    }
                }
            });
        });
        SheetFooter(|| {
            AppButton("닫기", None, ButtonKind::Default, true, true, false, request_close.clone());
        });
    });
    let keep = {
        let c = ui.confirm.clone();
        move || c.set(false)
    };
    EmberSheet("변경 사항", ui.confirm.get(), keep.clone(), None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
            Label(DISCARD_QUESTION, Fs::Md, Weight::Regular, None, Some(3));
        });
        SheetFooter(|| {
            AppButton("계속 편집", None, ButtonKind::Default, true, true, false, keep.clone());
            let u = ui.clone();
            AppButton("닫기", None, ButtonKind::Primary, true, true, false, move || u.leave());
        });
    });
}

fn DeviceRowView(r: &DeviceRow, ui: &AddUi, asking: bool, saving: bool) {
    let p = ember_theme().palette;
    let m = Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_MD);
    cu::Column().spacing(SP[2]).modifier(m.padding(SP[3])).content(|| {
        cu::Row().spacing(SP[2]).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(&r.name, Fs::Sm, Weight::Semibold, None, Some(1));
                Label(&r.kind, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
            if r.sample {
                Pill("예시", super::components::PillTone::Grey, true);
            }
            Pill(r.pill.0, r.pill.1.into(), true);
        });
        if r.can_add && !asking {
            let (u, id) = (ui.clone(), r.endpoint_id.clone());
            AppButton("컴퓨터로 추가", None, ButtonKind::Default, true, !saving && !r.sample, false, move || {
                u.failure.set(None);
                u.token.set(String::new());
                u.name.set(String::new());
                u.target.set(Some(id.clone()));
            });
        }
        if r.can_add && r.sample {
            Label(DEMO_CANNOT_ADD, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
        }
        if asking {
            if ui.step.get().is_confirm() {
                ConfirmAdd(r, ui, saving);
            } else {
                AskToken(r, ui, saving);
            }
        }
    });
}

/// The token and the optional name, then the send.
fn AskToken(r: &DeviceRow, ui: &AddUi, saving: bool) {
    let p = ember_theme().palette;
    let (token, name) = (ui.token.get(), ui.name.get());
    let valid = validate_add(&token, &name);
    Label(TOKEN_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    let (t, f) = (ui.token.clone(), ui.failure.clone());
    EmberField(FieldOptions {
        fill_max_width: true,
        placeholder: "토큰".to_string(),
        on_value_change: Some(Box::new(move |v| {
            f.set(None);
            t.set(v)
        })),
        ..Default::default()
    });
    Label(TOKEN_HELP, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
    Label(NAME_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    let n = ui.name.clone();
    EmberField(FieldOptions {
        fill_max_width: true,
        placeholder: r.name.clone(),
        on_value_change: Some(Box::new(move |v| n.set(v))),
        ..Default::default()
    });
    if let (Err(e), false) = (valid, token.is_empty() && name.is_empty()) {
        if e != AddError::TokenMissing || !token.is_empty() {
            Label(e.message(), Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
        }
    }
    if let Some(m) = ui.failure.get() {
        Label(&m, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
    }
    cu::Row().spacing(SP[1]).content(|| {
        let u = ui.clone();
        AppButton("취소", None, ButtonKind::Default, true, !saving, false, move || {
            u.target.set(None);
            u.token.set(String::new());
            u.name.set(String::new());
            u.failure.set(None);
            u.step.set(AddStep::cleared());
        });
        let (u, row) = (ui.clone(), r.clone());
        let ok = valid.is_ok();
        AppButton("컴퓨터로 추가", None, ButtonKind::Primary, true, ok && !saving, false, move || {
            if let Ok(step) = request_confirm(&row, &u.token.get_untracked(), &u.name.get_untracked()) {
                u.armed_at.set(crate::model::now_ms());
                u.step.set(step);
            }
        });
    });
}

/// The confirmation before the POST: device name, address and the name it gets. No token.
fn ConfirmAdd(r: &DeviceRow, ui: &AddUi, saving: bool) {
    let p = ember_theme().palette;
    let step = ui.step.get();
    let Some(rows) = step.rows(r) else { return };
    Label(CONFIRM_QUESTION, Fs::Sm, Weight::Medium, None, Some(4));
    for (k, v) in [("기기 이름", rows.device_name.as_str()), ("주소", rows.address_short.as_str()), ("컴퓨터 이름", rows.computer_name.as_str())] {
        cu::Row().spacing(SP[2]).content(|| {
            Label(k, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
            Label(v, Fs::Sm, Weight::Regular, None, Some(2));
        });
    }
    Label(&rows.address, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
    if let Some(m) = ui.failure.get() {
        Label(&m, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
    }
    cu::Row().spacing(SP[1]).content(|| {
        let u = ui.clone();
        AppButton("취소", None, ButtonKind::Default, true, !saving, false, move || {
            u.step.set(AddStep::cleared());
            u.token.set(String::new());
            u.failure.set(None);
        });
        let u = ui.clone();
        AppButton(if saving { "추가하는 중" } else { "확인하고 추가" }, None, ButtonKind::Primary, true, !saving, false, move || {
            let step = u.step.get_untracked();
            if !step.confirm_counts(u.armed_at.get_untracked(), crate::model::now_ms()) {
                return;
            }
            if let AddStep::Confirm { device_id, name } = step {
                add(u.clone(), device_id, u.token.get_untracked(), name);
            }
        });
    });
}
