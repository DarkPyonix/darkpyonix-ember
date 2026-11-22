// The leader mark is read only: it takes the value it shows and the member's name, and no
// change handler, so a caller cannot make a press do anything.
use ember_app::ui::cr::project::LeaderMark;

#[allow(dead_code)]
fn mark() {
    LeaderMark(true, "m-a", |_: bool| {});
}

fn main() {}
