//! Agent sandbox (SPEC FR-A11, #784, INTENT D37 answers 3 and 4).
//!
//! Three layers keep an agent out of Ember's own secrets:
//!
//! 1. **Environment.** [`scrub_env`] removes Ember secrets (`EMBER_SERVER_TOKEN`, node tokens and
//!    every `EMBER_*` variable named like a token, key or secret) from what an agent inherits from
//!    the server. A value Ember passes on purpose (an entry of the agent's own `env` list, such as
//!    `EMBER_RUNTIME_TOKEN`) is set after the scrub and so survives. Every harness, every platform.
//! 2. **OS sandbox at spawn.** [`wrap`] starts the agent under the platform's sandbox with the
//!    denied directories hidden: macOS `sandbox-exec` (Seatbelt profile with
//!    `(deny file-read* file-write* (subpath ...))`), Linux `bwrap` (a tmpfs over each directory).
//!    Directories the agent needs that sit inside a denied one (its account's `CLAUDE_CONFIG_DIR` or
//!    `CODEX_HOME` under `<data dir>/accounts/`) are allowed again. Where neither tool works the
//!    agent starts unwrapped and one warning is logged ("agent OS sandbox not available").
//! 3. **Per-harness file tools.** [`is_denied`] is the check Ember's own handlers use: the ACP file
//!    methods (Gemini, omp), the agy hook, and Claude Code's `--settings` deny rules
//!    ([`claude_settings`]).
//!
//! The denied directories are the server data dir and the OSE data root, set once at start by
//! [`install`]. Nothing is installed in tests unless a test does it, so the layers 2 and 3 are off
//! there and existing argument tests are unchanged.
//!
//! Not covered (recorded in FR-A11): a same-user process can still read the shim's or the
//! server's environment through `/proc` on Linux or attach a debugger; terminals of the node pty
//! are not wrapped; nesting with a harness's own Seatbelt sandbox on macOS is not measured.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// `EMBER_AGENT_SANDBOX=off` turns layer 2 off (layers 1 and 3 stay).
pub const ENV_SWITCH: &str = "EMBER_AGENT_SANDBOX";

/// Variables an agent never inherits from the server, beside the pattern in [`is_secret_name`].
pub const SECRET_VARS: &[&str] = &[
    "EMBER_SERVER_TOKEN",
    "EMBER_NODE_TOKEN",
    "EMBER_EXEC_NODE_TOKEN",
    "EMBER_RUNTIME_TOKEN",
    "EMBER_HUB_TOKEN",
];

/// Env variables whose value is a directory the agent itself must use (its account home).
const CONFIG_DIR_VARS: &[&str] = &["CLAUDE_CONFIG_DIR", "CODEX_HOME"];

/// An Ember variable that carries a secret: one of [`SECRET_VARS`], or `EMBER_*` with `TOKEN`,
/// `SECRET` or `PASSWORD` in it, or ending in `_KEY`.
pub fn is_secret_name(name: &str) -> bool {
    if SECRET_VARS.contains(&name) {
        return true;
    }
    let Some(rest) = name.strip_prefix("EMBER_") else { return false };
    rest.contains("TOKEN") || rest.contains("SECRET") || rest.contains("PASSWORD") || rest.ends_with("_KEY")
}

/// The inherited variables to remove before an agent's own `env` is applied.
pub fn scrubbed_names<'a>(inherited: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    inherited.into_iter().filter(|n| is_secret_name(n)).map(str::to_string).collect()
}

/// Remove every inherited Ember secret from `cmd` (layer 1). Call before the agent's own `env`.
pub fn scrub_env(cmd: &mut tokio::process::Command) {
    let names: Vec<String> = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect();
    for n in scrubbed_names(names.iter().map(String::as_str)) {
        cmd.env_remove(n);
    }
}

/// Which OS sandbox tool was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    /// macOS `sandbox-exec` at this path.
    SandboxExec(PathBuf),
    /// Linux bubblewrap at this path.
    Bwrap(PathBuf),
}

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Hidden from agents (absolute; canonical forms are added by [`Policy::new`]).
    pub denied: Vec<PathBuf>,
    /// Layer 2 tool; `None` when not available or switched off.
    pub tool: Option<Tool>,
}

