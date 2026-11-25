//! The protocol coverage ledger (`ember/editor-conn/coverage.toml`, SPEC FR-E9).
//!
//! The ledger says, for every `MainThread*` method of the pinned protocol, whether Ember bridges
//! it, acknowledges it with an empty answer on purpose, leaves it out of scope, or has not built it
//! yet. The session asks [`status`] when a request reaches the fallback reply, and a unit test keeps
//! the file and [`crate::actors::generated`] in step, so a pin bump cannot slip a new method in as a
//! silent empty answer.
//!
//! The fallback reply itself is chosen per method ([`answer`], Q-E8 (c)): the ledger may give a
//! method an `answer` (a JSON value) or an `error` (a message the call is rejected with); a method
//! without either answers by its declared result, an empty list for an array and `undefined`
//! otherwise.
//!
//! The file is a small subset of TOML, read without a TOML crate: blank lines, `#` comments,
//! `[ActorName]` headers, `note = "..."` lines, and method lines that are either
//! `"$method" = "<status>"` or an inline table
//! `"$method" = { status = "<status>", answer = '<JSON>' }` (or `error = "<message>"` in place of
//! `answer`). `answer` is a TOML literal string so the JSON inside may hold double quotes.

use std::sync::OnceLock;

use serde_json::Value;

use crate::actors::ActorInfo;
use crate::rpc::Arg;

/// What Ember does with one `MainThread*` method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Decoded and answered from the session's own state.
    Bridged,
    /// The empty answer is correct; a person confirmed it (the actor's note says who).
    Acked,
    /// Out of scope; the actor's note says why.
    Out,
    /// Not built yet: the fallback reply; a call that returns a value is recorded (FR-E9).
    Todo,
}

impl Status {
    fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "bridged" => Status::Bridged,
            "acked" => Status::Acked,
            "out" => Status::Out,
            "todo" => Status::Todo,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Bridged => "bridged",
            Status::Acked => "acked",
            Status::Out => "out",
            Status::Todo => "todo",
        }
    }
}

/// The fallback reply of a method no trait method answers (Q-E8 (c)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// `undefined`.
    Undefined,
    /// This JSON value.
    Value(Value),
    /// The call is rejected with an `Error` carrying this message.
    Error(String),
}

/// One actor of the ledger, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorEntry {
    pub proxy: String,
    pub note: Option<String>,
    pub methods: Vec<(String, Status)>,
    /// The methods the ledger gives an `answer` or an `error`, in file order.
    pub answers: Vec<(String, Answer)>,
}

impl ActorEntry {
    pub fn status(&self, method: &str) -> Option<Status> {
        self.methods.iter().find(|(m, _)| m == method).map(|(_, s)| *s)
    }

    /// The answer the ledger gives `method`, if it gives one.
    pub fn answer(&self, method: &str) -> Option<&Answer> {
        self.answers.iter().find(|(m, _)| m == method).map(|(_, a)| a)
    }
}

/// The parsed ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    pub actors: Vec<ActorEntry>,
}

/// The ledger text, as checked in.
pub const LEDGER_TOML: &str = include_str!("../coverage.toml");

/// The checked-in ledger. It is parsed once; the unit tests guarantee that it parses.
pub fn ledger() -> &'static Ledger {
    static LEDGER: OnceLock<Ledger> = OnceLock::new();
    LEDGER.get_or_init(|| Ledger::parse(LEDGER_TOML).unwrap_or_else(|e| panic!("coverage.toml: {e}")))
}

/// The ledger's status of `proxy.method`, or `None` when the ledger does not name it (an `ExtHost*`
/// id, a method outside the pinned protocol).
pub fn status(proxy: &str, method: &str) -> Option<Status> {
    ledger().actor(proxy).and_then(|a| a.status(method))
}

/// The fallback reply of `proxy.method` (Q-E8 (c)): the ledger's `answer` or `error` for the method
/// when it gives one; otherwise an empty list when the pinned protocol declares an array result (the
/// extension host iterates those results, and `undefined` throws `TypeError: ... is not iterable`,
/// seen with `MainThreadWorkspace.$loadCertificates`), and `undefined` for everything else.
pub fn answer(proxy: &str, method: &str) -> Answer {
    if let Some(a) = ledger().actor(proxy).and_then(|a| a.answer(method)) {
        return a.clone();
    }
    if declared_result(proxy, method).is_some_and(|t| t.ends_with("[]")) {
        return Answer::Value(Value::Array(Vec::new()));
    }
    Answer::Undefined
}

