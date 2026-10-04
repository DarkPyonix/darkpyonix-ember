// Unit tests for static/detach.js, plain node, no npm install:
//
//   node web/proxy/tests/js/detach.test.js        (or: node --test web/proxy/tests/js/)
//
// Covers the FR-B1 drag state machine, state extraction from VS Code's drag data, the
// FR-B2 message, incoming version checks (FR-B4) and the transport choice. The browser-
// fallback URL is checked against tests/vectors/bridge/detach_vectors.json, which the Rust
// bridge crate checks too.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const d = require('../../static/detach.js');
const VECTORS = JSON.parse(fs.readFileSync(
  path.join(__dirname, '..', '..', '..', '..', 'tests', 'vectors', 'bridge', 'detach_vectors.json'), 'utf8'));

const STRIP = { left: 0, top: 30, right: 800, bottom: 65 };   // a tab strip, client coords
const far = { x: 400, y: 65 + 200 };                            // well below the strip
const near = { x: 400, y: 65 + 10 };                            // just below the strip

function begin(t, extra) {
  t.begin(Object.assign({ strip: STRIP, point: { x: 100, y: 45 }, hasUri: true, editorCount: 1 }, extra));
}

// ---------------------------------------------------------------- FR-B1 state machine
test('a long drag that nothing accepted detaches', () => {
  const t = d.createTracker();
  begin(t);
  t.over(far);
  const r = t.end({ dropEffect: 'none', point: far, insideWindow: true });
  assert.equal(r.detach, true);
  assert.equal(r.reason, 'detach');
  assert.equal(t.dragging(), false);
});

test('a short drag (reorder inside the strip) never detaches', () => {
  const t = d.createTracker();
  begin(t);
  t.over({ x: 300, y: 45 });
  // VS Code accepted the drop on the strip: reorder.
  assert.equal(t.end({ dropEffect: 'move', point: { x: 300, y: 45 } }).reason, 'accepted-by-vscode');
  // Even if the effect were 'none', a drop inside the strip is within threshold.
  begin(t);
  assert.equal(t.end({ dropEffect: 'none', point: { x: 300, y: 45 } }).reason, 'inside-threshold');
});

test('a long drag that VS Code accepted (split / move to group / terminal) is VS Code\'s', () => {
  const t = d.createTracker();
  for (const effect of ['move', 'copy', 'link']) {
    begin(t);
    t.over(far);
    const r = t.end({ dropEffect: effect, point: far });
    assert.equal(r.detach, false, effect);
    assert.equal(r.reason, 'accepted-by-vscode');
  }
});

test('just past the strip but within the threshold does not detach', () => {
  const t = d.createTracker({ threshold: 48 });
  begin(t);
  assert.equal(t.end({ dropEffect: 'none', point: near }).reason, 'inside-threshold');
  begin(t);
  assert.equal(t.end({ dropEffect: 'none', point: { x: 400, y: 65 + 49 } }).detach, true);
});

test('missing dragend coordinates fall back to the last dragover point', () => {
  const t = d.createTracker();
  begin(t);
  t.over(far);
  assert.equal(t.end({ dropEffect: 'none', point: null }).detach, true);
  begin(t);
  t.over(near);
  assert.equal(t.end({ dropEffect: 'none', point: null }).detach, false);
});

test('leaving the document counts as past the threshold; re-entering resets it', () => {
  const t = d.createTracker();
  begin(t);
  t.over(near);
  t.leave();
  assert.equal(t.end({ dropEffect: 'none', point: null }).detach, true);
  begin(t);
  t.leave();
  t.over(near);
  assert.equal(t.end({ dropEffect: 'none', point: null }).detach, false);
});

test('alt-drag and VS Code new-window drags outside the window are left to VS Code', () => {
  const t = d.createTracker();
  begin(t, { altKey: true });
  assert.equal(t.end({ dropEffect: 'none', point: far }).reason, 'alt-drag');
  begin(t);
  assert.equal(t.end({ dropEffect: 'none', point: far, altKey: true }).reason, 'alt-drag');
  begin(t, { vscodeNewWindowOp: true });
  assert.equal(t.end({ dropEffect: 'none', point: far, insideWindow: false }).reason, 'vscode-aux-window');
  // Inside the window VS Code does not open an aux window, so the detach is ours.
  begin(t, { vscodeNewWindowOp: true });
  assert.equal(t.end({ dropEffect: 'none', point: far, insideWindow: true }).detach, true);
});

test('multi-select, no URI and dirty tabs are not detached', () => {
  const t = d.createTracker();
  begin(t, { editorCount: 2 });
  assert.equal(t.end({ dropEffect: 'none', point: far }).reason, 'multi-select');
  begin(t, { hasUri: false });
  assert.equal(t.end({ dropEffect: 'none', point: far }).reason, 'no-uri');
  begin(t, { dirty: true });
  assert.equal(t.end({ dropEffect: 'none', point: far }).reason, 'dirty');
  // A dirty tab dropped back on the strip is a plain reorder, not a "save first" toast.
  begin(t, { dirty: true });
  assert.equal(t.end({ dropEffect: 'none', point: { x: 1, y: 40 } }).reason, 'inside-threshold');
});

