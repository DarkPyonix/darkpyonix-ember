//! The bundled app font: Inter, then Pretendard, then system fonts, for every role but Mono.

use std::path::Path;

use dioxus_compose::prelude::*;
use ember_app::ui::fonts::{app_theme, INTER, LICENSES, PRETENDARD};

fn fonts_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts"))
}

#[test]
fn every_bundled_face_is_an_sfnt_font() {
    for face in INTER.iter().chain(PRETENDARD.iter()) {
        let magic = &face.bytes[..4];
        assert!(
            magic == [0x00, 0x01, 0x00, 0x00] || magic == b"OTTO",
            "{} is not a TrueType or OpenType font",
            face.file
        );
        assert!(fonts_dir().join(face.file).is_file(), "{} is missing", face.file);
    }
}

#[test]
fn license_texts_sit_beside_the_faces() {
    for license in LICENSES {
        let text = std::fs::read_to_string(fonts_dir().join(license)).expect(license);
        assert!(text.contains("SIL Open Font License, Version 1.1"), "{license} is not the OFL");
    }
}

#[test]
fn app_theme_sets_the_bundled_font_except_for_mono() {
    let theme = app_theme(ColorScheme::Light);
    assert!(theme.font(TypeRole::Body).is_some_and(|id| id != 0));
    assert!(theme.font(TypeRole::Title).is_some_and(|id| id != 0));
    assert_eq!(theme.font(TypeRole::Body), theme.font(TypeRole::Title));
    assert_eq!(theme.font(TypeRole::Mono), None);
}
