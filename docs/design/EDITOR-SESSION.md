# EDITOR-SESSION.md — The editor session between editor-conn and the CodeEditor widget

> Status: design + first code (`crates/editor/`, package `ember-editor`), **not compiled yet**.
> Milestone: M8 (`PROJECT.md`), issue #32, SPEC §E (`FR-E1`–`FR-E4`). Written 2026-10-03.
> Builds on `EDITOR-CONNECTION.md` (§6 step 3 and 4 of its plan).

## 1. Where it sits

```
 dioxus-compose CodeEditor (FR-38)          ember-editor (this crate)                 ember-editor-conn
 ─────────────────────────────────          ──────────────────────────                ─────────────────
 events  CodeChanged / CodeHovered /  ──▶   EditorSession ──▶ SessionCore  ──▶ Effect::Rpc     ──▶ RpcPeer ──▶ ext host
         CodeSaveRequested /                (driver task)     (pure state     Effect::ReadFile ──▶ RemoteFs ──▶ server
         DecorationActivated /                                 machine)        Effect::Respond  ──▶ RpcPeer
         CodeEditRejected                                                       Effect::Ui
 props   Text+key, Decorations, TabWidth ◀── SessionUpdate ◀──────────────────────┘
 command EditCode                        ◀──
```

`ember-editor` does not depend on dioxus-compose. The widget contract is modelled as plain data
(`widget.rs`); the UI layer (M3/M13 workbench) translates real widget events into `WidgetEvent`
and `SessionUpdate`s into props and commands. Everything in between is testable without a
renderer or a server.

`SessionCore` (`engine.rs`) is synchronous and does no I/O: each `Input` (UI command, widget event,
extension-host request, RPC reply, file result, watcher event, timer tick) produces an ordered
list of `Effect`s. `EditorSession` (`session.rs`) owns the sockets, executes the effects in order,
and feeds results back. Ordering matters: `$acceptModelChanged` for version *n* is always sent
before any provider request made at version *n*, because both are effects of the same queue.

## 2. Crate API

| Item | Surface |
| ---- | ------- |
| `EditorSession::connect(config, dial)` / `connect_tcp(config)` | version gate (`GET /version` = `PINNED_COMMIT`), management (`getEnvironmentData`, `scanExtensions`, `fileChange` subscription), extension host (init data, `$initializeConfiguration`, `$initializeWorkspace`); returns the session and a `UnboundedReceiver<SessionUpdate>` |
| `EditorSession` methods (all non-blocking sends) | `uri(path)`, `open(uri)`, `close(uri)`, `focus(uri)`, `widget_event(uri, generation, WidgetEvent)`, `trigger_inline_completion(uri, generation, pos)`, `resync(uri)`, `shutdown().await` |
| `SessionConfig` | `host`, `remote_authority`, `connection_token`, `workspace_path`, `workspace_name`, `app_language`, `user_settings`, `core: CoreOptions` |
| `CoreOptions` | `tab_width` (4), `insert_spaces`, `codelens_debounce` (250 ms), `codelens_resolve_cap` (100), `inline_completions` |
| `SessionUpdate` | `Opened{uri, generation, text, language_id, tab_width}`, `OpenFailed`, `Command{uri, WidgetCommand}`, `Decorations{uri, generation, Vec<Decoration>}`, `Hover(HoverPopup)`, `HoverHidden`, `Dirty`, `Saved`, `SaveFailed`, `ExternalChange{ChangedWhileDirty \| Deleted}`, `RunCommand{CommandDto}`, `Desync`, `Closed`, `ConnectionLost` |
| `WidgetEvent` | `CodeChanged{version, range, text}`, `CodeEditRejected{…}`, `CodeHovered{decoration, pos, phase}`, `CodeSaveRequested{version}`, `DecorationActivated{decoration}` |
| `WidgetCommand` | `SetText{generation, text}` (new node, key = generation), `EditCode{request_id, base_version, range, text}` |
| `Decoration` | `{id: DecorationId(u64 ≠ 0), version, kind: Underline\|CodeLens\|HoverAnchor\|GhostText, severity, color: 0, range, text}` — the FR-38 44-byte record |
| `SessionCore` | `new(LanguageRegistry, CoreOptions, now)`, `handle(now, Input) -> Vec<Effect>`, `next_wake()`, `resync(now, uri)` — for tests and for a driver on another transport |
| helpers | `coords` (`WidgetPos`/`WidgetRange` ↔ `IPosition`/`IRange`, UTF-16 ↔ byte/char, `transform_range`), `selector::score` (port of `languages.score`), `languages::LanguageRegistry`, `ids::DecorationIds` |

