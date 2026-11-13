//! `ember-server owner <verb>`: owner routes from a terminal, for a server with no app beside it
//! (SPEC `FR-S12`; INTENT D37 "Owner presence", #784).
//!
//! The command claims an owner key over `<data dir>/owner.sock` like the app does, holds it for
//! this one call, and sends it with the server token to the local listener. It refuses to run
//! when standard input is not a terminal, and the server refuses the claim from inside an Ember
//! agent or terminal tree, so it does not work in Ember's own terminal panel.

use serde_json::{json, Value};

pub const USAGE: &str = "\
usage: ember-server owner <verb>
  approve <session> <approval> <allow|always|deny>   answer a tool approval
  approve-all <session> <on|off>                     switch a session's approve-everything mode
  devices list                                       the devices allowed over the transport
  devices add <peer id> <name>                       allow a device
  devices rm <peer id>                               revoke a device
  devices scope <peer id> <full|read_only|chat_only> set a device's scope
  enrol-code <name> [ttl seconds]                    mint a code for a new computer
  rotate-token                                       replace the server token on the running server
  accounts add <agent> <label>                       add an account
  call <METHOD> <path under /api/> [json body]       any other owner route

Needs a terminal. The server shows a code on its console; type it here, or approve the claim
in a running Ember app.";

/// One HTTP call on the local listener.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}

/// One path segment given on the command line: non-empty, and nothing that would change the
/// path (`/`, `?`, `#`, `%`, whitespace, control characters).
fn segment(what: &str, s: &str) -> Result<String, String> {
    let bad = |c: char| matches!(c, '/' | '?' | '#' | '%') || c.is_whitespace() || c.is_control();
    if s.is_empty() || s == "." || s == ".." || s.chars().any(bad) {
        return Err(format!("{what} {s:?} is not valid"));
    }
    Ok(s.to_string())
}

fn method(s: &str) -> Result<&'static str, String> {
    match s.to_ascii_uppercase().as_str() {
        "GET" => Ok("GET"),
        "POST" => Ok("POST"),
        "PUT" => Ok("PUT"),
        "PATCH" => Ok("PATCH"),
        "DELETE" => Ok("DELETE"),
        other => Err(format!("unknown method {other:?}")),
    }
}

/// The call for `args` (the words after `owner`), or a message for the user.
pub fn parse(args: &[String]) -> Result<Call, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let call = |method, path: String, body| Ok(Call { method, path, body });
    match a.as_slice() {
        ["approve", session, approval, decision] => {
            let decision = match *decision {
                "allow" => "allow_once",
                "always" => "allow_always",
                "deny" => "deny",
                other => return Err(format!("decision {other:?} is not allow, always or deny")),
            };
            call(
                "POST",
                format!("/api/sessions/{}/approvals/{}", segment("session", session)?, segment("approval", approval)?),
                Some(json!({ "decision": decision })),
            )
        }
        ["approve-all", session, state] => {
            let enabled = match *state {
                "on" => true,
                "off" => false,
                other => return Err(format!("{other:?} is not on or off")),
            };
            call(
                "PUT",
                format!("/api/sessions/{}/approve-all", segment("session", session)?),
                Some(json!({ "enabled": enabled })),
            )
        }
        ["devices", "list"] => call("GET", "/api/devices".into(), None),
        ["devices", "add", peer, name] => {
            call("POST", "/api/devices".into(), Some(json!({ "peer_id": segment("peer id", peer)?, "name": name })))
        }
        ["devices", "rm", peer] => call("DELETE", format!("/api/devices/{}", segment("peer id", peer)?), None),
        ["devices", "scope", peer, scope] => {
            if !matches!(*scope, "full" | "read_only" | "chat_only") {
                return Err(format!("scope {scope:?} is not full, read_only or chat_only"));
            }
            call("PATCH", format!("/api/devices/{}", segment("peer id", peer)?), Some(json!({ "scope": scope })))
        }
        ["enrol-code", name] => call("POST", "/api/computers/enrol-codes".into(), Some(json!({ "name": name }))),
        ["enrol-code", name, ttl] => {
            let ttl: u64 = ttl.parse().map_err(|_| format!("ttl {ttl:?} is not a number of seconds"))?;
            call("POST", "/api/computers/enrol-codes".into(), Some(json!({ "name": name, "ttl_secs": ttl })))
        }
        ["rotate-token"] => call("POST", crate::auth::ROTATE_ROUTE.into(), None),
        ["accounts", "add", agent, label] => {
            call("POST", "/api/accounts".into(), Some(json!({ "agent": agent, "label": label })))
        }
        ["call", m, path] | ["call", m, path, _] => {
            if !path.starts_with("/api/") || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!("path {path:?} must start with /api/"));
            }
            let body = match a.get(3) {
                Some(text) => Some(serde_json::from_str::<Value>(text).map_err(|e| format!("body is not JSON: {e}"))?),
                None => None,
            };
            call(method(m)?, path.to_string(), body)
        }
        _ => Err(USAGE.to_string()),
    }
}

