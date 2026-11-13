//! Every route of the server, classified (SPEC `FR-S12`; INTENT D37 "Owner presence", #784).
//!
//! [`ROUTES`] lists each registered `(method, path template)` with its [`RouteClass`]. The test
//! `fr_s12_every_route_is_classified` scans `src/` for `.route(` calls and fails when a route is
//! missing here, so a new route cannot land unclassified. [`owner_route`] is the one list both
//! doors use: the owner layer on the local listener ([`super::require`]) and the scope check on
//! the transport ([`crate::devices::scope`]).
//!
//! An owner route grants authority, admits a party or changes a secret. Reads, messages, session
//! create, terminals, fs, the IDE, schedules and question answers are not owner routes: an
//! unsandboxed agent already has a shell, and a sandboxed one has no token, so gating them buys
//! nothing and costs every client.

use axum::http::Method;

/// What a caller needs on the local listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteClass {
    /// The server token and owner presence (the `X-Ember-Owner` key).
    Owner,
    /// The server token alone.
    Token,
    /// No server token: the route checks its own credential ([`crate::auth::token_exempt`]).
    Exempt,
    /// Not served on the local listener at all: the guest door or the enrol door.
    OtherDoor,
}

use RouteClass::{Exempt, OtherDoor, Owner, Token};

