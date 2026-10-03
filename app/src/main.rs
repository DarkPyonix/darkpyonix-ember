//! The desktop program. Everything is in the library beside this (dioxus-compose's sample
//! shape), so mobile entry points can reach the same screens later.

// A window application, not a console one, on Windows.
#![windows_subsystem = "windows"]

fn main() {
    ember_app::launch();
}
