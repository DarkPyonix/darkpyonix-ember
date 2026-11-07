//! The bundled app font: Inter for Latin, then Pretendard for Hangul, then the system fonts.
//!
//! Both are SIL OFL 1.1 and ship beside their license texts in `assets/fonts/`. They are
//! bundled so Windows and Linux render like the macOS mockup; Apple SD Gothic Neo cannot be
//! redistributed. Only the three weights `ui::theme::Weight` can name (400, 500, 600) are
//! included, because a face no text asks for is only download size.

use std::sync::OnceLock;

use compose_rust::{asset, AssetKind, Color, ColorRole, ColorScheme, DesignSystem, FontStack, Palette, Theme};

/// One bundled face: its file name under `assets/fonts/` and its bytes.
pub struct Face {
    pub file: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! face {
    ($file:literal) => {
        Face { file: $file, bytes: include_bytes!(concat!("../../assets/fonts/", $file)) }
    };
}

/// Inter 4.1 static faces (rsms/inter release v4.1, `extras/otf`).
pub static INTER: [Face; 3] =
    [face!("Inter-Regular.otf"), face!("Inter-Medium.otf"), face!("Inter-SemiBold.otf")];

/// Pretendard 1.3.9 static faces (orioncactus/pretendard release v1.3.9, `public/static`).
pub static PRETENDARD: [Face; 3] = [
    face!("Pretendard-Regular.otf"),
    face!("Pretendard-Medium.otf"),
    face!("Pretendard-SemiBold.otf"),
];

/// The license file that sits beside each family's faces.
pub const LICENSES: [&str; 2] = ["Inter-OFL.txt", "Pretendard-OFL.txt"];

fn register(faces: &[Face]) -> Vec<u32> {
    faces.iter().map(|f| asset(AssetKind::Font, f.bytes)).collect()
}

/// Registers the faces and the stack once and returns the stack's font id.
///
/// Registrations ride the batch and are re-sent to a new Host, so calling this while the
/// theme is built is early enough. The id is cached because the theme is built once per launch
/// but the call costs nothing to repeat.
pub fn app_font() -> u32 {
    static STACK: OnceLock<u32> = OnceLock::new();
    *STACK.get_or_init(|| {
        FontStack::new().family(register(&INTER)).family(register(&PRETENDARD)).register()
    })
}

// Overlay surfaces and ink follow the same tokens as Ember's own components.
const APP_PALETTE: Palette = Palette::new()
    .with(ColorRole::Background, Color::rgb(0xf3f2ee), Color::rgb(0x151514))
    .with(ColorRole::Surface, Color::rgb(0xffffff), Color::rgb(0x1f1f1d))
    .with(ColorRole::SurfaceContainer, Color::rgb(0xffffff), Color::rgb(0x1f1f1d))
    .with(ColorRole::SurfaceVariant, Color::rgb(0xf7f6f2), Color::rgb(0x252523))
    .with(ColorRole::OnSurface, Color::rgb(0x1c1b19), Color::rgb(0xf0eee9))
    .with(ColorRole::OnSurfaceVariant, Color::rgb(0x56534d), Color::rgb(0xc3bfb7))
    .with(ColorRole::OutlineVariant, Color::rgb(0xebe9e4), Color::rgb(0x2f2e2b));

/// The toolkit theme the app launches with: every role except Mono in the bundled font.
pub fn app_theme(scheme: ColorScheme) -> Theme {
    Theme::unified(DesignSystem::Cupertino)
        .with_palette(&APP_PALETTE)
        .with_color_scheme(scheme)
        .with_default_font(app_font())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr_l3_app_theme_is_unified() {
        let theme = app_theme(ColorScheme::Light);
        assert!(!theme.adaptive);
        assert_eq!(theme.design_system, DesignSystem::Cupertino);
        assert_eq!(theme.color_scheme, ColorScheme::Light);
    }
}
