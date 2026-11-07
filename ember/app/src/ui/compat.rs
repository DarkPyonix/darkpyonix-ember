//! Tone colour mapping helper.

use dioxus_compose::prelude::Color;

use crate::model::Tone;
use crate::ui::theme::Palette;

/// `(fill, ink)` of a tone: the strong colour on its soft pair, from the palette.
pub fn tone_colors(tone: Tone, p: &Palette) -> (Color, Color) {
    match tone {
        Tone::Neutral => (p.grey.soft, p.text_2),
        Tone::Accent => (p.blue.soft, p.blue.strong),
        Tone::Warning => (p.orange.soft, p.orange.strong),
        Tone::Error => (p.red.soft, p.red.strong),
        Tone::Success => (p.green.soft, p.green.strong),
    }
}
