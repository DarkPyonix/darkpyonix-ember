//! compose-rust settings sheet (#220 phase 8): theme preference (FR-L28), editor keymap and
//! shortcut reference (FR-L30), archived-session visibility (FR-L9), this device on the
//! transport (FR-N9), the editor server settings for the owner (FR-W7), updates (NFR-R2), and
//! server/data paths.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ShapeRole, TypeRole};

use super::components::{
    AppButton, ButtonKind, Choice, ChoiceButton, ChoiceRow, Clickable, EmberSheet, InsetPanel, Label, Pill, PillTone,
    SheetFooter, Txt,
};
use super::theme::*;
use crate::services::try_services;
use crate::ui::coexist::shared_ui;
use crate::ui::keymap::{Action, Keymap, Os};

/// One row of the shortcut reference table (`Action::ALL`) for `keymap` on `os`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRow {
    pub action: Action,
    pub label: &'static str,
    pub chord: String,
}

/// All bound shortcuts for `keymap` on `os` (unbound actions are omitted).
pub fn shortcut_rows(keymap: Keymap, os: Os) -> Vec<ShortcutRow> {
    Action::ALL
        .into_iter()
        .filter_map(|action| {
            let chord = keymap.hint(action, os);
            if chord.is_empty() {
                None
            } else {
                let label = match action {
                    Action::Find => keymap.find_label(),
                    Action::Palette => keymap.palette_label(),
                    other => other.label(),
                };
                Some(ShortcutRow { action, label, chord })
            }
        })
        .collect()
}

/// The settings modal (`EmberSheet`), composed with `open` and `on_dismiss`.
pub fn SettingsSheet(open: bool, on_dismiss: impl FnMut() + Clone + 'static) {
    let close_footer = on_dismiss.clone();
    EmberSheet("설정", open, on_dismiss, None, move || {
        SettingsContent();
        SheetFooter(move || {
            let mut dismiss = close_footer.clone();
            AppButton("닫기", None, ButtonKind::Default, true, true, false, move || dismiss());
        });
    });
}