## 3. Documents and versions

- **Coordinates.** The widget is 0-based lines and 0-based UTF-16 columns (FR-38 §38.2); the
  extension host is 1-based lines and columns, also UTF-16. Conversion is `±1` on both axes;
  nothing is re-counted. `DocumentMirror` rejects a column inside a surrogate pair.
- **Text form.** The widget is given the text with `\n` line breaks; the extension host mirror
  keeps the file's EOL (`\n` or `\r\n`, split like Monaco) and normalizes inserted text to it, so
  saving writes the file's own EOL. A UTF-8 BOM is stripped and written back. Non-UTF-8 files are
  refused (`OpenFailed`).
- **Versions.** Widget version 0 is the `Text` the session set; each `CodeChanged` is +1. The
  extension host needs one increasing `versionId` per URI. Each document keeps `ext_base` (the
  extension-host version at widget version 0) and sends widget version *v* as `ext_base + v`, one
  `$acceptModelChanged` per `CodeChanged`. A version that is not the next one is a lost or
  duplicated event: the change is not applied and `Desync` is reported; `resync(uri)` re-sends the
  session's text as a new generation.
- **Generation (node key).** FR-38 ignores a `Text` identical to the buffer and resets to version 0
  on a different one, so a reload with equal text would not reset. Every document therefore has a
  `generation` that the UI uses as the `CodeEditor` node key and passes back with every event.
  Events from an older generation are dropped, which also removes the race between a `SetText`
  and events the old node emitted before it saw it.
- **Reset (new `Text`).** On a reload or resync: the generation increments, the widget restarts at
  0, and the extension host gets `$acceptDocumentsAndEditorsDelta{removedDocuments, removedEditors}`
  then `$activateByEvent(onLanguage)` + `{addedDocuments (versionId = last + 1), addedEditors,
  newActiveEditor}`. Versions for the URI never go backwards. Ember-editor-conn gained
  `DocumentMirror::with_version` for this.
- **Save.** `CodeSaveRequested` → `remoteFilesystem.writeFile` of the mirror text → on success
  `$acceptModelSaved` (fires `onDidSaveTextDocument`) and clean; if the user typed while the write
  was in flight the document stays dirty (`$acceptDirtyStateChanged(true)`). Dirty state is
  "changed since open/save" (the widget does not report undo-to-saved).
- **File watching.** Each open file is watched (`remoteFilesystem.watch`, non-recursive). A change
  is read back: equal to what Ember last read/wrote → our own save, ignored; clean buffer → reset
  with the new content; dirty buffer → `ExternalChange::ChangedWhileDirty`, the UI decides.
  Deleted → `ExternalChange::Deleted`. Watcher events are matched to documents by path, because the
  authority in server-sent URIs is the transformer's choice.
- **Tabs.** After every open/close/focus/dirty change the session sends
  `ExtHostEditorTabs.$acceptEditorTabModel` (one group, one tab per document): the live OSE test
  found `vscode-languageclient` uses `window.tabGroups` to decide which documents to pull
  diagnostics for.
- **Languages.** The language id comes from the scanned extensions' `contributes.languages`
  (file name, file-name pattern, longest extension), the same source the workbench uses.

## 4. Bridges

All bridges keep their ranges in widget coordinates and carry them through every edit with
`coords::transform_range`, which moves ranges the way FR-38 §38.4 says the widget moves
decorations (shift after, grow/shrink around, a wholly deleted range disappears). So the
session's copy matches what the widget draws without a round trip, and position lookups (hover)
see what the user sees.

**Decoration ids** (`ids.rs`): non-zero, unique in the session, top byte = bridge
(1 diagnostics, 2 CodeLens, 3 hover anchor, 4 ghost text) so an activation is routed without a
lookup, and **stable**: keyed by content, not position. A diagnostic keeps its id when it is
re-published at another line; a CodeLens keeps its id when resolved or re-provided at the same
place. Ids are never reissued after a document is closed or reset.

### 4.1 Diagnostics (FR-E2)

`MainThreadDiagnostics.$changeMany(owner, [uri, markers | undefined][])` replaces the markers of
(owner, uri); `$clear(owner)` drops an owner everywhere. Each marker becomes an `Underline` with
severity 8/4/2/1 → Error/Warning/Information/Hint, range clamped to the text, a zero-width range
widened to one character. Id key: owner + severity + source + code + message + ordinal among
identical markers. Markers for files that are not open are kept and appear when opened.

