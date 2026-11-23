//! The Android entry (FR-L36). Android runs no `main` of ours: the Activity
//! (`android/MainActivity.kt`) owns the process and the frame loop, loads this crate's cdylib,
//! and the toolkit's `JNI_OnLoad` calls the entry point declared here.
//!
//! Only the entry, the paths and back are platform code. The Activity sets `EMBER_DATA_DIR`
//! to the application's files directory before it loads the library, and in a debuggable
//! build copies the launching intent's `EMBER_*` string extras into the environment, so the
//! settings below are read exactly as the desktop reads them ([`crate::config::AppConfig`],
//! [`crate::demo`], `EMBER_CR_SHELL`, `EMBER_CR_ROUTE`). The screens are the desktop's.

use std::path::PathBuf;

use ember_transport::PowerState;

use crate::config::AppConfig;

/// The string extras a debuggable build accepts from the launching intent, each under the
/// name of the environment variable it becomes. The Activity carries the same list.
pub const INTENT_SETTINGS: [&str; 4] =
    ["EMBER_DEMO_DATA", "EMBER_SERVER_URL", "EMBER_CR_SHELL", "EMBER_CR_ROUTE"];

/// The configuration for an Android launch: the application's files directory is the data
/// directory itself (the sandbox is already the app's own, so no `ember/` level is added),
/// and the server URL is the intent's or the default. Pure, for tests.
pub fn config_for(files_dir: &str, server_url: Option<String>) -> AppConfig {
    AppConfig::from_values(server_url, Some(PathBuf::from(files_dir)), None)
}

/// Maps Android foreground/background and OS power-save flags to the transport's [`PowerState`]
/// (FR-N5, NFR-N1). Background takes precedence over power save; otherwise power save maps to
/// [`PowerState::PowerSave`], and normal foreground maps to [`PowerState::Foreground`].
pub fn power_state_for(background: bool, power_save: bool) -> PowerState {
    if background {
        PowerState::Background
    } else if power_save {
        PowerState::PowerSave
    } else {
        PowerState::Foreground
    }
}

/// Applies an Android lifecycle and power-save transition to the sync client and its transport
/// (FR-L36, FR-N5, NFR-N1).
pub fn apply_lifecycle(client: &ember_client::Client, background: bool, power_save: bool) {
    if background {
        client.pause_push();
    } else {
        client.resume_push();
    }
    client.api().set_power_state(power_state_for(background, power_save));
}

#[cfg(target_os = "android")]
mod entry {
    use std::sync::OnceLock;

    use crate::services::services;
    use crate::ui::cr::shell;

    /// The root chosen at start (`EMBER_CR_ROUTE` can pick a preview root in a debug build).
    static CONTENT: OnceLock<fn()> = OnceLock::new();

    /// Services first, then the shell's own builder: the theme, fonts and root are the ones
    /// the desktop binary opens with.
    pub fn builder() -> compose_rust::LaunchBuilder {
        crate::prepare();
        crate::bridge::start_shared();
        let pref = shell::requested().unwrap_or_else(|| services().initial_prefs.theme);
        let (builder, content) = shell::launch_parts(pref);
        let _ = CONTENT.set(content);
        builder
    }

    pub fn root() {
        if let Some(content) = CONTENT.get() {
            content();
        }
    }

    /// `MainActivity.emberBack()`: one back step, on the UI thread where the Host lives.
    /// Returns JNI `true` when the app handled the press.
    ///
    /// The symbol carries the Activity's package, which is the packager's (`dev.dioxus.main`).
    ///
    /// TODO(compose-rust#302): replace with the toolkit's BackRequested event.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_dioxus_main_MainActivity_emberBack(
        _env: *mut std::ffi::c_void,
        _this: *mut std::ffi::c_void,
    ) -> u8 {
        match crate::services::try_services() {
            Some(s) => u8::from(crate::ui::back::back(&s.ui)),
            None => 0,
        }
    }

    /// `MainActivity.emberLifecycle(background, powerSave)`: forwards Android foreground/background
    /// and OS power-save state to the client and transport (FR-L36, FR-N5, NFR-N1).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_dioxus_main_MainActivity_emberLifecycle(
        _env: *mut std::ffi::c_void,
        _this: *mut std::ffi::c_void,
        background: u8,
        power_save: u8,
    ) {
        if let Some(s) = crate::services::try_services() {
            super::apply_lifecycle(&s.client, background != 0, power_save != 0);
        }
    }
}

compose_rust::android_application!({ crate::android::entry::builder() }, crate::android::entry::root);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l36_android_files_dir_is_the_data_dir_and_the_server_url_resolves_as_on_desktop() {
        let c = config_for("/data/user/0/dev.darkpyonix.ember/files", None);
        assert_eq!(c.server_url, crate::config::DEFAULT_SERVER_URL);
        assert_eq!(c.prefs_path(), Some(PathBuf::from("/data/user/0/dev.darkpyonix.ember/files/prefs.json")));
        assert_eq!(
            c.cache_path(),
            Some(PathBuf::from("/data/user/0/dev.darkpyonix.ember/files/client-cache.json"))
        );
        let c = config_for("/f", Some(" http://mini.local:8740/ ".into()));
        assert_eq!(c.server_url, "http://mini.local:8740");
        // The same resolution the desktop does from EMBER_DATA_DIR and EMBER_SERVER_URL.
        assert_eq!(
            c,
            AppConfig::from_values(Some("http://mini.local:8740".into()), Some(PathBuf::from("/f")), None)
        );
    }

    #[test]
    fn fr_l36_the_activity_passes_exactly_the_listed_intent_settings() {
        let activity = include_str!("../android/MainActivity.kt");
        for name in INTENT_SETTINGS {
            assert!(activity.contains(&format!("\"{name}\"")), "{name} is missing from MainActivity.kt");
        }
        assert!(activity.contains("\"EMBER_DATA_DIR\""));
        assert!(activity.contains("FLAG_DEBUGGABLE"), "intent settings are for debuggable builds only");
    }

    #[test]
    fn fr_n5_power_state_for_maps_background_and_power_save() {
        assert_eq!(power_state_for(false, false), PowerState::Foreground);
        assert_eq!(power_state_for(false, true), PowerState::PowerSave);
        assert_eq!(power_state_for(true, false), PowerState::Background);
        assert_eq!(power_state_for(true, true), PowerState::Background);
    }

    #[test]
    fn fr_l36_the_activity_wires_lifecycle_and_power_save_to_native() {
        let activity = include_str!("../android/MainActivity.kt");
        assert!(activity.contains("emberLifecycle"), "emberLifecycle is missing from MainActivity.kt");
        assert!(activity.contains("onStart"), "onStart is missing from MainActivity.kt");
        assert!(activity.contains("onStop"), "onStop is missing from MainActivity.kt");
        assert!(activity.contains("isPowerSaveMode"), "isPowerSaveMode is missing from MainActivity.kt");
    }
}
