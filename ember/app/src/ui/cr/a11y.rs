//! Accessible names of the icon-only controls of the rail, the channel header and the project
//! boxes (FR-L28, FR-L10, #501). The names are Korean and match the mockup's titles.
//!
//! compose-rust v0.0.7 names an icon-only `on_click` container with `Modifier::label`
//! (compose-rust#460, #429): `components::NamedClickable` passes these constants to it. The
//! toolkit keeps every distinct label for the life of the program, so only this fixed set goes
//! through `label`; a name built from data (a routine name) takes a `Button`'s
//! `accessible_name`, which is owned.

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
pub const CLOSE: &str = "닫기";
pub const SEND: &str = "보내기";
pub const SHARE: &str = "공유 시트 열기";
pub const OTHER_IDE: &str = "다른 IDE 선택";
pub const SESSION_MENU: &str = "세션 메뉴";
pub const OPEN_SCREEN: &str = "화면 열기";

/// The reviewed Korean names of the icon-only controls outside the nav menus (those take
/// `NavMenu::label`).
pub fn rail_labels() -> [&'static str; 18] {
    [
        THEME, SETTINGS, PLATFORM, MENU, NEW_CHANNEL, MEMBERS, MORE, ATTACH, EMOJI,
        VERSION_CONTROL, PROJECT_SETTINGS, OPEN_SESSION, CLOSE, SEND, SHARE, OTHER_IDE,
        SESSION_MENU, OPEN_SCREEN,
    ]
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