/// Runs `ember-server owner <args>` against the server whose data folder is `data_dir`.
#[cfg(not(unix))]
pub async fn run(_args: &[String], _data_dir: &std::path::Path) -> anyhow::Result<()> {
    anyhow::bail!("ember-server owner needs the owner socket, which exists on unix only")
}

/// Runs `ember-server owner <args>` against the server whose data folder is `data_dir`.
#[cfg(unix)]
pub async fn run(args: &[String], data_dir: &std::path::Path) -> anyhow::Result<()> {
    use std::io::IsTerminal;

    let call = parse(args).map_err(|m| anyhow::anyhow!(m))?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("ember-server owner needs a terminal: standard input is not one");
    }
    let token = std::fs::read_to_string(crate::auth::token_path(data_dir))
        .map_err(|e| anyhow::anyhow!("cannot read the server token in {}: {e}", data_dir.display()))?;
    let token = token.lines().next().unwrap_or_default().trim().to_string();
    let mut addr = crate::auth::parse_listen(std::env::var("EMBER_LISTEN").ok().as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }

    // The connection is the key's life: it stays open until the call is answered.
    let sock = super::door::socket_path(data_dir);
    let (_conn, key) = unix::claim(&sock)?;

    let mut owner = reqwest::header::HeaderValue::from_str(&key)?;
    owner.set_sensitive(true);
    drop(key);
    let method = reqwest::Method::from_bytes(call.method.as_bytes())?;
    let mut req = reqwest::Client::new()
        .request(method, format!("http://{addr}{}", call.path))
        .bearer_auth(&token)
        .header(super::OWNER_HEADER, owner);
    if let Some(body) = &call.body {
        req = req.json(body);
    }
    let res = req.send().await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let shown = match serde_json::from_str::<Value>(&text) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(text),
        Err(_) => text,
    };
    if !status.is_success() {
        anyhow::bail!("{} {} answered {status}: {shown}", call.method, call.path);
    }
    if !shown.is_empty() {
        println!("{shown}");
    }
    Ok(())
}

#[cfg(unix)]
mod unix {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;

    use serde_json::{json, Value};

    fn send(conn: &mut UnixStream, v: Value) -> std::io::Result<()> {
        let mut line = v.to_string();
        line.push('\n');
        conn.write_all(line.as_bytes())
    }

