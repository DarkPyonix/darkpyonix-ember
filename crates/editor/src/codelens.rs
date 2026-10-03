//! FR-E3: CodeLens → line-above inlay decorations.
//!
//! Protocol (extHost.protocol.ts at the pinned commit):
//!
//! * `$provideCodeLenses(handle, uri, token)` → `ICodeLensListDto | undefined`
//!   = `{ cacheId?: number, lenses: [{ cacheId?: [number, number], range: IRange, command?: ICommandDto }] }`.
//! * `$resolveCodeLens(handle, lens, token)` → `ICodeLensDto | undefined` for lenses that came
//!   without a `command` (pass the lens back exactly as received, `cacheId` included).
//! * `$releaseCodeLenses(handle, cacheId)` once a list is replaced or its document closes
//!   (upstream: `CodeLensList.dispose`, `mainThreadLanguageFeatures.ts` L193-201); otherwise the
//!   extension host keeps every list.
//! * `ICommandDto = { $ident?, id, title, tooltip?, arguments? }`. For a command with arguments
//!   the extension host sends its internal delegating command (`__vsc<uuid>`, registered through
//!   `$registerCommand`) with `arguments: ["<cmd> /<n>"]`; executing it with
//!   `$executeContributedCommand(id, ...arguments)` runs the extension's command with its real
//!   arguments (`extHostCommands.ts` `CommandsConverter.toInternal` L367-410).
//!
//! Each lens with a command becomes one [`DecorationKind::CodeLens`] decoration at its range with
//! the command title as text. Lenses without a command are resolved first (upstream resolves the
//! visible ones lazily; FR-38 has no viewport event, so Ember resolves up to a cap eagerly).
//! Ids are keyed by provider + line + ordinal on that line, fixed when the list arrives, so a lens
//! keeps its id when it is resolved and when the same list is provided again.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::coords::{clamp_range, transform_range, WidgetRange};
use crate::ids::{DecorationIds, IdSpace};
use crate::widget::{Decoration, DecorationKind};

/// `ICommandDto` (`languages.Command` + `$ident`).
#[derive(Debug, Clone, PartialEq)]
pub struct CommandDto {
    pub id: String,
    pub title: String,
    pub tooltip: Option<String>,
    pub arguments: Vec<Value>,
}

