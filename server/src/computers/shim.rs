//! `ember-exec`: the shell shim that runs Claude Code's Bash tool on an ember node
//! (`docs/design/INTERCEPTION.md`, option (a); binary in `src/bin/ember-exec.rs`).
//!
//! Claude Code (2.1.288, read from the binary) builds each Bash command as
//!
//! ```text
//! source <snapshot> 2>/dev/null || true && … && eval '<command>' && pwd -P >| <cwd-file>
//! ```
//!
//! and, when `CLAUDE_CODE_SHELL_PREFIX` is set, runs `$SHELL -c -l "<prefix> '<that string>'"`
//! (the prefix is shell-quoted; a prefix containing ` -` is split so its trailing flags stay
//! flags). So `ember-exec` receives the whole string as its **last argument**, with Claude's
//! tracked working directory as its own cwd. The same prefix also wraps hook commands and stdio
//! MCP server launches; those do not end in the cwd-file step.
//!
//! For a Bash-tool command the shim:
//! 1. replaces the trailing `pwd -P >| <cwd-file>` with a marker line on stderr,
//! 2. runs the string on the node (`/v1/exec`) as `<remote shell> -l -c <string>` in the same
//!    absolute directory, forwarding stdin and streaming stdout/stderr back,
//! 3. strips the marker from stderr and writes the node's final `pwd -P` to the **local**
//!    cwd-file, so Claude's cwd tracking follows `cd` on the node,
//! 4. exits with the command's code (128 + signal when it was killed).
//!
//! SIGINT/SIGTERM/SIGHUP to the shim are forwarded to the remote process group; if the shim is
//! killed outright, the node kills the command when the connection drops.
//!
//! Anything else (hooks, MCP launches) runs **locally** by default (`EMBER_EXEC_NON_TOOL=local`),
//! because those refer to this server's files; `remote` sends them to the node too.
//!
//! The server's shell snapshot path does not exist on the node; its `source` fails quietly
//! (`|| true`), so the node's own login environment applies (FR-X2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ember_node::client::NodeClient;
use ember_node::proto::{CommandSpec, ExecEvent, ExecInput, ExecRequest, Program};

/// Node API base URL (`http://host:port`).
pub const ENV_NODE_URL: &str = "EMBER_EXEC_NODE_URL";
/// Node API bearer token.
pub const ENV_NODE_TOKEN: &str = "EMBER_EXEC_NODE_TOKEN";
/// Shell that runs the command on the node (default `bash`); set from the node's `/v1/env`.
pub const ENV_REMOTE_SHELL: &str = "EMBER_EXEC_REMOTE_SHELL";
/// `local` (default) or `remote`: where non-Bash-tool invocations (hooks, MCP launches) run.
pub const ENV_NON_TOOL: &str = "EMBER_EXEC_NON_TOOL";
/// Comma-separated variable names copied from the shim's environment to the remote command.
pub const ENV_FORWARD: &str = "EMBER_EXEC_FORWARD_ENV";
/// Variables forwarded by default: what Claude Code sets for its tools, nothing server-specific.
pub const DEFAULT_FORWARD: &[&str] = &["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "GIT_EDITOR", "COLUMNS", "LINES"];

const MARK_START: &[u8] = b"\x1eEMBER_CWD:";
const MARK_END: &[u8] = b"\x1e\n";
const CWD_STEP: &str = " && pwd -P >| ";

/// Parsed command line: options, then the command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Force where this invocation runs, overriding the classification.
    pub force: Option<Where>,
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Local,
    Remote,
}

