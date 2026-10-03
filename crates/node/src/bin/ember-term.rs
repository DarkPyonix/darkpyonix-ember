//! `ember-term` — attach the terminal it runs in to an ember node persistent session.
//!
//! It is the shell of VS Code's terminal profiles (SPEC §P, `docs/design/TERMINALS.md`): VS Code
//! starts it in its own PTY, and it pipes that PTY to a session owned by ember node, so the
//! program keeps running when the VS Code window, the VS Code server or this process goes away.
//!
//! ```text
//! ember-term                         new session running the user's shell; attach
//! ember-term -c <command>            new session running `$SHELL -c <command>`; attach; exit with
//!                                    its status (VS Code tasks via automationProfile)
//! ember-term attach <id> [--passive] attach to an existing session
//! ember-term attach-or-create --key <key> [-- argv…]
//! ember-term list [--project <p>] [--all] [--json]
//! ember-term kill <id> [--signal <n>]
//!
//! common: --node <url> --origin <ide-vscode|ide-ember|agent|user> --project <p> --title <t>
//!         --device <name> --cwd <dir>
//! ```
//!
//! The node is found through `EMBER_NODE_URL`/`EMBER_NODE_TOKEN` or the endpoint file the node
//! writes (`~/.ember/node/local.json`). The session gets this process's whole environment (so a
//! task's `options.env` and VS Code's `terminal.integrated.env.*` apply), minus the node token.
//!
//! When this process is hung up (window closed, terminal disposed) it **detaches**; the
//! session continues. Killing a session is explicit (`kill`, or the companion extension when
//! the user closes the terminal).

use std::collections::BTreeMap;
use std::process::exit;
use std::time::{Duration, Instant};

use ember_node::client::NodeClient;
use ember_node::config::LocalEndpoint;
use ember_node::proto::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Debug, Default)]
struct Opts {
    node: Option<String>,
    origin: Option<TermOrigin>,
    project: Option<String>,
    title: Option<String>,
    device: Option<String>,
    cwd: Option<String>,
    key: Option<String>,
    passive: bool,
    all: bool,
    json: bool,
    signal: Option<i32>,
}

#[derive(Debug)]
enum Cmd {
    Shell,
    Command(String),
    Attach(String),
    AttachOrCreate(Vec<String>),
    List,
    Kill(String),
}

fn usage() -> ! {
    eprintln!(
        "usage: ember-term [-c <command>] | attach <id> [--passive] | attach-or-create --key <k> [-- argv…] | list [--all] [--json] | kill <id> [--signal n]\n\
         options: --node <url> --origin <ide-vscode|ide-ember|agent|user> --project <p> --title <t> --device <d> --cwd <dir>"
    );
    exit(2)
}

fn next_value(it: &mut impl Iterator<Item = String>, name: &str) -> String {
    it.next().unwrap_or_else(|| {
        eprintln!("ember-term: {name} needs a value");
        exit(2)
    })
}

fn parse(args: Vec<String>) -> (Cmd, Opts) {
    let mut o = Opts::default();
    let mut cmd: Option<Cmd> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                rest.extend(it.by_ref());
                break;
            }
            // VS Code runs automation shells as `<shell> [args] -c "<command line>"`.
            "-c" => cmd = Some(Cmd::Command(next_value(&mut it, "-c"))),
            // Shell-style flags VS Code or users may add; a session shell is interactive anyway.
            "-l" | "--login" | "-i" => {}
            "--node" => o.node = Some(next_value(&mut it, "--node")),
            "--origin" => {
                o.origin = Some(next_value(&mut it, "--origin").parse().unwrap_or_else(|e: String| {
                    eprintln!("ember-term: {e}");
                    exit(2)
                }))
            }
            "--project" => o.project = Some(next_value(&mut it, "--project")),
            "--title" => o.title = Some(next_value(&mut it, "--title")),
            "--device" => o.device = Some(next_value(&mut it, "--device")),
            "--cwd" => o.cwd = Some(next_value(&mut it, "--cwd")),
            "--key" => o.key = Some(next_value(&mut it, "--key")),
            "--signal" => o.signal = next_value(&mut it, "--signal").parse().ok(),
            "--passive" => o.passive = true,
            "--all" => o.all = true,
            "--json" => o.json = true,
            "-h" | "--help" => usage(),
            s if s.starts_with('-') => {
                eprintln!("ember-term: unknown option {s}");
                usage()
            }
            _ => positional.push(a),
        }
    }
    let cmd = match (cmd, positional.first().map(String::as_str)) {
        (Some(c), _) => c,
        (None, None) => Cmd::Shell,
        (None, Some("attach")) => Cmd::Attach(positional.get(1).cloned().unwrap_or_else(|| usage())),
        (None, Some("attach-or-create")) => {
            if o.key.is_none() {
                eprintln!("ember-term: attach-or-create needs --key");
                exit(2)
            }
            Cmd::AttachOrCreate(rest)
        }
        (None, Some("list")) => Cmd::List,
        (None, Some("kill")) => Cmd::Kill(positional.get(1).cloned().unwrap_or_else(|| usage())),
        (None, Some(_)) => usage(),
    };
    (cmd, o)
}

