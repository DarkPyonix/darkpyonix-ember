//! A window that shows the phase 1 components, for the side-by-side with the mockup (#220).
//!
//! Off by default. `EMBER_CR_GALLERY=light` or `=dark` (or `=1` for light) opens this window
//! instead of the app; the button at the top flips the scheme without saving the preference.
//! It goes away with the last phase.

use compose_rust::runtime::{composable, mutable_state_of, remember};
use compose_rust::schema::Window;
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ColorScheme, DesignSystem, IconRole, Theme, TypeRole};

use super::compat::*;
use super::components::*;
use super::theme::*;
use crate::model::Tone;

/// The scheme asked for by `EMBER_CR_GALLERY`, or `None` when the gallery is off.
pub fn requested() -> Option<ThemePref> {
    match std::env::var("EMBER_CR_GALLERY").ok()?.as_str() {
        "" | "0" => None,
        "dark" => Some(ThemePref::Dark),
        _ => Some(ThemePref::Light),
    }
}

/// Opens the gallery window and returns when it is closed. `services` must be installed.
pub fn launch(pref: ThemePref) {
    crate::ui::coexist::shared_ui().theme_pref.set(pref);
    let scheme = if pref == ThemePref::Dark { ColorScheme::Dark } else { ColorScheme::Light };
    compose_rust::LaunchBuilder::new()
        .with_theme(Theme::adaptive(DesignSystem::Material3).with_color_scheme(scheme))
        .with_window(Window::new().with_title("Ember components (compose-rust)").with_size(1100, 800))
        .launch(gallery);
}

fn section(title: &str, content: impl FnOnce()) {
    Label(title, Fs::Xs, Weight::Medium, Some(paint(ember_theme().palette.muted)), None);
    cu::Row().space_role(compose_rust::SpaceRole::Sm).alignment(Alignment::CenterStart).content(content);
}

#[composable]
pub fn gallery() {
    let theme = ember_theme();
    let p = theme.palette;
    let sheet_open = remember(|| mutable_state_of(false));
    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_size().background(paint(p.bg)).padding(24.0))
        .content(|| {
            cu::Row().space_role(compose_rust::SpaceRole::Sm).content(|| {
                Label("Components", Fs::Xl, Weight::Semibold, None, None);
                AppButton("라이트/다크 전환", None, ButtonKind::Default, true, true, false, || {
                    let ui = crate::ui::coexist::shared_ui();
                    let next = if ui.theme_pref.get_untracked() == ThemePref::Dark {
                        ThemePref::Light
                    } else {
                        ThemePref::Dark
                    };
                    ui.theme_pref.set(next);
                });
                let open = sheet_open.clone();
                AppButton("시트 열기", None, ButtonKind::Primary, true, true, false, move || open.set(true));
            });
            section("Buttons", || {
                AppButton("보내기", None, ButtonKind::Primary, false, true, false, || {});
                AppButton("취소", None, ButtonKind::Default, false, true, false, || {});
                AppButton("삭제", None, ButtonKind::Destructive, false, true, false, || {});
                AppButton("작게", Some(IconRole::Close), ButtonKind::Default, true, true, false, || {});
                IconButton(IconRole::Close, false, || {});
                IconButton(IconRole::Close, true, || {});
            });
            section("Pick chips", || {
                PickChip("프로젝트", Some(IconRole::Close), false, false, || {});
                PickChip("선택됨", None, true, false, || {});
                PickChip("아이콘만", Some(IconRole::Close), false, true, || {});
            });
            section("Pills", || {
                for (text, tone) in [
                    ("진행 중", PillTone::Blue),
                    ("대기", PillTone::Orange),
                    ("실패", PillTone::Red),
                    ("완료", PillTone::Green),
                    ("정보", PillTone::Sky),
                    ("보관", PillTone::Grey),
                ] {
                    Pill(text, tone, false);
                }
                Pill("작은 알약", PillTone::Blue, true);
            });
            section("Badges, dots, spinner", || {
                for tone in [Tone::Neutral, Tone::Accent, Tone::Warning, Tone::Error, Tone::Success] {
                    Badge("상태", tone);
                    Dot(tone);
                }
                Spinner(12.0);
            });
            section("Radius ruler (8, 12, 14, 24 dp)", || {
                for r in [8.0_f32, 12.0, 14.0, 24.0] {
                    cu::Box()
                        .modifier(
                            Modifier::new()
                                .size(64.0, 64.0)
                                .background(paint(p.ink))
                                .corner_radius(r),
                        )
                        .content(|| {});
                }
            });
            section("Cards", || {
                EmberCard(false, false, None, None, None, || {
                    Label("카드", Fs::Lg, Weight::Semibold, None, None);
                    InsetPanel(false, || Label("안쪽 패널", Fs::Md, Weight::Regular, None, None));
                });
                EmberCard(true, false, None, None, None, || {
                    Label("선택된 카드", Fs::Lg, Weight::Semibold, None, None);
                });
            });
            section("Text and field", || {
                cu::Column().content(|| {
                    Txt("Headline", TypeRole::Headline, None, None, None, None, None);
                    Txt("Body text 본문 글자", TypeRole::Body, None, None, None, None, None);
                    Txt("Caption", TypeRole::Caption, None, None, None, None, None);
                    CodeText("cargo test --lib", Some(2), false);
                    Banner("배너 메시지", Tone::Warning);
                    EmberField(FieldOptions {
                        placeholder: "메시지를 입력하세요".to_string(),
                        fill_max_width: true,
                        ..Default::default()
                    });
                });
            });
            let close = sheet_open.clone();
            EmberSheet("시트 제목", sheet_open.get(), move || close.set(false), None, || {
                Label("시트 내용", Fs::Md, Weight::Regular, None, None);
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    #[test]
    fn fr_l28_gallery_composes_every_phase_1_component() {
        let mut host = Host::new(|| Box::new(Recomposer::new(gallery)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        for want in ["Components", "보내기", "프로젝트", "진행 중", "카드", "배너 메시지"] {
            assert!(
                batch.iter().any(|m| matches!(m, Mutation::SetProp { value: PropertyValue::String(t), .. } if *t == want)),
                "gallery misses {want}"
            );
        }
    }
}