/// `ember-exec [--local|--remote] [--] <command string>` (arguments after `argv[0]`).
pub fn parse_args(args: &[String]) -> Result<Invocation, String> {
    let Some((command, opts)) = args.split_last() else {
        return Err("usage: ember-exec [--local|--remote] <command string>".into());
    };
    let mut force = None;
    for opt in opts {
        match opt.as_str() {
            "--local" => force = Some(Where::Local),
            "--remote" => force = Some(Where::Remote),
            "--" => {}
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(Invocation { force, command: command.clone() })
}

/// What kind of invocation a command string is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A Bash-tool command: everything before the cwd step, and the local cwd-file path.
    BashTool { body: String, cwd_file: PathBuf },
    /// A hook command, an MCP launch, or anything else.
    Other,
}

/// Recognise Claude Code's trailing `&& pwd -P >| <cwd-file>` step.
pub fn classify(command: &str) -> Kind {
    let Some(i) = command.rfind(CWD_STEP) else {
        return Kind::Other;
    };
    match unquote_word(&command[i + CWD_STEP.len()..]) {
        Some(path) if path.starts_with('/') => {
            Kind::BashTool { body: command[..i].to_string(), cwd_file: PathBuf::from(path) }
        }
        _ => Kind::Other,
    }
}

/// Undo one shell word as `shell-quote` writes it: bare, `'…'` (with `'\''` joins), or `"…"`
/// (with backslash escapes). `None` if the input is not exactly one word.
pub fn unquote_word(s: &str) -> Option<String> {
    let s = s.trim_end();
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    chars.peek()?;
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => out.push(c),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => out.push(chars.next()?),
                    c => out.push(c),
                }
            },
            '\\' => out.push(chars.next()?),
            c if c.is_whitespace() || ";&|<>()$`".contains(c) => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// The string to run on the node: the body, then the node's cwd as a marker on stderr.
pub fn remote_command(body: &str) -> String {
    format!("{body} && {{ printf '\\036EMBER_CWD:%s\\036\\n' \"$(pwd -P)\" >&2; }}")
}

/// Removes the cwd marker from a stderr stream, wherever chunk boundaries fall.
#[derive(Debug, Default)]
pub struct MarkerFilter {
    pending: Vec<u8>,
    cwd: Option<String>,
}

impl MarkerFilter {
    /// Feed a chunk; returns the bytes to pass through now.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            match find(&self.pending, MARK_START) {
                Some(i) => {
                    out.extend_from_slice(&self.pending[..i]);
                    self.pending.drain(..i);
                    let body = MARK_START.len();
                    match find(&self.pending[body..], MARK_END) {
                        Some(j) => {
                            let cwd = &self.pending[body..body + j];
                            self.cwd = Some(String::from_utf8_lossy(cwd).into_owned());
                            self.pending.drain(..body + j + MARK_END.len());
                        }
                        None => break, // wait for the rest of the marker
                    }
                }
                None => {
                    // Keep back only a tail that could be the start of a marker.
                    let keep = partial_prefix(&self.pending, MARK_START);
                    let emit = self.pending.len() - keep;
                    out.extend(self.pending.drain(..emit));
                    break;
                }
            }
        }
        out
    }

    /// End of stream: whatever is still held back, and the cwd if a marker was seen.
    pub fn finish(mut self) -> (Vec<u8>, Option<String>) {
        (std::mem::take(&mut self.pending), self.cwd)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Length of the longest suffix of `hay` that is a proper prefix of `needle`.
fn partial_prefix(hay: &[u8], needle: &[u8]) -> usize {
    (1..needle.len().min(hay.len() + 1)).rev().find(|&n| hay.ends_with(&needle[..n])).unwrap_or(0)
}

/// Exit status for the shim from the remote outcome.
pub fn exit_code(code: Option<i32>, signal: Option<i32>) -> i32 {
    match (code, signal) {
        (Some(c), _) => c,
        (None, Some(s)) => 128 + s,
        (None, None) => 1,
    }
}

/// Configuration from the environment.
#[derive(Debug, Clone)]
pub struct ShimEnv {
    pub node_url: Option<String>,
    pub node_token: Option<String>,
    pub remote_shell: String,
    pub non_tool: Where,
    pub forward: BTreeMap<String, String>,
}

impl ShimEnv {
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> ShimEnv {
        let names: Vec<String> = match get(ENV_FORWARD) {
            Some(list) => list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            None => DEFAULT_FORWARD.iter().map(|s| s.to_string()).collect(),
        };
        ShimEnv {
            node_url: get(ENV_NODE_URL).filter(|s| !s.is_empty()),
            node_token: get(ENV_NODE_TOKEN).filter(|s| !s.is_empty()),
            remote_shell: get(ENV_REMOTE_SHELL).filter(|s| !s.is_empty()).unwrap_or_else(|| "bash".into()),
            non_tool: match get(ENV_NON_TOOL).as_deref() {
                Some("remote") => Where::Remote,
                _ => Where::Local,
            },
            forward: names.into_iter().filter_map(|n| get(&n).map(|v| (n, v))).collect(),
        }
    }

    pub fn from_env() -> ShimEnv {
        Self::from_vars(|k| std::env::var(k).ok())
    }
}

/// What the shim will do.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Run the string with the local `$SHELL -c` (no node configured, or a non-tool invocation).
    Local { command: String },
    /// Run on the node; write the final cwd to `cwd_file` when there is one.
    Remote { request: ExecRequest, cwd_file: Option<PathBuf> },
}

