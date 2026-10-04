/*
 * detach.js: tab detach for the VS Code Web IDE window (SPEC FR-B1, FR-B2, FR-B4).
 *
 * Injected into the workbench top-level document only, exactly like overlay.js (see
 * dpx/vscode/inject.py). Inside the wrapper this is the embed iframe, not frame.html.
 *
 * What it does
 *   1. Watches VS Code's own HTML5 tab drag (VS Code tabs are `draggable`; reorder, split and
 *      move-to-group are all native drag-and-drop). It never cancels, re-targets or
 *      synthesises drag events, so VS Code's reorder is untouched.
 *   2. At `dragend` it decides "detach" only when NOTHING accepted the drop
 *      (`dataTransfer.dropEffect === 'none'`) and the drop point is past THRESHOLD_PX away
 *      from the tab strip. Every VS Code drop target (tab strip = reorder, editor area =
 *      split, terminal, explorer, chat) sets a non-'none' effect, so "a short drag reorders,
 *      a long drag detaches, never both" holds by construction (FR-B1).
 *   3. Builds a versioned `tab_detach` message (ARCHITECTURE.md §3) and hands it to
 *      `send()`: the native shell's message handler when one exists, else the browser
 *      fallback (a new window on the same workspace with the file at the cursor).
 *
 * Where the state comes from (measured against VS Code 1.138 workbench source; see
 * docs in web/proxy/README.md "Tab detach"):
 *   - dataTransfer 'ResourceURLs'  JSON array of URI strings. Set on every tab drag for
 *                                  editors backed by a file system provider. RELIABLE.
 *   - dataTransfer 'CodeEditors'   JSON array of untyped editor inputs; each carries
 *                                  `options.viewState` (ICodeEditorViewState) taken from the
 *                                  visible editor pane: cursorState[] (selectionStart,
 *                                  position) and viewState (scrollTop, scrollLeft).
 *                                  Present when the dragged editor is visible in its group
 *                                  (or has saved view state). RELIABLE for the visible tab.
 *   - DOM fallbacks (only when the transfer is empty/unreadable):
 *       .monaco-editor[data-uri]      model URI of a visible code editor
 *       #status.editor.selection      "Ln 42, Col 7" (localised text; digits parsed):
 *                                     active editor only, cursor only, no selection range
 *       .lines-content style.top      = -scrollTop (Monaco viewLines); visible editor only
 *
 * Positions on the bridge are 0-based (line, column), like LSP and the ARCHITECTURE §3
 * example; Monaco's 1-based values are converted here.
 *
 * The pure parts (tracker, extraction, message and URL building, transport choice) are
 * exported for web/proxy/tests/js/detach.test.js, which runs with plain `node`.
 */
