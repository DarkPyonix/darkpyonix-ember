//! CLI parsing, URL validation, origin derivation, title formatting, and
//! navigation policy for `ember-ide-webview`.

use std::path::PathBuf;

use ember_bridge::OpenWindow;

pub const DEFAULT_TITLE: &str = "Ember IDE";
pub const SMOKE_ENV: &str = "EMBER_IDE_WEBVIEW_SMOKE";
pub const DATA_DIR_ENV: &str = "EMBER_IDE_WEBVIEW_DATA_DIR";
const DEFAULT_SMOKE_EXIT_MS: u64 = 500;

/// Default desktop window dimensions in logical pixels.
pub const DEFAULT_WINDOW_WIDTH: f64 = 1280.0;
pub const DEFAULT_WINDOW_HEIGHT: f64 = 840.0;

/// Minimum window dimensions in logical pixels so narrow tablet and phone
/// responsive layouts remain usable.
pub const MIN_WINDOW_WIDTH: f64 = 360.0;
pub const MIN_WINDOW_HEIGHT: f64 = 480.0;

/// DarkPyonix Dark `editor.background` (`#17120E`), applied to the native
/// webview before first paint to prevent a white flash on launch.
pub const DARKPYONIX_BG_RGBA: (u8, u8, u8, u8) = (0x17, 0x12, 0x0E, 0xFF);
pub const DARKPYONIX_BG_HEX: &str = "#17120E";

/// Native window chrome behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeMode {
    /// Suppress redundant native titlebar chrome where the platform supports
    /// full-size content views while preserving native window controls.
    Integrated,
    /// Use the standard platform titlebar and window frame.
    Native,
}

/// Decision for a top-level navigation or popup request inside the webview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationDecision {
    /// Allow the webview to navigate directly (same origin or `about:blank`).
    Allow,
    /// Open in the OS default handler (external `http://` or `https://` link).
    OpenExternal,
    /// Block the request (`file://`, `javascript:`, `data:`, or malformed URL).
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub url: String,
    pub origin: String,
    pub title: String,
    pub window_id: String,
    pub data_dir: Option<PathBuf>,
    pub chrome_mode: ChromeMode,
    pub smoke_exit_ms: Option<u64>,
    pub validate_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Help(String),
    Version(String),
    Run(Config),
}

pub fn usage() -> String {
    format!(
        "Usage: ember-ide-webview --url <url> [--title <title>] [--window-id <id>] [--data-dir <dir>] [--native-chrome] [--smoke | --smoke-exit-ms <ms>]\n\
         \n\
         Options:\n\
           --url <url>             VS Code Web URL (http:// or https://)\n\
           --title <title>         Initial window title (default: derived from folder or {DEFAULT_TITLE})\n\
           --window-id <id>        Initial logical window id (default: win-1)\n\
           --data-dir <dir>        Persistent webview cache/data directory\n\
           --native-chrome         Keep the standard native titlebar instead of integrated chrome\n\
           --smoke                 Validate arguments and bridge wiring without opening a window\n\
           --smoke-exit-ms <ms>    Open the window and exit after <ms> milliseconds\n\
           -h, --help              Show this help message\n\
           -V, --version           Show version information"
    )
}

pub fn version_text() -> String {
    format!("ember-ide-webview {}", env!("CARGO_PKG_VERSION"))
}

/// Validates that `url` uses `http://` or `https://` with a non-empty authority
/// and returns the normalized origin (`scheme://authority`) without path,
/// query, or fragment.
pub fn validate_and_derive_origin(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("URL must not be empty".to_owned());
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("URL must not contain whitespace or control characters".to_owned());
    }
    let lower = trimmed.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("http://") {
        "http://".len()
    } else if lower.starts_with("https://") {
        "https://".len()
    } else {
        return Err(format!("refusing URL with unsupported scheme: {trimmed:?}"));
    };
    let rest = &trimmed[scheme_len..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.starts_with(':') {
        return Err(format!("URL is missing a host: {trimmed:?}"));
    }
    let scheme = &lower[..scheme_len];
    Ok(format!("{scheme}{authority}"))
}