test('end without begin is a no-op', () => {
  const t = d.createTracker();
  assert.deepEqual(t.end({ dropEffect: 'none' }), { detach: false, reason: 'not-dragging', distance: 0 });
  t.over(far); t.leave();   // must not throw
});

test('geometry helpers', () => {
  assert.equal(d.distanceToRect({ x: 5, y: 40 }, STRIP), 0);
  assert.equal(d.distanceToRect({ x: 803, y: 69 }, STRIP), 5);
  assert.equal(d.distanceToRect(null, STRIP), Infinity);
  const win = { screenX: 100, screenY: 50, outerWidth: 800, outerHeight: 600 };
  assert.equal(d.insideWindowBounds({ x: 500, y: 300 }, win), true);
  assert.equal(d.insideWindowBounds({ x: 950, y: 300 }, win), false);
});

// ---------------------------------------------------------------- state extraction
const URI = 'vscode-remote://localhost:8888/Users/me/proj/src/main.rs';
const viewState = {
  cursorState: [{ inSelectionMode: false, selectionStart: { lineNumber: 40, column: 1 },
                  position: { lineNumber: 42, column: 7 } }],
  viewState: { scrollLeft: 3, firstPosition: { lineNumber: 30, column: 1 },
               firstPositionDeltaTop: -4, scrollTop: 1180 },
  contributionsState: {}
};
function transfer(map) {
  return { types: Object.keys(map), get: (t) => (t in map ? map[t] : '') };
}

test('extracts URI, cursor, selection and scroll from VS Code drag data', () => {
  const tr = transfer({
    'text/plain': 'src/main.rs',
    ResourceURLs: JSON.stringify([URI]),
    CodeEditors: JSON.stringify([{ resource: { $mid: 1, scheme: 'vscode-remote', authority: 'localhost:8888',
                                               path: '/Users/me/proj/src/main.rs' },
                                   options: { viewState } }])
  });
  const s = d.extractFromTransfer(tr.types, tr.get);
  assert.equal(s.fileUri, URI);
  assert.deepEqual(s.cursor, { line: 41, column: 6 });
  assert.deepEqual(s.selection, { start: { line: 39, column: 0 }, end: { line: 41, column: 6 } });
  assert.deepEqual(s.scroll, { top: 1180, left: 3 });
  assert.equal(s.editorCount, 1);
  assert.equal(s.vscodeNewWindowOp, false);
  assert.equal(s.source, 'dataTransfer');
});

test('a backwards selection is normalised; the cursor stays on the active end', () => {
  const vs = { cursorState: [{ selectionStart: { lineNumber: 10, column: 5 }, position: { lineNumber: 3, column: 2 } }] };
  const s = d.fromViewState(vs);
  assert.deepEqual(s.cursor, { line: 2, column: 1 });
  assert.deepEqual(s.selection, { start: { line: 2, column: 1 }, end: { line: 9, column: 4 } });
  assert.equal(s.scroll, null);
});

test('diff editor view state uses the modified side', () => {
  const s = d.fromViewState({ original: {}, modified: viewState });
  assert.deepEqual(s.cursor, { line: 41, column: 6 });
});

test('missing text/plain marks a VS Code new-window drag; marshalled URIs are stringified', () => {
  const tr = transfer({
    CodeEditors: JSON.stringify([{ resource: { $mid: 1, scheme: 'vscode-remote', authority: 'h:1', path: '/a b.txt' } }])
  });
  const s = d.extractFromTransfer(tr.types, tr.get);
  assert.equal(s.vscodeNewWindowOp, true);
  assert.equal(s.fileUri, 'vscode-remote://h:1/a%20b.txt');
  assert.equal(s.cursor, null);
});

test('unrelated drags and garbage data yield nothing', () => {
  let s = d.extractFromTransfer(['Files'], () => '');
  assert.equal(s.fileUri, null);
  assert.equal(s.source, 'none');
  s = d.extractFromTransfer(['ResourceURLs', 'text/plain'], () => '{not json');
  assert.equal(s.fileUri, null);
});

test('status bar cursor text parses across locales', () => {
  assert.deepEqual(d.parseStatusSelection('Ln 42, Col 7'), { line: 41, column: 6 });
  assert.deepEqual(d.parseStatusSelection('줄 42, 열 7 (5 선택됨)'), { line: 41, column: 6 });
  assert.equal(d.parseStatusSelection('Spaces: 4 '), null);
});