/// The declared result of `proxy.method` without its `Promise<...>`, or `None` when the pinned
/// protocol does not have the method.
fn declared_result(proxy: &str, method: &str) -> Option<&'static str> {
    let m = ActorInfo::by_proxy(proxy)?.method(method)?;
    let t = m.returns.trim();
    Some(t.strip_prefix("Promise<").and_then(|t| t.strip_suffix('>')).unwrap_or(t).trim())
}

/// Whether the pinned protocol declares `proxy.method` to return a value (not `void`, `undefined`
/// or a `Promise` of those). `false` for a method the protocol does not have.
pub fn returns_value(proxy: &str, method: &str) -> bool {
    declared_result(proxy, method).is_some_and(|t| !matches!(t, "void" | "undefined" | "never"))
}

/// The extension a call names in its arguments, if any (SPEC FR-E9: the RPC wire does not say
/// which extension called, so this is the only exact source). A parameter counts when it is named
/// like an extension id or typed `ExtensionIdentifier` / `IExtensionDescription`; its value is
/// read as a string, an `ExtensionIdentifier` (`{ value }`) or a description (`{ identifier }`).
pub fn calling_extension(proxy: &str, method: &str, args: &[Arg]) -> Option<String> {
    let m = ActorInfo::by_proxy(proxy)?.method(method)?;
    m.params.iter().enumerate().find_map(|(i, p)| {
        let named = matches!(p.name, "extensionId" | "extension" | "extensionIdentifier" | "extId");
        let typed = p.ts_type.contains("ExtensionIdentifier") || p.ts_type.contains("IExtensionDescription");
        if !named && !typed {
            return None;
        }
        extension_id(args.get(i)?.as_json()?)
    })
}

fn extension_id(v: &Value) -> Option<String> {
    let id = match v {
        Value::String(s) => Some(s.as_str()),
        Value::Object(o) => match o.get("identifier") {
            Some(Value::String(s)) => Some(s.as_str()),
            Some(i) => i.get("value").and_then(Value::as_str),
            None => o.get("value").and_then(Value::as_str),
        },
        _ => None,
    }?;
    (!id.is_empty()).then(|| id.to_owned())
}

impl Ledger {
    pub fn actor(&self, proxy: &str) -> Option<&ActorEntry> {
        self.actors.iter().find(|a| a.proxy == proxy)
    }

    /// Parse the ledger. Errors name the line.
    pub fn parse(text: &str) -> Result<Ledger, String> {
        let mut actors: Vec<ActorEntry> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Err(format!("line {n}: bad actor header {line:?}"));
                }
                if actors.iter().any(|a| a.proxy == name) {
                    return Err(format!("line {n}: actor {name} listed twice"));
                }
                actors.push(ActorEntry { proxy: name.to_owned(), note: None, methods: Vec::new(), answers: Vec::new() });
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {n}: expected `key = \"value\"`"));
            };
            let (key, value) = (key.trim(), value.trim());
            let Some(actor) = actors.last_mut() else {
                return Err(format!("line {n}: entry before the first actor header"));
            };
            if key == "note" {
                let value = unquote(value).ok_or_else(|| format!("line {n}: value is not a quoted string"))?;
                if actor.note.is_some() {
                    return Err(format!("line {n}: {} has two notes", actor.proxy));
                }
                actor.note = Some(value.to_owned());
                continue;
            }
            let method = unquote(key).filter(|m| m.starts_with('$')).ok_or_else(|| format!("line {n}: bad method key {key:?}"))?;
            let (status, answer) = match value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
                Some(table) => method_table(table).map_err(|e| format!("line {n}: {e}"))?,
                None => (unquote(value).ok_or_else(|| format!("line {n}: value is not a quoted string"))?, None),
            };
            let status = Status::parse(status).ok_or_else(|| format!("line {n}: unknown status {status:?}"))?;
            if actor.status(method).is_some() {
                return Err(format!("line {n}: {}.{method} listed twice", actor.proxy));
            }
            actor.methods.push((method.to_owned(), status));
            if let Some(a) = answer {
                actor.answers.push((method.to_owned(), a));
            }
        }
        for a in &actors {
            let needs_note = a.methods.iter().any(|(_, s)| matches!(s, Status::Acked | Status::Out));
            if needs_note && a.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
                return Err(format!("{}: an `acked` or `out` method needs the actor's `note` (who decided, why)", a.proxy));
            }
        }
        Ok(Ledger { actors })
    }
}

