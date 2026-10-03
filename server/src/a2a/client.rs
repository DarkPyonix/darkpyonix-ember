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
ember-a2a — message other agent sessions in Ember, and schedule prompts

Usage:
  ember-a2a list [--json]
      Sessions you can message: id, agent, status, project, title.
  ember-a2a send <session-id> [--reply-to <message-id>] <text...>
      Send a message. Use `-` (or no text) to read the text from stdin.
  ember-a2a show <message-id>
      A message you sent or received, with its delivery time.

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

    pub fn schedules(&self) -> anyhow::Result<Value> {
        self.ok("GET", "/api/v1/a2a/schedules", None)
    }

    pub fn add_schedule(&self, body: &Value) -> anyhow::Result<Value> {
        self.ok("POST", "/api/v1/a2a/schedules", Some(body))
    }

    pub fn remove_schedule(&self, id: &str) -> anyhow::Result<Value> {
        self.ok("DELETE", &format!("/api/v1/a2a/schedules/{id}"), None)
    }
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
    let mut cron = None;
    let mut tz = None;
    let mut every = None;
    let mut at = None;
    let mut new = false;
    let mut title = None;
    let mut catch_up = false;
    let mut words: Vec<String> = Vec::new();
    let mut literal = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            _ if literal => words.push(a.clone()),
            "--" => literal = true,
            "--cron" => cron = Some(value("--cron")?),
            "--tz" => tz = Some(value("--tz")?),
            "--every" => every = Some(value("--every")?),
            "--at" => at = Some(value("--at")?),
            "--new" => new = true,
            "--title" => title = Some(value("--title")?),
            "--catch-up" => catch_up = true,
            _ => words.push(a.clone()),
        }
    }
    let kind = match (cron, every, at) {
        (Some(expr), None, None) => json!({ "type": "cron", "expr": expr, "tz": tz.unwrap_or_else(|| "UTC".into()) }),
        (None, Some(e), None) => {
            let seconds = parse_duration_secs(&e).ok_or_else(|| format!("bad --every {e:?} (use 30m, 2h, 1d or seconds)"))?;
            json!({ "type": "interval", "seconds": seconds })
        }
        (None, None, Some(t)) => json!({ "type": "once", "at": t }),
        _ => return Err("give exactly one of --cron, --every or --at".into()),
    };
    if title.is_some() && !new {
        return Err("--title needs --new".into());
    }
    let mut body = json!({ "kind": kind, "catch_up": catch_up, "prompt": "" });
    if new {
        body["target"] = json!({ "type": "new", "title": title });
    }
    let prompt = if words.is_empty() || words == ["-"] { None } else { Some(words.join(" ")) };
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
            let json_out = args[1..].iter().any(|a| a == "--json");
            let v = Client::from_env()?.targets()?;
            if json_out {
                return Ok(serde_json::to_string_pretty(&v).unwrap_or_default());
            }
            Ok(format_targets(&v))
        }
        "send" => {
            let mut to = None;
            let mut reply_to = None;
            let mut words: Vec<String> = Vec::new();
            let mut it = args[1..].iter();
            let mut literal = false;
            while let Some(a) = it.next() {
                if !literal && a == "--" {
                    literal = true;
                } else if !literal && a == "--reply-to" {
                    let id = it
                        .next()
                        .ok_or_else(|| CliError::Usage("--reply-to needs a message id".into()))?;
                    reply_to = Some(id.clone());
                } else if !literal && a.starts_with("--reply-to=") {
                    reply_to = Some(a["--reply-to=".len()..].to_string());
                } else if to.is_none() {
                    to = Some(a.clone());
                } else {
                    words.push(a.clone());
                }
            }
            let to = to.ok_or_else(|| CliError::Usage("missing <session-id>".into()))?;
            let text = if words.is_empty() || words == ["-"] {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(anyhow::Error::from)?;
                s
            } else {
                words.join(" ")
            };
            let v = Client::from_env()?.send(&to, &text, reply_to.as_deref())?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            Ok(match v.get("status").and_then(Value::as_str) {
                Some("delivered") => format!("Sent message {id} to {to}: delivered."),
                _ => format!(
                    "Sent message {id} to {to}: queued; it is delivered when that session's \
                     current turn ends."
                ),
            })
        }
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

fn run_schedule(args: &[String]) -> Result<String, CliError> {
    let Some(sub) = args.first() else {
        return Err(CliError::Usage("missing schedule command (add, list, rm)".into()));
    };
    match sub.as_str() {
        "list" | "ls" => {
            let v = Client::from_env()?.schedules()?;
            if args[1..].iter().any(|a| a == "--json") {
                return Ok(serde_json::to_string_pretty(&v).unwrap_or_default());
            }
            Ok(format_schedules(&v))
        }
        "add" => {
            let (mut body, prompt) = schedule_add_body(&args[1..]).map_err(CliError::Usage)?;
            let prompt = match prompt {
                Some(p) => p,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s).map_err(anyhow::Error::from)?;
                    s
                }
            };
            body["prompt"] = Value::String(prompt);
            let v = Client::from_env()?.add_schedule(&body)?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            let next = v.get("next_run_at").and_then(Value::as_i64).map(|t| format!(" Next run at {}.", crate::schedules::timing::rfc3339(t))).unwrap_or_default();
            Ok(format!("Created schedule {id}.{next}"))
        }
        "rm" | "remove" | "delete" => {
            let id = args.get(1).ok_or_else(|| CliError::Usage("missing <schedule-id>".into()))?;
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
