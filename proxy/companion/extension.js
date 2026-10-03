// Ember persistent terminals — the VS Code Web companion (SPEC §P, docs/design/TERMINALS.md).
//
// A web extension (package.json "browser"): it runs in VS Code Web's web worker extension host,
// in the browser, and patches nothing in VS Code. Integrated terminals become Pseudoterminals
// whose process is a persistent session owned by ember node on the workspace's computer,
// reached through the DarkPyonix proxy on the same origin:
//
//   xterm.js ⇄ Pseudoterminal (here) ⇄ wss://<proxy>/__terms/{id}/attach ⇄ ember node ⇄ PTY
//
// VS Code APIs used: window.registerTerminalProfileProvider + contributes.terminal.profiles
// (the "Ember (persistent)" profile), Pseudoterminal (open / handleInput / setDimensions /
// close, onDidWrite / onDidClose / onDidChangeName), window.createTerminal({ pty, isTransient })
// to re-show sessions after a reopen, window.onDidCloseTerminal + TerminalExitReason to tell
// "user closed the terminal" (kill) from "window went away" (detach), Terminal.processId to
// recognise ember-term task terminals of this window, and workspace.getConfiguration().update
// for terminal.integrated.defaultProfile.* / automationProfile.* (tasks run through the
// ember-term binary, see the setup command).
//
// Everything above `activate` is free of the `vscode` module, so `node --test` can exercise it.
'use strict';

// ---------------------------------------------------------------------------------------------
// Encoding

function bytesToBase64(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}

function base64ToBytes(b64) {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

const encoder = new TextEncoder();

/** A minimal event emitter whose `event` has the shape of vscode.Event<T>. */
class Emitter {
  constructor() {
    this.listeners = new Set();
    this.event = (listener, thisArg, disposables) => {
      const fn = thisArg ? listener.bind(thisArg) : listener;
      this.listeners.add(fn);
      const d = { dispose: () => this.listeners.delete(fn) };
      if (Array.isArray(disposables)) disposables.push(d);
      return d;
    };
  }
  fire(value) {
    for (const l of [...this.listeners]) l(value);
  }
  dispose() {
    this.listeners.clear();
  }
}

// ---------------------------------------------------------------------------------------------
// The proxy's /__terms API

function wsUrl(base, path) {
  if (base.startsWith('https://')) return 'wss://' + base.slice(8) + path;
  if (base.startsWith('http://')) return 'ws://' + base.slice(7) + path;
  return base + path;
}

class TermApi {
  /** `fetch` and `WebSocket` are injected (globals in the web worker; fakes in tests). */
  constructor({ baseUrl, fetch, WebSocket }) {
    this.baseUrl = (baseUrl || '').replace(/\/+$/, '');
    this.fetch = fetch;
    this.WebSocket = WebSocket;
  }

  async request(method, path, body) {
    const init = { method, credentials: 'include', headers: {} };
    if (body !== undefined) {
      init.headers['content-type'] = 'application/json';
      init.body = JSON.stringify(body);
    }
    const r = await this.fetch(this.baseUrl + path, init);
    const text = await r.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = null;
    }
    if (!r.ok) {
      const err = new Error((data && data.error) || `HTTP ${r.status}`);
      err.status = r.status;
      throw err;
    }
    return data;
  }

  list(query = {}) {
    const qs = Object.entries(query)
      .filter(([, v]) => v !== undefined && v !== null && v !== '')
      .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
      .join('&');
    return this.request('GET', '/__terms' + (qs ? '?' + qs : ''));
  }
  create(body) {
    return this.request('POST', '/__terms', body);
  }
  get(id) {
    return this.request('GET', `/__terms/${encodeURIComponent(id)}`);
  }
  kill(id, signal) {
    return this.request('POST', `/__terms/${encodeURIComponent(id)}/kill`, signal ? { signal } : {});
  }
  remove(id) {
    return this.request('DELETE', `/__terms/${encodeURIComponent(id)}`);
  }
  control(id, client, take) {
    return this.request('POST', `/__terms/${encodeURIComponent(id)}/control`, { client, take });
  }
  connect(id) {
    return new this.WebSocket(wsUrl(this.baseUrl, `/__terms/${encodeURIComponent(id)}/attach`));
  }
}

// ---------------------------------------------------------------------------------------------
// One terminal tab ⇄ one node session

const RECONNECT_MS = [250, 500, 1000, 2000, 5000, 10000];
const MAX_QUEUED_INPUT = 1 << 20;
const NOTICE_EVERY_MS = 3000;

