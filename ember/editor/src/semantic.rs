//! Document semantic tokens (FR-E6, #441): decoding the extension host's `VSBuffer` 5-tuple delta
//! stream (`$provideDocumentSemanticTokens`), mapping `SemanticTokensLegend` token types and
//! modifiers to [`SyntaxRole`], and overlaying semantic token runs onto TextMate [`DocRun`]s.
//!
//! Wire format of the `VSBuffer` returned by `ExtHostLanguageFeatures.$provideDocumentSemanticTokens`
//! (`common/semanticTokensDto.ts` `encodeSemanticTokensDto` / `uU` in `extensionHostProcess.js`):
//! - Little-endian `u32` words:
//!   - `word[0] = id: u32` (`resultId`)
//!   - `word[1] = kind: u32` (`1` = full, `2` = delta)
//!   - When `kind == 1` (`full`):
//!     - `word[2] = data_len: u32` (number of `u32` integers, a multiple of 5)
//!     - `word[3 .. 3 + data_len]`: `data_len / 5` tuples of `(delta_line, delta_start, length, token_type, token_modifiers)`.

use std::collections::BTreeMap;

use ember_editor_conn::exthost::SemanticTokensLegend;

use crate::coords::utf16_len;
use crate::textmate::{DocRun, SyntaxRole};

/// Maximum number of semantic token 5-tuples decoded from a single reply.
pub const MAX_SEMANTIC_TOKENS: usize = 200_000;

/// Maps an LSP / VS Code semantic token type name (and optional modifier bitmask) to a
/// [`SyntaxRole`].
pub fn role_for_semantic_token(
    token_type: &str,
    modifiers_mask: u32,
    legend_modifiers: &[String],
) -> Option<SyntaxRole> {
    let has_mod = |name: &str| -> bool {
        legend_modifiers
            .iter()
            .enumerate()
            .take(32)
            .any(|(bit, m)| (modifiers_mask & (1u32 << bit)) != 0 && m == name)
    };
    match token_type {
        "namespace" | "class" | "enum" | "interface" | "struct" | "typeParameter" | "type"
        | "builtinType" | "selfTypeKeyword" | "typeAlias" | "union" => Some(SyntaxRole::Type),
        "enumMember" | "boolean" => Some(SyntaxRole::Constant),
        "variable" | "parameter" => {
            if has_mod("readonly") && has_mod("static") {
                Some(SyntaxRole::Constant)
            } else {
                Some(SyntaxRole::Variable)
            }
        }
        "property" => Some(SyntaxRole::Property),
        "function" | "method" | "member" => Some(SyntaxRole::Function),
        "macro" | "derive" => Some(SyntaxRole::Macro),
        "keyword" | "modifier" | "selfKeyword" | "storage" => Some(SyntaxRole::Keyword),
        "comment" | "documentation" => Some(SyntaxRole::Comment),
        "string" | "regexp" | "character" => Some(SyntaxRole::String),
        "number" => Some(SyntaxRole::Number),
        "operator" => Some(SyntaxRole::Operator),
        "decorator" | "attribute" | "annotation" | "lifetime" => Some(SyntaxRole::Attribute),
        "escapeSequence" | "formatSpecifier" => Some(SyntaxRole::Escape),
        "punctuation" | "delimiter" | "brace" | "bracket" | "parenthesis" | "colon" | "comma"
        | "semicolon" | "dot" => Some(SyntaxRole::Punctuation),
        "tag" | "xmlElement" => Some(SyntaxRole::Tag),
        "label" => Some(SyntaxRole::Property),
        _ => None,
    }
}