impl Policy {
    /// `denied` plus the canonical form of each existing entry.
    pub fn new(denied: Vec<PathBuf>, tool: Option<Tool>) -> Policy {
        let mut all: Vec<PathBuf> = Vec::new();
        for d in denied.into_iter().filter(|d| d.is_absolute()) {
            let canon = std::fs::canonicalize(&d).ok();
            for p in std::iter::once(d).chain(canon) {
                if !all.contains(&p) {
                    all.push(p);
                }
            }
        }
        Policy { denied: all, tool }
    }
}

static POLICY: OnceLock<Policy> = OnceLock::new();

/// Set the process-wide policy once at server start. Later calls are ignored.
pub fn install(policy: Policy) {
    match &policy.tool {
        Some(t) => tracing::info!(tool = ?t, denied = ?policy.denied, "agent OS sandbox on"),
        None => tracing::warn!(
            denied = ?policy.denied,
            "agent OS sandbox not available (no working sandbox-exec or bwrap, or {ENV_SWITCH}=off); \
             agents start without it, file-tool denies and the env scrub still apply"
        ),
    }
    let _ = POLICY.set(policy);
}

/// The installed policy, if any.
pub fn policy() -> Option<&'static Policy> {
    POLICY.get()
}

/// The installed denied directories (empty when nothing is installed).
pub fn denied_dirs() -> &'static [PathBuf] {
    POLICY.get().map(|p| p.denied.as_slice()).unwrap_or(&[])
}

/// `p` without `.` and with `..` folded, lexically.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The canonical form of `p`, or of its nearest existing ancestor joined with the rest.
fn canonical_prefix(p: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            return Some(rest.iter().rev().fold(c, |acc: PathBuf, n: &std::ffi::OsString| acc.join(n)));
        }
        let name = cur.file_name()?.to_os_string();
        rest.push(name);
        if !cur.pop() {
            return None;
        }
    }
}

/// Is `path` inside one of `denied`? Checked lexically and after resolving symlinks, so neither
/// `..` nor a link into the data dir gets through.
pub fn is_denied_in(path: &Path, denied: &[PathBuf]) -> bool {
    if denied.is_empty() {
        return false;
    }
    let lexical = clean(path);
    let hit = |p: &Path| denied.iter().any(|d| p.starts_with(d));
    hit(&lexical) || canonical_prefix(&lexical).is_some_and(|c| hit(&c))
}

/// [`is_denied_in`] against the installed policy.
pub fn is_denied(path: &Path) -> bool {
    is_denied_in(path, denied_dirs())
}

/// The message an agent gets for a denied path.
pub fn denied_message(path: &Path) -> String {
    format!("{} is inside Ember's own data folder, which agents cannot use (FR-A11)", path.display())
}

/// Directories inside a denied one that this agent needs: the values of [`CONFIG_DIR_VARS`] in
/// its env that lie under a denied directory.
pub fn allowed_for(env: &[(String, String)], denied: &[PathBuf]) -> Vec<PathBuf> {
    env.iter()
        .filter(|(k, _)| CONFIG_DIR_VARS.contains(&k.as_str()))
        .map(|(_, v)| PathBuf::from(v))
        .filter(|p| p.is_absolute() && denied.iter().any(|d| clean(p).starts_with(d)))
        .collect()
}

/// A Seatbelt string literal.
fn sb_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The macOS profile: allow everything, deny reads and writes under each denied directory, then
/// allow the carve-outs again (a later Seatbelt rule wins).
pub fn macos_profile(denied: &[PathBuf], allowed: &[PathBuf]) -> String {
    let mut s = String::from("(version 1)\n(allow default)\n");
    for d in denied {
        s.push_str(&format!("(deny file-read* file-write* (subpath {}))\n", sb_quote(d)));
    }
    for a in allowed {
        s.push_str(&format!("(allow file-read* file-write* (subpath {}))\n", sb_quote(a)));
    }
    s
}