/// Every registered route. Sorted by path, then method.
pub const ROUTES: &[(&str, &str, RouteClass)] = &[
    ("POST", "/api/a2a/messages", Exempt),
    ("GET", "/api/a2a/messages/{id}", Exempt),
    ("GET", "/api/a2a/schedules", Exempt),
    ("POST", "/api/a2a/schedules", Exempt),
    ("DELETE", "/api/a2a/schedules/{id}", Exempt),
    ("GET", "/api/a2a/settings", Token),
    ("PUT", "/api/a2a/settings", Owner),
    ("GET", "/api/a2a/targets", Exempt),
    ("GET", "/api/a2a/team", Exempt),
    ("GET", "/api/a2a/team/mail", Exempt),
    ("POST", "/api/a2a/team/mail", Exempt),
    ("POST", "/api/a2a/team/members", Exempt),
    ("DELETE", "/api/a2a/team/members/{member}", Exempt),
    ("GET", "/api/a2a/team/tasks", Exempt),
    ("POST", "/api/a2a/team/tasks", Exempt),
    ("PATCH", "/api/a2a/team/tasks/{number}", Exempt),
    ("GET", "/api/a2a/terminal/{target}", Exempt),
    ("POST", "/api/a2a/terminal/{target}/interrupt", Exempt),
    ("POST", "/api/a2a/terminal/{target}/text", Exempt),
    ("GET", "/api/accounts", Token),
    ("POST", "/api/accounts", Owner),
    ("GET", "/api/accounts/route", Token),
    ("DELETE", "/api/accounts/{id}", Owner),
    ("GET", "/api/accounts/{id}", Token),
    ("POST", "/api/accounts/{id}/check", Owner),
    ("POST", "/api/accounts/{id}/clear-limit", Owner),
    ("POST", "/api/accounts/{id}/default", Owner),
    ("GET", "/api/agents", Token),
    ("POST", "/api/agents/antigravity/hook", Exempt),
    ("GET", "/api/agents/defaults", Token),
    ("PUT", "/api/agents/defaults", Token),
    ("GET", "/api/agents/settings", Token),
    ("PUT", "/api/agents/{agent}/settings", Owner),
    ("GET", "/api/browsers", Token),
    ("DELETE", "/api/browsers/{project}", Token),
    ("GET", "/api/browsers/{project}", Token),
    ("POST", "/api/browsers/{project}", Token),
    ("GET", "/api/browsers/{project}/agent-config", Token),
    ("GET", "/api/browsers/{project}/cdp", Token),
    ("GET", "/api/browsers/{project}/cdp/json/version", Token),
    ("GET", "/api/browsers/{project}/cdp/{nonce}", Exempt),
    ("GET", "/api/browsers/{project}/cdp/{nonce}/json/version", Exempt),
    ("DELETE", "/api/browsers/{project}/data", Token),
    ("GET", "/api/browsers/{project}/egress", Token),
    ("PUT", "/api/browsers/{project}/egress", Owner),
    ("POST", "/api/browsers/{project}/input", Token),
    ("GET", "/api/browsers/{project}/view", Token),
    ("GET", "/api/channels", Token),
    ("POST", "/api/channels", Token),
    ("GET", "/api/channels/{id}", Token),
    ("GET", "/api/channels/{id}/feed", Token),
    ("POST", "/api/channels/{id}/feed/{approval_id}/answer", Owner),
    ("GET", "/api/channels/{id}/guests", Token),
    ("DELETE", "/api/channels/{id}/guests/{guest_id}", Owner),
    ("DELETE", "/api/channels/{id}/guests/{guest_id}/allow", Owner),
    ("PUT", "/api/channels/{id}/guests/{guest_id}/allow", Owner),
    ("GET", "/api/channels/{id}/invites", Token),
    ("POST", "/api/channels/{id}/invites", Owner),
    ("DELETE", "/api/channels/{id}/invites/{invite_id}", Owner),
    ("POST", "/api/channels/{id}/mentions/preview", Token),
    ("GET", "/api/channels/{id}/messages", Token),
    ("POST", "/api/channels/{id}/messages", Token),
    ("POST", "/api/channels/{id}/post", Exempt),
    ("POST", "/api/channels/{id}/questions/{question_id}/answer", Token),
    ("POST", "/api/channels/{id}/read", Token),
    ("GET", "/api/chatgpt", Token),
    ("GET", "/api/chatgpt/accounts", Token),
    ("DELETE", "/api/chatgpt/accounts/{id}", Owner),
    ("GET", "/api/chatgpt/accounts/{id}", Token),
    ("POST", "/api/chatgpt/accounts/{id}/clear-limit", Owner),
    ("GET", "/api/chatgpt/accounts/{id}/models", Token),
    ("POST", "/api/chatgpt/accounts/{id}/refresh", Owner),
    ("POST", "/api/chatgpt/signin", Owner),
    ("POST", "/api/chatgpt/signin/complete", Owner),
    ("GET", "/api/chatgpt/signin/{state}", Token),
    ("GET", "/api/computers", Token),
    ("POST", "/api/computers", Owner),
    ("POST", "/api/computers/enrol-codes", Owner),
    ("DELETE", "/api/computers/enrol-codes/{id}", Owner),
    ("GET", "/api/computers/enrol-codes/{id}", Owner),
    ("POST", "/api/computers/enrol-codes/{id}/confirm", Owner),
    ("POST", "/api/computers/enrol-codes/{id}/deny", Owner),
    ("GET", "/api/computers/{computer}/ide/relay/{launch_id}", Token),
    ("DELETE", "/api/computers/{id}", Owner),
    ("GET", "/api/computers/{id}", Token),
    ("POST", "/api/computers/{id}/fs/{op}", Token),
    ("POST", "/api/computers/{id}/harnesses/detect", Token),
    ("POST", "/api/computers/{id}/ide/extensions", Token),
    ("DELETE", "/api/computers/{id}/ide/extensions/{extension}", Token),
    ("GET", "/api/computers/{id}/metrics", Token),
    ("GET", "/api/computers/{id}/ports", Token),
    ("GET", "/api/computers/{id}/terms", Token),
    ("POST", "/api/computers/{id}/terms", Token),
    ("DELETE", "/api/computers/{id}/terms/{tid}", Token),
    ("GET", "/api/computers/{id}/terms/{tid}", Token),
    ("GET", "/api/computers/{id}/terms/{tid}/attach", Token),
    ("POST", "/api/computers/{id}/terms/{tid}/control", Token),
    ("POST", "/api/computers/{id}/terms/{tid}/kill", Token),
    ("GET", "/api/computers/{id}/terms/{tid}/snapshot", Token),
    ("GET", "/api/devices", Owner),
    ("POST", "/api/devices", Owner),
    ("DELETE", "/api/devices/{peer}", Owner),
    ("PATCH", "/api/devices/{peer}", Owner),
    ("GET", "/api/guest-door", Token),
    ("POST", "/api/guest/accept", OtherDoor),
    ("GET", "/api/guest/channels/{id}", OtherDoor),
    ("GET", "/api/guest/channels/{id}/messages", OtherDoor),
    ("POST", "/api/guest/channels/{id}/messages", OtherDoor),
    ("GET", "/api/health", Exempt),
    ("GET", "/api/hub", Token),
    ("POST", "/api/hub/check", Owner),
    ("GET", "/api/hub/devices", Token),
    ("DELETE", "/api/hub/devices/{id}", Owner),
    ("POST", "/api/hub/devices/{id}/computer", Owner),
    ("DELETE", "/api/hub/link", Owner),
    ("POST", "/api/hub/link", Owner),
    ("GET", "/api/hub/link-codes/{code}", Token),
    ("POST", "/api/hub/link-codes/{code}", Owner),
    ("DELETE", "/api/hub/registration", Owner),
    ("POST", "/api/hub/sync-devices", Owner),
    ("PUT", "/api/hub/sync-devices", Owner),
    ("GET", "/api/ide/extensions/search", Token),
    ("GET", "/api/integrations/github", Token),
    ("GET", "/api/mcp", Token),
    ("POST", "/api/mcp", Owner),
    ("DELETE", "/api/mcp/{id}", Owner),
    ("GET", "/api/mcp/{id}", Token),
    ("PATCH", "/api/mcp/{id}", Owner),
    ("GET", "/api/projects", Token),
    ("POST", "/api/projects", Token),
    ("DELETE", "/api/projects/{name}", Token),
    ("GET", "/api/projects/{name}", Token),
    ("PATCH", "/api/projects/{name}", Token),
    ("DELETE", "/api/projects/{name}/computers/{computer_id}", Token),
    ("PUT", "/api/projects/{name}/computers/{computer_id}", Token),
    ("POST", "/api/projects/{name}/git/{*op}", Token),
    ("GET", "/api/providers", Token),
    ("POST", "/api/providers", Owner),
    ("DELETE", "/api/providers/{id}", Owner),
    ("GET", "/api/push", Token),
    ("GET", "/api/schedules", Token),
    ("POST", "/api/schedules", Token),
    ("DELETE", "/api/schedules/{id}", Token),
    ("GET", "/api/schedules/{id}", Token),
    ("PATCH", "/api/schedules/{id}", Token),
    ("POST", "/api/schedules/{id}/pause", Token),
    ("POST", "/api/schedules/{id}/resume", Token),
    ("POST", "/api/schedules/{id}/run", Token),
    ("GET", "/api/schedules/{id}/runs", Token),
    ("GET", "/api/search", Token),
    ("POST", "/api/server-token/rotate", Owner),
    ("GET", "/api/sessions", Token),
    ("POST", "/api/sessions", Token),
    ("GET", "/api/sessions/{id}", Token),
    ("PATCH", "/api/sessions/{id}", Token),
    ("GET", "/api/sessions/{id}/a2a", Token),
    ("PUT", "/api/sessions/{id}/a2a", Token),
    ("POST", "/api/sessions/{id}/approvals/{approval_id}", Owner),
    ("PUT", "/api/sessions/{id}/approve-all", Owner),
    ("GET", "/api/sessions/{id}/computer", Token),
    ("PUT", "/api/sessions/{id}/computer", Token),
    ("PUT", "/api/sessions/{id}/designated-computer", Token),
    ("GET", "/api/sessions/{id}/events", Token),
    ("GET", "/api/sessions/{id}/export", Token),
    ("POST", "/api/sessions/{id}/fork", Token),
    ("GET", "/api/sessions/{id}/ide", Token),
    ("POST", "/api/sessions/{id}/interrupt", Token),
    ("POST", "/api/sessions/{id}/lease", Token),
    ("GET", "/api/sessions/{id}/mentions", Token),
    ("POST", "/api/sessions/{id}/messages", Token),
    ("POST", "/api/sessions/{id}/questions/{question_id}/answer", Token),
    ("GET", "/api/sessions/{id}/team", Token),
    ("GET", "/api/sessions/{id}/terminal-grants", Token),
    ("PUT", "/api/sessions/{id}/terminal-grants", Token),
    ("GET", "/api/sessions/{id}/usage", Token),
    ("GET", "/api/teams/{team_id}", Token),
    ("GET", "/api/teams/{team_id}/mail", Token),
    ("POST", "/api/teams/{team_id}/members/{session_id}/end", Token),
    ("GET", "/api/transfers", Token),
    ("DELETE", "/api/transfers/history", Token),
    ("GET", "/api/transfers/history", Token),
    ("POST", "/api/transfers/offers", Token),
    ("POST", "/api/transfers/offers/{id}/accept", Token),
    ("POST", "/api/transfers/offers/{id}/decline", Token),
    ("PUT", "/api/transfers/settings", Token),
    ("POST", "/api/transfers/{id}/cancel", Token),
    ("POST", "/api/transfers/{id}/chunks", Token),
    ("POST", "/api/transfers/{id}/complete", Token),
    ("POST", "/api/transfers/{id}/pause", Token),
    ("POST", "/api/transfers/{id}/resume", Token),
    ("GET", "/api/transfers/{id}/resume-offset/{file}", Token),
    ("GET", "/api/usage", Token),
    ("GET", "/api/usage/summary", Token),
    ("GET", "/auth/callback", Exempt),
    ("POST", "/enrol/redeem", OtherDoor),
    ("DELETE", "/enrol/status/{enrol_id}", OtherDoor),
    ("GET", "/enrol/status/{enrol_id}", OtherDoor),
    ("GET", "/join", OtherDoor),
];