fn unquote(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('"') && !inner.contains('\\')).then_some(inner)
}

/// A TOML literal string (`'...'`): everything between the quotes, which may not hold `'`.
fn unquote_literal(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('\'')?.strip_suffix('\'')?;
    (!inner.contains('\'')).then_some(inner)
}

/// The inside of a method's inline table: `status` and at most one of `answer` and `error`.
fn method_table(table: &str) -> Result<(&str, Option<Answer>), String> {
    let (mut status, mut answer) = (None, None);
    for field in split_fields(table)? {
        let (k, v) = field.split_once('=').ok_or_else(|| format!("expected `key = value` in {field:?}"))?;
        let (k, v) = (k.trim(), v.trim());
        let set = match k {
            "status" => status.replace(unquote(v).ok_or_else(|| format!("status is not a quoted string: {v:?}"))?).is_some(),
            "answer" => {
                let json = unquote_literal(v).ok_or_else(|| format!("answer is not a literal string ('...'): {v:?}"))?;
                let value = serde_json::from_str(json).map_err(|e| format!("answer is not JSON: {e}"))?;
                answer.replace(Answer::Value(value)).is_some()
            }
            "error" => {
                let message = unquote(v).filter(|m| !m.trim().is_empty()).ok_or_else(|| format!("error is not a quoted message: {v:?}"))?;
                answer.replace(Answer::Error(message.to_owned())).is_some()
            }
            other => return Err(format!("unknown field {other:?}")),
        };
        if set {
            return Err("a field is given twice, or both `answer` and `error`".to_owned());
        }
    }
    Ok((status.ok_or("the table has no `status`")?, answer))
}

