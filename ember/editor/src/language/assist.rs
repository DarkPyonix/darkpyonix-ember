//! Code actions, signature help and the place a rename starts from.
//!
//! `ICodeActionListDto` is `{ cacheId, actions: ICodeActionDto[] }` (extHost.protocol.ts L2850) and
//! `ISignatureHelpDto` is `{ id, signatures, activeSignature, activeParameter }` (L2724), with
//! `SignatureInformation` and `ParameterInformation` from `languages.ts`. `RenameLocation` is
//! `{ range, text }`, or carries a `rejectReason`.

use serde_json::Value;

use crate::codelens::CommandDto;
use crate::coords::{byte_to_utf16, utf16_to_byte, WidgetPos, WidgetRange};
use crate::hover::MarkdownString;
use crate::language::edits::{parse_range, reject_reason, WorkspaceEdit};

/// One entry of the quick fix menu.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeAction {
    pub title: String,
    /// `quickfix`, `refactor.extract`, `source.organizeImports`, ...
    pub kind: Option<String>,
    pub preferred: bool,
    /// Why the action cannot be applied here; such an action is listed but not run.
    pub disabled: Option<String>,
    pub edit: Option<WorkspaceEdit>,
    pub command: Option<CommandDto>,
    /// The provider the action came from.
    pub handle: i64,
    /// What `$resolveCodeAction` takes.
    pub cache_id: Option<(i64, i64)>,
}

impl CodeAction {
    pub fn from_json(handle: i64, v: &Value) -> Option<Self> {
        Some(Self {
            title: v.get("title")?.as_str()?.to_owned(),
            kind: v.get("kind").and_then(Value::as_str).map(str::to_owned),
            preferred: v.get("isPreferred").and_then(Value::as_bool).unwrap_or(false),
            disabled: v.get("disabled").and_then(Value::as_str).map(str::to_owned),
            edit: v.get("edit").and_then(WorkspaceEdit::from_json),
            command: v.get("command").and_then(CommandDto::from_json),
            handle,
            cache_id: v.get("cacheId").and_then(Value::as_array).and_then(|a| Some((a.first()?.as_i64()?, a.get(1)?.as_i64()?))),
        })
    }
}

/// An `ICodeActionListDto`: the cache id to release and the actions. `undefined` gives neither.
pub fn parse_code_actions(handle: i64, v: &Value) -> (Option<i64>, Vec<CodeAction>) {
    let actions = v
        .get("actions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|action| CodeAction::from_json(handle, action)).collect())
        .unwrap_or_default();
    (v.get("cacheId").and_then(Value::as_i64), actions)
}

/// One parameter of a signature.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    /// The parameter's text.
    pub label: String,
    /// Where it is in the signature's label, as UTF-16 offsets (start, end), when the provider
    /// said so or the text is found there.
    pub span: Option<(u32, u32)>,
    pub documentation: Option<MarkdownString>,
}

/// One overload.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    pub label: String,
    pub documentation: Option<MarkdownString>,
    pub parameters: Vec<Parameter>,
    /// The signature's own active parameter, which wins over the help's.
    pub active_parameter: Option<usize>,
}

/// What the parameter hint shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureHelp {
    /// What `$releaseSignatureHelp` takes.
    pub id: Option<i64>,
    pub signatures: Vec<Signature>,
    pub active_signature: usize,
    pub active_parameter: usize,
    /// The DTO as received, to send back as `activeSignatureHelp` on a retrigger.
    pub raw: Value,
}

impl SignatureHelp {
    /// The overload to show and the index of the parameter to emphasise in it.
    pub fn active(&self) -> Option<(&Signature, Option<usize>)> {
        let signature = self.signatures.get(self.active_signature).or_else(|| self.signatures.first())?;
        let index = signature.active_parameter.unwrap_or(self.active_parameter);
        Some((signature, (index < signature.parameters.len()).then_some(index)))
    }
}