fn user_shell() -> String {
    std::env::var("EMBER_TERM_SHELL")
        .ok()
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| !s.is_empty() && !s.ends_with("/ember-term") && s != "ember-term")
        .unwrap_or_else(|| "/bin/sh".into())
}

fn default_origin() -> TermOrigin {
    match std::env::var("EMBER_TERM_ORIGIN").ok().and_then(|s| s.parse().ok()) {
        Some(o) => o,
        None if std::env::var("TERM_PROGRAM").as_deref() == Ok("vscode") => TermOrigin::IdeVscode,
        None => TermOrigin::User,
    }
}

fn device_name(o: &Opts) -> String {
    o.device.clone().or_else(|| std::env::var("EMBER_DEVICE").ok()).filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let host = std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "localhost".into());
        format!("ember-term@{host}")
    })
}

/// The local terminal's size.
fn local_size() -> Option<PtySize> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    for fd in [1, 0, 2] {
        // SAFETY: TIOCGWINSZ into a zeroed winsize.
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut ws as *mut libc::winsize) } == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return Some(PtySize { rows: ws.ws_row, cols: ws.ws_col });
        }
    }
    None
}

/// Raw mode on stdin while attached; restored on drop.
struct RawMode {
    orig: libc::termios,
}

impl RawMode {
    fn enable() -> Option<Self> {
        // SAFETY: termios calls on fd 0 with properly initialised structs.
        unsafe {
            if libc::isatty(0) != 1 {
                return None;
            }
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return None;
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                return None;
            }
            Some(RawMode { orig })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring the attributes read in `enable`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.orig);
        }
    }
}

fn status_code(code: Option<i32>, signal: Option<i32>) -> i32 {
    match (code, signal) {
        (Some(c), _) => c,
        (None, Some(s)) => 128 + s,
        // Unknown (re-adopted session): report success rather than inventing a failure.
        (None, None) => 0,
    }
}

#[tokio::main]
async fn main() {
    let (cmd, opts) = parse(std::env::args().skip(1).collect());
    let code = match run(cmd, opts).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ember-term: {e:#}");
            1
        }
    };
    // `exit` rather than returning: tokio's stdin reader thread would otherwise keep the
    // process alive until the next keystroke.
    exit(code)
}

async fn run(cmd: Cmd, o: Opts) -> anyhow::Result<i32> {
    let mut ep = LocalEndpoint::discover()?;
    if let Some(url) = &o.node {
        ep.url = url.clone();
    }
    let client = NodeClient::new(&ep.url, &ep.token)?;
    match cmd {
        Cmd::List => list(&client, &o).await,
        Cmd::Kill(id) => {
            client.term_kill(&id, o.signal).await?;
            Ok(0)
        }
        Cmd::Attach(id) => attach(&client, &id, &o).await,
        Cmd::Shell => {
            let id = create(&client, &o, vec![user_shell()].into_iter().chain(login_flag()).collect(), "shell").await?;
            attach(&client, &id, &o).await
        }
        Cmd::Command(line) => {
            let id = create(&client, &o, vec![user_shell(), "-c".into(), line], "task").await?;
            attach(&client, &id, &o).await
        }
        Cmd::AttachOrCreate(argv) => {
            let argv = if argv.is_empty() { vec![user_shell()].into_iter().chain(login_flag()).collect() } else { argv };
            let id = create(&client, &o, argv, "attach-or-create").await?;
            attach(&client, &id, &o).await
        }
    }
}

fn login_flag() -> Option<String> {
    cfg!(target_os = "macos").then(|| "-l".to_string())
}

