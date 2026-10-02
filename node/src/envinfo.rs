//! Environment description (feeds FR-X3's replaceable environment block).
//!
//! Toolchains are looked up on the daemon's own `PATH`. A daemon started by launchd or systemd
//! usually has a shorter `PATH` than the user's login shell; set `PATH` in the service definition
//! (or start the daemon from a login shell) so this matches what commands will find.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::proto::{EnvInfo, Toolchain};

/// Tools probed, with the arguments that print their version.
const TOOLS: &[(&str, &[&str])] = &[
    ("git", &["--version"]),
    ("python3", &["--version"]),
    ("python", &["--version"]),
    ("uv", &["--version"]),
    ("node", &["--version"]),
    ("npm", &["--version"]),
    ("pnpm", &["--version"]),
    ("bun", &["--version"]),
    ("deno", &["--version"]),
    ("cargo", &["--version"]),
    ("rustc", &["--version"]),
    ("go", &["version"]),
    ("java", &["-version"]),
    ("swift", &["--version"]),
    ("xcodebuild", &["-version"]),
    ("clang", &["--version"]),
    ("gcc", &["--version"]),
    ("make", &["--version"]),
    ("cmake", &["--version"]),
    ("docker", &["--version"]),
    ("rg", &["--version"]),
    ("nvidia-smi", &["--version"]),
];

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

async fn first_line(program: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    // A failing probe (e.g. macOS's `java` stub without a JDK) reports no version.
    if !out.status.success() {
        return None;
    }
    // Some tools (java) print their version on stderr.
    [out.stdout, out.stderr]
        .iter()
        .flat_map(|b| String::from_utf8_lossy(b).lines().map(str::trim).map(String::from).collect::<Vec<_>>())
        .find(|l| !l.is_empty())
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buffer and length are valid; result is NUL-terminated on success.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn kernel_release() -> Option<String> {
    // SAFETY: utsname is plain data; uname fills it.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return None;
    }
    // SAFETY: `release` is NUL-terminated after a successful uname.
    let s = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) };
    Some(s.to_string_lossy().into_owned())
}

async fn os_version() -> Option<String> {
    if cfg!(target_os = "macos") {
        let name = first_line(Path::new("/usr/bin/sw_vers"), &["-productName"]).await?;
        let ver = first_line(Path::new("/usr/bin/sw_vers"), &["-productVersion"]).await?;
        return Some(format!("{name} {ver}"));
    }
    let text = tokio::fs::read_to_string("/etc/os-release").await.ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
}

pub async fn describe(roots: Vec<PathBuf>) -> EnvInfo {
    let probes = TOOLS.iter().filter_map(|(name, args)| {
        which(name).map(|path| async move {
            let version = first_line(&path, args).await;
            (name.to_string(), Toolchain { path, version })
        })
    });
    let toolchains: BTreeMap<_, _> = futures::future::join_all(probes).await.into_iter().collect();
    EnvInfo {
        os: std::env::consts::OS.into(),
        os_version: os_version().await,
        kernel: kernel_release(),
        arch: std::env::consts::ARCH.into(),
        hostname: hostname(),
        user: std::env::var("USER").ok(),
        home: std::env::var_os("HOME").map(PathBuf::from),
        shell: std::env::var("SHELL").ok(),
        roots,
        toolchains,
    }
}
