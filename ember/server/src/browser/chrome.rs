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
    /// The persistent profile (cookies, storage, logins, history), FR-R2.
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
        args.push("--disable-gpu".into());
        args.push("--disable-dev-shm-usage".into());
    }
    if should_disable_sandbox() {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            tracing::warn!(
                "Chrome sandbox disabled via EMBER_CHROME_NO_SANDBOX; untrusted pages run without process isolation"
            );
        });
        args.push("--no-sandbox".into());
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

/// Default time to wait for Chrome to write `DevToolsActivePort` on launch.
pub const DEFAULT_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

fn should_disable_sandbox() -> bool {
    std::env::var_os("EMBER_CHROME_NO_SANDBOX")
        .is_some_and(|v| matches!(v.to_string_lossy().trim(), "1" | "true" | "yes"))
}

/// Time to wait for Chrome to write `DevToolsActivePort` on launch.
pub fn launch_timeout() -> Duration {
    if let Some(ms) = std::env::var("EMBER_CHROME_LAUNCH_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
    {
        return Duration::from_millis(ms);
    }
    if let Some(secs) = std::env::var("EMBER_CHROME_LAUNCH_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&secs| secs > 0)
    {
        return Duration::from_secs(secs);
    }
    DEFAULT_LAUNCH_TIMEOUT
}

/// Path to the captured Chrome stderr log inside `profile_dir`.
pub fn stderr_log_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("chrome-stderr.log")
}

/// Read the last lines of Chrome's captured stderr in `profile_dir`, if any.
pub fn stderr_tail(profile_dir: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(stderr_log_path(profile_dir)) else {
        return String::new();
    };
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(20);
    let mut joined = lines[start..].join("\n");
    if joined.len() > 2048 {
        let mut cut = joined.len() - 2048;
        while !joined.is_char_boundary(cut) {
            cut += 1;
        }
        joined = joined[cut..].to_string();
    }
    joined
}

fn format_stderr_suffix(profile_dir: &Path) -> String {
    let tail = stderr_tail(profile_dir);
    if tail.is_empty() {
        String::new()
    } else {
        format!("; stderr: {tail}")
    }
}

/// Launch Chrome and wait for its DevTools endpoint.
pub async fn launch(o: &LaunchOptions) -> anyhow::Result<Launched> {
    launch_with_timeout(o, launch_timeout()).await
}

fn open_stderr_log(profile_dir: &Path) -> std::io::Result<std::fs::File> {
    let path = stderr_log_path(profile_dir);
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

/// Launch Chrome and wait up to `timeout` for its DevTools endpoint.
pub async fn launch_with_timeout(o: &LaunchOptions, timeout: Duration) -> anyhow::Result<Launched> {
    std::fs::create_dir_all(&o.profile_dir)
        .with_context(|| format!("creating {}", o.profile_dir.display()))?;
    let port_file = o.profile_dir.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);
    let stderr = open_stderr_log(&o.profile_dir)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut child = Command::new(&o.chrome)
        .args(chrome_args(o))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", o.chrome.display()))?;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                let path = path.trim();
                if let Ok(port) = port.trim().parse::<u16>() {
                    if port > 0 && !path.is_empty() {
                        let ws_url = format!("ws://127.0.0.1:{port}{path}");
                        return Ok(Launched { child, port, ws_url });
                    }
                }
            }
        }
        if let Some(status) = child.try_wait()? {
            let suffix = format_stderr_suffix(&o.profile_dir);
            bail!(
                "browser exited during start ({status}); is the profile in use by another browser?{suffix}"
            );
        }
        if tokio::time::Instant::now() > deadline {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let suffix = format_stderr_suffix(&o.profile_dir);
            bail!(
                "browser did not open its DevTools port within {:.0} s{suffix}",
                timeout.as_secs_f64()
            );
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
        assert!(a.contains(&"--headless=new".to_string()));
        assert!(a.contains(&"--disable-gpu".to_string()));
        assert!(a.contains(&"--disable-dev-shm-usage".to_string()));
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launch_waits_for_slow_devtools_port_file() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let script = write_script(
            dir.path(),
            "fake-chrome.sh",
            "#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               case \"$arg\" in\n\
                 --user-data-dir=*) dir=\"${arg#--user-data-dir=}\" ;;\n\
               esac\n\
             done\n\
             sleep 0.15\n\
             printf '43210\\n/devtools/browser/fake-id\\n' > \"$dir/DevToolsActivePort\"\n\
             sleep 5\n",
        );
        let opts = LaunchOptions {
            chrome: script,
            profile_dir: profile,
            proxy: None,
            headless: true,
            window: (800, 600),
        };
        let mut launched = launch_with_timeout(&opts, Duration::from_secs(5)).await.unwrap();
        assert_eq!(launched.port, 43210);
        assert_eq!(launched.ws_url, "ws://127.0.0.1:43210/devtools/browser/fake-id");
        let _ = launched.child.kill().await;
        let _ = launched.child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launch_reports_stderr_on_early_exit_and_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let profile_exit = dir.path().join("profile-exit");
        let exit_script = write_script(
            dir.path(),
            "fail-chrome.sh",
            "#!/bin/sh\n\
             echo 'FATAL: profile SingletonLock held by pid 999' >&2\n\
             exit 21\n",
        );
        let err = launch_with_timeout(
            &LaunchOptions {
                chrome: exit_script,
                profile_dir: profile_exit.clone(),
                proxy: None,
                headless: true,
                window: (800, 600),
            },
            Duration::from_secs(15),
        )
        .await
        .err()
        .expect("early exit should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("browser exited during start"), "unexpected error: {msg}");
        assert!(msg.contains("SingletonLock held by pid 999"), "missing stderr in error: {msg}");
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(stderr_log_path(&profile_exit)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "chrome-stderr.log must be 0600, got {mode:04o}");
        }

        let profile_timeout = dir.path().join("profile-timeout");
        let hang_script = write_script(
            dir.path(),
            "hang-chrome.sh",
            "#!/bin/sh\n\
             echo 'ERROR: NSS initialization stalled in container' >&2\n\
             sleep 30\n",
        );
        let err = launch_with_timeout(
            &LaunchOptions {
                chrome: hang_script,
                profile_dir: profile_timeout,
                proxy: None,
                headless: true,
                window: (800, 600),
            },
            Duration::from_secs(2),
        )
        .await
        .err()
        .expect("timeout should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not open its DevTools port"), "unexpected error: {msg}");
        assert!(
            msg.contains("NSS initialization stalled in container"),
            "missing stderr in timeout error: {msg}"
        );
    }
}