    /// Claims an owner key over `sock`, asking the terminal for the code when the server wants
    /// one. Returns the open connection (the key's life) and the key.
    pub fn claim(sock: &Path) -> anyhow::Result<(UnixStream, String)> {
        let mut conn = UnixStream::connect(sock).map_err(|e| {
            anyhow::anyhow!("cannot reach the server's owner socket {}: {e} (is the server running?)", sock.display())
        })?;
        send(&mut conn, json!({ "op": "claim", "name": "ember-server owner" }))?;
        let mut lines = BufReader::new(conn.try_clone()?).lines();
        loop {
            let Some(line) = lines.next() else {
                anyhow::bail!("the server closed the owner socket before it gave a key");
            };
            let v: Value = serde_json::from_str(&line?)?;
            match v["state"].as_str() {
                Some("granted") => {
                    let key = v["key"].as_str().unwrap_or_default().to_string();
                    if key.is_empty() {
                        anyhow::bail!("the server granted the claim without a key");
                    }
                    return Ok((conn, key));
                }
                Some("refused") => {
                    let reason = v["reason"].as_str().unwrap_or("unknown");
                    let why = match reason {
                        "agent_tree" => "this command runs inside an Ember agent or terminal; use a terminal outside Ember",
                        "other_user" => "this command runs as another user than the server",
                        "wrong_code" => "the code was wrong too many times",
                        "denied" => "the claim was denied in the Ember app",
                        "timeout" => "the claim was not confirmed in time",
                        "busy" => "too many claims are waiting; try again",
                        other => other,
                    };
                    anyhow::bail!("owner claim refused: {why}");
                }
                Some("confirm") | Some("wrong_code") => {
                    if let Some(left) = v["left"].as_u64() {
                        eprintln!("wrong code ({left} tries left)");
                    }
                    eprint!("Code shown on the server console (or approve in the Ember app, then press Enter): ");
                    std::io::stderr().flush().ok();
                    let mut typed = String::new();
                    std::io::stdin().lock().read_line(&mut typed)?;
                    let typed = typed.trim();
                    // An empty line waits for the app's decision instead.
                    if !typed.is_empty() {
                        send(&mut conn, json!({ "op": "confirm", "code": typed }))?;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn fr_s12_cli_verbs_map_to_owner_routes() {
        use axum::http::Method;
        for line in [
            "approve s1 a1 allow",
            "approve s1 a1 deny",
            "approve-all s1 on",
            "devices list",
            "devices add abc phone",
            "devices rm abc",
            "devices scope abc chat_only",
            "enrol-code laptop",
            "enrol-code laptop 300",
            "rotate-token",
            "accounts add codex work",
        ] {
            let c = parse(&args(line)).unwrap_or_else(|e| panic!("{line}: {e}"));
            let m = Method::from_bytes(c.method.as_bytes()).unwrap();
            assert!(crate::owner::owner_route(&m, &c.path), "{line} -> {} {}", c.method, c.path);
        }
    }

    #[test]
    fn fr_s12_cli_bodies() {
        let c = parse(&args("approve s1 a1 always")).unwrap();
        assert_eq!(c.path, "/api/sessions/s1/approvals/a1");
        assert_eq!(c.body, Some(json!({ "decision": "allow_always" })));
        let c = parse(&args("devices scope abc read_only")).unwrap();
        assert_eq!((c.method, c.path.as_str()), ("PATCH", "/api/devices/abc"));
        assert_eq!(c.body, Some(json!({ "scope": "read_only" })));
        let c = parse(&["call".into(), "put".into(), "/api/a2a/settings".into(), "{\"enabled\":false}".into()]).unwrap();
        assert_eq!((c.method, c.path.as_str()), ("PUT", "/api/a2a/settings"));
        assert_eq!(c.body, Some(json!({ "enabled": false })));
    }

    #[test]
    fn fr_s12_cli_refuses_bad_words() {
        for line in [
            "",
            "approve s1 a1",
            "approve s1 a1 maybe",
            "approve s1/../x a1 allow",
            "approve s1 a1?x allow",
            "devices scope abc admin",
            "devices rm ..",
            "enrol-code laptop soon",
            "call TRACE /api/devices",
            "call GET /etc/passwd",
            "frobnicate",
        ] {
            assert!(parse(&args(line)).is_err(), "{line}");
        }
    }
}
