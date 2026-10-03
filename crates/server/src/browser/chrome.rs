//! Finding and launching Chrome/Chromium with a persistent profile and an egress proxy.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::process::{Child, Command};

/// Find a Chromium-family browser on this machine.
///
/// Order: `EMBER_CHROME_BIN`; Google Chrome / Chromium / Chrome for Testing app bundles (macOS);
/// `google-chrome`, `chromium`, … on `PATH`; the newest Playwright-managed Chromium in the cache.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EMBER_CHROME_BIN") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        for (app, exe) in [
            ("Google Chrome.app", "Google Chrome"),
            ("Chromium.app", "Chromium"),
            ("Google Chrome for Testing.app", "Google Chrome for Testing"),
            ("Google Chrome Canary.app", "Google Chrome Canary"),
        ] {
            candidates.push(root.join(app).join("Contents/MacOS").join(exe));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for exe in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome"] {
                candidates.push(dir.join(exe));
            }
        }
    }
    if let Some(p) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    playwright_chromium(&home)
}

/// The newest `chromium-<rev>` in Playwright's browser cache, if any.
fn playwright_chromium(home: &Path) -> Option<PathBuf> {
    let roots = [home.join("Library/Caches/ms-playwright"), home.join(".cache/ms-playwright")];
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rev) = name.strip_prefix("chromium-").and_then(|r| r.parse::<u32>().ok())
            else {
                continue;
            };
            for sub in [
                "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
                "chrome-linux64/chrome",
                "chrome-linux/chrome",
            ] {
                let p = e.path().join(sub);
                if p.is_file() {
                    found.push((rev, p));
                }
            }
        }
    }
    found.into_iter().max_by_key(|(rev, _)| *rev).map(|(_, p)| p)
}

/// How to launch one browser.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub chrome: PathBuf,
    /// The persistent profile (cookies, storage, logins, history) — FR-R2.
    pub profile_dir: PathBuf,
    /// Egress proxy, e.g. `socks5://127.0.0.1:1080` or `http://host:3128`. `None` = direct.
    pub proxy: Option<String>,
    pub headless: bool,
    pub window: (u32, u32),
}

/// A launched browser process and its DevTools browser endpoint.
pub struct Launched {
    pub child: Child,
    pub port: u16,
    pub ws_url: String,
}

/// Validate a proxy URL Chrome understands (`--proxy-server`).
pub fn check_proxy(proxy: &str) -> anyhow::Result<()> {
    let (scheme, rest) = proxy.split_once("://").context("proxy must be scheme://host:port")?;
    if !matches!(scheme, "socks5" | "socks4" | "http" | "https") {
        bail!("unsupported proxy scheme {scheme} (socks5, socks4, http, https)");
    }
    let hostport = rest.trim_end_matches('/');
    if hostport.is_empty()
        || hostport.contains(['/', '@', ' ', ',', ';'])
        || hostport.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()).is_none()
    {
        bail!("proxy must be scheme://host:port (no credentials, no path)");
    }
    Ok(())
}

pub fn chrome_args(o: &LaunchOptions) -> Vec<String> {
    let mut args: Vec<String> = vec![
        format!("--user-data-dir={}", o.profile_dir.display()),
        "--remote-debugging-port=0".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-sync".into(),
        "--disable-features=Translate,MediaRouter,OptimizationHints".into(),
        // Keep profile secrets readable across restarts without an OS keychain prompt on a
        // headless server.
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        format!("--window-size={},{}", o.window.0, o.window.1),
    ];
    if o.headless {
        args.push("--headless=new".into());
    }
    if let Some(p) = &o.proxy {
        args.push(format!("--proxy-server={p}"));
        // Chrome bypasses the proxy for loopback by default; the egress computer's `localhost`
        // must be the one reached (FR-R1), so send loopback through the proxy too.
        args.push("--proxy-bypass-list=<-loopback>".into());
    }
    args.push("about:blank".into());
    args
}

/// Launch Chrome and wait for its DevTools endpoint.
pub async fn launch(o: &LaunchOptions) -> anyhow::Result<Launched> {
    std::fs::create_dir_all(&o.profile_dir)
        .with_context(|| format!("creating {}", o.profile_dir.display()))?;
    let port_file = o.profile_dir.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);
    let mut child = Command::new(&o.chrome)
        .args(chrome_args(o))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", o.chrome.display()))?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    let ws_url = format!("ws://127.0.0.1:{port}{}", path.trim());
                    return Ok(Launched { child, port, ws_url });
                }
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("browser exited during start ({status}); is the profile in use by another browser?");
        }
        if tokio::time::Instant::now() > deadline {
            let _ = child.kill().await;
            bail!("browser did not open its DevTools port within 30 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_validation() {
        assert!(check_proxy("socks5://127.0.0.1:1080").is_ok());
        assert!(check_proxy("http://proxy.lan:3128").is_ok());
        assert!(check_proxy("socks5://u:p@h:1").is_err());
        assert!(check_proxy("ftp://h:1").is_err());
        assert!(check_proxy("socks5://h").is_err());
        assert!(check_proxy("socks5://h:1,--evil").is_err());
    }

    #[test]
    fn loopback_goes_through_proxy() {
        let o = LaunchOptions {
            chrome: "chrome".into(),
            profile_dir: "/p".into(),
            proxy: Some("socks5://127.0.0.1:9".into()),
            headless: true,
            window: (800, 600),
        };
        let a = chrome_args(&o);
        assert!(a.contains(&"--proxy-server=socks5://127.0.0.1:9".to_string()));
        assert!(a.contains(&"--proxy-bypass-list=<-loopback>".to_string()));
    }
}