fn parse_parameter(signature_label: &str, v: &Value) -> Option<Parameter> {
    let documentation = v.get("documentation").and_then(MarkdownString::from_json);
    match v.get("label")? {
        Value::String(text) => {
            let span = signature_label.find(text.as_str()).and_then(|start| {
                Some((byte_to_utf16(signature_label, start)?, byte_to_utf16(signature_label, start + text.len())?))
            });
            Some(Parameter { label: text.clone(), span, documentation })
        }
        Value::Array(pair) => {
            let (start, end) = (pair.first()?.as_u64()? as u32, pair.get(1)?.as_u64()? as u32);
            let label = utf16_to_byte(signature_label, start)
                .zip(utf16_to_byte(signature_label, end))
                .and_then(|(a, b)| signature_label.get(a..b))
                .unwrap_or_default()
                .to_owned();
            Some(Parameter { label, span: Some((start, end)), documentation })
        }
        _ => None,
    }
}

/// An `ISignatureHelpDto`. `None` for `undefined` or a help without signatures.
pub fn parse_signature_help(v: &Value) -> Option<SignatureHelp> {
    let signatures: Vec<Signature> = v
        .get("signatures")?
        .as_array()?
        .iter()
        .filter_map(|s| {
            let label = s.get("label")?.as_str()?.to_owned();
            let parameters =
                s.get("parameters").and_then(Value::as_array).map(|p| p.iter().filter_map(|p| parse_parameter(&label, p)).collect()).unwrap_or_default();
            Some(Signature {
                documentation: s.get("documentation").and_then(MarkdownString::from_json),
                parameters,
                active_parameter: s.get("activeParameter").and_then(Value::as_u64).map(|n| n as usize),
                label,
            })
        })
        .collect();
    if signatures.is_empty() {
        return None;
    }
    Some(SignatureHelp {
        id: v.get("id").and_then(Value::as_i64),
        signatures,
        active_signature: v.get("activeSignature").and_then(Value::as_u64).unwrap_or(0) as usize,
        active_parameter: v.get("activeParameter").and_then(Value::as_u64).unwrap_or(0) as usize,
        raw: v.clone(),
    })
}

/// Where a rename starts: the range of the name and the text to offer in the box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameLocation {
    pub range: WidgetRange,
    pub text: String,
}