/// The program and arguments that run `program args` under `tool` with `denied` hidden and
/// `allowed` visible.
pub fn wrap_with(
    tool: &Tool,
    denied: &[PathBuf],
    allowed: &[PathBuf],
    program: &Path,
    args: &[String],
) -> (PathBuf, Vec<String>) {
    let program_s = program.to_string_lossy().into_owned();
    match tool {
        Tool::SandboxExec(bin) => {
            let mut out = vec!["-p".to_string(), macos_profile(denied, allowed), program_s];
            out.extend(args.iter().cloned());
            (bin.clone(), out)
        }
        Tool::Bwrap(bin) => {
            let mut out: Vec<String> = vec!["--dev-bind".into(), "/".into(), "/".into()];
            for d in denied.iter().filter(|d| d.exists()) {
                out.extend(["--tmpfs".into(), d.to_string_lossy().into_owned()]);
            }
            for a in allowed.iter().filter(|a| a.exists()) {
                let a = a.to_string_lossy().into_owned();
                out.extend(["--bind".into(), a.clone(), a]);
            }
            out.extend(["--".into(), program_s]);
            out.extend(args.iter().cloned());
            (bin.clone(), out)
        }
    }
}

/// [`wrap_with`] against the installed policy; unchanged when no tool is installed or `enabled`
/// is false (a harness whose own sandbox does not nest, see FR-A11).
pub fn wrap(enabled: bool, env: &[(String, String)], program: &Path, args: &[String]) -> (PathBuf, Vec<String>) {
    match POLICY.get() {
        Some(Policy { tool: Some(tool), denied }) if enabled && !denied.is_empty() => {
            wrap_with(tool, denied, &allowed_for(env, denied), program, args)
        }
        _ => (program.to_path_buf(), args.to_vec()),
    }
}

/// `true` when `switch` (the value of [`ENV_SWITCH`]) turns the OS sandbox off.
pub fn switched_off(switch: Option<&str>) -> bool {
    matches!(switch.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("off" | "0" | "false" | "no"))
}

