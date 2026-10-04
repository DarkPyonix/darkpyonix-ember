//! The agent-side A2A tool, `ember-a2a` (SPEC FR-T2).
//!
//! Reads `EMBER_URL` and `EMBER_RUNTIME_TOKEN` from the environment the server gave the agent
//! process, and calls the A2A API. A deliberately tiny, blocking HTTP/1.1 client over a plain TCP
//! socket: the server is local to the agent process (D3) and this keeps the tool dependency-free.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::{json, Value};

const USAGE: &str = "\
ember-a2a: message other agent sessions in Ember, and schedule prompts

Usage:
  ember-a2a list [--json]
      Sessions you can message: id, agent, status, project, title.
  ember-a2a send <session-id> [--reply-to <message-id>] <text...>
      Send a message. Use `-` (or no text) to read the text from stdin.
  ember-a2a show <message-id>
      A message you sent or received, with its delivery time.

Teams (FR-T7):
  ember-a2a team list [--json]
      Your team: members (name, session, agent, status) and tasks.
  ember-a2a team spawn <name> [--agent <kind>] [--account <id>] [--computer <id>]
                       [--title <title>] <first prompt...>
      Start a teammate session in this project; the first spawn makes you the leader.
  ember-a2a team end <name>
      Stop a teammate (leader only).
  ember-a2a task list [--json]
  ember-a2a task add <title...> [--detail <text>] [--assign <name>]
  ember-a2a task update <n> [--status open|in_progress|blocked|done|cancelled]
                        [--assign <name>|none] [--title <title>] [--detail <text>]
  ember-a2a mail send <name>|--all <text...>
      Team mail to one member or everyone; it reaches them like a message.
  ember-a2a mail read [--after <n>] [--limit <n>] [--json]
      Your mailbox, oldest first.

Schedules (FR-A8):
  ember-a2a schedule list [--json]
      Schedules in this session's project.
  ember-a2a schedule add (--cron '<min hour dom mon dow>' [--tz <IANA zone>] | --every <30m|2h|1d|secs>
                          | --at <RFC 3339 time>) [--new [--title <title>]] [--catch-up] <prompt...>
      Send <prompt> on a schedule: by default into this session; with --new, into a new
      session in this project each time. Use `-` (or no prompt) to read it from stdin.
      --catch-up runs the latest trigger missed while the server was down.
  ember-a2a schedule rm <schedule-id>
      Delete a schedule of this project.

Environment (set by Ember for every agent session):
  EMBER_URL, EMBER_RUNTIME_TOKEN";

pub struct Client {
    host: String,
    port: u16,
    prefix: String,
    token: String,
}