/**
 * A vscode.Pseudoterminal attached to a persistent session. It either creates the session on
 * `open` (`create` given) or attaches to `sessionId`. Closing it only detaches.
 */
class EmberPty {
  constructor({ api, device, sessionId, create, passive = false, notify = () => {}, onSession = () => {}, onCreateSettled = () => {}, now = Date.now, setTimeout: st = setTimeout }) {
    this.api = api;
    this.device = device;
    this.sessionId = sessionId || null;
    this.createBody = create || null;
    this.passive = passive;
    this.notify = notify;
    this.onSession = onSession;
    this.onCreateSettled = onCreateSettled;
    this.now = now;
    this.setTimeout = st;

    this.writeEmitter = new Emitter();
    this.closeEmitter = new Emitter();
    this.nameEmitter = new Emitter();
    this.onDidWrite = this.writeEmitter.event;
    this.onDidClose = this.closeEmitter.event;
    this.onDidChangeName = this.nameEmitter.event;

    this.dims = null;
    this.ws = null;
    this.client = null;
    this.term = null;
    this.controller = null;
    this.clients = [];
    this.queue = [];
    this.queued = 0;
    this.decoder = new TextDecoder();
    this.attachedOnce = false;
    this.attempt = 0;
    this.exited = false;
    this.closed = false;
    this.fatal = false;
    this.lastNotice = -Infinity;
    this.lastName = null;
  }

  // --- vscode.Pseudoterminal -----------------------------------------------------------------

  async open(initialDimensions) {
    this.dims = initialDimensions ? { rows: initialDimensions.rows, cols: initialDimensions.columns } : null;
    if (!this.sessionId) {
      try {
        const body = Object.assign({}, this.createBody, this.dims ? { size: this.dims } : {});
        const res = await this.api.create(body);
        this.sessionId = res.term.id;
        this.term = res.term;
        this.onSession(this);
      } catch (e) {
        this.fatal = true;
        this.writeError(`could not start a persistent terminal: ${e.message}`);
        return;
      } finally {
        this.onCreateSettled();
      }
    } else {
      this.onSession(this);
    }
    this.connect();
  }

  handleInput(data) {
    if (this.exited || this.closed) return;
    this.send({ type: 'input', data: bytesToBase64(encoder.encode(data)) }, true);
  }

  setDimensions(d) {
    this.dims = { rows: d.rows, cols: d.columns };
    this.send({ type: 'resize', size: this.dims }, false);
  }