/// A `$resolveRenameLocation` reply: the location, `Ok(None)` when the provider has no opinion
/// (the word under the caret is used then), or the reason it refuses.
pub fn parse_rename_location(v: &Value) -> Result<Option<RenameLocation>, String> {
    if let Some(reason) = reject_reason(v) {
        return Err(reason);
    }
    Ok(v.get("range").and_then(parse_range).map(|range| RenameLocation {
        range,
        text: v.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
    }))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The word at or just before UTF-16 column `pos.col` of `line` (Monaco's `getWordAtPosition`
/// with the default word pattern, near enough): its range and text.
pub fn word_at(line: &str, pos: WidgetPos) -> Option<RenameLocation> {
    let at = utf16_to_byte(line, crate::coords::clamp_col(line, pos.col))?;
    let start = line[..at].char_indices().rev().take_while(|(_, c)| is_word_char(*c)).last().map_or(at, |(i, _)| i);
    let end = at + line[at..].char_indices().find(|(_, c)| !is_word_char(*c)).map_or(line.len() - at, |(i, _)| i);
    if start == end {
        return None;
    }
    let range = WidgetRange::new(WidgetPos::new(pos.line, byte_to_utf16(line, start)?), WidgetPos::new(pos.line, byte_to_utf16(line, end)?));
    Some(RenameLocation { range, text: line[start..end].to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn irange(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
        json!({"startLineNumber": sl, "startColumn": sc, "endLineNumber": el, "endColumn": ec})
    }

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
        WidgetRange::new(WidgetPos::new(sl, sc), WidgetPos::new(el, ec))
    }

    #[test]
    fn fr_l31_13_code_action_list_decodes() {
        let uri = json!({"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/w/a.rs"});
        let v = json!({"cacheId": 5, "actions": [
            {"cacheId": [5, 0], "title": "Import `HashMap`", "kind": "quickfix", "isPreferred": true,
             "edit": {"edits": [{"resource": uri, "textEdit": {"range": irange(1, 1, 1, 1), "text": "use std::collections::HashMap;\n"}}]}},
            {"cacheId": [5, 1], "title": "Extract into function", "kind": "refactor.extract", "disabled": "Select an expression"},
            {"cacheId": [5, 2], "title": "Run clippy fix", "command": {"id": "rust.fix", "title": "Fix", "arguments": [1]}},
            {"kind": "no title"}
        ]});
        let (cache, actions) = parse_code_actions(3, &v);
        assert_eq!(cache, Some(5));
        assert_eq!(actions.len(), 3);
        assert_eq!((actions[0].title.as_str(), actions[0].kind.as_deref(), actions[0].preferred), ("Import `HashMap`", Some("quickfix"), true));
        assert_eq!(actions[0].edit.as_ref().unwrap().files[0].1[0].text, "use std::collections::HashMap;\n");
        assert_eq!((actions[0].handle, actions[0].cache_id), (3, Some((5, 0))));
        assert_eq!(actions[1].disabled.as_deref(), Some("Select an expression"));
        assert!(actions[1].edit.is_none() && actions[1].command.is_none());
        assert_eq!(actions[2].command.as_ref().unwrap().id, "rust.fix");
        assert_eq!(parse_code_actions(3, &Value::Null), (None, Vec::new()));
    }

    #[test]
    fn fr_l31_13_signature_help_decodes_string_and_offset_parameters() {
        let v = json!({"id": 2, "activeSignature": 1, "activeParameter": 1, "signatures": [
            {"label": "new()", "parameters": []},
            {"label": "insert(&mut self, key: K, value: V)", "documentation": {"value": "Inserts a pair."},
             "parameters": [{"label": "key: K", "documentation": "The key."}, {"label": [26, 34]}]}
        ]});
        let help = parse_signature_help(&v).unwrap();
        assert_eq!(help.id, Some(2));
        assert_eq!(help.raw, v);
        let (signature, active) = help.active().unwrap();
        assert_eq!(signature.label, "insert(&mut self, key: K, value: V)");
        assert_eq!(signature.documentation.as_ref().unwrap().value, "Inserts a pair.");
        assert_eq!(signature.parameters[0].span, Some((18, 24)));
        assert_eq!(signature.parameters[0].documentation.as_ref().unwrap().value, "The key.");
        assert_eq!((signature.parameters[1].label.as_str(), signature.parameters[1].span), ("value: V", Some((26, 34))));
        assert_eq!(active, Some(1));
        assert!(parse_signature_help(&Value::Null).is_none());
        assert!(parse_signature_help(&json!({"id": 1, "signatures": [], "activeSignature": 0, "activeParameter": 0})).is_none());
    }

    #[test]
    fn fr_l31_13_signature_active_parameter_prefers_the_signatures_own_and_stays_in_range() {
        let v = json!({"id": 1, "activeSignature": 7, "activeParameter": 5, "signatures": [
            {"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": "b"}], "activeParameter": 0}
        ]});
        let help = parse_signature_help(&v).unwrap();
        // An active signature past the list falls back to the first one.
        assert_eq!(help.active().unwrap().1, Some(0));
        let v = json!({"id": 1, "activeSignature": 0, "activeParameter": 5, "signatures": [{"label": "f(a)", "parameters": [{"label": "a"}]}]});
        assert_eq!(parse_signature_help(&v).unwrap().active().unwrap().1, None);
    }

    #[test]
    fn fr_l31_13_rename_location_decodes_range_rejection_and_no_opinion() {
        let ok = parse_rename_location(&json!({"range": irange(3, 5, 3, 9), "text": "name"})).unwrap().unwrap();
        assert_eq!(ok, RenameLocation { range: range(2, 4, 2, 8), text: "name".into() });
        assert_eq!(parse_rename_location(&json!({"rejectReason": "You cannot rename this element."})), Err("You cannot rename this element.".to_string()));
        assert_eq!(parse_rename_location(&Value::Null), Ok(None));
    }

    #[test]
    fn fr_l31_13_word_at_the_caret() {
        let line = "let total_count = items.len();";
        let word = |col| word_at(line, WidgetPos::new(4, col)).map(|w| (w.text, w.range.start.col, w.range.end.col));
        assert_eq!(word(6), Some(("total_count".to_string(), 4, 15)));
        // Just after the word still counts, as it does in Monaco.
        assert_eq!(word(15), Some(("total_count".to_string(), 4, 15)));
        assert_eq!(word(4), Some(("total_count".to_string(), 4, 15)));
        assert_eq!(word(17), None);
        assert_eq!(word_at("가격 = 1", WidgetPos::new(0, 1)).map(|w| w.text), Some("가격".to_string()));
        assert_eq!(word_at("", WidgetPos::new(0, 0)), None);
    }
}
