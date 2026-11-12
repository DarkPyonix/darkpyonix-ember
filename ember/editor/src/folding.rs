//! Code folding ranges (FR-E6, #441): indentation-based fold computation (`IndentRangeProvider`
//! port from Code-OSS `indentRangeProvider.ts`) and extension-host folding range provider
//! merging (`$provideFoldingRanges`).
//!
//! `CodeEditorModel.setFolds` requires:
//! - `0 <= start_line < end_line < line_count`
//! - At most one fold starting on any `start_line` (`"two folds start on line $first"`).
//!
//! Therefore [`merge_folds`] deduplicates by `start_line` (provider ranges take precedence over
//! indentation ranges on the same `start_line`), sorts by `start_line`, and caps at
//! [`MAX_FOLD_REGIONS`].

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::coords::WidgetRange;

/// Maximum number of fold regions kept for a single document (Code-OSS default `foldingMaximumRegions` is 5000).
pub const MAX_FOLD_REGIONS: usize = 5000;

/// One foldable line range (`0 <= start_line < end_line < line_count`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FoldRange {
    pub start_line: u32,
    pub end_line: u32,
}

/// One fold region with its document version and collapsed state, matching `compose_rust::code::Fold`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FoldRegion {
    pub version: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub collapsed: bool,
}

/// Computes the visual column of the first non-whitespace character on `line`, or `None` if the
/// line is blank or whitespace-only. Tabs advance to the next multiple of `tab_width`.
pub fn compute_indent_level(line: &str, tab_width: u32) -> Option<u32> {
    let tab = tab_width.max(1);
    let mut indent = 0u32;
    for b in line.bytes() {
        match b {
            b' ' => indent = indent.saturating_add(1),
            b'\t' => indent = indent.saturating_add(tab - (indent % tab)),
            b'\r' => continue,
            _ => return Some(indent),
        }
    }
    None
}

#[derive(Clone, Copy)]
struct PreviousRegion {
    indent: i64,
    end_above: usize,
    _line: usize,
}

/// Computes indentation-based fold ranges from `lines` and `tab_width`, following Code-OSS
/// `computeRanges` (`indentRangeProvider.ts`).
///
/// Walking bottom-up, a non-blank line `line` with indentation `g` closes any region on the stack
/// whose indentation is strictly greater than `g` (`f.indent > g`), emitting `(line, f.end_above - 1)`
/// when `f.end_above - 1 > line`.
pub fn compute_indent_folds(lines: &[String], tab_width: u32) -> Vec<FoldRange> {
    if lines.len() < 2 {
        return Vec::new();
    }
    let mut prev_regions = vec![PreviousRegion {
        indent: -1,
        end_above: lines.len(),
        _line: lines.len(),
    }];
    let mut result = Vec::new();
    for line in (0..lines.len()).rev() {
        let Some(indent_u32) = compute_indent_level(&lines[line], tab_width) else {
            continue;
        };
        let indent = i64::from(indent_u32);
        let mut top = *prev_regions.last().expect("sentinel region");
        if top.indent > indent {
            loop {
                prev_regions.pop();
                top = *prev_regions.last().expect("sentinel region");
                if top.indent <= indent {
                    break;
                }
            }
            let end_line = top.end_above.saturating_sub(1);
            if end_line > line && end_line < lines.len() {
                result.push(FoldRange {
                    start_line: line as u32,
                    end_line: end_line as u32,
                });
                if result.len() >= MAX_FOLD_REGIONS {
                    break;
                }
            }
        }
        if top.indent == indent {
            if let Some(last) = prev_regions.last_mut() {
                last.end_above = line;
            }
        } else {
            prev_regions.push(PreviousRegion {
                indent,
                end_above: line,
                _line: line,
            });
        }
    }
    result.reverse();
    result
}