/// Split an inline table at its top-level commas; commas inside quotes, brackets or braces stay.
fn split_fields(table: &str) -> Result<Vec<&str>, String> {
    let (mut out, mut start, mut depth, mut quote) = (Vec::new(), 0, 0i32, None::<char>);
    for (i, c) in table.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(table[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err("unbalanced quotes or brackets".to_owned());
    }
    out.push(table[start..].trim());
    Ok(out.into_iter().filter(|f| !f.is_empty()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::generated::ACTORS;

    #[test]
    fn fr_e9_ledger_covers_generated_protocol() {
        let ledger = ledger();
        let generated: Vec<&str> = ACTORS.iter().map(|a| a.proxy).collect();
        let listed: Vec<&str> = ledger.actors.iter().map(|a| a.proxy.as_str()).collect();
        for p in &generated {
            assert!(listed.contains(p), "coverage.toml lacks the actor {p}: add it with its methods as `todo` or `out`");
        }
        for p in &listed {
            assert!(generated.contains(p), "coverage.toml names {p}, which the pinned protocol does not have");
        }
        assert_eq!(listed, generated, "coverage.toml lists the actors in another order than the protocol");
        for a in ACTORS {
            let entry = ledger.actor(a.proxy).unwrap();
            for m in a.methods {
                assert!(entry.status(m.name).is_some(), "coverage.toml lacks {}.{}: add it as `todo` or `out`", a.proxy, m.name);
            }
            for (m, _) in &entry.methods {
                assert!(a.method(m).is_some(), "coverage.toml names {}.{m}, which the pinned protocol does not have", a.proxy);
            }
            assert_eq!(entry.methods.len(), a.methods.len(), "{}", a.proxy);
            for (m, answer) in &entry.answers {
                if matches!(answer, Answer::Value(_)) {
                    assert!(returns_value(a.proxy, m), "coverage.toml gives {}.{m} an `answer`, but it returns nothing", a.proxy);
                }
            }
        }
    }

    // Q-E8 (c): the fallback reply is the ledger's per-method answer, else the declared result's.
    #[test]
    fn fr_e9_fallback_answer_is_chosen_per_method_from_the_ledger() {
        use serde_json::json;
        // The ledger's `answer`: Code-OSS's BrowserPowerService constants, and the window state.
        assert_eq!(answer("MainThreadPower", "$isOnBatteryPower"), Answer::Value(json!(false)));
        assert_eq!(answer("MainThreadPower", "$getCurrentThermalState"), Answer::Value(json!("unknown")));
        assert_eq!(answer("MainThreadPower", "$getSystemIdleTime"), Answer::Value(json!(0)));
        assert_eq!(answer("MainThreadPower", "$startPowerSaveBlocker"), Answer::Value(json!(-1)));
        assert_eq!(answer("MainThreadWindow", "$getInitialState"), Answer::Value(json!({"isFocused": true, "isActive": true})));
        // A bridged method's answer when the session cannot answer from its state.
        assert_eq!(answer("MainThreadWorkspace", "$checkExists"), Answer::Value(json!(false)));
        assert_eq!(answer("MainThreadWorkspace", "$isResourceTrusted"), Answer::Value(json!(true)));
        // The ledger's `error`.
        let no_provider = Answer::Error("No authentication provider is currently registered.".into());
        assert_eq!(answer("MainThreadAuthentication", "$getSession"), no_provider);
        assert_eq!(answer("MainThreadAuthentication", "$getAccounts"), no_provider);
        // No ledger answer: an array result answers `[]`, anything else `undefined`.
        assert_eq!(answer("MainThreadWorkspace", "$loadCertificates"), Answer::Value(json!([])));
        // `UriComponents[] | null` is not an array result.
        assert_eq!(answer("MainThreadWorkspace", "$startFileSearch"), Answer::Undefined);
        assert_eq!(answer("MainThreadCommands", "$getCommands"), Answer::Value(json!([])));
        assert_eq!(answer("MainThreadSecretState", "$getPassword"), Answer::Undefined);
        assert_eq!(answer("MainThreadStatusBar", "$setEntry"), Answer::Undefined);
        assert_eq!(answer("MainThreadWorkspace", "$nope"), Answer::Undefined);
        assert_eq!(answer("ExtHostDocuments", "$acceptModelChanged"), Answer::Undefined);
    }

    #[test]
    fn fr_e9_ledger_lookup_names_what_ember_does() {
        assert_eq!(status("MainThreadDiagnostics", "$changeMany"), Some(Status::Bridged));
        assert_eq!(status("MainThreadDocuments", "$tryOpenDocument"), Some(Status::Bridged));
        assert_eq!(status("MainThreadTextEditors", "$tryShowTextDocument"), Some(Status::Todo));
        // INTENT D54 (owner, Q-E7) and the leader's Q2 classification, 2026-11-23.
        assert_eq!(status("MainThreadLanguageModels", "$registerLanguageModelProvider"), Some(Status::Out));
        assert_eq!(status("MainThreadChatStatus", "$setEntry"), Some(Status::Out));
        assert_eq!(status("MainThreadTelemetry", "$publicLog2"), Some(Status::Acked));
        assert_eq!(status("MainThreadExtensionService", "$onExtensionActivationError"), Some(Status::Todo));
        let out_actors = ledger().actors.iter().filter(|a| !a.methods.is_empty() && a.methods.iter().all(|(_, s)| *s == Status::Out)).count();
        assert_eq!(out_actors, 19);
        assert_eq!(status("MainThreadDiagnostics", "$nope"), None);
        assert_eq!(status("ExtHostDocuments", "$acceptModelChanged"), None);
        let bridged = ledger().actors.iter().flat_map(|a| &a.methods).filter(|(_, s)| *s == Status::Bridged).count();
        assert!(bridged >= 60, "{bridged}");
    }

    #[test]
    fn fr_e9_records_know_value_returns_and_the_named_extension() {
        use serde_json::json;
        assert!(returns_value("MainThreadSecretState", "$getPassword"));
        assert!(returns_value("MainThreadPower", "$isOnBatteryPower"));
        assert!(!returns_value("MainThreadTelemetry", "$publicLog2"));
        assert!(!returns_value("MainThreadSecretState", "$setPassword"));
        assert!(!returns_value("MainThreadSecretState", "$nope"));

        let j = |v: Value| Arg::Json(v);
        // $getPassword(extensionId: string, key: string)
        let args = [j(json!("pub.ext")), j(json!("k"))];
        assert_eq!(calling_extension("MainThreadSecretState", "$getPassword", &args).as_deref(), Some("pub.ext"));
        // $registerTextEditorDecorationType(extensionId: ExtensionIdentifier, ...)
        let args = [j(json!({"value": "Pub.Deco", "_lower": "pub.deco"})), j(json!("key")), j(json!({}))];
        assert_eq!(calling_extension("MainThreadTextEditors", "$registerTextEditorDecorationType", &args).as_deref(), Some("Pub.Deco"));
        // $getSession(providerId, scopeListOrRequest, extensionId, extensionName, options)
        let args = [j(json!("github")), j(json!(["repo"])), j(json!("pub.auth")), j(json!("Auth")), j(json!({}))];
        assert_eq!(calling_extension("MainThreadAuthentication", "$getSession", &args).as_deref(), Some("pub.auth"));
        // No extension parameter, a missing argument, an empty id.
        assert_eq!(calling_extension("MainThreadPower", "$isOnBatteryPower", &[]), None);
        assert_eq!(calling_extension("MainThreadSecretState", "$getPassword", &[]), None);
        assert_eq!(calling_extension("MainThreadSecretState", "$getPassword", &[j(json!(""))]), None);
        assert_eq!(calling_extension("ExtHostDocuments", "$acceptModelChanged", &[]), None);
    }

    #[test]
    fn fr_e9_ledger_parser_rejects_what_the_test_could_not_trust() {
        let ok = "# c\n[MainThreadA]\nnote = \"leader, 2026-11-22: telemetry only\"\n\"$a\" = \"acked\"\n\"$b\" = \"todo\"\n";
        let l = Ledger::parse(ok).unwrap();
        assert_eq!(l.actors[0].status("$a"), Some(Status::Acked));
        assert_eq!(l.actors[0].note.as_deref(), Some("leader, 2026-11-22: telemetry only"));
        assert!(l.actors[0].answers.is_empty());

        // Inline tables: a JSON answer with commas and quotes inside, and an error message.
        let tables = "[A]\n\"$a\" = { status = \"todo\", answer = '{\"x\": [1, 2], \"y\": \"a, b\"}' }\n\"$b\" = { error = \"No provider.\", status = \"bridged\" }\n\"$c\" = { status = \"todo\" }\n";
        let l = Ledger::parse(tables).unwrap();
        let a = &l.actors[0];
        assert_eq!((a.status("$a"), a.status("$b"), a.status("$c")), (Some(Status::Todo), Some(Status::Bridged), Some(Status::Todo)));
        assert_eq!(a.answer("$a"), Some(&Answer::Value(serde_json::json!({"x": [1, 2], "y": "a, b"}))));
        assert_eq!(a.answer("$b"), Some(&Answer::Error("No provider.".into())));
        assert_eq!(a.answer("$c"), None);
        for (bad, why) in [
            ("[A]\n\"$a\" = \"maybe\"\n", "unknown status"),
            ("[A]\n\"$a\" = \"todo\"\n\"$a\" = \"todo\"\n", "twice"),
            ("[A]\n[A]\n", "twice"),
            ("\"$a\" = \"todo\"\n", "before the first actor"),
            ("[A]\n\"a\" = \"todo\"\n", "bad method key"),
            ("[A]\n\"$a\" = todo\n", "quoted"),
            ("[A]\n\"$a\" = \"out\"\n", "needs the actor's `note`"),
            ("[A]\n\"$a\" = \"acked\"\nnote = \"\"\n", "needs the actor's `note`"),
            ("[A]\n\"$a\" = { answer = 'false' }\n", "no `status`"),
            ("[A]\n\"$a\" = { status = \"todo\", answer = \"false\" }\n", "literal string"),
            ("[A]\n\"$a\" = { status = \"todo\", answer = '{nope' }\n", "not JSON"),
            ("[A]\n\"$a\" = { status = \"todo\", answer = 'false', error = \"x\" }\n", "both"),
            ("[A]\n\"$a\" = { status = \"todo\", error = \"\" }\n", "quoted message"),
            ("[A]\n\"$a\" = { status = \"todo\", reply = 'false' }\n", "unknown field"),
            ("[A]\n\"$a\" = { status = \"todo\", answer = 'false }\n", "unbalanced"),
        ] {
            let e = Ledger::parse(bad).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
    }
}