/// Classifies a navigation or popup target against the locked session origin.
///
/// Same-origin HTTP(S) URLs and `about:blank` stay inside the IDE window.
/// External HTTP(S) links are routed to the OS browser so clicking a doc link
/// never replaces the workbench. Non-HTTP(S) schemes are blocked.
pub fn classify_navigation(session_origin: &str, target_url: &str) -> NavigationDecision {
    let trimmed = target_url.trim();
    if trimmed.eq_ignore_ascii_case("about:blank") {
        return NavigationDecision::Allow;
    }
    let Ok(target_origin) = validate_and_derive_origin(trimmed) else {
        return NavigationDecision::Block;
    };
    if target_origin.eq_ignore_ascii_case(session_origin.trim_end_matches('/')) {
        NavigationDecision::Allow
    } else {
        NavigationDecision::OpenExternal
    }
}

/// Minimal percent-decoder for extracting a readable workspace folder name
/// from a `?folder=` query parameter.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Extracts the workspace folder basename from a VS Code Web URL's `folder`
/// query parameter, when present.
pub fn folder_name_from_url(url: &str) -> Option<String> {
    let (_, after_q) = url.split_once('?')?;
    let query = after_q.split('#').next().unwrap_or(after_q);
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == "folder" && !v.is_empty() {
            let decoded = percent_decode(v);
            let trimmed = decoded.trim_end_matches(['/', '\\']);
            let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed).trim();
            if !base.is_empty() {
                return Some(base.to_owned());
            }
        }
    }
    None
}

/// Formats the initial window title from an explicit override or the URL's
/// workspace folder name.
pub fn derive_window_title(explicit: Option<&str>, url: &str) -> String {
    if let Some(t) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return t.to_owned();
    }
    match folder_name_from_url(url) {
        Some(folder) => format!("{folder} - {DEFAULT_TITLE}"),
        None => DEFAULT_TITLE.to_owned(),
    }
}

/// The window title after the page at `page_url` has loaded: `<folder> - Ember IDE` for the
/// page's workspace folder, else `fallback` (the title the window opened with). The page's own
/// `document.title` (Code-OSS sets `<editor>, <folder>, <product>`) never becomes the window title,
/// so every IDE window is named the same way whichever runtime serves it.
pub fn window_title_for_page(page_url: &str, fallback: &str) -> String {
    match folder_name_from_url(page_url) {
        Some(folder) => format!("{folder} - {DEFAULT_TITLE}"),
        None => fallback.to_owned(),
    }
}

/// Resolves the persistent webview cache/data directory in priority order:
/// 1. `--data-dir` CLI flag,
/// 2. `EMBER_IDE_WEBVIEW_DATA_DIR` environment override,
/// 3. platform user cache directory.
pub fn resolve_data_dir(
    cli_dir: Option<&str>,
    env_dir: Option<&str>,
    home_dir: Option<&str>,
    xdg_cache_home: Option<&str>,
    local_app_data: Option<&str>,
) -> Option<PathBuf> {
    if let Some(p) = cli_dir.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = env_dir.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if cfg!(target_os = "macos") {
        let home = home_dir.map(str::trim).filter(|s| !s.is_empty())?;
        return Some(
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("com.darkpyonix.ember")
                .join("ide-webview"),
        );
    }
    if cfg!(target_os = "windows") {
        let base = local_app_data
            .or(home_dir)
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        return Some(PathBuf::from(base).join("Ember").join("ide-webview"));
    }
    if let Some(xdg) = xdg_cache_home.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(xdg).join("ember").join("ide-webview"));
    }
    let home = home_dir.map(str::trim).filter(|s| !s.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".cache")
            .join("ember")
            .join("ide-webview"),
    )
}