fn read_u32_le(buf: &[u8], word_index: usize) -> Option<u32> {
    let at = word_index.checked_mul(4)?;
    let slice = buf.get(at..at + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Decodes a `VSBuffer` reply from `$provideDocumentSemanticTokens(handle, uri, 0)` into
/// `(result_id, runs)`, clamping each token to the UTF-16 length of its line in `lines`.
pub fn decode_semantic_tokens(
    buf: &[u8],
    legend: &SemanticTokensLegend,
    lines: &[String],
) -> Option<(i64, Vec<DocRun>)> {
    if buf.len() < 12 {
        return None;
    }
    let result_id = i64::from(read_u32_le(buf, 0)?);
    let kind = read_u32_le(buf, 1)?;
    if kind != 1 {
        // Ember always passes previousResultId = 0, so the extension host always sends kind = 1 (full).
        return None;
    }
    let data_len = read_u32_le(buf, 2)? as usize;
    if data_len % 5 != 0 {
        return None;
    }
    let tuple_count = (data_len / 5).min(MAX_SEMANTIC_TOKENS);
    let needed_bytes = (3 + tuple_count * 5).checked_mul(4)?;
    if buf.len() < needed_bytes {
        return None;
    }

    let mut runs = Vec::with_capacity(tuple_count);
    let mut line = 0u32;
    let mut start = 0u32;

    for i in 0..tuple_count {
        let base = 3 + i * 5;
        let delta_line = read_u32_le(buf, base)?;
        let delta_start = read_u32_le(buf, base + 1)?;
        let length = read_u32_le(buf, base + 2)?;
        let token_type_idx = read_u32_le(buf, base + 3)? as usize;
        let token_modifiers = read_u32_le(buf, base + 4)?;

        if delta_line > 0 {
            line = line.saturating_add(delta_line);
            start = delta_start;
        } else {
            start = start.saturating_add(delta_start);
        }
        if length == 0 || (line as usize) >= lines.len() {
            continue;
        }
        let Some(type_name) = legend.token_types.get(token_type_idx) else {
            continue;
        };
        let Some(role) = role_for_semantic_token(type_name, token_modifiers, &legend.token_modifiers) else {
            continue;
        };
        let line_utf16 = utf16_len(&lines[line as usize]);
        let clamped_start = start.min(line_utf16);
        let clamped_end = start.saturating_add(length).min(line_utf16);
        if clamped_start < clamped_end {
            runs.push(DocRun {
                line,
                start: clamped_start,
                end: clamped_end,
                role,
            });
        }
    }

    Some((result_id, runs))
}

/// Encodes a full `ISemanticTokensDto` (`id`, `data`) into the extension host's `VSBuffer` binary
/// wire format (used by tests and mock providers).
pub fn encode_semantic_tokens_dto(id: u32, data: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity((3 + data.len()) * 4);
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    for &w in data {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Overlays `semantic` runs onto `textmate` runs within `window` (`first_line..=last_line`):
/// where a semantic token covers `[start, end)` on a line, it overrides any TextMate runs in that
/// character interval while keeping TextMate runs on uncovered intervals.
pub fn overlay_semantic_runs(
    textmate: &[DocRun],
    semantic: &[DocRun],
    window: (u32, u32),
) -> Vec<DocRun> {
    if semantic.is_empty() {
        return textmate.to_vec();
    }
    let mut by_line: BTreeMap<u32, (Vec<DocRun>, Vec<DocRun>)> = BTreeMap::new();
    for &r in textmate {
        if r.start < r.end {
            by_line.entry(r.line).or_default().0.push(r);
        }
    }
    for &r in semantic {
        if r.line >= window.0 && r.line <= window.1 && r.start < r.end {
            by_line.entry(r.line).or_default().1.push(r);
        }
    }

    let mut out = Vec::with_capacity(textmate.len() + semantic.len());
    for (line, (tm_runs, mut sem_runs)) in by_line {
        if sem_runs.is_empty() {
            out.extend(tm_runs);
            continue;
        }
        sem_runs.sort_by_key(|r| (r.start, r.end));
        // Deduplicate overlapping semantic tokens (earlier start wins).
        let mut clean_sem: Vec<DocRun> = Vec::with_capacity(sem_runs.len());
        for s in sem_runs {
            if let Some(last) = clean_sem.last() {
                if s.start < last.end {
                    if s.end <= last.end {
                        continue;
                    }
                    clean_sem.push(DocRun {
                        line,
                        start: last.end,
                        end: s.end,
                        role: s.role,
                    });
                    continue;
                }
            }
            clean_sem.push(s);
        }

        // Slice each TextMate run around the semantic runs on the same line.
        let mut line_runs: Vec<DocRun> = clean_sem.clone();
        for tm in tm_runs {
            let mut cur = tm.start;
            for s in &clean_sem {
                if s.end <= cur {
                    continue;
                }
                if s.start >= tm.end {
                    break;
                }
                if s.start > cur {
                    line_runs.push(DocRun {
                        line,
                        start: cur,
                        end: s.start.min(tm.end),
                        role: tm.role,
                    });
                }
                cur = cur.max(s.end);
                if cur >= tm.end {
                    break;
                }
            }
            if cur < tm.end {
                line_runs.push(DocRun {
                    line,
                    start: cur,
                    end: tm.end,
                    role: tm.role,
                });
            }
        }
        line_runs.sort_by_key(|r| (r.start, r.end));
        for r in line_runs {
            match out.last_mut() {
                Some(last) if last.line == r.line && last.end == r.start && last.role == r.role => {
                    last.end = r.end;
                }
                _ => out.push(r),
            }
        }
    }
    out
}

/// Per-document semantic tokens state held by [`crate::SessionCore`].
#[derive(Debug, Default, Clone)]
pub struct DocSemanticTokens {
    pub version: u32,
    pub runs: Vec<DocRun>,
    /// `(provider_handle, result_id)` to release when refreshed or closed.
    pub last_result: Option<(i64, i64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_e6_decode_semantic_tokens_parses_vsbuffer_delta_stream() {
        let legend = SemanticTokensLegend {
            token_types: vec![
                "namespace".into(),
                "struct".into(),
                "function".into(),
                "variable".into(),
            ],
            token_modifiers: vec!["declaration".into(), "readonly".into(), "static".into()],
        };
        let lines = vec![
            "mod foo {".to_string(),
            "    struct Bar;".to_string(),
            "    fn baz(x: i32) {}".to_string(),
        ];
        // Tuple 0: line 0, col 4, len 3, type 0 (namespace)
        // Tuple 1: delta_line 1, col 11, len 3, type 1 (struct)
        // Tuple 2: delta_line 1, col 7, len 3, type 2 (function)
        // Tuple 3: delta_line 0, delta_start 4 (col 11), len 1, type 3 (variable), modifiers 0b110 (readonly+static -> Constant)
        let data = [
            0, 4, 3, 0, 0,
            1, 11, 3, 1, 0,
            1, 7, 3, 2, 0,
            0, 4, 1, 3, 0b110,
        ];
        let buf = encode_semantic_tokens_dto(42, &data);
        let (result_id, runs) = decode_semantic_tokens(&buf, &legend, &lines).expect("valid dto");
        assert_eq!(result_id, 42);
        assert_eq!(
            runs,
            vec![
                DocRun { line: 0, start: 4, end: 7, role: SyntaxRole::Type },
                DocRun { line: 1, start: 11, end: 14, role: SyntaxRole::Type },
                DocRun { line: 2, start: 7, end: 10, role: SyntaxRole::Function },
                DocRun { line: 2, start: 11, end: 12, role: SyntaxRole::Constant },
            ]
        );
    }

    #[test]
    fn fr_e6_overlay_semantic_runs_overrides_covered_subranges_and_keeps_textmate() {
        let tm = vec![
            DocRun { line: 0, start: 0, end: 3, role: SyntaxRole::Keyword },
            DocRun { line: 0, start: 4, end: 14, role: SyntaxRole::Variable },
        ];
        // Semantic token reclassifies columns 4..9 as Type and 10..14 as Property, leaving 9..10 as Variable.
        let sem = vec![
            DocRun { line: 0, start: 4, end: 9, role: SyntaxRole::Type },
            DocRun { line: 0, start: 10, end: 14, role: SyntaxRole::Property },
        ];
        let merged = overlay_semantic_runs(&tm, &sem, (0, 10));
        assert_eq!(
            merged,
            vec![
                DocRun { line: 0, start: 0, end: 3, role: SyntaxRole::Keyword },
                DocRun { line: 0, start: 4, end: 9, role: SyntaxRole::Type },
                DocRun { line: 0, start: 9, end: 10, role: SyntaxRole::Variable },
                DocRun { line: 0, start: 10, end: 14, role: SyntaxRole::Property },
            ]
        );
    }
}