/// True when `template` matches `segs` segment by segment. `{name}` matches one non-empty
/// segment, `{*name}` one or more trailing segments, as the router does.
fn template_matches(template: &str, segs: &[&str]) -> bool {
    let mut i = 0;
    for t in template.split('/') {
        if t.starts_with("{*") {
            return i < segs.len() && segs[i..].iter().all(|s| !s.is_empty());
        }
        let Some(s) = segs.get(i) else { return false };
        if t.starts_with('{') {
            if s.is_empty() {
                return false;
            }
        } else if t != *s {
            return false;
        }
        i += 1;
    }
    i == segs.len()
}

/// Which segments of `template` are literal. The router prefers a literal segment over a
/// parameter at the first position where two matching templates differ; comparing these
/// vectors gives the same order.
fn literal_mask(template: &str) -> Vec<bool> {
    template.split('/').map(|t| !t.starts_with('{')).collect()
}

/// The class of `method path` (the request path, no query), or `None` for a path no route
/// serves. `HEAD` is read as `GET`, which is how the router answers it.
pub fn classify(method: &Method, path: &str) -> Option<RouteClass> {
    let method = if method == Method::HEAD { "GET" } else { method.as_str() };
    let segs: Vec<&str> = path.split('/').collect();
    ROUTES
        .iter()
        .filter(|(m, t, _)| *m == method && template_matches(t, &segs))
        .max_by_key(|(_, t, _)| literal_mask(t))
        .map(|(_, _, c)| *c)
}