impl Client {
    /// `base_url` must be `http://host[:port][/prefix]`.
    pub fn new(base_url: &str, token: &str) -> anyhow::Result<Client> {
        let rest = base_url
            .strip_prefix("http://")
            .with_context(|| format!("EMBER_URL must start with http:// (got {base_url})"))?;
        let (authority, prefix) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
            None => (rest, ""),
        };
        // The port follows the last ':' that is not inside an IPv6 literal.
        let port_sep = authority
            .rfind(':')
            .filter(|i| !authority[*i..].contains(']'));
        let (host, port) = match port_sep {
            Some(i) => (
                authority[..i].to_string(),
                authority[i + 1..]
                    .parse()
                    .with_context(|| format!("bad port in {base_url}"))?,
            ),
            None => (authority.to_string(), 80),
        };
        Ok(Client {
            host,
            port,
            prefix: prefix.to_string(),
            token: token.to_string(),
        })
    }

    pub fn from_env() -> anyhow::Result<Client> {
        let url = std::env::var("EMBER_URL").context(
            "EMBER_URL is not set; ember-a2a only works inside an agent session run by Ember",
        )?;
        let token =
            std::env::var("EMBER_RUNTIME_TOKEN").context("EMBER_RUNTIME_TOKEN is not set")?;
        Client::new(&url, &token)
    }

    /// One request; returns the status code and the JSON body (`null` when empty).
    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<(u16, Value)> {
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        let mut stream = TcpStream::connect((host, self.port)).with_context(|| {
            format!(
                "connecting to the Ember server at {}:{}",
                self.host, self.port
            )
        })?;
        // A send can wake a sleeping session, which starts its agent process.
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let req = format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.prefix,
            self.host,
            self.port,
            self.token,
            body.len()
        );
        stream.write_all(req.as_bytes())?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        parse_response(&raw)
    }

    fn ok(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let (status, v) = self.request(method, path, body)?;
        if !(200..300).contains(&status) {
            let msg = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            bail!("{msg} (HTTP {status})");
        }
        Ok(v)
    }

    pub fn targets(&self) -> anyhow::Result<Value> {
        self.ok("GET", "/api/v1/a2a/targets", None)
    }

    pub fn send(&self, to: &str, text: &str, reply_to: Option<&str>) -> anyhow::Result<Value> {
        let body = json!({ "to": to, "text": text, "reply_to": reply_to });
        self.ok("POST", "/api/v1/a2a/messages", Some(&body))
    }

    pub fn show(&self, id: &str) -> anyhow::Result<Value> {
        self.ok("GET", &format!("/api/v1/a2a/messages/{id}"), None)
    }

    pub fn team(&self) -> anyhow::Result<Value> {
        self.ok("GET", "/api/v1/a2a/team", None)
    }

    pub fn spawn(&self, body: &Value) -> anyhow::Result<Value> {
        self.ok("POST", "/api/v1/a2a/team/members", Some(body))
    }

    pub fn end(&self, member: &str) -> anyhow::Result<Value> {
        self.ok("DELETE", &format!("/api/v1/a2a/team/members/{}", path_seg(member)), None)
    }

    pub fn tasks(&self) -> anyhow::Result<Value> {
        self.ok("GET", "/api/v1/a2a/team/tasks", None)
    }

    pub fn add_task(&self, body: &Value) -> anyhow::Result<Value> {
        self.ok("POST", "/api/v1/a2a/team/tasks", Some(body))
    }

    pub fn update_task(&self, number: &str, body: &Value) -> anyhow::Result<Value> {
        self.ok("PATCH", &format!("/api/v1/a2a/team/tasks/{}", path_seg(number)), Some(body))
    }

    pub fn send_mail(&self, to: Option<&str>, text: &str) -> anyhow::Result<Value> {
        let body = json!({ "to": to, "text": text });
        self.ok("POST", "/api/v1/a2a/team/mail", Some(&body))
    }

    pub fn read_mail(&self, after: i64, limit: Option<usize>) -> anyhow::Result<Value> {
        let mut path = format!("/api/v1/a2a/team/mail?after={after}");
        if let Some(n) = limit {
            path.push_str(&format!("&limit={n}"));
        }
        self.ok("GET", &path, None)
    }

    pub fn schedules(&self) -> anyhow::Result<Value> {
        self.ok("GET", "/api/v1/a2a/schedules", None)
    }

    pub fn add_schedule(&self, body: &Value) -> anyhow::Result<Value> {
        self.ok("POST", "/api/v1/a2a/schedules", Some(body))
    }

    pub fn remove_schedule(&self, id: &str) -> anyhow::Result<Value> {
        self.ok("DELETE", &format!("/api/v1/a2a/schedules/{}", path_seg(id)), None)
    }
}

/// Percent-encode one path segment.
fn path_seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parsed arguments: `--flag value` / `--flag=value` for `valued`, bare `--flag` for
/// `switches`, everything else positional; `--` makes the rest positional.
#[derive(Debug, Default, PartialEq)]
struct Args {
    flags: std::collections::HashMap<String, String>,
    switches: Vec<String>,
    positional: Vec<String>,
}

fn parse_args(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Args, CliError> {
    let mut out = Args::default();
    let mut it = args.iter();
    let mut literal = false;
    while let Some(a) = it.next() {
        if literal || !a.starts_with("--") || a == "-" {
            out.positional.push(a.clone());
            continue;
        }
        if a == "--" {
            literal = true;
            continue;
        }
        let (name, inline) = match a[2..].split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (a[2..].to_string(), None),
        };
        if valued.contains(&name.as_str()) {
            let v = match inline {
                Some(v) => v,
                None => it
                    .next()
                    .cloned()
                    .ok_or_else(|| CliError::Usage(format!("--{name} needs a value")))?,
            };
            out.flags.insert(name, v);
        } else if switches.contains(&name.as_str()) && inline.is_none() {
            out.switches.push(name);
        } else {
            return Err(CliError::Usage(format!("unknown option --{name}")));
        }
    }
    Ok(out)
}