  close() {
    // The terminal tab went away (user close, window close or reload): detach only. Killing on
    // a user close is decided by the extension through onDidCloseTerminal's exit reason.
    this.closed = true;
    this.send({ type: 'detach' }, false);
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        /* already closed */
      }
    }
  }

  // --- control -------------------------------------------------------------------------------

  takeControl() {
    this.send({ type: 'take_control' }, false);
  }
  releaseControl() {
    this.send({ type: 'release_control' }, false);
  }
  hasControl() {
    return !!(this.controller && this.controller.client === this.client);
  }

  // --- connection ----------------------------------------------------------------------------

  connect() {
    if (this.closed || this.exited || this.fatal) return;
    const ws = this.api.connect(this.sessionId);
    this.ws = ws;
    this.client = null;
    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          device: this.device,
          kind: 'vscode-companion',
          size: this.dims || undefined,
          // A background re-show or a reconnect must not take the PTY to this window's size.
          active: !this.passive && !this.attachedOnce,
          read_only: false,
          snapshot: true,
        }),
      );
    };
    ws.onmessage = (m) => {
      let ev;
      try {
        ev = JSON.parse(typeof m.data === 'string' ? m.data : String(m.data));
      } catch {
        return;
      }
      this.handleEvent(ev);
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.client = null;
      if (this.closed || this.exited || this.fatal) return;
      const delay = RECONNECT_MS[Math.min(this.attempt, RECONNECT_MS.length - 1)];
      if (this.attempt === 2) this.writeNote('connection to ember node lost; reconnecting…');
      this.attempt++;
      this.setTimeout(() => this.connect(), delay);
    };
    ws.onerror = () => {
      /* onclose follows */
    };
  }

  send(msg, queueIfNotReady) {
    const ready = this.ws && this.client !== null && this.ws.readyState === 1;
    if (ready) {
      this.ws.send(JSON.stringify(msg));
    } else if (queueIfNotReady && this.queued < MAX_QUEUED_INPUT) {
      const s = JSON.stringify(msg);
      this.queue.push(s);
      this.queued += s.length;
    }
  }

  handleEvent(ev) {
    switch (ev.type) {
      case 'attached': {
        this.client = ev.client;
        this.term = ev.term;
        this.controller = ev.term.controller || null;
        this.clients = ev.term.clients || [];
        this.attachedOnce = true;
        this.attempt = 0;
        for (const s of this.queue) this.ws.send(s);
        this.queue = [];
        this.queued = 0;
        this.updateName();
        break;
      }
      case 'snapshot':
        // Starts with a full reset (ESC c), so a re-attach redraws from scratch.
        this.decoder = new TextDecoder();
        this.writeEmitter.fire(this.decoder.decode(base64ToBytes(ev.data), { stream: true }));
        break;
      case 'output':
        this.writeEmitter.fire(this.decoder.decode(base64ToBytes(ev.data), { stream: true }));
        break;
      case 'control':
        this.controller = ev.controller || null;
        this.updateName();
        break;
      case 'clients':
        this.clients = ev.clients || [];
        break;
      case 'title':
        if (this.term) this.term.title = ev.title || this.term.title;
        this.updateName();
        break;
      case 'refused': {
        this.controller = ev.controller || this.controller;
        const t = this.now();
        if (t - this.lastNotice >= NOTICE_EVERY_MS) {
          this.lastNotice = t;
          this.notify(refusalText(ev.reason, ev.controller));
        }
        this.updateName();
        break;
      }
      case 'exit': {
        this.exited = true;
        const code = ev.code;
        const status = code !== null && code !== undefined ? `exit code ${code}` : ev.signal ? `signal ${ev.signal}` : 'exited';
        this.writeNote(`process ended (${status})`);
        const task = this.term && this.term.tags && this.term.tags['ember-term.mode'] === 'task';
        if (code === 0 && !task) this.closeEmitter.fire(0);
        break;
      }
      case 'error':
        if (/fell behind/.test(ev.message || '')) {
          // Dropped for being slow: the reconnect fetches a fresh snapshot.
          break;
        }
        this.fatal = true;
        this.writeError(ev.message || 'error');
        break;
      default:
        break;
    }
  }

  displayName() {
    const base = (this.term && this.term.title) || 'terminal';
    if (this.controller && this.controller.client !== this.client) return `${base} — controlled by ${this.controller.device}`;
    if (this.controller && this.controller.client === this.client) return `${base} — in control`;
    return base;
  }

  updateName() {
    const n = this.displayName();
    if (n !== this.lastName) {
      this.lastName = n;
      this.nameEmitter.fire(n);
    }
  }

  writeNote(text) {
    this.writeEmitter.fire(`\r\n\x1b[2m[${text}]\x1b[0m\r\n`);
  }
  writeError(text) {
    this.writeEmitter.fire(`\r\n\x1b[31m[${text}]\x1b[0m\r\n`);
  }
}

function refusalText(reason, controller) {
  if (reason === 'controlled') return `Terminal is controlled by ${controller ? controller.device : 'another device'}; your input was not sent.`;
  if (reason === 'read_only') return 'This terminal is attached read-only.';
  if (reason === 'not_running') return 'The terminal session has ended.';
  return 'Input was not sent.';
}

// ---------------------------------------------------------------------------------------------
// Which sessions this window shows on its own

const RECENT_TASK_MS = 10 * 60 * 1000;

/**
 * Sessions to show in this window without being asked: this project's running VS Code
 * terminals, and VS Code tasks that finished in the last minutes (their output), minus those
 * already shown here, those dismissed here, and ember-term sessions whose ember-term runs in one
 * of this window's own terminals (`localPids`: Terminal.processId of this window's terminals).
 */
function planAutoAttach(sessions, { project, shown, dismissed, localPids, now }) {
  return sessions.filter((s) => {
    if (s.origin !== 'ide-vscode') return false;
    if (project && s.project !== project) return false;
    if (shown.has(s.id) || (dismissed && dismissed.has(s.id))) return false;
    const tags = s.tags || {};
    const termPid = tags['ember-term.pid'] ? Number(tags['ember-term.pid']) : null;
    if (termPid && localPids.has(termPid)) return false;
    if ((s.clients || []).some((c) => c.kind === 'ember-term' && c.pid && localPids.has(c.pid))) return false;
    if (s.state === 'running') return true;
    return (
      s.state === 'exited' &&
      tags['ember-term.mode'] === 'task' &&
      s.has_screen &&
      s.finished_ms !== null &&
      now - s.finished_ms < RECENT_TASK_MS
    );
  });
}