/// The first existing file among `candidates`.
fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// Find this platform's sandbox tool and check that it runs (`true` under a trivial policy).
/// `None` on other platforms, when the tool is missing, or when the probe fails (for example
/// bubblewrap without user namespaces).
pub fn detect() -> Option<Tool> {
    let probe = |bin: &Path, args: &[&str]| {
        std::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if cfg!(target_os = "macos") {
        let bin = first_file([PathBuf::from("/usr/bin/sandbox-exec")])?;
        let truth = first_file([PathBuf::from("/usr/bin/true"), PathBuf::from("/bin/true")])?;
        let t = truth.to_string_lossy().into_owned();
        return probe(&bin, &["-p", "(version 1)(allow default)", &t]).then_some(Tool::SandboxExec(bin));
    }
    if cfg!(target_os = "linux") {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let bin = first_file(std::env::split_paths(&path).map(|d| d.join("bwrap")))?;
        let truth = first_file([PathBuf::from("/usr/bin/true"), PathBuf::from("/bin/true")])?;
        let t = truth.to_string_lossy().into_owned();
        return probe(&bin, &["--dev-bind", "/", "/", "--", &t]).then_some(Tool::Bwrap(bin));
    }
    None
}

/// The policy for this server: deny `denied`, with the tool from [`detect`] unless switched off.
pub fn from_env(denied: Vec<PathBuf>) -> Policy {
    let tool = if switched_off(std::env::var(ENV_SWITCH).ok().as_deref()) { None } else { detect() };
    Policy::new(denied, tool)
}

/// Claude Code `--settings` JSON with `permissions.deny` rules for its file tools on each denied
/// directory (`//` marks an absolute path in Claude Code rules). `None` when nothing is denied.
pub fn claude_settings(denied: &[PathBuf]) -> Option<String> {
    if denied.is_empty() {
        return None;
    }
    let mut rules = Vec::new();
    for d in denied {
        let d = d.to_string_lossy();
        let d = d.trim_end_matches('/');
        for tool in ["Read", "Edit", "Write"] {
            rules.push(format!("{tool}(/{d}/**)"));
        }
    }
    Some(serde_json::json!({ "permissions": { "deny": rules } }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn fr_a11_secret_names_are_scrubbed_and_others_kept() {
        let names = [
            "EMBER_SERVER_TOKEN",
            "EMBER_NODE_TOKEN",
            "EMBER_HUB_SECRET",
            "EMBER_SIGNING_KEY",
            "EMBER_DATA_DIR",
            "EMBER_LISTEN",
            "PATH",
            "OPENAI_API_KEY",
        ];
        let got = scrubbed_names(names);
        assert_eq!(got, vec!["EMBER_SERVER_TOKEN", "EMBER_NODE_TOKEN", "EMBER_HUB_SECRET", "EMBER_SIGNING_KEY"]);
    }

    #[test]
    fn fr_a11_denied_paths_catch_dotdot_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let data = root.join(".ember");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("server-token"), "t").unwrap();
        std::fs::create_dir_all(root.join("proj")).unwrap();
        let denied = vec![data.clone()];
        assert!(is_denied_in(&data.join("server-token"), &denied));
        assert!(is_denied_in(&root.join("proj/../.ember/server-token"), &denied));
        assert!(is_denied_in(&data.join("new/file"), &denied));
        assert!(!is_denied_in(&root.join("proj/readme"), &denied));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&data, root.join("proj/link")).unwrap();
            assert!(is_denied_in(&root.join("proj/link/server-token"), &denied));
        }
        assert!(!is_denied_in(&data.join("server-token"), &[]));
    }

    #[test]
    fn fr_a11_macos_profile_denies_then_allows_carve_outs() {
        let prof = macos_profile(&[p("/h/.ember")], &[p("/h/.ember/accounts/a1")]);
        let deny = prof.find("(deny file-read* file-write* (subpath \"/h/.ember\"))").unwrap();
        let allow = prof.find("(allow file-read* file-write* (subpath \"/h/.ember/accounts/a1\"))").unwrap();
        assert!(prof.starts_with("(version 1)\n(allow default)\n"));
        assert!(deny < allow, "{prof}");
        assert!(macos_profile(&[p("/a\"b")], &[]).contains("\"/a\\\"b\""));
    }

    #[test]
    fn fr_a11_wrap_puts_the_agent_after_the_sandbox_tool() {
        let (prog, args) = wrap_with(
            &Tool::SandboxExec(p("/usr/bin/sandbox-exec")),
            &[p("/d")],
            &[],
            &p("/bin/claude"),
            &["--print".into()],
        );
        assert_eq!(prog, p("/usr/bin/sandbox-exec"));
        assert_eq!(args[0], "-p");
        assert_eq!(&args[2..], &["/bin/claude".to_string(), "--print".to_string()]);

        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data");
        let acct = d.join("accounts/x");
        std::fs::create_dir_all(&acct).unwrap();
        let (prog, args) = wrap_with(&Tool::Bwrap(p("/usr/bin/bwrap")), &[d.clone()], &[acct.clone()], &p("/bin/omp"), &["acp".into()]);
        assert_eq!(prog, p("/usr/bin/bwrap"));
        let joined = args.join(" ");
        assert!(joined.starts_with("--dev-bind / / --tmpfs "), "{joined}");
        assert!(joined.contains(&format!("--tmpfs {}", d.display())));
        assert!(joined.contains(&format!("--bind {0} {0}", acct.display())));
        assert!(joined.ends_with("-- /bin/omp acp"), "{joined}");
    }

    #[test]
    fn fr_a11_account_home_under_the_data_dir_is_carved_out() {
        let env = vec![
            ("CLAUDE_CONFIG_DIR".to_string(), "/h/.ember/accounts/a1".to_string()),
            ("CODEX_HOME".to_string(), "/elsewhere/codex".to_string()),
            ("OTHER".to_string(), "/h/.ember/x".to_string()),
        ];
        assert_eq!(allowed_for(&env, &[p("/h/.ember")]), vec![p("/h/.ember/accounts/a1")]);
    }

    #[test]
    fn fr_a11_switch_and_claude_settings() {
        assert!(switched_off(Some("off")) && switched_off(Some("0")) && !switched_off(None) && !switched_off(Some("on")));
        assert!(claude_settings(&[]).is_none());
        let s: serde_json::Value = serde_json::from_str(&claude_settings(&[p("/h/.ember/")]).unwrap()).unwrap();
        let deny: Vec<&str> = s["permissions"]["deny"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(deny, vec!["Read(//h/.ember/**)", "Edit(//h/.ember/**)", "Write(//h/.ember/**)"]);
    }
}
