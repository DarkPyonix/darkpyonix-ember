// A modal closes on Escape, on the scrim and on its close button through one dismiss
// handler, so a Dialog called without one must not compile.
use compose_rust::ui::Modifier;

#[allow(dead_code)]
fn modal(open: bool, modifier: Modifier) {
    { use ::compose_rust::ui::Dialog; Dialog!(open, modifier = modifier) };
}

fn main() {}