/// Body of the settings sheet: theme (FR-L28), keymap (FR-L30), archived sessions (FR-L9), the
/// editor server settings (FR-W7), and server/data paths. There is no device-wide default IDE:
/// the chooser remembers the last pick per project (FR-W7b), and the default for a project with
/// no pick is the server's editor settings (`default_ide`, FR-W7, INTENT D56), edited in the
/// "에디터 서버" section by the owner.
pub fn SettingsContent() {
    let theme = ember_theme();
    let p = theme.palette;
    let ui = shared_ui().clone();
    let current_theme = ui.theme_pref.get();
    let prefs = ui.prefs.get();
    let current_keymap = prefs.keymap;
    let show_archived = ui.show_archived.get();
    let shortcuts_open = remember(|| mutable_state_of(false));

    let (server_url, data_dir) = match try_services() {
        Some(s) => (
            s.config.server_url.clone(),
            s.config
                .data_dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "메모리 전용".to_string()),
        ),
        None => ("http://127.0.0.1:8880".to_string(), "기본 경로".to_string()),
    };

    { use ::compose_rust::ui::Column; Column!(spacing = SP[3], modifier = Modifier::new().fill_max_width().padding(SP[4]), {
            // 1. Theme preference (FR-L28)
            SectionBlock("화면 테마", "앱 전체의 색상 모드를 선택합니다.", || {
                ChoiceRow(|| {
                    for pref in [ThemePref::System, ThemePref::Light, ThemePref::Dark] {
                        let u = ui.clone();
                        ChoiceButton(pref.label(), Choice::Radio, current_theme == pref, true, true, move || {
                            u.set_theme_pref(pref);
                        });
                    }
                });
            });

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 2. Editor keymap (FR-L30)
            SectionBlock(
                "에디터 단축키",
                "Ember 에디터의 기본 단축키 체계를 선택합니다.",
                || {
                    { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                        // The choices wrap in the room the table button leaves: a zero width
                        // keeps a weighted child from asking for its whole line first.
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).width(0.0), {
                            ChoiceRow(|| {
                                for km in Keymap::ALL {
                                    let u = ui.clone();
                                    ChoiceButton(km.label(), Choice::Radio, current_keymap == km, true, true, move || {
                                        u.update_prefs(|pr| pr.keymap = km);
                                    });
                                }
                            });
                        }) };
                        let toggle = shortcuts_open.clone();
                        let is_open = shortcuts_open.get();
                        AppButton(
                            if is_open { "단축키 표 접기" } else { "단축키 표 보기" },
                            None,
                            ButtonKind::Default,
                            true,
                            true,
                            false,
                            move || toggle.set(!is_open),
                        );
                    }) };
                    if shortcuts_open.get() {
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
                        ShortcutTableView(current_keymap, Os::current());
                    }
                },
            );

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 3. Archived sessions visibility (FR-L9)
            SectionBlock(
                "세션 목록",
                "보관 처리된 이전 세션을 목록에 함께 표시합니다.",
                || {
                    let u = ui.clone();
                    Clickable(
                        Modifier::new()
                            .fill_max_width()
                            .height(H_BTN)
                            .background(paint(p.surface_2))
                            .corner_radius(CORNER_MD),
                        Alignment::CenterStart,
                        move || u.set_show_archived(!show_archived),
                        || {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                                    Label(
                                        "보관된 세션 표시",
                                        Fs::Sm,
                                        Weight::Medium,
                                        Some(paint(p.text)),
                                        Some(1),
                                    );
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                    Pill(
                                        if show_archived { "켜짐" } else { "꺼짐" },
                                        if show_archived { PillTone::Green } else { PillTone::Grey },
                                        true,
                                    );
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                                }) };
                        },
                    );
                },
            );

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 4. This device on the transport (FR-N9, #856)
            SectionBlock(
                super::this_device::TITLE,
                super::this_device::SUBTITLE,
                super::this_device::ThisDeviceSection,
            );

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 5. Per-device scope (FR-N8, #817)
            SectionBlock(
                "기기 권한",
                "서버에 등록된 기기마다 할 수 있는 작업을 정합니다.",
                super::device_scope::DeviceScopeSection,
            );

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 6. The editor server settings, owner only (FR-W7, INTENT D56)
            SectionBlock(
                super::editor_settings::TITLE,
                super::editor_settings::SUBTITLE,
                super::editor_settings::EditorServerSection,
            );

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 7. Updates of the installed app (NFR-R2, #880)
            SectionBlock(
                super::self_update::TITLE,
                super::self_update::SUBTITLE,
                super::self_update::SelfUpdateSection,
            );

            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            // 8. Connection & local storage info
            SectionBlock("연결 및 저장소", "현재 연결된 메인 서버 주소와 로컬 데이터 폴더입니다.", || {
                InsetPanel(true, || {
                    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                        InfoRow("메인 서버", &server_url);
                        InfoRow("데이터 경로", &data_dir);
                    }) };
                });
            });
        }) };
}

fn SectionBlock(title: &str, subtitle: &str, content: impl FnOnce()) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
        Label(title, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
        Label(subtitle, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(2.0)) };
        content();
    }) };
}

fn InfoRow(label: &str, value: &str) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(24.0), {
            Label(label, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            Txt(value, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
        }) };
}

