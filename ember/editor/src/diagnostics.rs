//! FR-E2: diagnostics → squiggle decorations.
//!
//! The extension host reports markers with `MainThreadDiagnostics.$changeMany(owner, [uri,
//! markers | undefined][])` and `$clear(owner)` (extHost.protocol.ts L254-257). Upstream the
//! `MarkerService` keeps them per (owner, resource) and the editor draws one decoration per marker
//! (`markerDecorationsService.ts`), validating the range against the current model.
//!
//! Here: markers are kept per document and owner in widget coordinates; each becomes one
//! [`DecorationKind::Underline`] with the severity mapped 8/4/2/1 → Error/Warning/Information/Hint.
//! Between `$changeMany` calls the session carries the stored ranges through every edit
//! ([`Diagnostics::on_edit`]) exactly as the widget moves the drawn ones, so a hover lookup by
//! position finds what the user sees.
//!
//! Decoration ids are keyed by owner + severity + source + code + message + ordinal among identical
//! markers, not by range: a diagnostic that only moved keeps its id.

use std::collections::{BTreeMap, HashMap, HashSet};

use ember_editor_conn::exthost::MarkerData;
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use crate::coords::{clamp_range, transform_range, utf16_len, WidgetPos, WidgetRange};
use crate::ids::{DecorationIds, IdSpace};
use crate::widget::{Decoration, DecorationKind, Severity};

#[derive(Debug, Clone, PartialEq)]
pub struct TrackedMarker {
    pub marker: MarkerData,
    /// Current position in widget coordinates.
    pub range: WidgetRange,
}

/// One entry of the problems list: a marker of some file, in widget coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub uri: UriComponents,
    pub owner: String,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    pub range: WidgetRange,
}

/// What a hover shows for a diagnostic under the pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverDiagnostic {
    pub owner: String,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Default)]
pub struct Diagnostics {
    /// uri key → owner → markers
    by_doc: HashMap<String, BTreeMap<String, Vec<TrackedMarker>>>,
    /// uri key → the uri, for the problems list.
    uris: HashMap<String, UriComponents>,
    /// decoration id → (doc key, clamped range) from the latest `decorations` call
    by_id: HashMap<u64, (String, WidgetRange)>,
}