/// Decide where and how to run `inv`, with `cwd` as the remote working directory.
pub fn plan(inv: &Invocation, env: &ShimEnv, cwd: &Path) -> Plan {
    let kind = classify(&inv.command);
    let configured = env.node_url.is_some() && env.node_token.is_some();
    let target = match inv.force {
        Some(w) => w,
        None if !configured => Where::Local,
        None => match kind {
            Kind::BashTool { .. } => Where::Remote,
            Kind::Other => env.non_tool,
        },
    };
    if target == Where::Local || !configured {
        return Plan::Local { command: inv.command.clone() };
    }
    let (command, cwd_file) = match kind {
        Kind::BashTool { body, cwd_file } => (remote_command(&body), Some(cwd_file)),
        Kind::Other => (inv.command.clone(), None),
    };
    Plan::Remote {
        request: ExecRequest {
            command: CommandSpec {
                program: Program::Argv(vec![env.remote_shell.clone(), "-l".into(), "-c".into(), command]),
                cwd: cwd.to_path_buf(),
                env: env.forward.clone(),
                env_clear: false,
            },
            pty: None,
        },
        cwd_file,
    }
}

/// Run a [`Plan::Remote`]: stream the command on the node and return the shim's exit code.
pub async fn run_remote(env: &ShimEnv, request: ExecRequest, cwd_file: Option<PathBuf>) -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let url = env.node_url.clone().unwrap_or_default();
    let client = match NodeClient::new(&url, env.node_token.clone().unwrap_or_default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ember-exec: {e}");
            return 255;
        }
    };
    let session = match client.exec(&request).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ember-exec: could not run the command on {url}: {e}");
            return 255;
        }
    };
    let (tx, mut rx) = session.into_split();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(tx));

    // stdin → node, then close it.
    let stdin_task = {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.lock().await.stdin(buf[..n].to_vec()).await.is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = tx.lock().await.send(ExecInput::CloseStdin).await;
        })
    };
    // Signals → the remote process group.
    let signal_task = {
        let tx = tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let (Ok(mut int), Ok(mut term), Ok(mut hup)) = (
                signal(SignalKind::interrupt()),
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            loop {
                let sig = tokio::select! {
                    _ = int.recv() => libc::SIGINT,
                    _ = term.recv() => libc::SIGTERM,
                    _ = hup.recv() => libc::SIGHUP,
                };
                let _ = tx.lock().await.send(ExecInput::Kill { signal: Some(sig) }).await;
            }
        })
    };

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut filter = MarkerFilter::default();
    let code = loop {
        match rx.recv().await {
            Ok(Some(ExecEvent::Stdout { data })) => {
                let _ = stdout.write_all(&data).await;
                let _ = stdout.flush().await;
            }
            Ok(Some(ExecEvent::Stderr { data })) => {
                let pass = filter.feed(&data);
                if !pass.is_empty() {
                    let _ = stderr.write_all(&pass).await;
                    let _ = stderr.flush().await;
                }
            }
            Ok(Some(ExecEvent::Exit { code, signal })) => break exit_code(code, signal),
            Ok(Some(ExecEvent::Error { message })) => {
                eprintln!("ember-exec: {message}");
                break 255;
            }
            Ok(Some(ExecEvent::Started { .. })) => {}
            Ok(None) => {
                eprintln!("ember-exec: connection to {url} closed before the command exited");
                break 255;
            }
            Err(e) => {
                eprintln!("ember-exec: {e}");
                break 255;
            }
        }
    };
    stdin_task.abort();
    signal_task.abort();
    let (rest, cwd) = filter.finish();
    if !rest.is_empty() {
        let _ = stderr.write_all(&rest).await;
        let _ = stderr.flush().await;
    }
    if let (Some(path), Some(cwd)) = (cwd_file, cwd) {
        if let Err(e) = std::fs::write(&path, format!("{cwd}\n")) {
            eprintln!("ember-exec: could not record the working directory in {}: {e}", path.display());
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    const CLAUDE_CMD: &str = "source /Users/me/.claude/shell-snapshots/snapshot-zsh-1.sh 2>/dev/null || true && { shopt -u extglob || setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL; } >/dev/null 2>&1 || true && eval 'cd src && ls' && pwd -P >| /var/folders/x/T/claude-ab12-cwd";

    #[test]
    fn last_argument_is_the_command_and_options_come_first() {
        assert_eq!(
            parse_args(&args(&["echo hi"])).unwrap(),
            Invocation { force: None, command: "echo hi".into() }
        );
        assert_eq!(parse_args(&args(&["--remote", "--", "x"])).unwrap().force, Some(Where::Remote));
        assert_eq!(parse_args(&args(&["--local", "x"])).unwrap().force, Some(Where::Local));
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&args(&["--bogus", "x"])).is_err());
        // A command that itself starts with dashes is still the command.
        assert_eq!(parse_args(&args(&["--version"])).unwrap().command, "--version");
    }

    #[test]
    fn classifies_bash_tool_commands_by_the_cwd_step() {
        match classify(CLAUDE_CMD) {
            Kind::BashTool { body, cwd_file } => {
                assert!(body.ends_with("eval 'cd src && ls'"), "{body}");
                assert_eq!(cwd_file, PathBuf::from("/var/folders/x/T/claude-ab12-cwd"));
            }
            other => panic!("{other:?}"),
        }
        // Quoted paths (shell-quote style).
        let quoted = "eval 'x' && pwd -P >| '/tmp/my dir/claude-1-cwd'";
        assert!(matches!(classify(quoted), Kind::BashTool { cwd_file, .. } if cwd_file == Path::new("/tmp/my dir/claude-1-cwd")));
        let dq = "eval 'x' && pwd -P >| \"/tmp/a\\\"b\"";
        assert!(matches!(classify(dq), Kind::BashTool { cwd_file, .. } if cwd_file == Path::new("/tmp/a\"b")));
        // Hooks and MCP launches.
        assert_eq!(classify("'/usr/local/bin/my-mcp' '--stdio'"), Kind::Other);
        assert_eq!(classify("echo done && pwd -P >| /tmp/x; rm -rf /"), Kind::Other);
        assert_eq!(classify("pwd -P >| relative"), Kind::Other);
    }

    #[test]
    fn remote_command_reports_cwd_on_stderr() {
        let cmd = remote_command("eval 'cd /tmp'");
        assert_eq!(cmd, "eval 'cd /tmp' && { printf '\\036EMBER_CWD:%s\\036\\n' \"$(pwd -P)\" >&2; }");
    }

    #[test]
    fn marker_is_stripped_across_chunk_boundaries() {
        let stream = b"warn: x\n\x1eEMBER_CWD:/home/pi/proj/src\x1e\n";
        for split in 0..stream.len() {
            let mut f = MarkerFilter::default();
            let mut out = f.feed(&stream[..split]);
            out.extend(f.feed(&stream[split..]));
            let (rest, cwd) = f.finish();
            out.extend(rest);
            assert_eq!(out, b"warn: x\n", "split at {split}");
            assert_eq!(cwd.as_deref(), Some("/home/pi/proj/src"), "split at {split}");
        }
        // Output that only looks like the start of a marker is passed through at the end.
        let mut f = MarkerFilter::default();
        let mut out = f.feed(b"abc\x1eEMB");
        let (rest, cwd) = f.finish();
        out.extend(rest);
        assert_eq!(out, b"abc\x1eEMB");
        assert_eq!(cwd, None);
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code(Some(3), None), 3);
        assert_eq!(exit_code(None, Some(9)), 137);
        assert_eq!(exit_code(None, None), 1);
    }

    fn env_with_node() -> ShimEnv {
        ShimEnv::from_vars(|k| match k {
            ENV_NODE_URL => Some("http://10.0.0.2:8741".into()),
            ENV_NODE_TOKEN => Some("tok".into()),
            ENV_REMOTE_SHELL => Some("/bin/zsh".into()),
            "CLAUDECODE" => Some("1".into()),
            "PATH" => Some("/server/only".into()),
            _ => None,
        })
    }

    #[test]
    fn plan_sends_bash_tool_commands_to_the_node() {
        let env = env_with_node();
        let inv = parse_args(&args(&[CLAUDE_CMD])).unwrap();
        match plan(&inv, &env, Path::new("/home/pi/proj")) {
            Plan::Remote { request, cwd_file } => {
                assert_eq!(cwd_file, Some(PathBuf::from("/var/folders/x/T/claude-ab12-cwd")));
                assert_eq!(request.command.cwd, PathBuf::from("/home/pi/proj"));
                let Program::Argv(argv) = &request.command.program else { panic!() };
                assert_eq!(&argv[..3], ["/bin/zsh", "-l", "-c"]);
                assert!(argv[3].contains("eval 'cd src && ls' && { printf"), "{}", argv[3]);
                assert!(!argv[3].contains("claude-ab12-cwd"));
                // Only allow-listed variables travel; the server's PATH does not.
                assert_eq!(request.command.env.get("CLAUDECODE").map(String::as_str), Some("1"));
                assert!(!request.command.env.contains_key("PATH"));
                assert!(request.pty.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plan_keeps_hooks_local_unless_asked() {
        let env = env_with_node();
        let hook = parse_args(&args(&["'/Users/me/bin/hook.sh'"])).unwrap();
        assert!(matches!(plan(&hook, &env, Path::new("/x")), Plan::Local { .. }));
        let forced = parse_args(&args(&["--remote", "'/Users/me/bin/hook.sh'"])).unwrap();
        assert!(matches!(plan(&forced, &env, Path::new("/x")), Plan::Remote { cwd_file: None, .. }));
        let mut remote_all = env.clone();
        remote_all.non_tool = Where::Remote;
        assert!(matches!(plan(&hook, &remote_all, Path::new("/x")), Plan::Remote { .. }));
        // No node configured: everything is local, even a forced remote.
        let none = ShimEnv::from_vars(|_| None);
        assert_eq!(none.remote_shell, "bash");
        let inv = parse_args(&args(&[CLAUDE_CMD])).unwrap();
        assert!(matches!(plan(&inv, &none, Path::new("/x")), Plan::Local { .. }));
        assert!(matches!(plan(&forced, &none, Path::new("/x")), Plan::Local { .. }));
    }
}