fn ShortcutTableView(keymap: Keymap, os: Os) {
    let p = ember_theme().palette;
    let rows = shortcut_rows(keymap, os);
    InsetPanel(true, || {
        { use ::compose_rust::ui::Column; Column!(spacing = 4.0, modifier = Modifier::new().fill_max_width(), {
            for row in rows.iter().take(8) {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(24.0), {
                        Label(row.label, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = Modifier::new()
                                    .height(20.0)
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .shape_role(ShapeRole::Full), {
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                                Txt(
                                    &row.chord,
                                    TypeRole::Mono,
                                    Some(paint(p.text)),
                                    Some(1),
                                    None,
                                    None,
                                    None,
                                );
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                            }) };
                    }) };
            }
        }) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    fn texts(mutations: &[Mutation<'_>]) -> Vec<String> {
        mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l30_shortcut_rows_reflect_selected_keymap_and_os() {
        let vs_mac = shortcut_rows(Keymap::VsCode, Os::Mac);
        let jb_mac = shortcut_rows(Keymap::JetBrains, Os::Mac);
        assert!(vs_mac.iter().any(|r| r.action == Action::Find && r.label == "파일로 이동" && r.chord == "\u{2318}P"));
        assert!(jb_mac.iter().any(|r| r.action == Action::Find && r.label == "모든 곳에서 찾기" && r.chord == "\u{21E7}\u{21E7}"));
        assert!(vs_mac.iter().all(|r| !r.chord.is_empty()));
        assert!(jb_mac.iter().all(|r| !r.chord.is_empty()));
    }

    #[test]
    fn fr_l28_fr_l30_settings_sheet_composes_and_updates_prefs() {
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        ui.set_theme_pref(ThemePref::System);
        ui.update_prefs(|p| {
            p.keymap = Keymap::VsCode;
        });
        ui.set_show_archived(false);

        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                SettingsSheet(true, || {});
            }))
        });
        let raw = host.rebuild().expect("rebuild").to_vec();
        let batch = decode_batch(&raw).expect("decode");
        let all = texts(&batch);
        for want in [
            "설정",
            "화면 테마",
            "시스템",
            "라이트",
            "다크",
            "에디터 단축키",
            "VS Code",
            "JetBrains",
            "보관된 세션 표시",
            "꺼짐",
            "메인 서버",
            "이 기기",
            "이 기기의 peer id",
            "서버 주소",
            "릴레이 (선택)",
            "저장",
            "닫기",
        ] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        // FR-W7: the demo is the owner, so the sheet has the editor server section's form.
        for want in [crate::ui::cr::editor_settings::TITLE, "Code OSE", "직접 스크립트", crate::ui::cr::editor_settings::LICENSE_URL, "IDE 기본값"] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        // FR-W7b, INTENT D56: no device-wide default IDE; the chooser remembers per project and
        // the server's editor settings hold the default (FR-W7).
        for gone in ["기본 IDE", "매번 선택", "JetBrains Gateway"] {
            assert!(!all.iter().any(|t| t == gone), "{gone} is not in the settings: {all:?}");
        }

        // Updating theme, keymap and show_archived re-renders the sheet cleanly.
        ui.set_theme_pref(ThemePref::Dark);
        ui.update_prefs(|p| {
            p.keymap = Keymap::JetBrains;
        });
        ui.set_show_archived(true);

        let next = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
        let next_texts = texts(&next);
        assert!(next_texts.iter().any(|t| t == "켜짐"), "expected 켜짐 after toggling show_archived: {next_texts:?}");
        assert_eq!(ui.prefs.get_untracked().keymap, Keymap::JetBrains);
        assert_eq!(ui.theme_pref.get_untracked(), ThemePref::Dark);

        // Restore defaults for other tests.
        ui.set_theme_pref(ThemePref::System);
        ui.set_show_archived(false);
        ui.update_prefs(|p| {
            p.keymap = Keymap::VsCode;
        });
    }

    #[test]
    fn nfr_r2_settings_show_the_version_and_the_update_state() {
        use crate::self_update::UpdateState;
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        let before = ui.self_update.get_untracked();
        ui.self_update.set(UpdateState::NotInstalled);

        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                SettingsSheet(true, || {});
            }))
        });
        let raw = host.rebuild().expect("rebuild").to_vec();
        let all = texts(&decode_batch(&raw).expect("decode"));
        for want in [
            "업데이트",
            "현재 버전",
            env!("CARGO_PKG_VERSION"),
            "업데이트 확인",
            "설치 프로그램으로 설치한 앱이 아니므로 업데이트를 확인하지 않습니다.",
        ] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        assert!(!all.iter().any(|t| t == "업데이트 후 다시 시작"), "no restart without a downloaded update");

        ui.self_update.set(UpdateState::Ready("9.9.9".into()));
        let next = texts(&decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode"));
        assert!(next.iter().any(|t| t == "업데이트 후 다시 시작"), "{next:?}");
        assert!(next.iter().any(|t| t == "새 버전 9.9.9을(를) 내려받았습니다. 다시 시작하면 적용됩니다."), "{next:?}");

        ui.self_update.set(before);
    }

    /// Find the `Clickable` ancestor of the `Text` node carrying `label` and return its
    /// `(node_id, handler_id)` for `PropertyKind::OnPointer`.
    fn clickable_for_label(batch: &[Mutation<'_>], label: &str) -> (u32, u64) {
        use compose_rust::PropertyKind;
        use std::collections::HashMap;

        let mut parent_of: HashMap<u32, u32> = HashMap::new();
        let mut pointer_handlers: HashMap<u32, u64> = HashMap::new();
        let mut label_node: Option<u32> = None;

        for m in batch {
            match m {
                Mutation::Insert {
                    parent_id, node_id, ..
                } => {
                    parent_of.insert(*node_id, *parent_id);
                }
                Mutation::SetProp {
                    node_id,
                    property: PropertyKind::OnClick,
                    value: PropertyValue::Integer(h),
                } => {
                    pointer_handlers.insert(*node_id, *h as u64);
                }
                Mutation::SetProp {
                    node_id,
                    property: PropertyKind::Text,
                    value: PropertyValue::String(s),
                } if *s == label => {
                    label_node = Some(*node_id);
                }
                _ => {}
            }
        }

        let mut cur = label_node.unwrap_or_else(|| panic!("missing label node for {label}"));
        loop {
            if let Some(&h) = pointer_handlers.get(&cur) {
                return (cur, h);
            }
            cur = *parent_of
                .get(&cur)
                .unwrap_or_else(|| panic!("no Clickable ancestor for label {label}"));
        }
    }

    /// The renderer fires `OnClick` for a container `on_click` (compose-rust#372).
    fn pointer_up_event(node_id: u32, handler_id: u64) -> compose_rust::protocol::HostEvent<'static> {
        use compose_rust::protocol::HostEvent;
        use compose_rust::EventPayload;
        HostEvent::new(node_id, handler_id, EventPayload::Clicked)
    }

    #[test]
    fn fr_l28_fr_l30_clicking_settings_chips_updates_theme_keymap_and_archived() {
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        ui.set_theme_pref(ThemePref::System);
        ui.update_prefs(|p| {
            p.keymap = Keymap::VsCode;
        });
        ui.set_show_archived(false);

        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                SettingsSheet(true, || {});
            }))
        });
        let raw = host.rebuild().expect("rebuild").to_vec();
        let batch = decode_batch(&raw).expect("decode");

        // 1. Click "다크" theme chip -> theme_pref becomes Dark.
        let (node, handler) = clickable_for_label(&batch, "다크");
        host.dispatch(pointer_up_event(node, handler)).expect("click 다크");
        assert_eq!(ui.theme_pref.get_untracked(), ThemePref::Dark);

        // 2. Click "JetBrains" keymap chip -> prefs.keymap becomes JetBrains.
        let (node, handler) = clickable_for_label(&batch, "JetBrains");
        host.dispatch(pointer_up_event(node, handler)).expect("click JetBrains");
        assert_eq!(ui.prefs.get_untracked().keymap, Keymap::JetBrains);

        // 3. Click "꺼짐" archived toggle -> show_archived becomes true.
        let (node, handler) = clickable_for_label(&batch, "꺼짐");
        host.dispatch(pointer_up_event(node, handler)).expect("click 꺼짐");
        assert!(ui.show_archived.get_untracked());

        // Restore defaults.
        ui.set_theme_pref(ThemePref::System);
        ui.set_show_archived(false);
        ui.update_prefs(|p| {
            p.keymap = Keymap::VsCode;
        });
    }
}
