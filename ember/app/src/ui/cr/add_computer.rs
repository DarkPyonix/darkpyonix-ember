//! The add-computer sheet of the Project screen (FR-C1 / FR-C2, D39, #490). It lists the
//! account's hub devices read-only. It takes no token and sends nothing: a new computer joins
//! with a one-time enrol code, and that screen is later. The decisions are in `ui::add_computer`.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;

use super::components::{AppButton, ButtonKind, EmberSheet, Label, Pill, SheetFooter};
use super::theme::*;
use crate::services::{run_shared, services};
use crate::ui::add_computer::*;

#[derive(Clone)]
pub struct AddUi {
    pub open: MutableState<bool>,
    devices: MutableState<DevicesState>,
    /// A debug review capture: sample devices, nothing calls the server.
    review: bool,
}

impl AddUi {
    pub fn new() -> AddUi {
        AddUi { open: mutable_state_of(false), devices: mutable_state_of(DevicesState::Loading), review: false }
    }

    /// The debug review hook (`EMBER_CR_PROJECT_SHEET=add-computer`).
    pub fn for_review() -> AddUi {
        let mode = review_mode(std::env::var("EMBER_CR_PROJECT_SHEET").ok().as_deref(), cfg!(debug_assertions));
        let mut ui = AddUi::new();
        if mode == ReviewMode::Off {
            return ui;
        }
        ui.review = true;
        ui.open.set(true);
        ui.devices.set(DevicesState::Ready(demo_devices()));
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

pub fn AddComputerSheet(ui: &AddUi) {
    let p = ember_theme().palette;
    let close = {
        let u = ui.clone();
        move || u.leave()
    };
    let w = compose_rust::runtime::current_window_size();
    let body_h = super::components::sheet_body_max_height(w.width_dp, w.height_dp, false, 60.0).min(700.0);
    let sample = services().demo.is_some() || ui.review;
    EmberSheet("컴퓨터 추가", ui.open.get(), close.clone(), None, || {
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
                            DeviceRowView(&r);
                        }
                    }
                }
                Label(ENROL_NOTE, Fs::Xs, Weight::Regular, muted, Some(3));
            });
        });
        SheetFooter(|| {
            AppButton("닫기", None, ButtonKind::Default, true, true, false, close.clone());
        });
    });
}

fn DeviceRowView(r: &DeviceRow) {
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
            Pill(r.online.0, r.online.1.into(), true);
        });
    });
}