// ---------------------------------------------------------------- FR-B2 message
test('the detach message carries the ARCHITECTURE §3 fields and a version', () => {
  const m = d.buildDetachMessage({
    sourceWindowId: 'win-a', folder: '/Users/me/proj', fileUri: URI,
    cursor: { line: 41, column: 6 }, selection: { start: { line: 39, column: 0 }, end: { line: 41, column: 6 } },
    scroll: { top: 1180, left: 0 }, screen: { x: 900, y: 500 }, label: 'main.rs',
    stateSource: 'dataTransfer', sentAtMs: 1
  });
  assert.equal(m.kind, 'tab_detach');
  assert.equal(m.version, d.VERSIONS.tab_detach);
  for (const k of ['sourceWindowId', 'fileUri', 'cursor', 'scroll', 'selection']) assert.ok(k in m, k);
  assert.deepEqual(m.workspace, { folder: '/Users/me/proj' });
  assert.equal(m.editor, 'vscode-web');
  // Round-trips through JSON unchanged (that is what crosses the bridge).
  assert.deepEqual(JSON.parse(JSON.stringify(m)), m);
});

test('incoming: matching versions pass, mismatches are logged and still delivered', () => {
  const logs = [];
  const log = (...a) => logs.push(a.join(' '));
  let r = d.decodeIncoming(JSON.stringify({ kind: 'sibling_window_closed', version: 1, windowId: 'w' }), log);
  assert.equal(r.mismatch, null);
  assert.equal(r.message.windowId, 'w');
  assert.equal(logs.length, 0);

  r = d.decodeIncoming({ kind: 'sibling_window_closed', version: 2, windowId: 'w', extra: true }, log);
  assert.deepEqual(r.mismatch, { kind: 'sibling_window_closed', received: 2, expected: 1 });
  assert.equal(r.message.windowId, 'w');
  assert.equal(logs.length, 1);

  r = d.decodeIncoming({ kind: 'from_the_future', version: 1 }, log);
  assert.equal(r.mismatch.expected, null);
  assert.equal(logs.length, 2);

  assert.equal(d.decodeIncoming('nope', log), null);
});

test('additive fields need no version bump', () => {
  const r = d.decodeIncoming({ kind: 'sibling_window_closed', version: 1, windowId: 'w', newField: 1 });
  assert.equal(r.mismatch, null);
});

// ---------------------------------------------------------------- browser fallback URL
for (const c of VECTORS.cases) {
  test(`fallback URL vector: ${c.name}`, () => {
    const q = d.buildWorkspaceQuery({
      folder: c.detach.workspace && c.detach.workspace.folder,
      fileUri: c.detach.fileUri, cursor: c.detach.cursor
    });
    assert.equal(q === null ? null : '/' + q, c.expectedPath);
  });
}

test('the payload decodes back to VS Code\'s [key, value] pairs', () => {
  const q = d.buildWorkspaceQuery({ folder: '/p', fileUri: URI, cursor: { line: 41, column: 6 } });
  const sp = new URLSearchParams(q);
  assert.equal(sp.get('folder'), '/p');
  assert.deepEqual(JSON.parse(sp.get('payload')),
    [['openFile', URI + ':42:7'], ['gotoLineMode', 'true']]);
});

// ---------------------------------------------------------------- transport
test('transport: WKWebView handler, WebView2, parent frame, then browser fallback', () => {
  const sent = [];
  const wk = { webkit: { messageHandlers: { emberBridge: { postMessage: (s) => sent.push(['wk', s]) } } } };
  wk.parent = wk; wk.top = wk;
  assert.equal(d.pickTransport(wk).name, 'webkit');

  const wv = { chrome: { webview: { postMessage: (s) => sent.push(['wv', s]) } } };
  wv.parent = wv; wv.top = wv;
  assert.equal(d.pickTransport(wv).name, 'webview2');

  // The workbench runs in frame.html's iframe; the handler may only exist on the parent.
  const top = { webkit: { messageHandlers: { emberBridge: { postMessage: (s) => sent.push(['top', s]) } } } };
  top.parent = top; top.top = top;
  const child = { parent: top, top };
  const r = d.send(child, { kind: 'tab_detach', version: 1 }, () => assert.fail('no fallback'));
  assert.deepEqual(r, { transport: 'webkit', ok: true });
  assert.equal(sent.at(-1)[0], 'top');
  assert.equal(typeof sent.at(-1)[1], 'string');   // JSON string on the wire

  const plain = {}; plain.parent = plain; plain.top = plain;
  assert.equal(d.pickTransport(plain), null);
  let fellBack = null;
  assert.deepEqual(d.send(plain, { kind: 'x' }, (m) => { fellBack = m; return true; }),
                   { transport: 'browser', ok: true });
  assert.deepEqual(fellBack, { kind: 'x' });
});

test('transport: a cross-origin parent is skipped, not thrown on', () => {
  const w = {};
  Object.defineProperty(w, 'parent', { get() { throw new Error('SecurityError'); } });
  w.top = w;
  assert.equal(d.pickTransport(w), null);
});

test('transport: a throwing host reports failure instead of falling back', () => {
  const w = { chrome: { webview: { postMessage() { throw new Error('boom'); } } } };
  w.parent = w; w.top = w;
  const r = d.send(w, { kind: 'tab_detach' }, () => assert.fail('no fallback'));
  assert.equal(r.ok, false);
  assert.equal(r.transport, 'webview2');
});