/// The positional words as text, or stdin when there are none (or just `-`).
fn text_or_stdin(words: &[String]) -> Result<String, CliError> {
    if words.is_empty() || (words.len() == 1 && words[0] == "-") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(anyhow::Error::from)?;
        Ok(s)
    } else {
        Ok(words.join(" "))
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// `30s`, `15m`, `2h`, `1d` or plain seconds.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: u64 = num.parse().ok()?;
    let mult = match unit {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" => 3600,
        "d" | "day" | "days" => 86_400,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// The request body for `schedule add`, from its arguments (after `add`). The prompt is `None`
/// when it should be read from stdin.
pub fn schedule_add_body(args: &[String]) -> Result<(Value, Option<String>), String> {
    let a = parse_args(args, &["cron", "tz", "every", "at", "title"], &["new", "catch-up"])
        .map_err(CliError::into_message)?;
    let flag = |k: &str| a.flags.get(k).cloned();
    let switch = |k: &str| a.switches.iter().any(|s| s == k);
    let (new, catch_up) = (switch("new"), switch("catch-up"));
    let kind = match (flag("cron"), flag("every"), flag("at")) {
        (Some(expr), None, None) => json!({ "type": "cron", "expr": expr, "tz": flag("tz").unwrap_or_else(|| "UTC".into()) }),
        (None, Some(e), None) => {
            let seconds = parse_duration_secs(&e).ok_or_else(|| format!("bad --every {e:?} (use 30m, 2h, 1d or seconds)"))?;
            json!({ "type": "interval", "seconds": seconds })
        }
        (None, None, Some(t)) => json!({ "type": "once", "at": t }),
        _ => return Err("give exactly one of --cron, --every or --at".into()),
    };
    let title = flag("title");
    if title.is_some() && !new {
        return Err("--title needs --new".into());
    }
    let mut body = json!({ "kind": kind, "catch_up": catch_up, "prompt": "" });
    if new {
        body["target"] = json!({ "type": "new", "title": title });
    }
    let words = &a.positional;
    let prompt = if words.is_empty() || *words == ["-"] { None } else { Some(words.join(" ")) };
    Ok((body, prompt))
}

fn parse_response(raw: &[u8]) -> anyhow::Result<(u16, Value)> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("malformed HTTP response from the Ember server")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut body = raw[split + 4..].to_vec();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .context("malformed HTTP status line")?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        body = dechunk(&body)?;
    }
    let value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or_else(|_| {
            json!({
                "error": String::from_utf8_lossy(&body).into_owned()
            })
        })
    };
    Ok((status, value))
}

fn dechunk(mut data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("bad chunk")?;
        let size_str = String::from_utf8_lossy(&data[..eol]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
            .context("bad chunk size")?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(data.len() >= size, "truncated chunk");
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or_default();
    }
}

/// Entry point of `ember-a2a`; returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match run_inner(args) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(CliError::Usage(msg)) => {
            eprintln!("{msg}\n\n{USAGE}");
            2
        }
        Err(CliError::Failed(e)) => {
            eprintln!("ember-a2a: {e:#}");
            1
        }
    }
}

enum CliError {
    Usage(String),
    Failed(anyhow::Error),
}

impl CliError {
    fn into_message(self) -> String {
        match self {
            CliError::Usage(m) => m,
            CliError::Failed(e) => format!("{e:#}"),
        }
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        CliError::Failed(e)
    }
}