/// True when `method path` needs owner presence (`FR-S12`).
pub fn owner_route(method: &Method, path: &str) -> bool {
    classify(method, path) == Some(Owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_s12_owner_routes_match_by_template() {
        assert!(owner_route(&Method::POST, "/api/sessions/s1/approvals/a1"));
        assert!(owner_route(&Method::PUT, "/api/sessions/s1/approve-all"));
        assert!(owner_route(&Method::GET, "/api/devices"));
        assert!(owner_route(&Method::HEAD, "/api/devices"));
        assert!(owner_route(&Method::PATCH, "/api/devices/abc"));
        assert!(owner_route(&Method::POST, "/api/server-token/rotate"));
        assert!(owner_route(&Method::DELETE, "/api/computers/c1"));
        assert!(owner_route(&Method::POST, "/api/computers/enrol-codes"));
        assert!(owner_route(&Method::PUT, "/api/agents/codex/settings"));
    }

    #[test]
    fn fr_s12_reads_messages_and_question_answers_are_not_owner_routes() {
        for (m, p) in [
            (Method::GET, "/api/sessions"),
            (Method::POST, "/api/sessions"),
            (Method::POST, "/api/sessions/s1/messages"),
            (Method::POST, "/api/sessions/s1/questions/q1/answer"),
            (Method::POST, "/api/channels/c1/questions/q1/answer"),
            (Method::GET, "/api/accounts"),
            (Method::GET, "/api/computers/c1"),
            (Method::GET, "/api/mcp"),
            (Method::PUT, "/api/agents/defaults"),
            (Method::GET, "/api/hub/link-codes/abc"),
            (Method::POST, "/api/computers/c1/terms"),
        ] {
            assert!(!owner_route(&m, p), "{m} {p}");
        }
    }

    #[test]
    fn fr_s12_a_path_no_route_serves_has_no_class() {
        assert_eq!(classify(&Method::GET, "/api/no-such-route"), None);
        assert_eq!(classify(&Method::POST, "/api/devices/"), None);
        assert_eq!(classify(&Method::POST, "/api//devices"), None);
        assert_eq!(classify(&Method::POST, "/api/projects/p/git/"), None);
        assert_eq!(classify(&Method::POST, "/api/projects/p/git/pr/list"), Some(Token));
    }

    #[test]
    fn fr_s12_a_literal_segment_wins_over_a_parameter() {
        // `/api/accounts/route` is a read; `/api/accounts/{id}` shares its shape.
        assert_eq!(classify(&Method::GET, "/api/accounts/route"), Some(Token));
        assert_eq!(classify(&Method::GET, "/api/computers/enrol-codes/e1"), Some(Owner));
        assert_eq!(classify(&Method::GET, "/api/computers/c1/terms"), Some(Token));
    }
}
