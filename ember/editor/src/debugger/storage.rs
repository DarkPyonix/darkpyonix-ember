//! The breakpoints a window keeps when it closes (FR-L31.42), as Code-OSS's `DebugStorage`
//! (`debugStorage.ts`) keeps them in the workspace's storage: source breakpoints
//! (`debug.breakpoint`) with their condition, hit condition, log message and enabled flag,
//! function breakpoints (`debug.functionbreakpoint`) and the exception filters chosen
//! (`debug.exceptionbreakpoint`). The fields carry the names of the models' `toJSON`. A source
//! breakpoint keeps the file's path on the workspace's server rather than a whole URI: the
//! authority of a remote URI belongs to one connection, and [`super::DebugState`] puts the
//! workspace's back when it restores them.
//!
//! What adapters said about a breakpoint (verified, the line it moved to) is not kept, as
//! Code-OSS does not keep it. Data breakpoints do not exist in Ember yet.

use serde::{Deserialize, Serialize};

use super::breakpoints::{Breakpoints, Touched};
use ember_editor_conn::uri::UriComponents;

/// One source breakpoint (`Breakpoint.toJSON`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredBreakpoint {
    pub path: String,
    pub line_number: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

/// One function breakpoint (`FunctionBreakpoint.toJSON`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredFunctionBreakpoint {
    pub name: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

/// One exception filter and whether it was chosen (`ExceptionBreakpoint.toJSON`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredExceptionBreakpoint {
    pub filter: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub supports_condition: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// Everything [`Breakpoints`] keeps across a reopen. Empty lists are left out of the file, as
/// `storeBreakpoints` removes a key that has nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredBreakpoints {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breakpoints: Vec<StoredBreakpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_breakpoints: Vec<StoredFunctionBreakpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exception_breakpoints: Vec<StoredExceptionBreakpoint>,
}

fn enabled_default() -> bool {
    true
}

impl StoredBreakpoints {
    pub fn is_empty(&self) -> bool {
        self.breakpoints.is_empty()
            && self.function_breakpoints.is_empty()
            && self.exception_breakpoints.is_empty()
    }

    /// What the window's breakpoint lists keep, from the registry the session layer reports
    /// ([`super::DebugUpdate::Breakpoints`]).
    pub fn from_lists(
        source: &[super::SourceBreakpoint],
        functions: &[super::FunctionBreakpoint],
        filters: &[super::ExceptionFilter],
    ) -> Self {
        Self {
            breakpoints: source
                .iter()
                .map(|b| StoredBreakpoint {
                    path: b.uri.path.clone(),
                    line_number: b.line,
                    column: b.column,
                    enabled: b.enabled,
                    condition: b.condition.clone(),
                    hit_condition: b.hit_condition.clone(),
                    log_message: b.log_message.clone(),
                })
                .collect(),
            function_breakpoints: functions
                .iter()
                .map(|b| StoredFunctionBreakpoint {
                    name: b.name.clone(),
                    enabled: b.enabled,
                    condition: b.condition.clone(),
                    hit_condition: b.hit_condition.clone(),
                    log_message: b.log_message.clone(),
                })
                .collect(),
            exception_breakpoints: filters
                .iter()
                .map(|f| StoredExceptionBreakpoint {
                    filter: f.filter.clone(),
                    label: f.label.clone(),
                    enabled: f.enabled,
                    supports_condition: f.supports_condition,
                    condition: f.condition.clone(),
                })
                .collect(),
        }
    }
}

impl Breakpoints {
    /// What [`StoredBreakpoints::from_lists`] makes of this registry.
    pub fn to_stored(&self) -> StoredBreakpoints {
        StoredBreakpoints::from_lists(&self.source, &self.functions, &self.exception_filters)
    }