### 4.2 CodeLens (FR-E3)

Registrations: `$registerCodeLensSupport(handle, selector, eventHandle?)`. On open, on any edit
(debounced 250 ms), on `$emitCodeLensEvent(eventHandle)` and on a new matching registration, every
matching provider gets `$provideCodeLenses(handle, uri)`; an outstanding request for the same
(document, provider) is cancelled; a reply for an older document version is released and dropped.
Lenses without a `command` are resolved with `$resolveCodeLens(handle, lens-as-received)` (up to
`codelens_resolve_cap`; FR-38 has no viewport event, so it is eager). A replaced or closed list is
released with `$releaseCodeLenses(handle, cacheId)`. Each lens with a non-empty title is a
`CodeLens` decoration (text = title, `$(icon)` syntax passed through).

Click (`DecorationActivated`): the lens command `{id, arguments}`. If the extension host registered
`id` (`$registerCommand`, which includes its internal delegating command `__vsc<uuid>` used for
commands with arguments) → `ExtHostCommands.$executeContributedCommand(id, ...arguments)`.
Otherwise `$activateByEvent("onCommand:<id>")` first; if it is still not registered, it is a
workbench command (`editor.action.showReferences`, …) and goes to the UI as `RunCommand`.

### 4.3 Hover (FR-E3)

