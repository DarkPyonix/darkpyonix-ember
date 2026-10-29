//! The base UI on compose-rust (#220, INTENT D9). Each module mirrors the Dioxus module of the
//! same name in `ui/` and replaces it when its last screen has moved (phase 8 deletes the
//! originals). Nothing in here imports `dioxus_compose`.

pub mod theme;
pub mod components;
pub mod compat;
pub mod gallery;
pub mod shell;
pub mod launcher;
pub mod notice;
pub mod search;
pub mod new_session;
pub mod ko;