/// Builds the full URL for a detached sibling window on the same origin.
pub fn resolve_detached_url(origin: &str, request: &OpenWindow) -> Option<String> {
    let path = request.vscode_web_path()?;
    let base = origin.trim_end_matches('/');
    Some(format!("{base}{path}"))
}

pub fn parse_args<I, S>(args: I, env_smoke: Option<&str>) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let env_data_dir = std::env::var(DATA_DIR_ENV).ok();
    let home_dir = std::env::var("HOME").ok();
    let xdg_cache = std::env::var("XDG_CACHE_HOME").ok();
    let local_app_data = std::env::var("LOCALAPPDATA").ok();
    parse_args_with_env(
        args,
        env_smoke,
        env_data_dir.as_deref(),
        home_dir.as_deref(),
        xdg_cache.as_deref(),
        local_app_data.as_deref(),
    )
}

pub fn parse_args_with_env<I, S>(
    args: I,
    env_smoke: Option<&str>,
    env_data_dir: Option<&str>,
    home_dir: Option<&str>,
    xdg_cache_home: Option<&str>,
    local_app_data: Option<&str>,
) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    // Skip argv[0] (program name).
    let _ = iter.next();

    let mut url: Option<String> = None;
    let mut title: Option<String> = None;
    let mut window_id: Option<String> = None;
    let mut data_dir: Option<String> = None;
    let mut chrome_mode = ChromeMode::Integrated;
    let mut smoke_exit_ms: Option<u64> = None;
    let mut validate_only = false;

    while let Some(arg) = iter.next() {
        let a = arg.as_ref();
        match a {
            "-h" | "--help" => return Ok(CliAction::Help(usage())),
            "-V" | "--version" => return Ok(CliAction::Version(version_text())),
            "--url" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--url requires a value".to_owned())?;
                url = Some(value.as_ref().to_owned());
            }
            "--title" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--title requires a value".to_owned())?;
                title = Some(value.as_ref().to_owned());
            }
            "--window-id" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--window-id requires a value".to_owned())?;
                window_id = Some(value.as_ref().to_owned());
            }
            "--data-dir" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--data-dir requires a value".to_owned())?;
                data_dir = Some(value.as_ref().to_owned());
            }
            "--native-chrome" => {
                chrome_mode = ChromeMode::Native;
            }
            "--smoke" => {
                validate_only = true;
            }
            "--smoke-exit-ms" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--smoke-exit-ms requires a millisecond value".to_owned())?;
                let ms = value
                    .as_ref()
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --smoke-exit-ms value: {:?}", value.as_ref()))?;
                smoke_exit_ms = Some(ms);
            }
            other => {
                return Err(format!("unknown argument: {other:?}\n\n{}", usage()));
            }
        }
    }

    let raw_url = url.ok_or_else(|| format!("missing required --url argument\n\n{}", usage()))?;
    let origin = validate_and_derive_origin(&raw_url)?;
    let trimmed_url = raw_url.trim().to_owned();

    if !validate_only
        && smoke_exit_ms.is_none()
        && env_smoke.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        smoke_exit_ms = Some(DEFAULT_SMOKE_EXIT_MS);
    }

    let resolved_title = derive_window_title(title.as_deref(), &trimmed_url);
    let resolved_id = window_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "win-1".to_owned());
    let resolved_data_dir = resolve_data_dir(
        data_dir.as_deref(),
        env_data_dir,
        home_dir,
        xdg_cache_home,
        local_app_data,
    );

    Ok(CliAction::Run(Config {
        url: trimmed_url,
        origin,
        title: resolved_title,
        window_id: resolved_id,
        data_dir: resolved_data_dir,
        chrome_mode,
        smoke_exit_ms,
        validate_only,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_bridge::{Position, Workspace};

    #[test]
    fn fr_l7_cli_parses_required_and_optional_flags() {
        let action = parse_args_with_env(
            [
                "ember-ide-webview",
                "--url",
                "http://127.0.0.1:8890/?folder=%2Fwork%2Frepo",
                "--title",
                "My Repo",
                "--window-id",
                "main-7",
                "--data-dir",
                "/cache/ide",
                "--native-chrome",
                "--smoke-exit-ms",
                "250",
            ],
            None,
            None,
            None,
            None,
            None,
        )
        .expect("parse args");
        assert_eq!(
            action,
            CliAction::Run(Config {
                url: "http://127.0.0.1:8890/?folder=%2Fwork%2Frepo".into(),
                origin: "http://127.0.0.1:8890".into(),
                title: "My Repo".into(),
                window_id: "main-7".into(),
                data_dir: Some(PathBuf::from("/cache/ide")),
                chrome_mode: ChromeMode::Native,
                smoke_exit_ms: Some(250),
                validate_only: false,
            })
        );
    }

    #[test]
    fn fr_l7_cli_defaults_and_smoke_env() {
        let action = parse_args_with_env(
            [
                "ember-ide-webview",
                "--url",
                "https://ide.example.com/app?folder=%2Fp",
            ],
            Some("1"),
            Some("/env/cache"),
            None,
            None,
            None,
        )
        .expect("parse args");
        assert_eq!(
            action,
            CliAction::Run(Config {
                url: "https://ide.example.com/app?folder=%2Fp".into(),
                origin: "https://ide.example.com".into(),
                title: "p - Ember IDE".into(),
                window_id: "win-1".into(),
                data_dir: Some(PathBuf::from("/env/cache")),
                chrome_mode: ChromeMode::Integrated,
                smoke_exit_ms: Some(DEFAULT_SMOKE_EXIT_MS),
                validate_only: false,
            })
        );
    }

    #[test]
    fn fr_l7_cli_help_version_and_smoke_flag() {
        assert!(matches!(
            parse_args(["ember-ide-webview", "--help"], None).unwrap(),
            CliAction::Help(text) if text.contains("--url") && text.contains("--data-dir")
        ));
        assert!(matches!(
            parse_args(["ember-ide-webview", "--version"], None).unwrap(),
            CliAction::Version(text) if text.starts_with("ember-ide-webview ")
        ));
        let smoke = parse_args(
            ["ember-ide-webview", "--url", "http://localhost:8890", "--smoke"],
            None,
        )
        .unwrap();
        assert!(matches!(smoke, CliAction::Run(cfg) if cfg.validate_only));
    }

    #[test]
    fn fr_l7_url_validation_rejects_disallowed_schemes_and_malformed_inputs() {
        assert!(validate_and_derive_origin("file:///etc/passwd").is_err());
        assert!(validate_and_derive_origin("javascript:alert(1)").is_err());
        assert!(validate_and_derive_origin("http://").is_err());
        assert!(validate_and_derive_origin("http://:8890").is_err());
        assert!(validate_and_derive_origin("http://host/a b").is_err());
        assert!(validate_and_derive_origin("").is_err());
        assert_eq!(
            validate_and_derive_origin("HTTPS://IDE.Example.com:8443/?folder=%2Fwork#top").unwrap(),
            "https://IDE.Example.com:8443"
        );
    }

    #[test]
    fn fr_l7_detached_url_combines_origin_with_vscode_web_path() {
        let req = OpenWindow {
            window_id: "w-2".into(),
            source_window_id: Some("w-1".into()),
            workspace: Some(Workspace {
                folder: Some("/work/demo".into()),
            }),
            file_uri: Some("vscode-remote://h/work/demo/src/lib.rs".into()),
            cursor: Some(Position { line: 9, column: 3 }),
            selection: None,
            scroll: None,
            screen: None,
        };
        let url = resolve_detached_url("http://127.0.0.1:8890/", &req).expect("detached url");
        assert!(url.starts_with("http://127.0.0.1:8890/?folder=%2Fwork%2Fdemo&payload="));
        assert!(url.contains("gotoLineMode"));
    }

    #[test]
    fn fr_w1_persistent_data_dir_resolves_for_asset_caching() {
        assert_eq!(
            resolve_data_dir(
                Some("/cli/dir"),
                Some("/env/dir"),
                Some("/home/u"),
                None,
                None
            ),
            Some(PathBuf::from("/cli/dir"))
        );
        assert_eq!(
            resolve_data_dir(None, Some("/env/dir"), Some("/home/u"), None, None),
            Some(PathBuf::from("/env/dir"))
        );
        let default_dir = resolve_data_dir(None, None, Some("/Users/dev"), None, None)
            .expect("default cache dir should resolve when HOME is set");
        assert!(
            default_dir.ends_with("ide-webview"),
            "default data dir should end with ide-webview, got {}",
            default_dir.display()
        );
    }

    #[test]
    fn fr_w2_window_geometry_supports_desktop_and_mobile_responsive_widths() {
        const {
            assert!(DEFAULT_WINDOW_WIDTH >= 1024.0);
            assert!(DEFAULT_WINDOW_HEIGHT >= 720.0);
            assert!(MIN_WINDOW_WIDTH <= 400.0);
            assert!(MIN_WINDOW_HEIGHT <= 500.0);
        }
    }

    #[test]
    fn fr_w3_chrome_defaults_to_integrated_and_title_derives_from_folder() {
        let action = parse_args_with_env(
            [
                "ember-ide-webview",
                "--url",
                "http://127.0.0.1:8890/?folder=%2Fwork%2Fember%20app",
            ],
            None,
            None,
            None,
            None,
            None,
        )
        .expect("parse");
        let CliAction::Run(cfg) = action else {
            panic!("expected Run");
        };
        assert_eq!(cfg.chrome_mode, ChromeMode::Integrated);
        assert_eq!(cfg.title, "ember app - Ember IDE");
        assert_eq!(derive_window_title(None, "http://127.0.0.1:8890/"), DEFAULT_TITLE);
        // After a page load the title still has the SPEC form, from the page's folder; a page
        // with no folder keeps the title the window opened with.
        assert_eq!(
            window_title_for_page("http://127.0.0.1:8890/?folder=%2Fwork%2Fother", "ember app - Ember IDE"),
            "other - Ember IDE"
        );
        assert_eq!(
            window_title_for_page("http://127.0.0.1:8890/", "ember app - Ember IDE"),
            "ember app - Ember IDE"
        );
        // The page's document.title is not passed through to the window.
        let window_rs = include_str!("window.rs");
        assert!(!window_rs.contains("with_document_title_changed_handler"));
        assert!(window_rs.contains("window_title_for_page("));
    }

    #[test]
    fn fr_w6_pre_paint_background_matches_darkpyonix_dark_theme() {
        assert_eq!(DARKPYONIX_BG_RGBA, (0x17, 0x12, 0x0E, 0xFF));
        assert_eq!(DARKPYONIX_BG_HEX, "#17120E");
    }

    #[test]
    fn fr_w1_navigation_policy_restricts_webview_to_session_origin() {
        let origin = "http://127.0.0.1:8890";
        assert_eq!(
            classify_navigation(origin, "http://127.0.0.1:8890/?folder=%2Fwork"),
            NavigationDecision::Allow
        );
        assert_eq!(
            classify_navigation(origin, "about:blank"),
            NavigationDecision::Allow
        );
        assert_eq!(
            classify_navigation(origin, "https://docs.example.com/guide"),
            NavigationDecision::OpenExternal
        );
        assert_eq!(
            classify_navigation(origin, "http://127.0.0.1:9999/other"),
            NavigationDecision::OpenExternal
        );
        assert_eq!(
            classify_navigation(origin, "file:///etc/passwd"),
            NavigationDecision::Block
        );
        assert_eq!(
            classify_navigation(origin, "javascript:alert(1)"),
            NavigationDecision::Block
        );
        assert_eq!(
            classify_navigation(origin, "data:text/html,<h1>x</h1>"),
            NavigationDecision::Block
        );
    }
}