`CodeHovered(Rest, pos)` → cancel the previous hover's requests → `$provideHover(handle, uri,
pos+1, undefined)` to every matching provider (best first). The popup model (`HoverPopup`) is
diagnostics under the position (most severe first), then provider Markdown (`IMarkdownString`:
`value`, `isTrusted`, `supportThemeIcons`, `supportHtml`) in provider order, plus the first
provider range. It is emitted immediately if there are diagnostics, and again as each reply
arrives (`complete` once none is outstanding). `Leave`, any edit, or close → cancel + `HoverHidden`.
Like upstream (`mainThreadLanguageFeatures.ts`, finalization registry commented out),
`$releaseHover` is never sent.

### 4.4 Inline completions (FR-E4)

Registration: `$registerInlineCompletionsSupport` (17 args; Ember reads `supportsHandleEvents` (2),
`extensionId` (3), `yieldsToExtensionIds` (6), `debounceDelayMs` (8), `excludesExtensionIds` (9)).

- **Trigger.** Every `CodeChanged` restarts a debounce: the largest `debounceDelayMs` among
  matching providers, else 50 ms (Monaco's `InlineCompletionsDebounce`). The cursor is estimated
  as the end of the last change (FR-38 has no cursor event). `trigger_inline_completion` is the
  explicit trigger (`triggerKind: 1`, no debounce). `$emitInlineCompletionsChange` re-triggers in the
  active document.
- **Cancellation.** A change or a new trigger cancels the outstanding request (RPC `Cancel` for
  every provider); late replies are dropped. A reply computed for an older document version frees
  every list of that request (`$freeInlineCompletionsList(…, {kind: "lostRace"})`).
- **Choice.** When every provider has answered, the first item (provider order, then item order)
  that can be ghost text wins: its range lies on the cursor line and ends at the cursor, and the
  text from the range start to the cursor is a prefix of `insertText`; the ghost is the rest.
  Snippet `insertText` is flattened to its default text; `isInlineEdit` items are skipped. Lists
  not chosen are freed (`notTaken`, or `empty`). The ghost is one `GhostText` decoration: an empty
  range at the cursor with the remaining text. `$handleInlineCompletionDidShow` is sent if the
  provider wants events.
- **Typing through.** Typing exactly the next characters shrinks the ghost (same id, no new
  request). Anything else ends it: `$handleInlineCompletionEndOfLifetime(…, {kind: 2,
  userTypingDisagreed: true})` (if wanted) + free (`other`), then a new debounce.
- **Accept.** FR-38: on Tab the widget inserts the ghost (a `CodeChanged` indistinguishable from
  typing all of it) and then sends `DecorationActivated(id)` in the same frame. So a ghost typed
  out completely is parked for one event: an activation for its id → `Accepted` (`{kind: 0,
  alternativeAction: false}`) + free, then `additionalTextEdits` as `EditCode`s (base = the version
  the item was computed for) and the item's `command` run as in §4.2; anything else → `Ignored`.
  If an activation arrives while ghost text is still pending (a widget that does not insert), the
  session inserts it with `EditCode`.

## 5. The extension host's other requests

Answered by `replies::session_reply` = `exthost::default_reply` plus what `live_ose.rs` learned:
`MainThreadLanguages.$getLanguages` → every contributed language id; `MainThreadOutputService.$register`
→ a channel id string. With state, in the core:

- `MainThreadDocuments.$tryOpenDocument(uri)` (`workspace.openTextDocument`): read the file, add it
  to the extension host as a document without an editor, reply with the URI. If the user opens it
  later it is removed and re-added at the next version.
- `MainThreadCommands.$executeCommand(id, args)`: the extension host runs its own commands locally,
  so what arrives is a workbench command → `RunCommand` to the UI (except `setContext`, a no-op),
  reply `undefined`.
- `$registerCommand` / `$unregisterCommand`, `$register…Provider` / `$unregister`: tracked in
  `registry.rs`, with selector matching ported from `languageSelector.ts` `score()` (scheme,
  language, `*` → 5, glob or relative patterns, notebook filters never match text).

## 6. What the widget side must provide

1. **The FR-38 events exactly as specified**, decoded into `WidgetEvent`, plus the node key the
   event came from (the `generation` from `Opened` / `SetText`). If the UI cannot attach the key,
   a `SetText` racing with in-flight events will be reported as `Desync`.
2. **Line splitting:** the widget must treat `\r\n` in pasted text as one line break (or normalize
   to `\n`) so its line count matches the session's.
3. **Decoration records** built from `Decoration` (44-byte record + text blob); `version` is the
   widget version the ranges refer to.
4. **Ghost-text accept order** as §38.4 says (`CodeChanged` then `DecorationActivated` in the same
   frame). Esc / rejection is not reported by FR-38, so Ember can never send `Rejected`;
   a `CodeGhostDismissed` event (or `DecorationActivated` with a reject flag) would close that gap.
5. **Gaps worth adding to FR-38** (not blocking): a cursor / selection event (for inline
   completions at the real cursor, `$acceptEditorPropertiesChanged`, and `activeTextEditor.selection`);
   a viewport event (lazy CodeLens resolve); an undo/redo flag on `CodeChanged`
   (`isUndoing`/`isRedoing`, and clean-after-undo); marker tags (unnecessary/deprecated) on
   underlines; partial ghost-text accept (word / line).
6. **Hover popup and Markdown** are drawn by the UI with dioxus-compose's overlay widgets and the
   FR-26 Markdown renderer; `command:` links only when `is_trusted`.

## 7. Uncertain APIs (not compiled, not run)

- **Nothing in `crates/editor/` has been compiled or run.** The unit tests (`src/*`) and the pure flow
  tests (`tests/engine_flow.rs`) were checked by reading only. The live test
  (`tests/live_session.rs`, `#[ignore]`, `EMBER_OSE_SERVER`) has never run.
- `tokio::select!` handlers that reassign a variable borrowed by a branch future
  (`changes = None` in `session.rs`) rely on tokio dropping the branch futures before running the
  handler.
- `$provideHover` is sent with `undefined` context; upstream sends `{ verbosityRequest: undefined }`.
  Both should deserialize the same in the extension host.
- Ghost-text acceptance relies on FR-38 §38.4 ordering; the widget's actual behaviour for
  multi-line ghost text and for ranges is unconfirmed until the widget exists (10-15 / 10-18).
- What the extension host does with an inline-completion list whose request Ember cancelled (the
  reply, and so its `pid`, never reaches Ember, so it cannot be freed).
- `$acceptEditorTabModel` shape (`IEditorTabGroupDto[]`) is copied from the live test, where it was
  best effort.
- Remove-then-add of the same URI in two consecutive deltas on reset; upstream never does this (it
  sends `isFlush` changes on reload). Chosen per issue #32 so extensions see close/open around
  content they did not see typed; `DocumentMirror::replace_all` (flush) is the alternative if a
  language server misbehaves.
- `$registerInlineCompletionsSupport` argument positions are from extHost.protocol.ts at the pinned
  commit (the most volatile signature in the subset, `EDITOR-CONNECTION.md` §5).

## 8. Not yet

Reconnect (a lost connection ends the session with `ConnectionLost`; `handshake::reconnect_loop`
is the building block), several workspace folders, runtime configuration changes
(`$acceptConfigurationChanged`), `$acceptEditorPropertiesChanged` (selections), save participants
(`$participateInSave`, format on save), `$tryApplyWorkspaceEdit` (rename, code actions), encodings
other than UTF-8, first-line language detection, semantic tokens / syntax spans.