function deviceLabel(override, userAgent, machineId) {
  if (override) return override;
  const ua = userAgent || '';
  const kind = /iPad/.test(ua)
    ? 'iPad'
    : /iPhone/.test(ua)
      ? 'iPhone'
      : /Android/.test(ua)
        ? 'Android'
        : /Macintosh|Mac OS X/.test(ua)
          ? 'Mac'
          : /Windows/.test(ua)
            ? 'Windows'
            : /Linux|X11|CrOS/.test(ua)
              ? 'Linux'
              : 'browser';
  return `VS Code on ${kind}` + (machineId ? ` (${String(machineId).slice(0, 6)})` : '');
}

function randomId() {
  if (globalThis.crypto && globalThis.crypto.randomUUID) return globalThis.crypto.randomUUID();
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

// ---------------------------------------------------------------------------------------------
// Activation (the only part that needs the `vscode` module)

const PROFILE_ID = 'ember.terminal';
const PROFILE_TITLE = 'Ember (persistent)';
const POLL_MS = 5000;
const DISMISSED_KEY = 'ember.terminals.dismissed';

let deactivating = false;

function activate(context) {
  const vscode = require('vscode');
  const cfg = () => vscode.workspace.getConfiguration('ember.terminals');
  const origin = globalThis.location && globalThis.location.origin && globalThis.location.origin !== 'null' ? globalThis.location.origin : '';
  const api = new TermApi({
    baseUrl: cfg().get('proxyUrl') || origin,
    fetch: globalThis.fetch.bind(globalThis),
    WebSocket: globalThis.WebSocket,
  });
  const device = deviceLabel(cfg().get('device'), globalThis.navigator && globalThis.navigator.userAgent, vscode.env.machineId);
  const shown = new Map(); // session id -> EmberPty
  let pendingCreates = 0;
  let polling = false;

  const project = () => {
    const f = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
    return f ? f.uri.path : undefined;
  };
  const dismissed = () => new Set(context.workspaceState.get(DISMISSED_KEY, []));
  const dismiss = (id) => {
    const list = context.workspaceState.get(DISMISSED_KEY, []).filter((x) => x !== id).concat([id]).slice(-200);
    context.workspaceState.update(DISMISSED_KEY, list);
  };
  const notify = (msg) => vscode.window.showWarningMessage(msg);
  const ptyOf = (t) => {
    const p = t && t.creationOptions && t.creationOptions.pty;
    return p instanceof EmberPty ? p : null;
  };

  function createBody() {
    const p = project();
    const os = /Mac/.test((globalThis.navigator && globalThis.navigator.userAgent) || '') ? 'osx' : 'linux';
    // terminal.integrated.env.* applies to ordinary terminals; pass it on (variables such as
    // ${workspaceFolder} are not resolved here).
    const env = Object.assign({}, vscode.workspace.getConfiguration('terminal.integrated').get(`env.${os}`) || {});
    if (p) env.EMBER_PROJECT = p;
    return {
      cwd: p,
      project: p,
      origin: 'ide-vscode',
      key: `vscode:${randomId()}`,
      env,
      tags: { 'vscode.device': device, 'vscode.companion': '1' },
    };
  }

  function makePty(opts) {
    if (opts.create) pendingCreates++;
    const pty = new EmberPty(
      Object.assign({ api, device, notify }, opts, {
        onSession: (p) => shown.set(p.sessionId, p),
        onCreateSettled: () => pendingCreates--,
      }),
    );
    return pty;
  }

  function showSession(info, { passive, focus }) {
    const existing = shown.get(info.id);
    if (existing) {
      const t = vscode.window.terminals.find((x) => ptyOf(x) === existing);
      if (t && focus) t.show();
      return;
    }
    const pty = makePty({ sessionId: info.id, passive });
    shown.set(info.id, pty);
    const t = vscode.window.createTerminal({ name: info.title || 'terminal', pty, isTransient: true });
    if (focus) t.show();
  }

  async function poll() {
    if (polling || pendingCreates > 0 || !cfg().get('autoAttach')) return;
    polling = true;
    try {
      const sessions = await api.list({ project: project() });
      const pids = await Promise.all(vscode.window.terminals.map((t) => Promise.resolve(t.processId).catch(() => undefined)));
      const localPids = new Set(pids.filter((x) => typeof x === 'number'));
      const plan = planAutoAttach(sessions, { project: project(), shown, dismissed: dismissed(), localPids, now: Date.now() });
      for (const s of plan) showSession(s, { passive: true, focus: false });
    } catch {
      // The proxy or the node is not reachable; try again next round.
    } finally {
      polling = false;
    }
  }

  context.subscriptions.push(
    vscode.window.registerTerminalProfileProvider(PROFILE_ID, {
      provideTerminalProfile() {
        return new vscode.TerminalProfile({ name: 'ember', pty: makePty({ create: createBody() }), isTransient: true });
      },
    }),

    vscode.window.onDidCloseTerminal((t) => {
      const p = ptyOf(t);
      if (!p) return;
      if (p.sessionId) shown.delete(p.sessionId);
      const userClosed = t.exitStatus && t.exitStatus.reason === vscode.TerminalExitReason.User;
      if (!p.sessionId || deactivating || !userClosed || !cfg().get('killOnClose')) {
        // A finished task the user looked at elsewhere stays dismissed only when closed by hand.
        return;
      }
      dismiss(p.sessionId);
      if (p.exited) api.remove(p.sessionId).catch(() => {});
      else api.kill(p.sessionId).catch(() => {});
    }),

    vscode.commands.registerCommand('ember.terminals.new', () => {
      const t = vscode.window.createTerminal({ name: 'ember', pty: makePty({ create: createBody() }), isTransient: true });
      t.show();
    }),

    vscode.commands.registerCommand('ember.terminals.attach', async () => {
      let sessions;
      try {
        sessions = await api.list({});
      } catch (e) {
        vscode.window.showErrorMessage(`Ember: could not list terminals: ${e.message}`);
        return;
      }
      const items = sessions
        .filter((s) => s.state === 'running' || s.has_screen)
        .reverse()
        .map((s) => ({
          label: s.title,
          description: `${s.origin}${s.project ? ' · ' + s.project : ''}`,
          detail:
            `${s.state === 'running' ? 'running' : s.state}` +
            ` · ${(s.clients || []).length} attached` +
            (s.controller ? ` · controlled by ${s.controller.device}` : ''),
          session: s,
        }));
      const pick = await vscode.window.showQuickPick(items, { placeHolder: 'Persistent terminals on this computer' });
      if (pick) showSession(pick.session, { passive: false, focus: true });
    }),

    vscode.commands.registerCommand('ember.terminals.takeControl', () => {
      const p = ptyOf(vscode.window.activeTerminal);
      if (!p) return vscode.window.showInformationMessage('Ember: the active terminal is not a persistent terminal.');
      p.takeControl();
    }),

    vscode.commands.registerCommand('ember.terminals.releaseControl', () => {
      const p = ptyOf(vscode.window.activeTerminal);
      if (p) p.releaseControl();
    }),

    vscode.commands.registerCommand('ember.terminals.kill', async () => {
      const t = vscode.window.activeTerminal;
      const p = ptyOf(t);
      if (!p || !p.sessionId) return;
      await api.kill(p.sessionId).catch((e) => vscode.window.showErrorMessage(`Ember: ${e.message}`));
    }),

    vscode.commands.registerCommand('ember.terminals.setup', async () => {
      const term = vscode.workspace.getConfiguration('terminal.integrated');
      const target = vscode.ConfigurationTarget.Global;
      for (const os of ['osx', 'linux']) {
        await term.update(`defaultProfile.${os}`, PROFILE_TITLE, target);
      }
      const path = await vscode.window.showInputBox({
        prompt: 'Absolute path of ember-term on the workspace computer (for tasks); leave empty to skip',
        value: cfg().get('emberTermPath') || '',
      });
      if (path) {
        await cfg().update('emberTermPath', path, target);
        for (const os of ['osx', 'linux']) {
          await term.update(`automationProfile.${os}`, { path, args: [] }, target);
        }
      }
      vscode.window.showInformationMessage(
        path
          ? 'Ember: new terminals and tasks now run in persistent sessions.'
          : 'Ember: new terminals now run in persistent sessions (tasks unchanged).',
      );
    }),

    vscode.window.onDidChangeWindowState((s) => {
      if (s.focused) poll();
    }),
  );

  const timer = setInterval(() => {
    if (vscode.window.state.focused) poll();
  }, POLL_MS);
  context.subscriptions.push({ dispose: () => clearInterval(timer) });
  poll();
}

function deactivate() {
  // Terminals closing from here on are the window going away, never a user's close.
  deactivating = true;
}

module.exports = {
  activate,
  deactivate,
  // for tests
  Emitter,
  TermApi,
  EmberPty,
  planAutoAttach,
  deviceLabel,
  refusalText,
  bytesToBase64,
  base64ToBytes,
  wsUrl,
};