/// Parses an extension-host `$provideFoldingRanges` reply (`IFoldingRangeDto[]`, 1-based `start`
/// and `end` lines as converted by `Eb.from` in `extHostTypeConverters.ts`) into 0-based
/// [`FoldRange`]s clamped to `line_count`.
pub fn parse_provider_folds(value: &Value, line_count: usize) -> Vec<FoldRange> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    if line_count < 2 {
        return Vec::new();
    }
    let max_line = (line_count - 1) as u32;
    let mut out = Vec::with_capacity(arr.len().min(MAX_FOLD_REGIONS));
    for item in arr {
        let Some(start_1) = item.get("start").and_then(Value::as_u64) else {
            continue;
        };
        let Some(end_1) = item.get("end").and_then(Value::as_u64) else {
            continue;
        };
        let start_line = (start_1 as u32).saturating_sub(1);
        let end_line = (end_1 as u32).saturating_sub(1).min(max_line);
        if start_line < end_line {
            out.push(FoldRange { start_line, end_line });
            if out.len() >= MAX_FOLD_REGIONS {
                break;
            }
        }
    }
    out
}

/// Merges extension-host provider fold ranges with indentation fallback ranges, ensuring at most
/// one fold starts on any `start_line` (provider wins over indentation, and earlier provider wins
/// over later provider), sorted by `start_line`, with `collapsed` applied from `collapsed_lines`.
pub fn merge_folds(
    version: u32,
    line_count: usize,
    provider_lists: &[Vec<FoldRange>],
    indent: &[FoldRange],
    collapsed_lines: &HashSet<u32>,
) -> Vec<FoldRegion> {
    if line_count < 2 {
        return Vec::new();
    }
    let max_line = (line_count - 1) as u32;
    let mut by_start: BTreeMap<u32, u32> = BTreeMap::new();
    for list in provider_lists {
        for r in list {
            let end = r.end_line.min(max_line);
            if r.start_line < end && by_start.len() < MAX_FOLD_REGIONS {
                by_start.entry(r.start_line).or_insert(end);
            }
        }
    }
    // When no provider contributed ranges, fall back to indentation folds; when providers did
    // contribute ranges, still fill in indentation folds on lines no provider started on.
    for r in indent {
        let end = r.end_line.min(max_line);
        if r.start_line < end && by_start.len() < MAX_FOLD_REGIONS {
            by_start.entry(r.start_line).or_insert(end);
        }
    }
    by_start
        .into_iter()
        .map(|(start_line, end_line)| FoldRegion {
            version,
            start_line,
            end_line,
            collapsed: collapsed_lines.contains(&start_line),
        })
        .collect()
}

/// Shifts a set of collapsed `start_line`s across a text edit `(range, text)`.
/// A fold whose start line is strictly after `range.end.line` shifts by the line delta; a fold
/// whose start line is strictly inside the replaced multi-line range `(range.start.line + 1 ..= range.end.line)`
/// is dropped; an insertion at `(line, 0)` shifts `line` down by the added lines; an edit starting
/// strictly after column 0 on `line` leaves `line` unchanged.
pub fn shift_collapsed_on_edit(collapsed: &mut HashSet<u32>, range: WidgetRange, text: &str) {
    if collapsed.is_empty() {
        return;
    }
    let new_lines = text.bytes().filter(|b| *b == b'\n').count() as i64;
    let old_lines = i64::from(range.end.line) - i64::from(range.start.line);
    let delta = new_lines - old_lines;
    if delta == 0 && old_lines == 0 {
        return;
    }
    let prev = std::mem::take(collapsed);
    for line in prev {
        if line < range.start.line || (line == range.start.line && range.start.col > 0) {
            collapsed.insert(line);
        } else if line == range.start.line && range.start.col == 0 && range.end.line == range.start.line {
            let shifted = i64::from(line) + delta;
            if shifted >= 0 {
                collapsed.insert(shifted as u32);
            }
        } else if line > range.end.line {
            let shifted = i64::from(line) + delta;
            if shifted >= 0 {
                collapsed.insert(shifted as u32);
            }
        }
    }
}

