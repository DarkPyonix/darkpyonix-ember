//! The breakpoints of the window: source breakpoints per file, function breakpoints and the
//! exception filters an adapter offers.
//!
//! The registry outlives sessions. A session gets all of it when the adapter sends `initialized`
//! and the affected file again whenever it changes. What an adapter says about a breakpoint
//! (verified, the line it moved to, its own id) is kept next to it and cleared when no session
//! is left, so a breakpoint outside a session reads as not verified.

use ember_editor_conn::debug::{BreakpointDto, RegisteredBreakpoint};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Map, Value};

/// A source breakpoint. `line` and `column` are the user's, 1-based; `adapter_line` is where the
/// adapter put it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceBreakpoint {
    pub id: String,
    pub uri: UriComponents,
    pub line: u32,
    pub column: Option<u32>,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub log_message: Option<String>,
    pub enabled: bool,
    pub verified: bool,
    pub adapter_line: Option<u32>,
    pub message: Option<String>,
    pub adapter_id: Option<i64>,
    /// The adapter's `DebugProtocol.Breakpoint`, for `$getDebugProtocolBreakpoint`.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionBreakpoint {
    pub id: String,
    pub name: String,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub log_message: Option<String>,
    pub enabled: bool,
    pub verified: bool,
    pub message: Option<String>,
    pub adapter_id: Option<i64>,
    pub raw: Value,
}

/// One of the adapter's `exceptionBreakpointFilters`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExceptionFilter {
    pub filter: String,
    pub label: String,
    pub enabled: bool,
    pub supports_condition: bool,
    pub condition: Option<String>,
}

/// What a change did to the registry, so the caller can tell the adapters and the extension host.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Touched {
    /// Files whose breakpoints must be sent again (a file that lost its last breakpoint too).
    pub files: Vec<UriComponents>,
    pub functions: bool,
    pub exceptions: bool,
}

