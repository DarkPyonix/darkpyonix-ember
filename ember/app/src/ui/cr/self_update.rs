//! The "업데이트" section of the settings sheet (SPEC NFR-R2, #880): the running version, where
//! the update check stands, 업데이트 확인, and 업데이트 후 다시 시작 once an update is
//! downloaded. Logic and texts in `crate::self_update`.

#![allow(non_snake_case)]

use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, TypeRole};

use super::components::{AppButton, ButtonKind, InsetPanel, Label, Txt};
use super::theme::*;
use crate::self_update::{
    self, can_check, can_restart, state_text, UpdateState, CHECK_LABEL, RESTART_LABEL, RUNNING_VERSION,
    VERSION_LABEL,
};
use crate::services::try_services;
use crate::ui::coexist::shared_ui;

pub use crate::self_update::{SUBTITLE, TITLE};

pub fn SelfUpdateSection() {
    let p = ember_theme().palette;
    let state = shared_ui().self_update.get();
    let line = state_text(&state);
    let line_color = if matches!(state, UpdateState::Failed { .. }) { p.red.strong } else { p.text_2 };

    InsetPanel(true, || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .modifier(Modifier::new().fill_max_width().height(24.0))
                .content(|| {
                    Label(VERSION_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Txt(RUNNING_VERSION, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
                });
            Label(&line, Fs::Xs, Weight::Regular, Some(paint(line_color)), Some(3));
        });
    });

    cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
        AppButton(CHECK_LABEL, None, ButtonKind::Default, true, can_check(&state), false, || {
            if let Some(s) = try_services() {
                self_update::request_check(s.rt.handle());
            }
        });
        if can_restart(&state) {
            AppButton(RESTART_LABEL, None, ButtonKind::Primary, true, true, false, || {
                // Prefs and cache are written before Velopack replaces the app.
                self_update::request_apply(|| {
                    let _ = crate::shutdown();
                });
            });
        }
    });
}