impl CommandDto {
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = v.get("id")?.as_str()?.to_owned();
        Some(Self {
            id,
            title: v.get("title").and_then(Value::as_str).unwrap_or_default().to_owned(),
            tooltip: v.get("tooltip").and_then(Value::as_str).map(str::to_owned),
            arguments: v.get("arguments").and_then(Value::as_array).cloned().unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lens {
    /// The lens as received (passed back to `$resolveCodeLens`).
    pub dto: Value,
    pub range: WidgetRange,
    pub command: Option<CommandDto>,
    /// Stable id key, fixed when the list arrived.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LensList {
    pub handle: i64,
    /// `ICodeLensListDto.cacheId`, for `$releaseCodeLenses`.
    pub cache_id: Option<i64>,
    /// Increases with every list from this provider for this document; resolve replies for an
    /// older serial are stale.
    pub serial: u64,
    pub lenses: Vec<Lens>,
}

/// Result of storing a new list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SetOutcome {
    /// Cache id of the list it replaced, to release.
    pub release: Option<i64>,
    /// `(index, lens dto)` of lenses that need `$resolveCodeLens`.
    pub to_resolve: Vec<(usize, Value)>,
    pub serial: u64,
}

#[derive(Debug, Default)]
pub struct CodeLenses {
    /// doc key → provider handle → list
    by_doc: HashMap<String, BTreeMap<i64, LensList>>,
    /// decoration id → (doc, handle, index)
    by_id: HashMap<u64, (String, i64, usize)>,
    serial: u64,
}

impl CodeLenses {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a `$provideCodeLenses` reply (`Value::Null` = provider returned nothing).
    /// `resolve_cap` bounds how many unresolved lenses are returned for resolving.
    pub fn set_list(&mut self, doc: &str, handle: i64, reply: &Value, resolve_cap: usize) -> SetOutcome {
        self.serial += 1;
        let serial = self.serial;
        let mut lenses = Vec::new();
        let mut per_line: HashMap<u32, u32> = HashMap::new();
        for dto in reply.get("lenses").and_then(Value::as_array).into_iter().flatten() {
            let Some(range) = dto.get("range").and_then(|r| serde_json::from_value(r.clone()).ok()) else { continue };
            let range = WidgetRange::from_exthost(range);
            let n = per_line.entry(range.start.line).or_insert(0);
            let key = format!("{handle}|{}|{n}", range.start.line);
            *n += 1;
            let command = dto.get("command").and_then(CommandDto::from_json);
            lenses.push(Lens { dto: dto.clone(), range, command, key });
        }
        let to_resolve = lenses
            .iter()
            .enumerate()
            .filter(|(_, l)| l.command.is_none())
            .take(resolve_cap)
            .map(|(i, l)| (i, l.dto.clone()))
            .collect();
        let list = LensList { handle, cache_id: reply.get("cacheId").and_then(Value::as_i64), serial, lenses };
        let release = self.by_doc.entry(doc.to_owned()).or_default().insert(handle, list).and_then(|old| old.cache_id);
        SetOutcome { release, to_resolve, serial }
    }

    /// Store a `$resolveCodeLens` reply. Returns whether anything changed.
    pub fn resolved(&mut self, doc: &str, handle: i64, serial: u64, index: usize, reply: &Value) -> bool {
        let Some(list) = self.by_doc.get_mut(doc).and_then(|m| m.get_mut(&handle)) else { return false };
        if list.serial != serial {
            return false;
        }
        let Some(lens) = list.lenses.get_mut(index) else { return false };
        let Some(command) = reply.get("command").and_then(CommandDto::from_json) else { return false };
        lens.command = Some(command);
        true
    }

    /// Carry lens ranges through an edit; a lens whose range was deleted disappears.
    pub fn on_edit(&mut self, doc: &str, edit: WidgetRange, text: &str) {
        let Some(lists) = self.by_doc.get_mut(doc) else { return };
        for list in lists.values_mut() {
            for lens in list.lenses.iter_mut() {
                // Keep deleted lenses in place (so resolve indices stay valid) but empty their
                // range; `decorations` skips them.
                match transform_range(lens.range, edit, text) {
                    Some(r) => lens.range = r,
                    None => lens.command = None,
                }
            }
        }
    }

    pub fn decorations(&mut self, doc: &str, version: u32, lines: &[String], ids: &mut DecorationIds) -> Vec<Decoration> {
        let mut out = Vec::new();
        let mut live = HashSet::new();
        self.by_id.retain(|_, (d, _, _)| d.as_str() != doc);
        if let Some(lists) = self.by_doc.get(doc) {
            for (handle, list) in lists {
                for (i, lens) in list.lenses.iter().enumerate() {
                    let Some(cmd) = &lens.command else { continue };
                    if cmd.title.is_empty() {
                        continue;
                    }
                    let id = ids.id(IdSpace::CodeLens, doc, &lens.key);
                    live.insert(id.get());
                    self.by_id.insert(id.get(), (doc.to_owned(), *handle, i));
                    out.push(Decoration {
                        id,
                        version,
                        kind: DecorationKind::CodeLens,
                        severity: None,
                        color: 0,
                        range: clamp_range(lines, lens.range),
                        text: cmd.title.clone(),
                    });
                }
            }
        }
        ids.retain(IdSpace::CodeLens, doc, &live);
        out.sort_by(|a, b| a.range.cmp(&b.range).then(a.id.cmp(&b.id)));
        out
    }

    /// The command behind a clicked CodeLens decoration.
    pub fn command_for(&self, id: u64) -> Option<(i64, CommandDto)> {
        let (doc, handle, i) = self.by_id.get(&id)?;
        let lens = self.by_doc.get(doc)?.get(handle)?.lenses.get(*i)?;
        lens.command.clone().map(|c| (*handle, c))
    }

    /// Drop a provider's lists everywhere. Returns `(doc, cache_id)` to release.
    pub fn remove_provider(&mut self, handle: i64) -> Vec<(String, i64)> {
        let mut out = Vec::new();
        for (doc, lists) in self.by_doc.iter_mut() {
            if let Some(LensList { cache_id: Some(c), .. }) = lists.remove(&handle) {
                out.push((doc.clone(), c));
            }
        }
        out
    }

    /// Drop a document's lists. Returns `(handle, cache_id)` to release.
    pub fn close(&mut self, doc: &str) -> Vec<(i64, i64)> {
        self.by_id.retain(|_, (d, _, _)| d.as_str() != doc);
        self.by_doc
            .remove(doc)
            .map(|lists| lists.into_values().filter_map(|l| l.cache_id.map(|c| (l.handle, c))).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::WidgetPos;
    use serde_json::json;

    fn range(l: u32) -> Value {
        json!({"startLineNumber": l, "startColumn": 1, "endLineNumber": l, "endColumn": 5})
    }

    fn lines() -> Vec<String> {
        (0..10).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn list_with_resolved_and_unresolved_lenses() {
        let mut c = CodeLenses::new();
        let mut ids = DecorationIds::new();
        let reply = json!({"cacheId": 3, "lenses": [
            {"cacheId": [3, 0], "range": range(2), "command": {"id": "__vsc1", "title": "2 references", "arguments": ["refs /1"]}},
            {"cacheId": [3, 1], "range": range(5)}
        ]});
        let out = c.set_list("d", 7, &reply, 10);
        assert_eq!(out.release, None);
        assert_eq!(out.to_resolve.len(), 1);
        assert_eq!(out.to_resolve[0].0, 1);
        assert_eq!(out.to_resolve[0].1["cacheId"], json!([3, 1]));

        let decs = c.decorations("d", 0, &lines(), &mut ids);
        assert_eq!(decs.len(), 1);
        assert_eq!(decs[0].kind, DecorationKind::CodeLens);
        assert_eq!(decs[0].text, "2 references");
        assert_eq!(decs[0].range.start, WidgetPos::new(1, 0));
        let first_id = decs[0].id;

        // Resolve the second; the first keeps its id.
        assert!(c.resolved("d", 7, out.serial, 1, &json!({"range": range(5), "command": {"id": "run", "title": "Run"}})));
        let decs = c.decorations("d", 0, &lines(), &mut ids);
        assert_eq!(decs.len(), 2);
        assert_eq!(decs[0].id, first_id);
        let (handle, cmd) = c.command_for(decs[1].id.get()).unwrap();
        assert_eq!((handle, cmd.id.as_str(), cmd.title.as_str()), (7, "run", "Run"));
        let (_, cmd) = c.command_for(first_id.get()).unwrap();
        assert_eq!(cmd.arguments, vec![json!("refs /1")]);
    }

    #[test]
    fn replacing_a_list_releases_the_old_one_and_keeps_ids_for_same_positions() {
        let mut c = CodeLenses::new();
        let mut ids = DecorationIds::new();
        let list = |cache: i64| json!({"cacheId": cache, "lenses": [{"range": range(2), "command": {"id": "x", "title": "T"}}]});
        let first = c.set_list("d", 7, &list(1), 10);
        let a = c.decorations("d", 0, &lines(), &mut ids)[0].id;
        let second = c.set_list("d", 7, &list(2), 10);
        assert_eq!(second.release, Some(1));
        assert!(second.serial > first.serial);
        // Stale resolve replies for the old list are ignored.
        assert!(!c.resolved("d", 7, first.serial, 0, &json!({"command": {"id": "y", "title": "U"}})));
        let b = c.decorations("d", 1, &lines(), &mut ids)[0].id;
        assert_eq!(a, b);
        assert_eq!(c.close("d"), vec![(7, 2)]);
        assert!(c.command_for(b.get()).is_none());
    }

    #[test]
    fn lenses_follow_edits_and_vanish_with_their_line() {
        let mut c = CodeLenses::new();
        let mut ids = DecorationIds::new();
        c.set_list("d", 1, &json!({"lenses": [{"range": range(3), "command": {"id": "x", "title": "T"}}]}), 10);
        c.on_edit("d", WidgetRange::empty(WidgetPos::new(0, 0)), "a\nb\n");
        assert_eq!(c.decorations("d", 1, &lines(), &mut ids)[0].range.start, WidgetPos::new(4, 0));
        c.on_edit("d", WidgetRange::new(WidgetPos::new(4, 0), WidgetPos::new(4, 6)), "");
        assert!(c.decorations("d", 2, &lines(), &mut ids).is_empty());
        assert_eq!(c.remove_provider(1), vec![]);
    }
}
