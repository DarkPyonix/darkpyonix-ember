// A modal closes on Escape, on the scrim and on its close button through one dismiss
// handler, so a Sheet called without one must not compile.
use compose_rust::ui::Modifier;

#[allow(dead_code)]
fn modal(open: bool, modifier: Modifier) {
    { use ::compose_rust::ui::Sheet; Sheet!(open, modifier = modifier) };
}

fn main() {}
