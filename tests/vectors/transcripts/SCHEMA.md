# Transcript test vectors

Shared vectors for every parser that reads an agent's **native transcript file** (Claude Code's
`~/.claude/projects/*/*.jsonl`, Codex's `~/.codex/sessions/**/rollout-*.jsonl`): the Python
adapters in `web/proxy/dpx/agents/` and the Rust importers in `crates/server/src/history/`.

```
tests/vectors/transcripts/<agent>/<case>/input.jsonl     native file, synthetic content
tests/vectors/transcripts/<agent>/<case>/expected.json   the normalised form below
```

Run both suites with `scripts/test-transcript-vectors.sh`.

## Normalised form

```json
{
  "native_id": "…" | null,
  "cwd": "/work/project" | null,
  "title": "…" | null,
  "messages": [
    {"role": "user", "text": "…"},
    {"role": "assistant", "text": "…"},
    {"role": "assistant", "text": "{\"command\":\"ls\"}", "tool_name": "Bash", "tool_call_id": "toolu_01"},
    {"role": "tool", "text": "…", "tool_name": "Bash", "tool_call_id": "toolu_01", "is_error": false}
  ]
}
```

* `role` is `user`, `assistant` or `tool`. An assistant message with `tool_name` is a **tool
  call**; its `text` is the tool input. A `tool` message is a **tool result**; it always has
  `tool_call_id` (`""` if the file has none) and `is_error`, and has `tool_name` when an earlier
  call with the same id names it. Fields that do not apply are omitted, not `null`.
* Tool-call `text`: a JSON input (object/array) is written canonically — keys sorted, no
  whitespace, non-ASCII unescaped. An input that is a plain string is used as-is.
* Text from several text blocks of one native line is joined with `"\n"`; blocks that are empty
  or whitespace-only are dropped. Text is otherwise kept verbatim (no trimming). A message whose
  text is empty or whitespace-only is dropped (tool calls and results are never dropped).
* Within one native line, its text message comes first, then its tool calls/results in block order.
* Lines that are not a JSON object (malformed, truncated, blank, arrays) are skipped. Unknown line
  types and unknown content-block types are skipped. Files are split on `\n` only (a raw U+2028
  inside a JSON string is text, not a line break); a trailing `\r` is ignored.
* Metadata comes from the file's contents only. `null` when the file does not say.

### Claude Code

* `native_id`: the first `sessionId`. `cwd`: the first `cwd`.
* `title`: the **last** `custom-title` line's `customTitle`; else the **first** `summary` line's
  `summary`; else `null`.
* Messages come from `type: user|assistant` lines only. Skipped: `isSidechain` (subagent),
  `isMeta`, `isCompactSummary` lines; `thinking`/`redacted_thinking` and other unknown blocks;
  `summary`, `system` (incl. `compact_boundary`), `custom-title`, `attachment`, etc.
* `tool_use` block → tool call (`id`, `name`, `input`). `tool_result` block → tool result
  (`tool_use_id`, `is_error` defaulting to `false`); its `content` is a string, or a block list
  whose text blocks are joined with `"\n"` (images dropped).

### Codex

* `native_id`: `session_meta.payload.id`. `cwd`: `session_meta.payload.cwd`, else the first
  `turn_context.payload.cwd`. `title`: always `null` (Codex keeps thread names elsewhere).
* Messages come from `response_item` lines only. `event_msg` lines (which repeat the same
  messages), `compacted` (its `replacement_history` repeats earlier lines), `reasoning`,
  `turn_context`, `session_meta` and unknown types are skipped.
* `message` with role `user` (`input_text` blocks) or `assistant` (`output_text` blocks).
  Role `developer`/`system` is skipped. In user messages, text blocks that Codex injects as
  context are dropped: blocks starting with `<environment_context>`, `<user_instructions>`,
  `<recommended_plugins>`, `<permissions instructions>` or `# AGENTS.md instructions`.
* `function_call` (`name`, `arguments` JSON string, `call_id`) and `custom_tool_call` (`name`,
  `input` raw string, `call_id`) → tool calls. `arguments` that parse as JSON are written
  canonically; otherwise used as-is.
* `function_call_output` / `custom_tool_call_output` (`call_id`, `output`) → tool results.
  `output` as a block list: its text blocks joined with `"\n"`. As a string that parses as a JSON
  object with a string `output` (the older exec wrapper): that string, with `is_error` true when
  `metadata.exit_code` is a non-zero number. Any other string: as-is, `is_error` false.

## How each implementation is checked

* **Rust** (`crates/server/src/history/`) must reproduce `expected.json` exactly.
* **Python** (`web/proxy/dpx/agents/`) is a display parser that summarises tools instead of keeping
  them, so `web/proxy/tests/test_transcript_vectors.py` compares a projection: the ordered
  `(role, text)` of user/assistant messages without `tool_name`, against the adapter's
  `kind == "text"` messages; plus `cwd`, and `title` where it is not `null`. `native_id` is not
  compared (the Python adapter identifies a conversation by its file name).