fn code_string(code: &Option<Value>) -> Option<String> {
    match code {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        // `{ value, target }` (a code with a link).
        Some(Value::Object(o)) => o.get("value").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())),
        Some(other) => Some(other.to_string()),
    }
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    /// `$changeMany`. Returns the uri keys whose markers changed.
    pub fn change_many(&mut self, owner: &str, entries: Vec<(UriComponents, Option<Vec<MarkerData>>)>) -> Vec<String> {
        let mut touched = Vec::new();
        for (uri, markers) in entries {
            let key = uri.key();
            self.uris.insert(key.clone(), uri.clone());
            let doc = self.by_doc.entry(key.clone()).or_default();
            match markers {
                Some(ms) if !ms.is_empty() => {
                    let tracked = ms
                        .into_iter()
                        .map(|m| {
                            let range = WidgetRange::from_exthost(ember_editor_conn::exthost::Range {
                                start_line_number: m.start_line_number,
                                start_column: m.start_column,
                                end_line_number: m.end_line_number,
                                end_column: m.end_column,
                            });
                            TrackedMarker { marker: m, range }
                        })
                        .collect();
                    doc.insert(owner.to_owned(), tracked);
                }
                _ => {
                    doc.remove(owner);
                }
            }
            if doc.is_empty() {
                self.by_doc.remove(&key);
                self.uris.remove(&key);
            }
            touched.push(key);
        }
        touched
    }

    /// Every marker of every file as a problem: errors first, then warnings, information and
    /// hints, each group by file path and position.
    pub fn problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        for (key, owners) in &self.by_doc {
            let Some(uri) = self.uris.get(key) else { continue };
            for (owner, markers) in owners {
                for t in markers {
                    out.push(Problem {
                        uri: uri.clone(),
                        owner: owner.clone(),
                        severity: Severity::from_marker(t.marker.severity),
                        message: t.marker.message.clone(),
                        source: t.marker.source.clone(),
                        code: code_string(&t.marker.code),
                        range: t.range,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            (a.severity as u8, &a.uri.path, a.range.start.line, a.range.start.col, &a.message)
                .cmp(&(b.severity as u8, &b.uri.path, b.range.start.line, b.range.start.col, &b.message))
        });
        out
    }

    /// `$clear(owner)`. Returns the uri keys that had markers from `owner`.
    pub fn clear(&mut self, owner: &str) -> Vec<String> {
        let mut touched = Vec::new();
        for (key, doc) in self.by_doc.iter_mut() {
            if doc.remove(owner).is_some() {
                touched.push(key.clone());
            }
        }
        self.by_doc.retain(|_, d| !d.is_empty());
        let by_doc = &self.by_doc;
        self.uris.retain(|key, _| by_doc.contains_key(key));
        touched
    }

    /// Remove all markers recorded for a document key (e.g. when a file closes).
    /// Returns true if markers were present for this document.
    pub fn remove_document(&mut self, key: &str) -> bool {
        self.uris.remove(key);
        let removed = self.by_doc.remove(key).is_some();
        if removed {
            self.by_id.retain(|_, (doc, _)| doc != key);
        }
        removed
    }

    /// Carry every marker of a document through one widget edit; markers whose range was deleted
    /// disappear (as their squiggles do in the widget).
    pub fn on_edit(&mut self, doc: &str, edit: WidgetRange, text: &str) {
        if let Some(owners) = self.by_doc.get_mut(doc) {
            for markers in owners.values_mut() {
                markers.retain_mut(|t| match transform_range(t.range, edit, text) {
                    Some(r) => {
                        t.range = r;
                        true
                    }
                    None => false,
                });
            }
        }
    }

    /// Every marker of a document with its owner, owners in name order.
    pub fn markers(&self, doc: &str) -> impl Iterator<Item = (&str, &TrackedMarker)> {
        self.by_doc
            .get(doc)
            .into_iter()
            .flat_map(|owners| owners.iter().flat_map(|(o, ms)| ms.iter().map(move |m| (o.as_str(), m))))
    }

    /// Underline decorations for a document at widget `version`. Ranges are clamped to the text;
    /// an empty range is widened to one character, like Monaco does for zero-width markers.
    pub fn decorations(&mut self, doc: &str, version: u32, lines: &[String], ids: &mut DecorationIds) -> Vec<Decoration> {
        let mut out = Vec::new();
        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut live = HashSet::new();
        self.by_id.retain(|_, (d, _)| d.as_str() != doc);
        let markers: Vec<(String, TrackedMarker)> = self.markers(doc).map(|(o, t)| (o.to_owned(), t.clone())).collect();
        for (owner, t) in markers {
            let m = &t.marker;
            let base = format!(
                "{owner}|{}|{}|{}|{}",
                m.severity,
                m.source.as_deref().unwrap_or(""),
                code_string(&m.code).unwrap_or_default(),
                m.message
            );
            let n = seen.entry(base.clone()).or_insert(0);
            let key = format!("{base}|{n}");
            *n += 1;
            let id = ids.id(IdSpace::Diagnostic, doc, &key);
            live.insert(id.get());
            let mut range = clamp_range(lines, t.range);
            if range.is_empty() {
                if let Some(line) = lines.get(range.start.line as usize) {
                    let len = utf16_len(line);
                    if range.start.col < len {
                        // Widen to cover the next character (2 units for a surrogate pair).
                        let width = crate::coords::utf16_to_byte(line, range.start.col)
                            .and_then(|b| line[b..].chars().next())
                            .map_or(1, |c| c.len_utf16() as u32);
                        range.end = WidgetPos { line: range.start.line, col: (range.start.col + width).min(len) };
                    } else if range.start.col > 0 {
                        // At the end of a line: cover the last character instead.
                        range.start.col = crate::coords::clamp_col(line, range.start.col - 1);
                    }
                }
            }
            self.by_id.insert(id.get(), (doc.to_owned(), range));
            out.push(Decoration {
                id,
                version,
                kind: DecorationKind::Underline,
                severity: Some(Severity::from_marker(m.severity)),
                color: 0,
                range,
                text: String::new(),
            });
        }
        ids.retain(IdSpace::Diagnostic, doc, &live);
        out.sort_by(|a, b| a.range.cmp(&b.range).then(a.id.cmp(&b.id)));
        out
    }

    /// Look up the range of a diagnostic decoration by its id in `doc`.
    pub fn range_of_id(&self, doc: &str, id: u64, lines: &[String], ids: &mut DecorationIds) -> Option<WidgetRange> {
        if let Some((d, r)) = self.by_id.get(&id) {
            if d == doc {
                return Some(*r);
            }
        }
        let mut seen: HashMap<String, u32> = HashMap::new();
        for (owner, t) in self.markers(doc) {
            let m = &t.marker;
            let base = format!(
                "{owner}|{}|{}|{}|{}",
                m.severity,
                m.source.as_deref().unwrap_or(""),
                code_string(&m.code).unwrap_or_default(),
                m.message
            );
            let n = seen.entry(base.clone()).or_insert(0);
            let key = format!("{base}|{n}");
            *n += 1;
            if ids.id(IdSpace::Diagnostic, doc, &key).get() == id {
                return Some(clamp_range(lines, t.range));
            }
        }
        None
    }

    /// Diagnostics under a position, most severe first (what a hover shows above provider content).
    pub fn at(&self, doc: &str, pos: WidgetPos) -> Vec<HoverDiagnostic> {
        let mut v: Vec<HoverDiagnostic> = self
            .markers(doc)
            .filter(|(_, t)| t.range.contains(pos))
            .map(|(owner, t)| HoverDiagnostic {
                owner: owner.to_owned(),
                severity: Severity::from_marker(t.marker.severity),
                message: t.marker.message.clone(),
                source: t.marker.source.clone(),
                code: code_string(&t.marker.code),
            })
            .collect();
        v.sort_by_key(|d| d.severity);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn marker(sev: u8, msg: &str, l: u32, c1: u32, c2: u32) -> MarkerData {
        serde_json::from_value(json!({
            "severity": sev, "message": msg, "source": "json",
            "startLineNumber": l, "startColumn": c1, "endLineNumber": l, "endColumn": c2
        }))
        .unwrap()
    }

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_owned).collect()
    }

    #[test]
    fn change_many_and_clear() {
        let mut d = Diagnostics::new();
        let a = UriComponents::remote("h", "/w/a.json");
        let b = UriComponents::remote("h", "/w/b.json");
        let touched = d.change_many("json", vec![(a.clone(), Some(vec![marker(8, "x", 1, 1, 2)])), (b.clone(), None)]);
        assert_eq!(touched, vec![a.key(), b.key()]);
        assert_eq!(d.markers(&a.key()).count(), 1);
        d.change_many("other", vec![(a.clone(), Some(vec![marker(4, "y", 1, 1, 2)]))]);
        assert_eq!(d.markers(&a.key()).count(), 2);
        assert_eq!(d.clear("json"), vec![a.key()]);
        assert_eq!(d.markers(&a.key()).count(), 1);
        d.change_many("other", vec![(a.clone(), Some(vec![]))]);
        assert_eq!(d.markers(&a.key()).count(), 0);
    }

    #[test]
    fn decorations_map_severity_and_zero_based_ranges() {
        let mut d = Diagnostics::new();
        let mut ids = DecorationIds::new();
        let a = UriComponents::remote("h", "/w/a.json");
        d.change_many(
            "json",
            vec![(a.clone(), Some(vec![marker(8, "Expected comma", 3, 13, 14), marker(4, "w", 1, 1, 2), marker(1, "h", 2, 1, 1)]))],
        );
        let text = lines("{\n  \"name\": \"ember\",\n  \"ok\": true,,\n}\n");
        let decs = d.decorations(&a.key(), 1, &text, &mut ids);
        assert_eq!(decs.len(), 3);
        assert_eq!(decs[0].severity, Some(Severity::Warning));
        assert_eq!(decs[0].range, WidgetRange::new(WidgetPos::new(0, 0), WidgetPos::new(0, 1)));
        // zero-width hint widened to one character
        assert_eq!(decs[1].severity, Some(Severity::Hint));
        assert_eq!(decs[1].range, WidgetRange::new(WidgetPos::new(1, 0), WidgetPos::new(1, 1)));
        assert_eq!(decs[2].severity, Some(Severity::Error));
        assert_eq!(decs[2].range, WidgetRange::new(WidgetPos::new(2, 12), WidgetPos::new(2, 13)));
        assert!(decs.iter().all(|x| x.kind == DecorationKind::Underline && x.version == 1 && x.id.get() != 0));
    }

    #[test]
    fn ids_are_stable_when_markers_move_and_distinct_for_duplicates() {
        let mut d = Diagnostics::new();
        let mut ids = DecorationIds::new();
        let a = UriComponents::remote("h", "/w/a.json");
        let text = lines("aaaa\nbbbb\ncccc");
        d.change_many("json", vec![(a.clone(), Some(vec![marker(8, "dup", 1, 1, 2), marker(8, "dup", 2, 1, 2)]))]);
        let first = d.decorations(&a.key(), 0, &text, &mut ids);
        assert_ne!(first[0].id, first[1].id);
        // Re-published one line lower (user inserted a line above): same ids.
        d.change_many("json", vec![(a.clone(), Some(vec![marker(8, "dup", 2, 1, 2), marker(8, "dup", 3, 1, 2)]))]);
        let second = d.decorations(&a.key(), 1, &text, &mut ids);
        assert_eq!(first.iter().map(|x| x.id).collect::<Vec<_>>(), second.iter().map(|x| x.id).collect::<Vec<_>>());
        // A different message is a different decoration.
        d.change_many("json", vec![(a.clone(), Some(vec![marker(8, "other", 1, 1, 2)]))]);
        let third = d.decorations(&a.key(), 2, &text, &mut ids);
        assert!(!first.iter().any(|x| x.id == third[0].id));
    }

    #[test]
    fn markers_follow_edits_and_hover_finds_them() {
        let mut d = Diagnostics::new();
        let a = UriComponents::remote("h", "/w/a.json");
        d.change_many("json", vec![(a.clone(), Some(vec![marker(8, "bad", 2, 3, 6)]))]);
        let k = a.key();
        assert_eq!(d.at(&k, WidgetPos::new(1, 3)).len(), 1);
        // A new line above moves it down.
        d.on_edit(&k, WidgetRange::empty(WidgetPos::new(0, 0)), "\n");
        assert!(d.at(&k, WidgetPos::new(1, 3)).is_empty());
        let hit = d.at(&k, WidgetPos::new(2, 3));
        assert_eq!(hit[0].message, "bad");
        assert_eq!(hit[0].severity, Severity::Error);
        // Deleting its text removes it.
        d.on_edit(&k, WidgetRange::new(WidgetPos::new(2, 0), WidgetPos::new(2, 9)), "");
        assert_eq!(d.markers(&k).count(), 0);
    }

    #[test]
    fn remove_document_clears_markers_and_updates_problems() {
        let mut d = Diagnostics::new();
        let a = UriComponents::remote("h", "/w/a.json");
        let b = UriComponents::remote("h", "/w/b.json");
        d.change_many("json", vec![
            (a.clone(), Some(vec![marker(8, "error in a", 1, 1, 2)])),
            (b.clone(), Some(vec![marker(4, "warn in b", 2, 1, 2)])),
        ]);
        assert_eq!(d.problems().len(), 2);

        // Remove document a: only b remains
        assert!(d.remove_document(&a.key()));
        let problems = d.problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].uri.path, "/w/b.json");

        // Removing non-existent returns false
        assert!(!d.remove_document(&a.key()));
    }
}