fn run_inner(args: &[String]) -> Result<String, CliError> {
    let Some(cmd) = args.first() else {
        return Err(CliError::Usage("missing command".into()));
    };
    match cmd.as_str() {
        "help" | "--help" | "-h" => Ok(USAGE.to_string()),
        "list" => {
            let a = parse_args(&args[1..], &[], &["json"])?;
            let v = Client::from_env()?.targets()?;
            if a.switches.iter().any(|s| s == "json") {
                return Ok(pretty(&v));
            }
            Ok(format_targets(&v))
        }
        "send" => {
            let a = parse_args(&args[1..], &["reply-to"], &[])?;
            let (to, words) = a
                .positional
                .split_first()
                .ok_or_else(|| CliError::Usage("missing <session-id>".into()))?;
            let text = text_or_stdin(words)?;
            let reply_to = a.flags.get("reply-to").map(String::as_str);
            let v = Client::from_env()?.send(to, &text, reply_to)?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            Ok(match v.get("status").and_then(Value::as_str) {
                Some("delivered") => format!("Sent message {id} to {to}: delivered."),
                _ => format!(
                    "Sent message {id} to {to}: queued; it is delivered when that session's \
                     current turn ends."
                ),
            })
        }
        "team" => run_team(&args[1..]),
        "task" => run_task(&args[1..]),
        "mail" => run_mail(&args[1..]),
        "show" => {
            let id = args
                .get(1)
                .ok_or_else(|| CliError::Usage("missing <message-id>".into()))?;
            let v = Client::from_env()?.show(id)?;
            Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
        }
        "schedule" => run_schedule(&args[1..]),
        other => Err(CliError::Usage(format!("unknown command {other}"))),
    }
}

fn run_team(args: &[String]) -> Result<String, CliError> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = args.get(1..).unwrap_or_default();
    match sub {
        "list" | "show" => {
            let a = parse_args(rest, &[], &["json"])?;
            let v = Client::from_env()?.team()?;
            if a.switches.iter().any(|s| s == "json") {
                return Ok(pretty(&v));
            }
            Ok(format_team(&v))
        }
        "spawn" => {
            let a = parse_args(rest, &["agent", "account", "computer", "title"], &[])?;
            let (name, words) = a
                .positional
                .split_first()
                .ok_or_else(|| CliError::Usage("missing teammate <name>".into()))?;
            let prompt = text_or_stdin(words)?;
            let body = json!({
                "name": name,
                "prompt": prompt,
                "agent": a.flags.get("agent"),
                "account": a.flags.get("account"),
                "computer": a.flags.get("computer"),
                "title": a.flags.get("title"),
            });
            let v = Client::from_env()?.spawn(&body)?;
            let s = |k: &str| v["member"][k].as_str().unwrap_or("?").to_string();
            Ok(format!(
                "Spawned teammate {} (session {}, {}) in team {}; first prompt {}.",
                s("name"),
                s("session_id"),
                s("agent"),
                v["team_id"].as_str().unwrap_or("?"),
                v["message"]["status"].as_str().unwrap_or("sent")
            ))
        }
        "end" => {
            let who = rest
                .first()
                .ok_or_else(|| CliError::Usage("missing teammate <name>".into()))?;
            let v = Client::from_env()?.end(who)?;
            Ok(format!("Ended teammate {}.", v["name"].as_str().unwrap_or(who)))
        }
        other => Err(CliError::Usage(format!("unknown team command {other}"))),
    }
}

fn run_task(args: &[String]) -> Result<String, CliError> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = args.get(1..).unwrap_or_default();
    match sub {
        "list" => {
            let a = parse_args(rest, &[], &["json"])?;
            let v = Client::from_env()?.tasks()?;
            if a.switches.iter().any(|s| s == "json") {
                return Ok(pretty(&v));
            }
            Ok(format_tasks(&v))
        }
        "add" => {
            let a = parse_args(rest, &["detail", "assign"], &[])?;
            if a.positional.is_empty() {
                return Err(CliError::Usage("missing task <title>".into()));
            }
            let body = json!({
                "title": a.positional.join(" "),
                "detail": a.flags.get("detail"),
                "assignee": a.flags.get("assign"),
            });
            let v = Client::from_env()?.add_task(&body)?;
            Ok(format!("Added {}", format_task(&v)))
        }
        "update" => {
            let a = parse_args(rest, &["status", "assign", "title", "detail"], &[])?;
            let n = a
                .positional
                .first()
                .ok_or_else(|| CliError::Usage("missing task <n>".into()))?;
            if a.flags.is_empty() {
                return Err(CliError::Usage(
                    "nothing to update: give --status, --assign, --title or --detail".into(),
                ));
            }
            let body = json!({
                "status": a.flags.get("status"),
                "assignee": a.flags.get("assign"),
                "title": a.flags.get("title"),
                "detail": a.flags.get("detail"),
            });
            let v = Client::from_env()?.update_task(n.trim_start_matches('#'), &body)?;
            Ok(format!("Updated {}", format_task(&v)))
        }
        other => Err(CliError::Usage(format!("unknown task command {other}"))),
    }
}

