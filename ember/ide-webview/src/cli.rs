//! CLI parsing, URL validation, and origin derivation for `ember-ide-webview`.

use ember_bridge::OpenWindow;

pub const DEFAULT_TITLE: &str = "Ember IDE";
pub const SMOKE_ENV: &str = "EMBER_IDE_WEBVIEW_SMOKE";
const DEFAULT_SMOKE_EXIT_MS: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub url: String,
    pub origin: String,
    pub title: String,
    pub window_id: String,
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
        "Usage: ember-ide-webview --url <url> [--title <title>] [--window-id <id>] [--smoke | --smoke-exit-ms <ms>]\n\
         \n\
         Options:\n\
           --url <url>             VS Code Web URL (http:// or https://)\n\
           --title <title>         Initial window title (default: {DEFAULT_TITLE})\n\
           --window-id <id>        Initial logical window id (default: win-1)\n\
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
/// and returns the origin (`scheme://authority`) without path, query, or fragment.
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
    let mut iter = args.into_iter();
    // Skip argv[0] (program name).
    let _ = iter.next();

    let mut url: Option<String> = None;
    let mut title: Option<String> = None;
    let mut window_id: Option<String> = None;
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

    let resolved_title = title
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned());
    let resolved_id = window_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "win-1".to_owned());

    Ok(CliAction::Run(Config {
        url: trimmed_url,
        origin,
        title: resolved_title,
        window_id: resolved_id,
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
        let action = parse_args(
            [
                "ember-ide-webview",
                "--url",
                "http://127.0.0.1:8890/?folder=%2Fwork%2Frepo",
                "--title",
                "My Repo",
                "--window-id",
                "main-7",
                "--smoke-exit-ms",
                "250",
            ],
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
                smoke_exit_ms: Some(250),
                validate_only: false,
            })
        );
    }

    #[test]
    fn fr_l7_cli_defaults_and_smoke_env() {
        let action = parse_args(
            ["ember-ide-webview", "--url", "https://ide.example.com/app?folder=%2Fp"],
            Some("1"),
        )
        .expect("parse args");
        assert_eq!(
            action,
            CliAction::Run(Config {
                url: "https://ide.example.com/app?folder=%2Fp".into(),
                origin: "https://ide.example.com".into(),
                title: DEFAULT_TITLE.into(),
                window_id: "win-1".into(),
                smoke_exit_ms: Some(DEFAULT_SMOKE_EXIT_MS),
                validate_only: false,
            })
        );
    }

    #[test]
    fn fr_l7_cli_help_version_and_smoke_flag() {
        assert!(matches!(
            parse_args(["ember-ide-webview", "--help"], None).unwrap(),
            CliAction::Help(text) if text.contains("--url")
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
}
