//! Where a symbol is defined or used, and the symbols of a document.
//!
//! `$provideDefinition` answers `ILocationLinkDto[]` (`{ originSelectionRange?, uri, range,
//! targetSelectionRange? }`), `$provideReferences` answers `ILocationDto[]` (`{ uri, range }`), and
//! `$provideDocumentSymbols` answers `DocumentSymbol[]` (`{ name, detail, kind, tags, range,
//! selectionRange, children? }`), all from `languages.ts`.

use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use crate::coords::{WidgetPos, WidgetRange};
use crate::language::edits::parse_range;

/// A place in a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub uri: UriComponents,
    /// The whole definition (or the reference itself).
    pub range: WidgetRange,
    /// `targetSelectionRange`: the name inside the definition, which is what gets selected.
    pub selection: Option<WidgetRange>,
    /// The text of the line, when the file is open in the session.
    pub preview: Option<String>,
}

impl Location {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            uri: serde_json::from_value(v.get("uri")?.clone()).ok()?,
            range: parse_range(v.get("range")?)?,
            selection: v.get("targetSelectionRange").and_then(parse_range),
            preview: None,
        })
    }

    /// The range to select when the reader goes there.
    pub fn target(&self) -> WidgetRange {
        self.selection.unwrap_or(self.range)
    }
}

/// A list of locations or links. A single location (older providers) counts as a list of one.
pub fn parse_locations(v: &Value) -> Vec<Location> {
    match v {
        Value::Array(a) => a.iter().filter_map(Location::from_json).collect(),
        Value::Object(_) => Location::from_json(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Several providers' answers as one list: provider order kept, the same place listed once.
pub fn merge_locations(lists: Vec<Vec<Location>>) -> Vec<Location> {
    let mut out: Vec<Location> = Vec::new();
    for location in lists.into_iter().flatten() {
        let key = location.uri.key();
        if !out.iter().any(|seen| seen.range == location.range && seen.uri.key() == key) {
            out.push(location);
        }
    }
    out
}

/// One symbol of a document and the symbols inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentSymbol {
    pub name: String,
    pub detail: String,
    /// `SymbolKind` as the protocol numbers it (0 File, 4 Class, 5 Method, 11 Function, 22 Struct, ...).
    pub kind: u32,
    pub range: WidgetRange,
    pub selection_range: WidgetRange,
    pub children: Vec<DocumentSymbol>,
}

impl DocumentSymbol {
    pub fn from_json(v: &Value) -> Option<Self> {
        let range = parse_range(v.get("range")?)?;
        Some(Self {
            name: v.get("name")?.as_str()?.to_owned(),
            detail: v.get("detail").and_then(Value::as_str).unwrap_or_default().to_owned(),
            kind: v.get("kind").and_then(Value::as_u64).unwrap_or(0) as u32,
            range,
            selection_range: v.get("selectionRange").and_then(parse_range).unwrap_or(range),
            children: v.get("children").map(parse_document_symbols).unwrap_or_default(),
        })
    }
}

/// A `DocumentSymbol[]` reply, in document order.
pub fn parse_document_symbols(v: &Value) -> Vec<DocumentSymbol> {
    let mut symbols: Vec<DocumentSymbol> = v.as_array().map(|a| a.iter().filter_map(DocumentSymbol::from_json).collect()).unwrap_or_default();
    symbols.sort_by(|a, b| a.range.start.cmp(&b.range.start));
    symbols
}

/// The symbols around `pos`, outermost first: what the breadcrumbs show after the file name.
pub fn symbol_path(symbols: &[DocumentSymbol], pos: WidgetPos) -> Vec<&DocumentSymbol> {
    let mut path = Vec::new();
    let mut level = symbols;
    while let Some(symbol) = level.iter().find(|s| s.range.contains(pos)) {
        path.push(symbol);
        level = &symbol.children;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uri(path: &str) -> Value {
        json!({"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": path})
    }

    fn irange(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
        json!({"startLineNumber": sl, "startColumn": sc, "endLineNumber": el, "endColumn": ec})
    }

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
        WidgetRange::new(WidgetPos::new(sl, sc), WidgetPos::new(el, ec))
    }

    #[test]
    fn fr_l31_13_location_links_and_plain_locations_decode() {
        let links = json!([{
            "originSelectionRange": irange(1, 1, 1, 4),
            "uri": uri("/w/lib.rs"),
            "range": irange(10, 1, 14, 2),
            "targetSelectionRange": irange(10, 4, 10, 8)
        }]);
        let found = parse_locations(&links);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].uri.path, "/w/lib.rs");
        assert_eq!(found[0].range, range(9, 0, 13, 1));
        assert_eq!(found[0].target(), range(9, 3, 9, 7));

        let plain = json!([{"uri": uri("/w/a.rs"), "range": irange(2, 3, 2, 6)}, {"range": irange(1, 1, 1, 1)}]);
        let found = parse_locations(&plain);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target(), range(1, 2, 1, 5));

        assert_eq!(parse_locations(&json!({"uri": uri("/w/a.rs"), "range": irange(1, 1, 1, 2)})).len(), 1);
        assert!(parse_locations(&Value::Null).is_empty());
    }

    #[test]
    fn fr_l31_13_locations_of_several_providers_merge_without_repeats() {
        let one = parse_locations(&json!([{"uri": uri("/w/a.rs"), "range": irange(2, 3, 2, 6)}]));
        let two = parse_locations(&json!([
            {"uri": uri("/w/a.rs"), "range": irange(2, 3, 2, 6)},
            {"uri": uri("/w/b.rs"), "range": irange(2, 3, 2, 6)}
        ]));
        let merged = merge_locations(vec![one, two]);
        assert_eq!(merged.iter().map(|l| l.uri.path.as_str()).collect::<Vec<_>>(), ["/w/a.rs", "/w/b.rs"]);
    }

    #[test]
    fn fr_l31_13_document_symbols_nest_and_give_the_path_at_a_position() {
        let v = json!([
            {"name": "tail", "detail": "", "kind": 11, "tags": [], "range": irange(20, 1, 22, 2), "selectionRange": irange(20, 4, 20, 8)},
            {"name": "Stack", "detail": "impl", "kind": 22, "tags": [], "range": irange(1, 1, 10, 2), "selectionRange": irange(1, 6, 1, 11),
             "children": [
                {"name": "push", "detail": "fn(&mut self)", "kind": 5, "tags": [], "range": irange(2, 5, 4, 6), "selectionRange": irange(2, 8, 2, 12)},
                {"name": "pop", "detail": "", "kind": 5, "tags": [], "range": irange(6, 5, 8, 6), "selectionRange": irange(6, 8, 6, 11)}
             ]}
        ]);
        let symbols = parse_document_symbols(&v);
        // Document order, whatever order the provider answered in.
        assert_eq!(symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Stack", "tail"]);
        assert_eq!(symbols[0].kind, 22);
        assert_eq!(symbols[0].children[0].selection_range, range(1, 7, 1, 11));
        let names = |line, col| symbol_path(&symbols, WidgetPos::new(line, col)).into_iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(6, 8), ["Stack", "pop"]);
        assert_eq!(names(4, 0), ["Stack"]);
        assert_eq!(names(20, 2), ["tail"]);
        assert!(names(15, 0).is_empty());
        assert!(parse_document_symbols(&Value::Null).is_empty());
    }
}