async fn create(client: &NodeClient, o: &Opts, argv: Vec<String>, mode: &str) -> anyhow::Result<String> {
    let cwd = match &o.cwd {
        Some(c) => std::path::PathBuf::from(c),
        None => std::env::current_dir()?,
    };
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| k != "EMBER_NODE_TOKEN" && k != "EMBER_NODE_URL" && k != "EMBER_TERM_ID")
        .collect();
    let mut tags = BTreeMap::new();
    tags.insert("ember-term.pid".to_string(), std::process::id().to_string());
    tags.insert("ember-term.mode".to_string(), mode.to_string());
    if let Ok(w) = std::env::var("EMBER_VSCODE_WINDOW") {
        tags.insert("vscode.window".to_string(), w);
    }
    let project = o
        .project
        .clone()
        .or_else(|| std::env::var("EMBER_PROJECT").ok().filter(|s| !s.is_empty()))
        .or_else(|| Some(cwd.display().to_string()));
    let title = o.title.clone().or_else(|| match mode {
        "task" => argv.last().map(|l| format!("task: {}", l.chars().take(60).collect::<String>())),
        _ => None,
    });
    let res = client
        .term_create(&TermCreateRequest {
            program: Some(Program::Argv(argv)),
            cwd,
            env,
            env_clear: true,
            size: local_size(),
            origin: o.origin.unwrap_or_else(default_origin),
            project,
            title,
            key: o.key.clone(),
            tags,
        })
        .await?;
    Ok(res.term.id)
}

async fn list(client: &NodeClient, o: &Opts) -> anyhow::Result<i32> {
    let q = TermListQuery {
        project: if o.all { None } else { o.project.clone().or_else(|| std::env::var("EMBER_PROJECT").ok()) },
        origin: o.origin,
        running: None,
    };
    let terms = client.terms(&q).await?;
    if o.json {
        println!("{}", serde_json::to_string_pretty(&terms)?);
        return Ok(0);
    }
    println!("{:<32}  {:<8}  {:<10}  {:>3}  TITLE", "ID", "STATE", "ORIGIN", "ATT");
    for t in terms {
        let state = match t.state {
            TermState::Running => "running".to_string(),
            TermState::Exited => format!("exit {}", status_code(t.exit_code, t.signal)),
            TermState::Lost => "lost".to_string(),
        };
        println!("{:<32}  {:<8}  {:<10}  {:>3}  {}", t.id, state, t.origin.as_str(), t.clients.len(), t.title);
    }
    Ok(0)
}

async fn attach(client: &NodeClient, id: &str, o: &Opts) -> anyhow::Result<i32> {
    let hello = TermHello {
        device: device_name(o),
        kind: Some("ember-term".into()),
        pid: Some(std::process::id()),
        size: local_size(),
        active: !o.passive,
        read_only: false,
        snapshot: true,
    };
    let att = client.term_attach(id, &hello).await?;
    let _raw = RawMode::enable();
    let (mut tx, mut rx) = att.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut winch = signal(SignalKind::window_change())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut term = signal(SignalKind::terminate())?;
    let mut buf = vec![0u8; 16 * 1024];
    let mut last_notice: Option<Instant> = None;
    let mut stdin_open = true;
    let code = loop {
        tokio::select! {
            ev = rx.recv() => match ev? {
                Some(TermEvent::Snapshot { data, .. }) | Some(TermEvent::Output { data }) => {
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                Some(TermEvent::Exit { code, signal }) => break status_code(code, signal),
                Some(TermEvent::Refused { reason, controller }) => {
                    // Tell the user once in a while, without drawing into the program's screen:
                    // a bell and the window title.
                    if last_notice.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
                        last_notice = Some(Instant::now());
                        let who = controller.map(|c| c.device).unwrap_or_default();
                        let msg = match reason {
                            TermRefusal::Controlled => format!("read-only: controlled by {who}"),
                            TermRefusal::ReadOnly => "read-only".to_string(),
                            TermRefusal::NotRunning => "session ended".to_string(),
                        };
                        stdout.write_all(format!("\x07\x1b]0;{msg}\x07").as_bytes()).await?;
                        stdout.flush().await?;
                    }
                }
                Some(TermEvent::Error { message }) => anyhow::bail!("{message}"),
                Some(_) => {}
                None => anyhow::bail!("ember node closed the connection"),
            },
            n = stdin.read(&mut buf), if stdin_open => match n {
                Ok(0) | Err(_) => stdin_open = false,
                Ok(n) => tx.input(buf[..n].to_vec()).await?,
            },
            _ = winch.recv() => {
                if let Some(size) = local_size() {
                    tx.send(TermInput::Resize { size }).await?;
                }
            }
            // The VS Code terminal (or window) went away: detach, the session lives on.
            _ = hup.recv() => break 129,
            _ = term.recv() => break 143,
        }
    };
    let _ = tx.detach().await;
    Ok(code)
}
