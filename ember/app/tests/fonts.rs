//! The bundled app fonts: Inter, then Pretendard, then system fonts, for every role but Mono;
//! JetBrains Mono, then Pretendard, for Mono (design review 2026-11-13 C6).

use std::path::Path;

use compose_rust::{ColorScheme, TypeRole};
use ember_app::ui::fonts::{app_theme, editor_theme, mono_font, INTER, JETBRAINS_MONO, LICENSES, PRETENDARD};

fn fonts_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts"))
}

#[test]
fn every_bundled_face_is_an_sfnt_font() {
    for face in INTER.iter().chain(PRETENDARD.iter()).chain(JETBRAINS_MONO.iter()) {
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
fn app_theme_sets_the_bundled_font_and_jetbrains_mono_for_mono() {
    let theme = app_theme(ColorScheme::Light);
    assert!(theme.font(TypeRole::Body).is_some_and(|id| id != 0));
    assert!(theme.font(TypeRole::Title).is_some_and(|id| id != 0));
    assert_eq!(theme.font(TypeRole::Body), theme.font(TypeRole::Title));
    assert_eq!(theme.font(TypeRole::Mono), Some(mono_font()));
    assert_ne!(theme.font(TypeRole::Mono), theme.font(TypeRole::Body));
}

#[test]
fn design_c6_jetbrains_mono_ships_with_its_license() {
    let files: Vec<&str> = JETBRAINS_MONO.iter().map(|f| f.file).collect();
    assert_eq!(files, ["JetBrainsMono-Regular.ttf", "JetBrainsMono-Medium.ttf", "JetBrainsMono-Italic.ttf"]);
    assert!(LICENSES.contains(&"JetBrainsMono-OFL.txt"));
    let text = std::fs::read_to_string(fonts_dir().join("JetBrainsMono-OFL.txt")).unwrap();
    assert!(text.contains("JetBrains Mono Project Authors"));
    assert_eq!(editor_theme(ColorScheme::Dark).font(TypeRole::Mono), Some(mono_font()));
}
