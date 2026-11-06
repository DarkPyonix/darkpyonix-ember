//! Accessible names of the icon-only controls of the rail, the channel header and the project
//! boxes (FR-L28, FR-L10, #501). The names are Korean and match the mockup's titles.
//!
//! The pinned compose-rust (v0.0.6) has no label or content-description modifier on a
//! container `on_click` (compose-rust#460, earlier #429), and a toolkit `Button` with an empty
//! text takes its name from the icon role, in English. Until the toolkit has the setter,
//! `Named` puts the label as a fully transparent `Text` child of the control; the renderer's
//! semantics merge descendant text into the control's name. TODO(compose-rust#460): replace
//! `Named` by the label modifier when it lands.

#![allow(non_snake_case)]

use compose_rust::ui as cu;
use compose_rust::ui::Modifier;

use super::components::Label;
use super::theme::{Fs, Weight};

pub const THEME: &str = "테마 전환";
pub const SETTINGS: &str = "설정";
pub const PLATFORM: &str = "플랫폼";
pub const MENU: &str = "메뉴";
pub const NEW_CHANNEL: &str = "새 채널";
pub const MEMBERS: &str = "멤버";
pub const MORE: &str = "더 보기";
pub const ATTACH: &str = "첨부";
pub const EMOJI: &str = "이모지";
pub const VERSION_CONTROL: &str = "버전 관리";
pub const PROJECT_SETTINGS: &str = "프로젝트 설정";
pub const OPEN_SESSION: &str = "세션 열기";

/// Every label of the icon-only controls outside the nav menus (those take `NavMenu::label`).
pub fn rail_labels() -> [&'static str; 12] {
    [
        THEME, SETTINGS, PLATFORM, MENU, NEW_CHANNEL, MEMBERS, MORE, ATTACH, EMOJI,
        VERSION_CONTROL, PROJECT_SETTINGS, OPEN_SESSION,
    ]
}

/// The accessible name of the control this is composed inside: an invisible one-line text.
pub fn Named(label: &str) {
    cu::Box()
        .modifier(Modifier::new().alpha(0.0))
        .content(|| Label(label, Fs::Xs, Weight::Regular, None, Some(1)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l28_icon_only_labels_are_korean_non_empty_and_unique() {
        let labels = rail_labels();
        for l in labels {
            assert!(!l.is_empty());
            assert!(l.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)), "{l} is not Korean");
        }
        let mut sorted = labels.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "labels repeat: {labels:?}");
    }

    #[test]
    fn fr_l28_project_box_icons_are_not_the_english_toolkit_roles() {
        let source = include_str!("project.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap();
        for role in ["IconRole::History, true", "IconRole::Settings, true", "icon(IconRole::Forward)"] {
            assert!(!prod.contains(role), "project box still uses the toolkit glyph {role}");
        }
    }
}
