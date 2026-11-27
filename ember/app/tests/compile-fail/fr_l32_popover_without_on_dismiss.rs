// A modal closes on Escape, on the scrim and on its close button through one dismiss
// handler, so a Popover called without one must not compile.
use compose_rust::ui::Modifier;

#[allow(dead_code)]
fn modal(open: bool, modifier: Modifier) {
    { use ::compose_rust::ui::Popover; Popover!(open, modifier = modifier) };
}

fn main() {}