    /// Put stored breakpoints back, in files of the workspace `base` (its scheme and authority).
    /// A breakpoint the registry already has (the same file, line and column; a function of the
    /// same name) is skipped, and an exception filter it has keeps its state, so restoring twice
    /// adds nothing. Returns the ids added and what to send.
    pub fn restore(
        &mut self,
        stored: &StoredBreakpoints,
        base: &UriComponents,
    ) -> (Vec<String>, Touched) {
        let mut ids = Vec::new();
        let mut touched = Touched::default();
        for b in &stored.breakpoints {
            if b.line_number == 0 || b.path.is_empty() {
                continue;
            }
            let uri = UriComponents::new(base.scheme.clone(), base.authority.clone(), &b.path);
            let exists = self
                .source
                .iter()
                .any(|s| s.uri == uri && s.line == b.line_number && s.column == b.column);
            if exists {
                continue;
            }
            if !touched.files.contains(&uri) {
                touched.files.push(uri.clone());
            }
            ids.push(self.add_source(
                None,
                uri,
                b.line_number,
                b.column,
                b.condition.clone(),
                b.hit_condition.clone(),
                b.log_message.clone(),
                b.enabled,
            ));
        }
        for f in &stored.function_breakpoints {
            if f.name.is_empty() || self.functions.iter().any(|b| b.name == f.name) {
                continue;
            }
            touched.functions = true;
            ids.push(self.add_function(
                None,
                f.name.clone(),
                f.condition.clone(),
                f.hit_condition.clone(),
                f.log_message.clone(),
                f.enabled,
            ));
        }
        for e in &stored.exception_breakpoints {
            if e.filter.is_empty() || self.exception_filters.iter().any(|f| f.filter == e.filter) {
                continue;
            }
            touched.exceptions = true;
            self.exception_filters.push(super::ExceptionFilter {
                filter: e.filter.clone(),
                label: if e.label.is_empty() {
                    e.filter.clone()
                } else {
                    e.label.clone()
                },
                enabled: e.enabled,
                supports_condition: e.supports_condition,
                condition: e.condition.clone().filter(|c| !c.is_empty()),
            });
        }
        (ids, touched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> UriComponents {
        UriComponents::new("vscode-remote", "host+1", "/w")
    }

    #[test]
    fn fr_l31_42_stored_breakpoints_round_trip_through_the_registry() {
        let mut bps = Breakpoints::default();
        let uri = UriComponents::new("vscode-remote", "old+0", "/w/a.py");
        bps.add_source(
            None,
            uri,
            3,
            Some(5),
            Some("x > 1".into()),
            Some("2".into()),
            Some("x={x}".into()),
            false,
        );
        bps.add_function(None, "main".into(), Some("y".into()), None, None, true);
        bps.set_filters(&json!([
            { "filter": "raised", "label": "Raised Exceptions", "default": false },
            { "filter": "uncaught", "label": "Uncaught Exceptions", "default": true, "supportsCondition": true },
        ]));
        bps.set_filter_enabled("raised", true);
        let stored = bps.to_stored();

        let mut back = Breakpoints::default();
        let (ids, touched) = back.restore(&stored, &base());
        assert_eq!(ids.len(), 2);
        let restored = &back.source[0];
        assert_eq!(restored.uri, UriComponents::new("vscode-remote", "host+1", "/w/a.py"));
        assert_eq!(
            (restored.line, restored.column, restored.enabled),
            (3, Some(5), false)
        );
        assert_eq!(restored.condition.as_deref(), Some("x > 1"));
        assert_eq!(restored.hit_condition.as_deref(), Some("2"));
        assert_eq!(restored.log_message.as_deref(), Some("x={x}"));
        assert!(!restored.verified, "what an adapter said is not kept");
        assert_eq!(back.functions[0].name, "main");
        assert_eq!(back.functions[0].condition.as_deref(), Some("y"));
        let chosen: Vec<(&str, bool)> = back
            .exception_filters
            .iter()
            .map(|f| (f.filter.as_str(), f.enabled))
            .collect();
        assert_eq!(chosen, [("raised", true), ("uncaught", true)]);
        assert_eq!(touched.files.len(), 1);
        assert!(touched.functions && touched.exceptions);
        assert_eq!(back.to_stored(), stored);

        // Restoring again adds nothing.
        let (ids, touched) = back.restore(&stored, &base());
        assert!(ids.is_empty() && touched.is_empty());
    }

    #[test]
    fn fr_l31_42_stored_breakpoints_use_code_oss_names_and_leave_out_empty_lists() {
        let stored = StoredBreakpoints {
            breakpoints: vec![StoredBreakpoint {
                path: "/w/a.py".into(),
                line_number: 7,
                column: None,
                enabled: true,
                condition: None,
                hit_condition: Some("3".into()),
                log_message: None,
            }],
            ..Default::default()
        };
        let value = serde_json::to_value(&stored).unwrap();
        assert_eq!(
            value,
            json!({ "breakpoints": [{ "path": "/w/a.py", "lineNumber": 7, "enabled": true, "hitCondition": "3" }] })
        );
        let empty: StoredBreakpoints = serde_json::from_value(json!({})).unwrap();
        assert!(empty.is_empty());
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
        // A filter re-offered by an adapter keeps the stored choice (`set_filters`).
        let mut bps = Breakpoints::default();
        bps.restore(
            &StoredBreakpoints {
                exception_breakpoints: vec![StoredExceptionBreakpoint {
                    filter: "raised".into(),
                    label: "Raised".into(),
                    enabled: true,
                    supports_condition: false,
                    condition: None,
                }],
                ..Default::default()
            },
            &base(),
        );
        bps.set_filters(&json!([{ "filter": "raised", "label": "Raised", "default": false }]));
        assert!(bps.exception_filters[0].enabled);
    }
}
