//! The desktop program. Everything is in the library beside this (dioxus-compose's sample
//! shape), so mobile entry points can reach the same screens later.

// A window application, not a console one, on Windows.
#![windows_subsystem = "windows"]

fn main() {
    // Velopack's install, update and uninstall hooks exit here, before any window, in every
    // process including `--editor` (SPEC NFR-R2, #880).
    ember_app::self_update::startup();
    ember_app::launch();
}