fn run_mail(args: &[String]) -> Result<String, CliError> {
    let sub = args.first().map(String::as_str).unwrap_or("read");
    let rest = args.get(1..).unwrap_or_default();
    match sub {
        "send" => {
            let a = parse_args(rest, &[], &["all"])?;
            let all = a.switches.iter().any(|s| s == "all");
            let (to, words) = if all {
                (None, a.positional.as_slice())
            } else {
                let (to, words) = a.positional.split_first().ok_or_else(|| {
                    CliError::Usage("missing recipient: a member name, or --all".into())
                })?;
                (Some(to.as_str()), words)
            };
            let text = text_or_stdin(words)?;
            let v = Client::from_env()?.send_mail(to, &text)?;
            let n = v["deliveries"].as_array().map(Vec::len).unwrap_or(0);
            Ok(format!(
                "Sent team mail #{} to {} ({n} recipient{}).",
                v["mail"]["id"],
                to.unwrap_or("the whole team"),
                if n == 1 { "" } else { "s" }
            ))
        }
        "read" => {
            let a = parse_args(rest, &["after", "limit"], &["json"])?;
            let after = match a.flags.get("after") {
                Some(s) => s
                    .trim_start_matches('#')
                    .parse()
                    .map_err(|_| CliError::Usage(format!("bad --after {s}")))?,
                None => 0,
            };
            let limit = match a.flags.get("limit") {
                Some(s) => Some(s.parse().map_err(|_| CliError::Usage(format!("bad --limit {s}")))?),
                None => None,
            };
            let v = Client::from_env()?.read_mail(after, limit)?;
            if a.switches.iter().any(|s| s == "json") {
                return Ok(pretty(&v));
            }
            Ok(format_mail(&v))
        }
        other => Err(CliError::Usage(format!("unknown mail command {other}"))),
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn format_team(v: &Value) -> String {
    if v.is_null() {
        return "Not in a team. `ember-a2a team spawn <name> \"<prompt>\"` starts one with you as \
                the leader."
            .into();
    }
    let mut out = format!("Team {} (project {})\nMembers:", str_of(v, "id"), str_of(v, "project"));
    for m in v["members"].as_array().cloned().unwrap_or_default() {
        let state = if m["ended_at"].is_null() {
            m["status"].as_str().unwrap_or("?").to_string()
        } else {
            "ended".into()
        };
        out.push_str(&format!(
            "\n  {}  {}  {}  {}  {}  {}",
            str_of(&m, "name"),
            str_of(&m, "role"),
            str_of(&m, "session_id"),
            str_of(&m, "agent"),
            state,
            str_of(&m, "title")
        ));
    }
    out.push_str("\nTasks:");
    let tasks = v["tasks"].as_array().cloned().unwrap_or_default();
    if tasks.is_empty() {
        out.push_str("\n  (none)");
    }
    for t in &tasks {
        out.push_str(&format!("\n  {}", format_task(t)));
    }
    out
}

fn format_task(t: &Value) -> String {
    let who = t["assignee_name"]
        .as_str()
        .or_else(|| t["assignee"].as_str())
        .unwrap_or("unassigned");
    format!(
        "#{}  [{}]  {}  ({who})",
        t["number"],
        str_of(t, "status"),
        str_of(t, "title")
    )
}

fn format_tasks(v: &Value) -> String {
    let rows = v.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "No tasks.".into();
    }
    rows.iter().map(format_task).collect::<Vec<_>>().join("\n")
}

fn format_mail(v: &Value) -> String {
    let rows = v.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "No team mail.".into();
    }
    rows.iter()
        .map(|m| {
            let from = m["from_name"].as_str().map(String::from).unwrap_or_else(|| str_of(m, "from_session"));
            let to = if m["to_session"].is_null() {
                "everyone".to_string()
            } else {
                m["to_name"].as_str().map(String::from).unwrap_or_else(|| str_of(m, "to_session"))
            };
            format!("#{} {from} -> {to}:\n{}", m["id"], str_of(m, "text"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn run_schedule(args: &[String]) -> Result<String, CliError> {
    let Some(sub) = args.first() else {
        return Err(CliError::Usage("missing schedule command (add, list, rm)".into()));
    };
    let rest = &args[1..];
    match sub.as_str() {
        "list" | "ls" => {
            let a = parse_args(rest, &[], &["json"])?;
            let v = Client::from_env()?.schedules()?;
            if a.switches.iter().any(|s| s == "json") {
                return Ok(pretty(&v));
            }
            Ok(format_schedules(&v))
        }
        "add" => {
            let (mut body, prompt) = schedule_add_body(rest).map_err(CliError::Usage)?;
            let prompt = match prompt {
                Some(p) => p,
                None => text_or_stdin(&[])?,
            };
            body["prompt"] = Value::String(prompt);
            let v = Client::from_env()?.add_schedule(&body)?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            let next = v.get("next_run_at").and_then(Value::as_i64).map(|t| format!(" Next run at {}.", crate::schedules::timing::rfc3339(t))).unwrap_or_default();
            Ok(format!("Created schedule {id}.{next}"))
        }
        "rm" | "remove" | "delete" => {
            let a = parse_args(rest, &[], &[])?;
            let id = a
                .positional
                .first()
                .ok_or_else(|| CliError::Usage("missing <schedule-id>".into()))?;
            Client::from_env()?.remove_schedule(id)?;
            Ok(format!("Deleted schedule {id}."))
        }
        other => Err(CliError::Usage(format!("unknown schedule command {other}"))),
    }
}

fn format_schedules(v: &Value) -> String {
    let rows = v.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "No schedules in this project.".into();
    }
    rows.iter()
        .map(|r| {
            let kind = &r["kind"];
            let when = match kind["type"].as_str() {
                Some("cron") => format!("cron \"{}\" {}", kind["expr"].as_str().unwrap_or(""), kind["tz"].as_str().unwrap_or("")),
                Some("interval") => format!("every {}s", kind["seconds"]),
                Some("once") => format!("once at {}", kind["at"]),
                _ => "?".into(),
            };
            let target = match r["target"]["type"].as_str() {
                Some("continue") => format!("continue {}", r["target"]["session_id"].as_str().unwrap_or("")),
                _ => "new session".into(),
            };
            let state = if r["paused"].as_bool() == Some(true) { "paused" } else { "active" };
            let prompt: String = r["prompt"].as_str().unwrap_or("").chars().take(60).collect();
            format!(
                "{}  {state}  {when}  -> {target}  next={}  {prompt}",
                r["id"].as_str().unwrap_or("?"),
                r["next_run_at"].as_i64().map(crate::schedules::timing::rfc3339).unwrap_or_else(|| "-".into())
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_targets(v: &Value) -> String {
    let rows = v.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "No other sessions to message.".into();
    }
    let s = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    rows.iter()
        .map(|r| {
            format!(
                "{}  {}  {}  project={}  {}",
                s(r, "id"),
                s(r, "agent"),
                s(r, "status"),
                s(r, "project"),
                s(r, "title")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_urls() {
        let c = Client::new("http://127.0.0.1:8740", "t").unwrap();
        assert_eq!(
            (c.host.as_str(), c.port, c.prefix.as_str()),
            ("127.0.0.1", 8740, "")
        );
        let c = Client::new("http://localhost/ember/", "t").unwrap();
        assert_eq!(
            (c.host.as_str(), c.port, c.prefix.as_str()),
            ("localhost", 80, "/ember")
        );
        let c = Client::new("http://[::1]:9000", "t").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("[::1]", 9000));
        assert!(Client::new("https://x", "t").is_err());
    }

    #[test]
    fn parses_flags_and_positionals() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = parse_args(&v(&["alice", "--agent", "codex", "fix", "--title=T x", "it"]), &["agent", "title"], &[]).ok().unwrap();
        assert_eq!(a.positional, v(&["alice", "fix", "it"]));
        assert_eq!(a.flags.get("agent").map(String::as_str), Some("codex"));
        assert_eq!(a.flags.get("title").map(String::as_str), Some("T x"));
        let a = parse_args(&v(&["--all", "--", "--not-a-flag"]), &[], &["all"]).ok().unwrap();
        assert_eq!((a.switches, a.positional), (v(&["all"]), v(&["--not-a-flag"])));
        assert!(parse_args(&v(&["--bogus"]), &[], &[]).is_err());
        assert!(parse_args(&v(&["--agent"]), &["agent"], &[]).is_err());
        assert_eq!(path_seg("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn formats_team_and_tasks() {
        let team = json!({
            "id": "team_1", "project": "acme",
            "members": [
                {"name": "lead", "role": "leader", "session_id": "s1", "agent": "codex", "status": "idle", "title": "Lead", "ended_at": null},
                {"name": "alice", "role": "teammate", "session_id": "s2", "agent": "codex", "status": "running", "title": "A", "ended_at": 5}
            ],
            "tasks": [{"number": 1, "status": "open", "title": "write tests", "assignee": "s2", "assignee_name": "alice"}]
        });
        let out = format_team(&team);
        assert!(out.contains("alice  teammate  s2  codex  ended"), "{out}");
        assert!(out.contains("#1  [open]  write tests  (alice)"), "{out}");
        assert!(format_team(&Value::Null).contains("Not in a team"));
        assert_eq!(format_tasks(&json!([])), "No tasks.");
    }

    #[test]
    fn schedule_add_arguments() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (b, p) = schedule_add_body(&args(&["--cron", "0 9 * * 1-5", "--tz", "Asia/Seoul", "check", "CI"])).unwrap();
        assert_eq!(b["kind"], json!({ "type": "cron", "expr": "0 9 * * 1-5", "tz": "Asia/Seoul" }));
        assert!(b.get("target").is_none(), "continues the caller by default");
        assert_eq!(p.as_deref(), Some("check CI"));
        let (b, p) = schedule_add_body(&args(&["--every", "2h", "--new", "--title", "Nightly", "--catch-up", "-"])).unwrap();
        assert_eq!(b["kind"], json!({ "type": "interval", "seconds": 7200 }));
        assert_eq!(b["target"], json!({ "type": "new", "title": "Nightly" }));
        assert_eq!(b["catch_up"], true);
        assert_eq!(p, None, "stdin");
        let (b, _) = schedule_add_body(&args(&["--at", "2026-10-04T09:00:00+09:00", "x"])).unwrap();
        assert_eq!(b["kind"]["at"], "2026-10-04T09:00:00+09:00");
        assert!(schedule_add_body(&args(&["x"])).is_err());
        assert!(schedule_add_body(&args(&["--every", "1h", "--cron", "* * * * *", "x"])).is_err());
        assert!(schedule_add_body(&args(&["--every", "soon", "x"])).is_err());
        assert_eq!(parse_duration_secs("90"), Some(90));
        assert_eq!(parse_duration_secs("15m"), Some(900));
        assert_eq!(parse_duration_secs("1d"), Some(86_400));
        assert_eq!(parse_duration_secs("1w"), None);
    }

    #[test]
    fn parses_plain_and_chunked_responses() {
        let raw = b"HTTP/1.1 429 Too Many Requests\r\ncontent-length: 15\r\n\r\n{\"error\":\"no\"}";
        let (s, v) = parse_response(raw).unwrap();
        assert_eq!(s, 429);
        assert_eq!(v["error"], "no");
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n[1,2\r\n2\r\n,3\r\n1\r\n]\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap(), (200, json!([1, 2, 3])));
        let raw = b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap(), (202, Value::Null));
    }
}