impl Touched {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && !self.functions && !self.exceptions
    }

    /// Fold `other` into `self`.
    pub fn absorb(&mut self, other: Touched) {
        for f in other.files {
            if !self.files.contains(&f) {
                self.files.push(f);
            }
        }
        self.functions |= other.functions;
        self.exceptions |= other.exceptions;
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Breakpoints {
    pub source: Vec<SourceBreakpoint>,
    pub functions: Vec<FunctionBreakpoint>,
    pub exception_filters: Vec<ExceptionFilter>,
    next: u64,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// The `setBreakpoints` arguments for `uri`: its enabled breakpoints, in line order.
pub fn set_breakpoints_args(uri: &UriComponents, breakpoints: &[&SourceBreakpoint]) -> Value {
    let items: Vec<Value> = breakpoints
        .iter()
        .map(|b| {
            let mut m = Map::new();
            m.insert("line".into(), json!(b.line));
            if let Some(c) = b.column {
                m.insert("column".into(), json!(c));
            }
            if let Some(c) = &b.condition {
                m.insert("condition".into(), json!(c));
            }
            if let Some(c) = &b.hit_condition {
                m.insert("hitCondition".into(), json!(c));
            }
            if let Some(c) = &b.log_message {
                m.insert("logMessage".into(), json!(c));
            }
            Value::Object(m)
        })
        .collect();
    json!({
        "source": { "name": file_name(&uri.path), "path": uri.path },
        "lines": breakpoints.iter().map(|b| b.line).collect::<Vec<_>>(),
        "breakpoints": items,
        "sourceModified": false,
    })
}

/// The `setFunctionBreakpoints` arguments: the enabled function breakpoints.
pub fn set_function_breakpoints_args(breakpoints: &[&FunctionBreakpoint]) -> Value {
    let items: Vec<Value> = breakpoints
        .iter()
        .map(|b| {
            let mut m = Map::new();
            m.insert("name".into(), json!(b.name));
            if let Some(c) = &b.condition {
                m.insert("condition".into(), json!(c));
            }
            if let Some(c) = &b.hit_condition {
                m.insert("hitCondition".into(), json!(c));
            }
            Value::Object(m)
        })
        .collect();
    json!({ "breakpoints": items })
}

impl Breakpoints {
    fn next_id(&mut self) -> String {
        self.next += 1;
        format!("ember-bp-{}", self.next)
    }

    /// The distinct files that have a breakpoint, in the order the first one was added.
    pub fn files(&self) -> Vec<UriComponents> {
        let mut files: Vec<UriComponents> = Vec::new();
        for b in &self.source {
            if !files.contains(&b.uri) {
                files.push(b.uri.clone());
            }
        }
        files
    }

    /// The enabled breakpoints of `uri` in line order: what a session is told.
    pub fn enabled_in(&self, uri: &UriComponents) -> Vec<&SourceBreakpoint> {
        let mut v: Vec<&SourceBreakpoint> = self.source.iter().filter(|b| b.enabled && &b.uri == uri).collect();
        v.sort_by_key(|b| (b.line, b.column.unwrap_or(0)));
        v
    }

    pub fn enabled_functions(&self) -> Vec<&FunctionBreakpoint> {
        self.functions.iter().filter(|b| b.enabled).collect()
    }

    pub fn find_source(&self, id: &str) -> Option<&SourceBreakpoint> {
        self.source.iter().find(|b| b.id == id)
    }

    /// The DTO the extension host knows a source or function breakpoint by.
    pub fn dto(&self, id: &str) -> Option<BreakpointDto> {
        if let Some(b) = self.find_source(id) {
            return Some(BreakpointDto::Source {
                id: b.id.clone(),
                enabled: b.enabled,
                condition: b.condition.clone(),
                hit_condition: b.hit_condition.clone(),
                log_message: b.log_message.clone(),
                uri: b.uri.clone(),
                line: b.line.saturating_sub(1),
                character: b.column.map_or(0, |c| c.saturating_sub(1)),
            });
        }
        self.functions.iter().find(|b| b.id == id).map(|b| BreakpointDto::Function {
            id: b.id.clone(),
            enabled: b.enabled,
            condition: b.condition.clone(),
            hit_condition: b.hit_condition.clone(),
            log_message: b.log_message.clone(),
            function_name: b.name.clone(),
        })
    }

    /// Add a source breakpoint at `line` (1-based). Returns its id.
    #[allow(clippy::too_many_arguments)]
    pub fn add_source(
        &mut self,
        id: Option<String>,
        uri: UriComponents,
        line: u32,
        column: Option<u32>,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
        enabled: bool,
    ) -> String {
        let id = id.unwrap_or_else(|| self.next_id());
        self.source.push(SourceBreakpoint {
            id: id.clone(),
            uri,
            line,
            column,
            condition: non_empty(condition),
            hit_condition: non_empty(hit_condition),
            log_message: non_empty(log_message),
            enabled,
            verified: false,
            adapter_line: None,
            message: None,
            adapter_id: None,
            raw: Value::Null,
        });
        id
    }

    pub fn add_function(
        &mut self,
        id: Option<String>,
        name: String,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
        enabled: bool,
    ) -> String {
        let id = id.unwrap_or_else(|| self.next_id());
        self.functions.push(FunctionBreakpoint {
            id: id.clone(),
            name,
            condition: non_empty(condition),
            hit_condition: non_empty(hit_condition),
            log_message: non_empty(log_message),
            enabled,
            verified: false,
            message: None,
            adapter_id: None,
            raw: Value::Null,
        });
        id
    }

    /// Add what an extension registered. Returns the ids added and what to send.
    pub fn add_registered(&mut self, registered: Vec<RegisteredBreakpoint>) -> (Vec<String>, Touched) {
        let mut ids = Vec::new();
        let mut touched = Touched::default();
        for r in registered {
            match r {
                RegisteredBreakpoint::Source { id, uri, line, character, enabled, condition, hit_condition, log_message } => {
                    if self.find_source(&id).is_some() {
                        continue;
                    }
                    let column = (character > 0).then_some(character + 1);
                    if !touched.files.contains(&uri) {
                        touched.files.push(uri.clone());
                    }
                    ids.push(self.add_source(Some(id), uri, line + 1, column, condition, hit_condition, log_message, enabled));
                }
                RegisteredBreakpoint::Function { id, name, enabled, condition, hit_condition, log_message } => {
                    if id.as_ref().is_some_and(|i| self.functions.iter().any(|f| &f.id == i)) {
                        continue;
                    }
                    touched.functions = true;
                    ids.push(self.add_function(id, name, condition, hit_condition, log_message, enabled));
                }
            }
        }
        (ids, touched)
    }

    /// Toggle the breakpoint at `line` of `uri`: remove it when there is one, add one otherwise.
    /// Returns the id and whether it was added.
    pub fn toggle(&mut self, uri: &UriComponents, line: u32) -> (String, bool, Touched) {
        let touched = Touched { files: vec![uri.clone()], ..Default::default() };
        if let Some(i) = self.source.iter().position(|b| &b.uri == uri && b.line == line) {
            let removed = self.source.remove(i);
            return (removed.id, false, touched);
        }
        let id = self.add_source(None, uri.clone(), line, None, None, None, None, true);
        (id, true, touched)
    }

    /// Remove a source or a function breakpoint. Returns what to send, or `None` for an unknown id.
    pub fn remove(&mut self, id: &str) -> Option<Touched> {
        if let Some(i) = self.source.iter().position(|b| b.id == id) {
            let removed = self.source.remove(i);
            return Some(Touched { files: vec![removed.uri], ..Default::default() });
        }
        if let Some(i) = self.functions.iter().position(|b| b.id == id) {
            self.functions.remove(i);
            return Some(Touched { functions: true, ..Default::default() });
        }
        None
    }

    /// Replace the condition, hit condition, log message and enabled flag of a breakpoint.
    pub fn edit(
        &mut self,
        id: &str,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
        enabled: bool,
    ) -> Option<Touched> {
        if let Some(b) = self.source.iter_mut().find(|b| b.id == id) {
            b.condition = non_empty(condition);
            b.hit_condition = non_empty(hit_condition);
            b.log_message = non_empty(log_message);
            b.enabled = enabled;
            return Some(Touched { files: vec![b.uri.clone()], ..Default::default() });
        }
        if let Some(b) = self.functions.iter_mut().find(|b| b.id == id) {
            b.condition = non_empty(condition);
            b.hit_condition = non_empty(hit_condition);
            b.log_message = non_empty(log_message);
            b.enabled = enabled;
            return Some(Touched { functions: true, ..Default::default() });
        }
        None
    }

    /// Take the filters of an adapter's capabilities. A filter keeps its enabled flag when it is
    /// offered again; a new one starts as the adapter's `default` says.
    pub fn set_filters(&mut self, filters: &Value) {
        let Some(list) = filters.as_array() else { return };
        let old = std::mem::take(&mut self.exception_filters);
        for f in list {
            let Some(filter) = f.get("filter").and_then(Value::as_str) else { continue };
            let previous = old.iter().find(|o| o.filter == filter);
            self.exception_filters.push(ExceptionFilter {
                filter: filter.to_owned(),
                label: f.get("label").and_then(Value::as_str).unwrap_or(filter).to_owned(),
                enabled: previous.map_or_else(|| f.get("default").and_then(Value::as_bool).unwrap_or(false), |p| p.enabled),
                supports_condition: f.get("supportsCondition").and_then(Value::as_bool).unwrap_or(false),
                condition: previous.and_then(|p| p.condition.clone()),
            });
        }
    }

    pub fn set_filter_enabled(&mut self, filter: &str, enabled: bool) -> bool {
        match self.exception_filters.iter_mut().find(|f| f.filter == filter) {
            Some(f) => {
                f.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// The `setExceptionBreakpoints` arguments for an adapter that does (`filterOptions`) or does
    /// not (`filters`) support filter conditions.
    pub fn exception_args(&self, supports_filter_options: bool) -> Value {
        let on: Vec<&ExceptionFilter> = self.exception_filters.iter().filter(|f| f.enabled).collect();
        if supports_filter_options {
            let options: Vec<Value> = on
                .iter()
                .map(|f| match &f.condition {
                    Some(c) => json!({ "filterId": f.filter, "condition": c }),
                    None => json!({ "filterId": f.filter }),
                })
                .collect();
            json!({ "filters": [], "filterOptions": options })
        } else {
            json!({ "filters": on.iter().map(|f| f.filter.clone()).collect::<Vec<_>>() })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_adapter(raw: &Value, verified: &mut bool, line: Option<&mut Option<u32>>, message: &mut Option<String>, adapter_id: &mut Option<i64>, store: &mut Value) {
        *verified = raw.get("verified").and_then(Value::as_bool).unwrap_or(false);
        if let Some(line) = line {
            *line = raw.get("line").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        }
        *message = raw.get("message").and_then(Value::as_str).map(str::to_owned);
        *adapter_id = raw.get("id").and_then(Value::as_i64);
        *store = raw.clone();
    }

    /// The adapter's answer to `setBreakpoints`: `ids` are the breakpoints that were sent, in order.
    pub fn apply_source_response(&mut self, ids: &[String], answers: &Value) {
        let Some(answers) = answers.as_array() else { return };
        for (id, raw) in ids.iter().zip(answers) {
            if let Some(b) = self.source.iter_mut().find(|b| &b.id == id) {
                Self::apply_adapter(raw, &mut b.verified, Some(&mut b.adapter_line), &mut b.message, &mut b.adapter_id, &mut b.raw);
            }
        }
    }

    pub fn apply_function_response(&mut self, ids: &[String], answers: &Value) {
        let Some(answers) = answers.as_array() else { return };
        for (id, raw) in ids.iter().zip(answers) {
            if let Some(b) = self.functions.iter_mut().find(|b| &b.id == id) {
                Self::apply_adapter(raw, &mut b.verified, None, &mut b.message, &mut b.adapter_id, &mut b.raw);
            }
        }
    }

    /// A DAP `breakpoint` event: the adapter changed what it knows about one of its breakpoints.
    /// Returns whether a breakpoint of ours was updated.
    pub fn apply_event(&mut self, reason: &str, raw: &Value) -> bool {
        let Some(adapter_id) = raw.get("id").and_then(Value::as_i64) else { return false };
        if reason == "removed" {
            for b in self.source.iter_mut().filter(|b| b.adapter_id == Some(adapter_id)) {
                b.verified = false;
            }
            return false;
        }
        if let Some(b) = self.source.iter_mut().find(|b| b.adapter_id == Some(adapter_id)) {
            Self::apply_adapter(raw, &mut b.verified, Some(&mut b.adapter_line), &mut b.message, &mut b.adapter_id, &mut b.raw);
            return true;
        }
        if let Some(b) = self.functions.iter_mut().find(|b| b.adapter_id == Some(adapter_id)) {
            Self::apply_adapter(raw, &mut b.verified, None, &mut b.message, &mut b.adapter_id, &mut b.raw);
            return true;
        }
        false
    }

    /// Forget what adapters said: no session is left.
    pub fn clear_adapter_data(&mut self) {
        for b in &mut self.source {
            b.verified = false;
            b.adapter_line = None;
            b.message = None;
            b.adapter_id = None;
            b.raw = Value::Null;
        }
        for b in &mut self.functions {
            b.verified = false;
            b.message = None;
            b.adapter_id = None;
            b.raw = Value::Null;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(path: &str) -> UriComponents {
        UriComponents::remote("a", path)
    }

    #[test]
    fn fr_l31_42_toggle_adds_then_removes_and_reports_the_file() {
        let mut bps = Breakpoints::default();
        let (id, added, touched) = bps.toggle(&uri("/w/m.py"), 2);
        assert!(added);
        assert_eq!(touched.files, vec![uri("/w/m.py")]);
        assert_eq!(bps.source.len(), 1);
        let (again, added, touched) = bps.toggle(&uri("/w/m.py"), 2);
        assert!(!added);
        assert_eq!(again, id);
        assert!(bps.source.is_empty());
        assert_eq!(touched.files.len(), 1, "a file that lost its last breakpoint is still sent");
    }

    #[test]
    fn fr_l31_42_set_breakpoints_args_carry_condition_hit_count_and_log_message() {
        let mut bps = Breakpoints::default();
        bps.add_source(None, uri("/w/m.py"), 5, Some(3), Some("x > 1".into()), Some("2".into()), None, true);
        bps.add_source(None, uri("/w/m.py"), 2, None, None, None, Some("x is {x}".into()), true);
        bps.add_source(None, uri("/w/m.py"), 9, None, None, None, None, false);
        bps.add_source(None, uri("/w/other.py"), 1, None, None, None, None, true);
        let enabled = bps.enabled_in(&uri("/w/m.py"));
        let args = set_breakpoints_args(&uri("/w/m.py"), &enabled);
        assert_eq!(args["source"], json!({"name": "m.py", "path": "/w/m.py"}));
        assert_eq!(args["lines"], json!([2, 5]));
        assert_eq!(args["breakpoints"][0], json!({"line": 2, "logMessage": "x is {x}"}));
        assert_eq!(args["breakpoints"][1], json!({"line": 5, "column": 3, "condition": "x > 1", "hitCondition": "2"}));
        assert_eq!(bps.files(), vec![uri("/w/m.py"), uri("/w/other.py")]);
    }

    #[test]
    fn fr_l31_42_adapter_answers_set_verified_and_the_moved_line() {
        let mut bps = Breakpoints::default();
        let a = bps.add_source(None, uri("/w/m.py"), 1, None, None, None, None, true);
        let b = bps.add_source(None, uri("/w/m.py"), 5, None, None, None, None, true);
        bps.apply_source_response(&[a.clone(), b.clone()], &json!([{"verified": true, "line": 1, "id": 7}, {"verified": false, "message": "no code"}]));
        assert!(bps.find_source(&a).unwrap().verified);
        assert_eq!(bps.find_source(&a).unwrap().adapter_id, Some(7));
        assert_eq!(bps.find_source(&b).unwrap().message.as_deref(), Some("no code"));
        assert!(bps.apply_event("changed", &json!({"id": 7, "verified": true, "line": 3})));
        assert_eq!(bps.find_source(&a).unwrap().adapter_line, Some(3));
        assert!(!bps.apply_event("changed", &json!({"id": 99, "verified": true})));
        bps.clear_adapter_data();
        assert!(!bps.find_source(&a).unwrap().verified);
        assert_eq!(bps.find_source(&a).unwrap().raw, Value::Null);
    }

    #[test]
    fn fr_l31_42_exception_filters_keep_their_state_and_pick_the_request_form() {
        let mut bps = Breakpoints::default();
        bps.set_filters(&json!([{"filter": "raised", "label": "Raised", "default": true}, {"filter": "uncaught", "label": "Uncaught", "supportsCondition": true}]));
        assert!(bps.exception_filters[0].enabled);
        assert!(!bps.exception_filters[1].enabled);
        assert!(bps.set_filter_enabled("uncaught", true));
        assert!(!bps.set_filter_enabled("nope", true));
        bps.set_filters(&json!([{"filter": "uncaught", "label": "Uncaught"}]));
        assert!(bps.exception_filters[0].enabled, "an offered-again filter keeps its state");
        assert_eq!(bps.exception_args(false), json!({"filters": ["uncaught"]}));
        assert_eq!(bps.exception_args(true), json!({"filters": [], "filterOptions": [{"filterId": "uncaught"}]}));
    }

    #[test]
    fn fr_l31_42_registered_breakpoints_use_one_based_lines_and_dto_zero_based() {
        let mut bps = Breakpoints::default();
        let (ids, touched) = bps.add_registered(vec![
            RegisteredBreakpoint::Source {
                id: "x1".into(),
                uri: uri("/w/m.py"),
                line: 1,
                character: 4,
                enabled: true,
                condition: Some("c".into()),
                hit_condition: None,
                log_message: None,
            },
            RegisteredBreakpoint::Function { id: Some("f1".into()), name: "main".into(), enabled: true, condition: None, hit_condition: None, log_message: None },
        ]);
        assert_eq!(ids, vec!["x1".to_owned(), "f1".to_owned()]);
        assert!(touched.functions && touched.files == vec![uri("/w/m.py")]);
        let b = bps.find_source("x1").unwrap();
        assert_eq!((b.line, b.column), (2, Some(5)));
        let Some(BreakpointDto::Source { line, character, .. }) = bps.dto("x1") else { panic!("no dto") };
        assert_eq!((line, character), (1, 4));
        assert!(matches!(bps.dto("f1"), Some(BreakpointDto::Function { .. })));
        let (again, _) = bps.add_registered(vec![RegisteredBreakpoint::Function { id: Some("f1".into()), name: "main".into(), enabled: true, condition: None, hit_condition: None, log_message: None }]);
        assert!(again.is_empty(), "an id that is already known is not added twice");
        assert!(bps.remove("x1").unwrap().files.len() == 1);
        assert!(bps.remove("f1").unwrap().functions);
        assert!(bps.remove("x1").is_none());
    }
}