/// Per-document folding state held by [`crate::SessionCore`].
#[derive(Debug, Default, Clone)]
pub struct DocFolds {
    /// Provider handle → ranges returned by `$provideFoldingRanges`.
    pub providers: HashMap<i64, Vec<FoldRange>>,
    /// Providerorder (best first) from the last query.
    pub order: Vec<i64>,
    /// Start lines the user has collapsed in the gutter.
    pub collapsed: HashSet<u32>,
}

impl DocFolds {
    pub fn set_fold(&mut self, start_line: u32, is_collapsed: bool) -> bool {
        if is_collapsed {
            self.collapsed.insert(start_line)
        } else {
            self.collapsed.remove(&start_line)
        }
    }

    pub fn on_edit(&mut self, range: WidgetRange, text: &str) {
        shift_collapsed_on_edit(&mut self.collapsed, range, text);
    }

    pub fn regions(&self, version: u32, lines: &[String], tab_width: u32) -> Vec<FoldRegion> {
        let indent = compute_indent_folds(lines, tab_width);
        let ordered_providers: Vec<Vec<FoldRange>> = self
            .order
            .iter()
            .filter_map(|h| self.providers.get(h).cloned())
            .collect();
        merge_folds(version, lines.len(), &ordered_providers, &indent, &self.collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::WidgetPos;
    use serde_json::json;

    #[test]
    fn fr_e6_compute_indent_folds_nested_blocks_and_blank_lines() {
        let lines: Vec<String> = vec![
            "fn main() {".into(),
            "    if true {".into(),
            "        println!(\"hi\");".into(),
            "".into(),
            "        println!(\"bye\");".into(),
            "    }".into(),
            "}".into(),
        ];
        let folds = compute_indent_folds(&lines, 4);
        assert_eq!(
            folds,
            vec![
                FoldRange { start_line: 0, end_line: 5 },
                FoldRange { start_line: 1, end_line: 4 },
            ]
        );
    }

    #[test]
    fn fr_e6_parse_provider_folds_converts_1_based_to_0_based_and_clamps() {
        let dto = json!([
            { "start": 1, "end": 7, "kind": 3 },
            { "start": 2, "end": 6 },
            { "start": 5, "end": 5 },
            { "start": 3, "end": 99 }
        ]);
        let parsed = parse_provider_folds(&dto, 7);
        assert_eq!(
            parsed,
            vec![
                FoldRange { start_line: 0, end_line: 6 },
                FoldRange { start_line: 1, end_line: 5 },
                FoldRange { start_line: 2, end_line: 6 },
            ]
        );
    }

    #[test]
    fn fr_e6_merge_folds_deduplicates_start_line_and_preserves_collapsed() {
        let provider = vec![vec![
            FoldRange { start_line: 0, end_line: 6 },
            FoldRange { start_line: 1, end_line: 5 },
        ]];
        let indent = vec![
            FoldRange { start_line: 0, end_line: 5 },
            FoldRange { start_line: 3, end_line: 4 },
        ];
        let mut collapsed = HashSet::new();
        collapsed.insert(1);
        let merged = merge_folds(2, 7, &provider, &indent, &collapsed);
        assert_eq!(
            merged,
            vec![
                FoldRegion { version: 2, start_line: 0, end_line: 6, collapsed: false },
                FoldRegion { version: 2, start_line: 1, end_line: 5, collapsed: true },
                FoldRegion { version: 2, start_line: 3, end_line: 4, collapsed: false },
            ]
        );
    }

    #[test]
    fn fr_e6_shift_collapsed_on_edit_moves_lines_after_insertion() {
        let mut collapsed: HashSet<u32> = [1, 5, 10].into_iter().collect();
        let range = WidgetRange::new(WidgetPos::new(3, 0), WidgetPos::new(4, 0));
        shift_collapsed_on_edit(&mut collapsed, range, "a\nb\nc");
        // Replaced 1 line break (3..4) with 2 line breaks -> delta +1 for lines > 4; line 4 would be dropped.
        assert!(collapsed.contains(&1));
        assert!(collapsed.contains(&6));
        assert!(collapsed.contains(&11));
    }
}