(function (root, factory) {
  'use strict';
  var api = factory();
  if (typeof module === 'object' && module && module.exports) module.exports = api;
  if (typeof window !== 'undefined' && typeof document !== 'undefined' && window.document === document) {
    api.install(window);
  }
})(this, function () {
  'use strict';

  // ---------------------------------------------------------------------------------------
  // Protocol constants: keep in sync with crates/bridge/src/messages.rs
  // ---------------------------------------------------------------------------------------
  var VERSIONS = { tab_detach: 1, sibling_window_closed: 1, open_window: 1 };
  var HANDLER_NAME = 'emberBridge';
  var EDITOR_KIND = 'vscode-web';
  // How far (CSS px) past the tab strip's rectangle the drop must land to count as a detach.
  var THRESHOLD_PX = 48;

  var MIME_RESOURCES = 'ResourceURLs';   // VS Code DataTransfers.RESOURCES
  var MIME_EDITORS = 'CodeEditors';      // VS Code CodeDataTransfers.EDITORS

  // ---------------------------------------------------------------------------------------
  // Geometry
  // ---------------------------------------------------------------------------------------
  /** Euclidean distance from point p to rect r (0 when inside). */
  function distanceToRect(p, r) {
    if (!p || !r) return Infinity;
    var dx = Math.max(r.left - p.x, 0, p.x - r.right);
    var dy = Math.max(r.top - p.y, 0, p.y - r.bottom);
    return Math.sqrt(dx * dx + dy * dy);
  }

  /** Screen point inside the browser window's outer bounds: the same test VS Code uses
   *  (maybeCreateAuxiliaryEditorPartAt) to decide whether a drop left the window. */
  function insideWindowBounds(screenPt, win) {
    if (!screenPt || !win) return true;
    return screenPt.x >= win.screenX && screenPt.x <= win.screenX + win.outerWidth &&
           screenPt.y >= win.screenY && screenPt.y <= win.screenY + win.outerHeight;
  }

  // ---------------------------------------------------------------------------------------
  // The drag state machine (FR-B1). Pure: no DOM access.
  //
  //   idle --begin()--> dragging --end()--> idle   (end() returns the decision)
  //   over(point) / leave() only update the last known position while dragging.
  // ---------------------------------------------------------------------------------------
  function createTracker(opts) {
    var threshold = (opts && typeof opts.threshold === 'number') ? opts.threshold : THRESHOLD_PX;
    var s = null;

    function begin(info) {
      s = {
        strip: info.strip || null,
        point: info.point || null,
        altKey: !!info.altKey,
        // VS Code calls doFillResourceDataTransfers(..., disableStandardTransfer = u) with
        // u = isNewWindowOperation(e). When u is true it skips 'text/plain', and on a drop
        // outside the browser window it opens its OWN auxiliary window. We read that from
        // the transfer types and stay out of its way.
        vscodeNewWindowOp: !!info.vscodeNewWindowOp,
        editorCount: typeof info.editorCount === 'number' ? info.editorCount : 1,
        dirty: !!info.dirty,
        hasUri: !!info.hasUri,
        leftDocument: false
      };
    }
    function over(point) {
      if (!s) return;
      s.point = point;
      s.leftDocument = false;
    }
    function leave() { if (s) s.leftDocument = true; }
    function dragging() { return !!s; }
    function reset() { s = null; }

    /**
     * end({ dropEffect, point, screen, insideWindow, altKey }) -> { detach, reason, distance }
     *   point        client coords at dragend, or null when the browser reports none
     *   insideWindow whether the screen point lies inside the browser window
     */
    function end(info) {
      var st = s; s = null;
      info = info || {};
      if (!st) return { detach: false, reason: 'not-dragging', distance: 0 };
      var outsideWindow = info.insideWindow === false;
      var p = info.point || st.point;
      var distance = (st.leftDocument || outsideWindow) ? Infinity : distanceToRect(p, st.strip);
      function no(reason) { return { detach: false, reason: reason, distance: distance }; }

      if (info.dropEffect && info.dropEffect !== 'none') return no('accepted-by-vscode');
      if (st.altKey || info.altKey) return no('alt-drag');
      if (st.vscodeNewWindowOp && outsideWindow) return no('vscode-aux-window');
      if (st.editorCount !== 1) return no('multi-select');
      if (!st.hasUri) return no('no-uri');
      if (!(distance > threshold)) return no('inside-threshold');
      if (st.dirty) return no('dirty');
      return { detach: true, reason: 'detach', distance: distance };
    }

    return { begin: begin, over: over, leave: leave, end: end, reset: reset, dragging: dragging,
             threshold: threshold };
  }

  // ---------------------------------------------------------------------------------------
  // State extraction
  // ---------------------------------------------------------------------------------------
  function safeJson(text) {
    if (typeof text !== 'string' || !text) return null;
    try { return JSON.parse(text); } catch (e) { return null; }
  }

  /** VS Code's marshalled URI ({$mid:1, scheme, authority, path, query, fragment}) → string. */
  function uriToString(u) {
    if (!u) return null;
    if (typeof u === 'string') return u;
    if (typeof u.external === 'string') return u.external;
    if (typeof u.scheme !== 'string') return null;
    var s = u.scheme + ':';
    if (u.authority || u.scheme === 'file' || u.scheme === 'vscode-remote') s += '//' + (u.authority || '');
    s += encodeURI(u.path || '');
    if (u.query) s += '?' + u.query;
    if (u.fragment) s += '#' + u.fragment;
    return s;
  }

  function toPos0(p) {
    if (!p || typeof p.lineNumber !== 'number' || typeof p.column !== 'number') return null;
    return { line: Math.max(0, p.lineNumber - 1), column: Math.max(0, p.column - 1) };
  }

  function comparePos(a, b) { return a.line - b.line || a.column - b.column; }

  /** ICodeEditorViewState (or IDiffEditorViewState) → { cursor, selection, scroll }. */
  function fromViewState(vs) {
    var out = { cursor: null, selection: null, scroll: null };
    if (!vs || typeof vs !== 'object') return out;
    if (vs.modified && !vs.cursorState) vs = vs.modified;   // diff editor: modified side
    var cs = Array.isArray(vs.cursorState) ? vs.cursorState[0] : null;
    if (cs) {
      var active = toPos0(cs.position);
      var anchor = toPos0(cs.selectionStart) || active;
      if (active) {
        out.cursor = active;
        out.selection = comparePos(anchor, active) <= 0 ? { start: anchor, end: active }
                                                        : { start: active, end: anchor };
      }
    }
    var v = vs.viewState;
    if (v && (typeof v.scrollTop === 'number' || typeof v.scrollTopWithoutViewZones === 'number')) {
      out.scroll = {
        top: typeof v.scrollTop === 'number' ? v.scrollTop : v.scrollTopWithoutViewZones,
        left: typeof v.scrollLeft === 'number' ? v.scrollLeft : 0
      };
    }
    return out;
  }

  /**
   * Reads what VS Code put on the drag. `getData(type)` must be a function returning the
   * string for a type (dataTransfer.getData during dragstart, or a stub in tests).
   */
  function extractFromTransfer(types, getData) {
    var res = {
      fileUri: null, cursor: null, selection: null, scroll: null,
      editorCount: 0, vscodeNewWindowOp: false, source: 'none'
    };
    types = Array.prototype.slice.call(types || []);
    if (types.indexOf(MIME_RESOURCES) < 0 && types.indexOf(MIME_EDITORS) < 0) return res;
    // 'text/plain' is withheld exactly when VS Code treats this drag as a new-window drag.
    res.vscodeNewWindowOp = types.indexOf('text/plain') < 0;

    var resources = safeJson(getData(MIME_RESOURCES));
    var editors = safeJson(getData(MIME_EDITORS));
    if (Array.isArray(editors)) res.editorCount = editors.length;
    else if (Array.isArray(resources)) res.editorCount = resources.length;

    if (Array.isArray(resources) && typeof resources[0] === 'string') res.fileUri = resources[0];
    var ed = Array.isArray(editors) ? editors[0] : null;
    if (ed) {
      if (!res.fileUri) res.fileUri = uriToString(ed.resource);
      var st = fromViewState(ed.options && ed.options.viewState);
      res.cursor = st.cursor; res.selection = st.selection; res.scroll = st.scroll;
      if (!res.cursor && ed.options && ed.options.selection) {
        var sel = ed.options.selection;   // ITextEditorSelection (1-based)
        res.cursor = toPos0({ lineNumber: sel.startLineNumber, column: sel.startColumn });
      }
    }
    if (res.fileUri) res.source = 'dataTransfer';
    return res;
  }

  /** "Ln 42, Col 7" / "줄 42, 열 7" / "Ln 42, Col 7 (5 selected)" → 0-based cursor. */
  function parseStatusSelection(text) {
    var m = /(\d+)\D+(\d+)/.exec(String(text || ''));
    if (!m) return null;
    return { line: Math.max(0, parseInt(m[1], 10) - 1), column: Math.max(0, parseInt(m[2], 10) - 1) };
  }

  // ---------------------------------------------------------------------------------------
  // Messages (FR-B2, FR-B4)
  // ---------------------------------------------------------------------------------------
  function now() {
    try { return performance.timeOrigin + performance.now(); } catch (e) { return Date.now(); }
  }

  function buildDetachMessage(o) {
    return {
      kind: 'tab_detach',
      version: VERSIONS.tab_detach,
      sourceWindowId: o.sourceWindowId,
      workspace: o.folder ? { folder: o.folder } : null,
      fileUri: o.fileUri,
      cursor: o.cursor || null,
      scroll: o.scroll || null,
      selection: o.selection || null,
      // Additive fields (FR-B4: no version bump). The host may ignore them.
      screen: o.screen || null,
      label: o.label || null,
      editor: EDITOR_KIND,
      stateSource: o.stateSource || 'none',
      sentAtMs: typeof o.sentAtMs === 'number' ? o.sentAtMs : now()
    };
  }

  /**
   * Checks an incoming message against the versions this script speaks. A mismatch is
   * logged and the message is still delivered (FR-B2: "logged, not dropped").
   * Returns { message, mismatch } or null for unparseable input.
   */
  function decodeIncoming(data, log) {
    var msg = typeof data === 'string' ? safeJson(data) : data;
    if (!msg || typeof msg !== 'object' || typeof msg.kind !== 'string') {
      if (log) log('ember-bridge: unparseable message dropped', data);
      return null;
    }
    var expected = VERSIONS[msg.kind];
    var mismatch = null;
    if (expected === undefined) {
      mismatch = { kind: msg.kind, received: msg.version, expected: null };
      if (log) log('ember-bridge: unknown message kind "' + msg.kind + '" (version ' + msg.version + ')');
    } else if (msg.version !== expected) {
      mismatch = { kind: msg.kind, received: msg.version, expected: expected };
      if (log) log('ember-bridge: version mismatch for ' + msg.kind + ': received ' + msg.version +
                   ', expected ' + expected + '; delivering anyway');
    }
    return { message: msg, mismatch: mismatch };
  }

  // ---------------------------------------------------------------------------------------
  // The browser-fallback URL (VS Code Web's own workspace + payload query)
  //
  // src/vs/code/browser/workbench/workbench.ts (WorkspaceProvider): `?folder=` opens the
  // folder, `?payload=` is marshalling-parsed into [key, value][] pairs.
  // src/vs/workbench/services/environment/browser/environmentService.ts:
  //   filesToOpenOrCreate: payload 'openFile' is URI.parse'd; with 'gotoLineMode' present,
  //   `:line:column` (1-based) is split off the URI path and becomes the selection start.
  // src/vs/workbench/browser/layout.ts: when files are passed this way, the workspace's
  //   previously open editors are NOT restored (unless window.restoreWindows = preserve).
  // Keep in sync with OpenWindow::vscode_web_path in crates/bridge/src/messages.rs; both are
  // checked against tests/vectors/bridge/detach_vectors.json.
  // ---------------------------------------------------------------------------------------
  function buildWorkspaceQuery(target) {
    if (!target || !target.folder) return null;
    var q = '?folder=' + encodeURIComponent(target.folder);
    if (target.fileUri) {
      var payload;
      var c = target.cursor;
      // A suffix after a query or fragment would land in the wrong URI component.
      if (c && !/[?#]/.test(target.fileUri)) {
        payload = [['openFile', target.fileUri + ':' + (c.line + 1) + ':' + (c.column + 1)],
                   ['gotoLineMode', 'true']];
      } else {
        payload = [['openFile', target.fileUri]];
      }
      q += '&payload=' + encodeURIComponent(JSON.stringify(payload));
    }
    return q;
  }

  // ---------------------------------------------------------------------------------------
  // Transport selection (one function, three hosts)
  // ---------------------------------------------------------------------------------------
  /** Candidate windows: ourselves, then the wrapper (frame.html), then the top. */
  function hostWindows(win) {
    var out = [];
    [function () { return win; }, function () { return win.parent; }, function () { return win.top; }]
      .forEach(function (get) {
        try { var w = get(); if (w && out.indexOf(w) < 0) out.push(w); } catch (e) { /* cross-origin */ }
      });
    return out;
  }

  /**
   * The native message API, if this page runs inside the Ember shell.
   *   macOS/iOS  WKScriptMessageHandler: window.webkit.messageHandlers.emberBridge
   *   Windows    WebView2:               window.chrome.webview.postMessage
   * Payloads go over as a JSON string so both hosts decode the same bytes.
   */
  function pickTransport(win) {
    var ws = hostWindows(win);
    for (var i = 0; i < ws.length; i++) {
      var w = ws[i];
      try {
        var h = w.webkit && w.webkit.messageHandlers && w.webkit.messageHandlers[HANDLER_NAME];
        if (h && typeof h.postMessage === 'function') {
          return { name: 'webkit', post: function (s) { h.postMessage(s); } };
        }
      } catch (e) { /* ignore */ }
      try {
        var wv = w.chrome && w.chrome.webview;
        if (wv && typeof wv.postMessage === 'function') {
          return { name: 'webview2', post: function (s) { wv.postMessage(s); } };
        }
      } catch (e) { /* ignore */ }
    }
    return null;
  }

  /**
   * THE send function. Native host when present, otherwise the browser fallback.
   * `fallback(msg)` is called with the message when no native host exists; it returns
   * true if it handled it. Returns { transport, ok }.
   */
  function send(win, msg, fallback) {
    var t = pickTransport(win);
    if (t) {
      try { t.post(JSON.stringify(msg)); return { transport: t.name, ok: true }; }
      catch (e) { return { transport: t.name, ok: false, error: String(e) }; }
    }
    if (typeof fallback === 'function') return { transport: 'browser', ok: !!fallback(msg) };
    return { transport: 'none', ok: false };
  }

  // ---------------------------------------------------------------------------------------
  // DOM wiring (only in a browser)
  // ---------------------------------------------------------------------------------------
  function install(win) {
    var doc = win.document;
    if (win.__emberDetachInstalled) return;
    win.__emberDetachInstalled = true;

    var log = function () { try { win.console.warn.apply(win.console, arguments); } catch (e) {} };
    var tracker = createTracker();
    var drag = null;   // { tab, state, label }

    function windowId() {
      var key = 'ember_window_id';
      try {
        var v = win.sessionStorage.getItem(key);
        if (v) return v;
        v = (win.crypto && win.crypto.randomUUID) ? win.crypto.randomUUID()
          : 'w-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2);
        win.sessionStorage.setItem(key, v);
        return v;
      } catch (e) { return 'w-unknown'; }
    }
    function folder() {
      try { return new URL(win.location.href).searchParams.get('folder'); } catch (e) { return null; }
    }
    function stripRect(tab) {
      var strip = tab.closest('.tabs-and-actions-container') || tab.closest('.tabs-container') ||
                  tab.parentElement;
      var r = strip ? strip.getBoundingClientRect() : null;
      return r ? { left: r.left, top: r.top, right: r.right, bottom: r.bottom } : null;
    }
    function basename(uri) {
      try { return decodeURIComponent(String(uri).split(/[?#]/)[0].split('/').pop() || ''); }
      catch (e) { return ''; }
    }

    /** DOM fallback for a visible editor of the tab's group. */
    function domState(tab) {
      var out = { fileUri: null, cursor: null, selection: null, scroll: null };
      var group = tab.closest('.editor-group-container');
      if (!group || !tab.classList.contains('active')) return out;
      var name = tab.getAttribute('data-resource-name') || '';
      var eds = group.querySelectorAll('.editor-container .monaco-editor[data-uri]');
      var ed = null;
      for (var i = 0; i < eds.length; i++) {
        if (!name || basename(eds[i].getAttribute('data-uri')) === name) { ed = eds[i]; break; }
      }
      if (!ed) return out;
      out.fileUri = ed.getAttribute('data-uri');
      var lines = ed.querySelector('.lines-content');
      if (lines) {
        var top = parseFloat(lines.style.top), left = parseFloat(lines.style.left);
        if (!isNaN(top)) out.scroll = { top: -top, left: isNaN(left) ? 0 : -left };
      }
      if (group.classList.contains('active')) {
        var sb = doc.getElementById('status.editor.selection');
        if (sb) out.cursor = parseStatusSelection(sb.textContent);
      }
      return out;
    }

    function closeTab(tab) {
      var btn = tab.querySelector('.tab-actions .action-label');
      if (btn) { btn.click(); return true; }
      // Middle click closes a tab in VS Code (tab MOUSE_UP with button 1).
      try {
        tab.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 1 }));
        tab.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 1 }));
        tab.dispatchEvent(new MouseEvent('auxclick', { bubbles: true, button: 1 }));
        return true;
      } catch (e) { return false; }
    }

    function toast(text, actionLabel, action) {
      var host = doc.body; if (!host) return;
      var el = doc.createElement('div');
      el.setAttribute('role', 'status');
      el.style.cssText = 'position:fixed;right:16px;bottom:40px;z-index:100000;max-width:360px;' +
        'padding:10px 12px;border-radius:6px;font:13px/1.4 system-ui,sans-serif;' +
        'background:var(--vscode-notifications-background,#252526);' +
        'color:var(--vscode-notifications-foreground,#ccc);' +
        'box-shadow:0 2px 8px rgba(0,0,0,.36);display:flex;gap:10px;align-items:center';
      var span = doc.createElement('span'); span.textContent = text; el.appendChild(span);
      if (actionLabel) {
        var b = doc.createElement('button');
        b.textContent = actionLabel;
        b.style.cssText = 'flex:none;cursor:pointer;border:0;border-radius:4px;padding:4px 10px;' +
          'background:var(--vscode-button-background,#0e639c);color:var(--vscode-button-foreground,#fff)';
        b.addEventListener('click', function () { el.remove(); action(); });
        el.appendChild(b);
      }
      host.appendChild(el);
      win.setTimeout(function () { el.remove(); }, 8000);
    }

    function openBrowserWindow(msg, tab) {
      var q = buildWorkspaceQuery({ folder: msg.workspace && msg.workspace.folder,
                                    fileUri: msg.fileUri, cursor: msg.cursor });
      if (!q) return false;
      var top = win; try { top = win.top; void top.location.href; } catch (e) { top = win; }
      var url = win.location.origin + '/' + q;
      var w = Math.round(top.outerWidth * 0.8), h = Math.round(top.outerHeight * 0.8);
      var x = msg.screen ? Math.round(msg.screen.x - w / 2) : top.screenX + 40;
      var y = msg.screen ? Math.round(msg.screen.y - 30) : top.screenY + 40;
      var features = 'popup=yes,width=' + w + ',height=' + h + ',left=' + Math.max(0, x) +
                     ',top=' + Math.max(0, y);
      function go() {
        // No 'noopener' in features: with it window.open always returns null and a blocked
        // popup becomes indistinguishable from success. The opener link is cut right after.
        var nw = win.open(url, '_blank', features);
        if (!nw) return false;
        try { nw.opener = null; } catch (e) {}
        closeTab(tab);
        return true;
      }
      if (go()) return true;
      // Blocked: dragend is not an activation-triggering event; the drag's mousedown only
      // carries transient activation for a few seconds. A click restores it.
      toast('Pop-up blocked. Open the detached tab in a new window?', 'Open', go);
      return true;
    }

    // dragstart: capture on the window so we see it first, then read the data VS Code
    // writes from a listener on the tab itself, added now, so it runs after VS Code's
    // own target-phase listener while the DataTransfer is still writable/readable.
    win.addEventListener('dragstart', function (e) {
      tracker.reset(); drag = null;
      var tab = e.target && e.target.closest && e.target.closest('.tabs-container .tab');
      if (!tab) return;
      tab.addEventListener('dragstart', function (ev) {
        var dt = ev.dataTransfer;
        var st = dt ? extractFromTransfer(dt.types, function (t) {
          try { return dt.getData(t); } catch (err) { return ''; }
        }) : extractFromTransfer([], null);
        if (!st.fileUri) {
          var d = domState(tab);
          if (d.fileUri) {
            st.fileUri = d.fileUri; st.cursor = d.cursor; st.scroll = d.scroll;
            st.selection = d.cursor ? { start: d.cursor, end: d.cursor } : null;
            st.editorCount = 1; st.source = 'dom';
          }
        } else if (!st.cursor || !st.scroll) {
          var d2 = domState(tab);
          if (!st.cursor && d2.cursor && basename(d2.fileUri) === basename(st.fileUri)) st.cursor = d2.cursor;
          if (!st.scroll && d2.scroll && d2.fileUri === st.fileUri) st.scroll = d2.scroll;
        }
        drag = { tab: tab, state: st,
                 label: tab.getAttribute('data-resource-name') || (tab.textContent || '').trim() };
        tracker.begin({
          strip: stripRect(tab),
          point: { x: ev.clientX, y: ev.clientY },
          altKey: ev.altKey,
          vscodeNewWindowOp: st.vscodeNewWindowOp,
          editorCount: st.editorCount || 1,
          dirty: tab.classList.contains('dirty'),
          hasUri: !!st.fileUri
        });
      }, { once: true });
    }, true);

    doc.addEventListener('dragover', function (e) {
      if (tracker.dragging()) tracker.over({ x: e.clientX, y: e.clientY });
    }, true);
    doc.addEventListener('dragleave', function (e) {
      // relatedTarget null = the pointer left this document (out of the iframe/window).
      if (tracker.dragging() && !e.relatedTarget) tracker.leave();
    }, true);

    win.addEventListener('dragend', function (e) {
      if (!tracker.dragging() || !drag) { tracker.reset(); return; }
      var d = drag; drag = null;
      var dt = e.dataTransfer;
      var hasScreen = !!(e.screenX || e.screenY);
      var screen = hasScreen ? { x: e.screenX, y: e.screenY } : null;
      var topWin = win; try { topWin = win.top; void topWin.screenX; } catch (err) { topWin = win; }
      var decision = tracker.end({
        dropEffect: dt ? dt.dropEffect : 'none',
        // Firefox reports 0,0 client coords on dragend; fall back to the last dragover.
        point: (e.clientX || e.clientY) ? { x: e.clientX, y: e.clientY } : null,
        insideWindow: screen ? insideWindowBounds(screen, topWin) : undefined,
        altKey: e.altKey
      });
      if (!decision.detach) {
        if (decision.reason === 'dirty') toast('Save "' + d.label + '" before detaching it.');
        return;
      }
      var msg = buildDetachMessage({
        sourceWindowId: windowId(), folder: folder(), fileUri: d.state.fileUri,
        cursor: d.state.cursor, selection: d.state.selection, scroll: d.state.scroll,
        screen: screen, label: d.label, stateSource: d.state.source
      });
      var r = send(win, msg, function (m) { return openBrowserWindow(m, d.tab); });
      // Native host: the shell opens the window (FR-B3); the tab is not dirty, so closing
      // it here loses nothing.
      if (r.transport !== 'browser' && r.ok) closeTab(d.tab);
      if (!r.ok) log('ember-bridge: detach not delivered', r);
    }, true);

    // ---- native → webview (FR-B4) ----
    function receive(data) {
      var res = decodeIncoming(data, log);
      if (!res) return;
      try { win.dispatchEvent(new CustomEvent('ember-bridge', { detail: res })); } catch (e) {}
    }
    win.__emberBridge = {
      versions: VERSIONS,
      handler: HANDLER_NAME,
      receive: receive,   // WKWebView: evaluateJavaScript("__emberBridge.receive(...)")
      send: function (msg) { return send(win, msg, null); }
    };
    hostWindows(win).forEach(function (w) {
      try {
        if (w.chrome && w.chrome.webview && w.chrome.webview.addEventListener) {
          w.chrome.webview.addEventListener('message', function (ev) { receive(ev.data); });
        }
      } catch (e) {}
    });
  }

  return {
    VERSIONS: VERSIONS,
    HANDLER_NAME: HANDLER_NAME,
    THRESHOLD_PX: THRESHOLD_PX,
    distanceToRect: distanceToRect,
    insideWindowBounds: insideWindowBounds,
    createTracker: createTracker,
    uriToString: uriToString,
    fromViewState: fromViewState,
    extractFromTransfer: extractFromTransfer,
    parseStatusSelection: parseStatusSelection,
    buildDetachMessage: buildDetachMessage,
    decodeIncoming: decodeIncoming,
    buildWorkspaceQuery: buildWorkspaceQuery,
    pickTransport: pickTransport,
    send: send,
    install: install
  };
});
