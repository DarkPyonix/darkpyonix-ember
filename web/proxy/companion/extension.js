// Ember persistent terminals: the VS Code Web companion (SPEC §P, docs/design/TERMINALS.md).
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
// Debuggees (FR-P5): debug.registerDebugConfigurationProvider('*', {
// resolveDebugConfigurationWithSubstitutedVariables }) turns a node/debugpy `launch` with
// `console: integratedTerminal` into a node session + an `attach` configuration;
// debug.onDidStart/TerminateDebugSession and debug.startDebugging (re-attach). See the section
// "Debuggees in persistent sessions" below.
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
        const tags = (this.term && this.term.tags) || {};
        // Task and debuggee terminals stay open with their output, like VS Code's own.
        const keep = tags['ember-term.mode'] === 'task' || !!tags['vscode.debug.kind'];
        if (code === 0 && !keep) this.closeEmitter.fire(0);
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
    if (this.controller && this.controller.client !== this.client) return `${base} (controlled by ${this.controller.device})`;
    if (this.controller && this.controller.client === this.client) return `${base} (in control)`;
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
// Debuggees in persistent sessions (FR-P5; docs/design/TERMINALS.md §3c)
//
// VS Code serves a debug adapter's `runInTerminal` itself (extension host `$runInTerminal`), and
// an extension cannot answer it: trackers only observe, and a DebugAdapterDescriptorFactory may
// only be registered by the extension that defines the debug type. Even when that terminal is a
// node session (automationProfile = ember-term), a *launch* debuggee dies with its adapter
// (js-debug's watchdog kills the target, debugpy's launcher kills the debuggee). So the companion
// rewrites a `launch` + `console: integratedTerminal` configuration, in
// resolveDebugConfigurationWithSubstitutedVariables, into:
//   1. a node session (origin ide-vscode) running the program with the debugger listening on
//      127.0.0.1, port chosen by the OS (`node --inspect-brk=127.0.0.1:0`, debugpy.listen(0)),
//   2. an `attach` configuration to the port printed by the debuggee.
// An attach session leaves the debuggee running when it ends, and can be started again from any
// window while the debuggee runs ("Re-attach debugger").

const DEBUG_TAG = {
  id: 'vscode.debug.id', // our id; also `__emberDebugId` in the debug configuration
  name: 'vscode.debug.name',
  type: 'vscode.debug.type', // the configuration's type: node, pwa-node, debugpy, python
  kind: 'vscode.debug.kind', // node-inspect | debugpy
  attach: 'vscode.debug.attach', // JSON attach configuration without the endpoint
};
const NODE_TYPES = new Set(['node', 'pwa-node']);
const PY_TYPES = new Set(['debugpy', 'python']);
const READY_TIMEOUT_MS = 30000;

/** POSIX shell quoting. */
function shQuote(s) {
  s = String(s);
  return /^[A-Za-z0-9_\/.,:=+@%-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}

/**
 * Runs `argv` through the user's interactive login shell, as VS Code's runInTerminal does (the
 * command is typed into a shell), so PATH from the shell profile (nvm, pyenv…) applies.
 */
function loginShellArgv(argv) {
  return ['/bin/sh', '-c', 'exec "${SHELL:-/bin/sh}" -l -i -c "$1"', 'ember-debug', argv.map(shQuote).join(' ')];
}

function basename(p) {
  return String(p).split(/[\\/]/).pop();
}

function stringEnv(env) {
  const out = {};
  for (const [k, v] of Object.entries(env || {})) if (v !== null && v !== undefined) out[k] = String(v);
  return out;
}

function pick(obj, keys) {
  const out = {};
  for (const k of keys) if (obj[k] !== undefined) out[k] = obj[k];
  return out;
}

// Debuggee side of debugpy: listen on an OS-chosen port, print it, wait for the client, run the
// program like `python file` / `python -m module`. `debugpy` must be importable (e.g. `uv add
// debugpy`), or EMBER_DEBUGPY_PATH = the directory that contains the debugpy package.
const DEBUGPY_BOOT = [
  'import os, sys, runpy',
  "p = os.environ.pop('EMBER_DEBUGPY_PATH', '')",
  'if p: sys.path.insert(0, p)',
  'import debugpy',
  "h, port = debugpy.listen(('127.0.0.1', 0))",
  "sys.stderr.write('[ember] debugpy listening on %s:%d\\n' % (h, port)); sys.stderr.flush()",
  'debugpy.wait_for_client()',
  'mode, target = sys.argv[1], sys.argv[2]',
  'sys.argv = [target] + sys.argv[3:]',
  "if mode == '-m': runpy.run_module(target, run_name='__main__', alter_sys=True)",
  'else:',
  '    sys.path.insert(0, os.path.dirname(os.path.abspath(target)))',
  "    runpy.run_path(target, run_name='__main__')",
].join('\n');

/**
 * Decides whether a resolved debug configuration is routed into a persistent session, and how.
 * Returns `{ skip: reason }` or `{ kind, create, attach }`: `create` is the POST /__terms body,
 * `attach` the attach configuration without its endpoint (see `attachFor`).
 */
function planDebugLaunch(config, { project, device, debugId, debugpyPath, enabled = true }) {
  if (!enabled) return { skip: 'disabled' };
  if (!config || config.request !== 'launch') return { skip: 'not a launch' };
  if (config.__emberDebugId || config.emberPersistent === false) return { skip: 'opted out' };
  if (config.console !== 'integratedTerminal') return { skip: 'console is not integratedTerminal' };
  const cwd = config.cwd || project;
  const env = stringEnv(config.env);
  if (project) env.EMBER_PROJECT = project;
  const name = config.name || 'debug';
  const common = pick(config, ['cwd', 'skipFiles', 'outFiles', 'sourceMaps', 'resolveSourceMapLocations', 'smartStep', 'sourceMapPathOverrides', 'pauseForSourceMap', 'showAsyncStacks']);
  let kind;
  let argv;
  let attach;
  if (NODE_TYPES.has(config.type)) {
    const runtime = config.runtimeExecutable || 'node';
    if (!/^node(\.exe)?$/i.test(basename(runtime))) return { skip: `runtimeExecutable ${basename(runtime)} is not node` };
    const rest = [].concat(config.program ? [config.program] : [], config.args || []);
    if (typeof config.args === 'string' || rest.length === 0) return { skip: 'no program' };
    kind = 'node-inspect';
    argv = [runtime, '--inspect-brk=127.0.0.1:0', ...(config.runtimeArgs || []), ...rest];
    attach = Object.assign(common, {
      type: config.type,
      request: 'attach',
      name,
      // --inspect-brk waits on the first line until breakpoints are set; then go on.
      continueOnAttach: !config.stopOnEntry,
    });
  } else if (PY_TYPES.has(config.type)) {
    const py = Array.isArray(config.python) ? config.python : [config.python || config.pythonPath || 'python3'];
    const target = config.module ? ['-m', config.module] : config.program ? ['-f', config.program] : null;
    if (!target) return { skip: 'no program or module' };
    if (typeof config.args === 'string') return { skip: 'args as a string' };
    kind = 'debugpy';
    argv = [...py, ...(config.pythonArgs || []), '-c', DEBUGPY_BOOT, ...target, ...(config.args || [])];
    if (debugpyPath) env.EMBER_DEBUGPY_PATH = debugpyPath;
    attach = Object.assign(pick(config, ['cwd', 'justMyCode', 'rules', 'showReturnValue', 'subProcess', 'django', 'jinja', 'pyramid', 'gevent']), {
      type: config.type,
      request: 'attach',
      name,
    });
  } else {
    return { skip: `debug type ${config.type} is not supported` };
  }
  return {
    kind,
    attach,
    create: {
      program: { argv: loginShellArgv(argv) },
      cwd,
      env,
      project,
      origin: 'ide-vscode',
      title: name,
      key: `vscode:debug:${debugId}`,
      tags: {
        'vscode.device': device,
        'vscode.companion': '1',
        [DEBUG_TAG.id]: debugId,
        [DEBUG_TAG.name]: name,
        [DEBUG_TAG.type]: String(config.type),
        [DEBUG_TAG.kind]: kind,
        [DEBUG_TAG.attach]: JSON.stringify(attach),
      },
    },
  };
}

const ANSI = /\x1b\[[0-9;?]*[ -\/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g;

/** The debugger endpoint the debuggee printed, from its terminal text; the last one wins. */
function parseDebugEndpoint(kind, text) {
  const t = String(text || '').replace(ANSI, '').replace(/\r?\n/g, '\n');
  // `Debugger listening on ws://127.0.0.1:9229/<uuid>` (node), our debugpy bootstrap's line.
  const re = kind === 'node-inspect' ? /Debugger listening on ws:\/\/([^\s/]+):(\d+)\//g : /\[ember\] debugpy listening on ([^\s:]+):(\d+)/g;
  let m;
  let last = null;
  while ((m = re.exec(t))) last = { host: m[1].replace(/^\[|\]$/g, ''), port: Number(m[2]) };
  return last;
}

/** The attach configuration for a debuggee listening on `ep`, tied to its session. */
function attachFor(kind, attach, ep, { debugId, termId }) {
  const out = Object.assign({}, attach, { __emberDebugId: debugId, __emberTermId: termId });
  if (kind === 'node-inspect') Object.assign(out, { address: ep.host, port: ep.port });
  else out.connect = { host: ep.host, port: ep.port };
  return out;
}

/** The attach configuration for a running debuggee session (re-attach), or null. */
function reattachConfig(session, text) {
  const tags = session.tags || {};
  const kind = tags[DEBUG_TAG.kind];
  if (!kind || !tags[DEBUG_TAG.attach]) return null;
  const ep = parseDebugEndpoint(kind, text);
  if (!ep) return null;
  let attach;
  try {
    attach = JSON.parse(tags[DEBUG_TAG.attach]);
  } catch {
    return null;
  }
  // Re-attaching never resumes on its own: the debuggee is already past --inspect-brk.
  delete attach.continueOnAttach;
  return attachFor(kind, attach, ep, { debugId: tags[DEBUG_TAG.id], termId: session.id });
}

/**
 * Debuggee sessions to offer "Re-attach debugger" for: running, of this project, with a debug
 * id that no debug session in this window is attached to and that was not offered here yet.
 */
function planReattach(sessions, { project, attached, offered }) {
  return sessions.filter((s) => {
    const tags = s.tags || {};
    const id = tags[DEBUG_TAG.id];
    if (!id || !tags[DEBUG_TAG.kind] || s.state !== 'running' || s.origin !== 'ide-vscode') return false;
    if (project && s.project !== project) return false;
    return !attached.has(id) && !offered.has(id);
  });
}

/**
 * Starts a debuggee in a node session and waits for its debugger endpoint. `api` is a TermApi;
 * `show(term)` shows the session's terminal. Resolves to the attach configuration; rejects with
 * the reason (and kills the session) when the debuggee ends or prints nothing in time.
 */
async function launchDebuggee(plan, { api, debugId, show = () => {}, sleep, now = Date.now, timeoutMs = READY_TIMEOUT_MS, pollMs = 150 }) {
  const res = await api.create(plan.create);
  const term = res.term;
  show(term);
  const deadline = now() + timeoutMs;
  let text = '';
  for (;;) {
    try {
      const snap = await api.request('GET', `/__terms/${encodeURIComponent(term.id)}/snapshot`);
      text = (snap && snap.text) || '';
      const ep = parseDebugEndpoint(plan.kind, text);
      if (ep) return attachFor(plan.kind, plan.attach, ep, { debugId, termId: term.id });
    } catch (e) {
      if (e.status !== 409) throw e; // 409: no screen yet
    }
    const info = await api.get(term.id).catch(() => null);
    if (info && info.state !== 'running') {
      const tail = text.trim().split('\n').slice(-3).join(' / ');
      throw new Error(`the debuggee ended before the debugger could attach${tail ? ': ' + tail : ''}`);
    }
    if (now() >= deadline) {
      await api.kill(term.id).catch(() => {});
      throw new Error('the debuggee did not report a debugger port in time');
    }
    await sleep(pollMs);
  }
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
  const dcfg = () => vscode.workspace.getConfiguration('ember.debug');
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
    if (focus) t.show(focus === 'preserve');
  }

  async function poll() {
    const auto = cfg().get('autoAttach');
    const offer = dcfg().get('offerReattach');
    if (polling || pendingCreates > 0 || (!auto && !offer)) return;
    polling = true;
    try {
      const sessions = await api.list({ project: project() });
      if (auto) {
        const pids = await Promise.all(vscode.window.terminals.map((t) => Promise.resolve(t.processId).catch(() => undefined)));
        const localPids = new Set(pids.filter((x) => typeof x === 'number'));
        const plan = planAutoAttach(sessions, { project: project(), shown, dismissed: dismissed(), localPids, now: Date.now() });
        for (const s of plan) showSession(s, { passive: true, focus: false });
      }
      if (offer) {
        const attached = new Set([...debugSessions.values()].map((d) => d.debugId));
        for (const s of planReattach(sessions, { project: project(), attached, offered: debugOffered })) {
          debugOffered.add(s.tags[DEBUG_TAG.id]);
          offerReattach(s);
        }
      }
    } catch {
      // The proxy or the node is not reachable; try again next round.
    } finally {
      polling = false;
    }
  }

  // --- debuggees (FR-P5) ---------------------------------------------------------------------

  const debugSessions = new Map(); // vscode DebugSession.id -> { debugId, termId, name }
  const debugOffered = new Set(); // debug ids offered for re-attach (or started) in this window

  async function snapshotText(id) {
    const snap = await api.request('GET', `/__terms/${encodeURIComponent(id)}/snapshot`).catch(() => null);
    return (snap && snap.text) || '';
  }

  async function reattach(session) {
    const config = reattachConfig(session, await snapshotText(session.id));
    if (!config) {
      vscode.window.showWarningMessage(`Ember: could not find the debugger port of "${session.title}" in its terminal output.`);
      return false;
    }
    const folder = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
    showSession(session, { passive: false, focus: 'preserve' });
    return vscode.debug.startDebugging(folder, config);
  }

  async function offerReattach(session) {
    const name = session.tags[DEBUG_TAG.name] || session.title;
    const pick = await vscode.window.showInformationMessage(
      `Ember: "${name}" is still running from an earlier debug session. Re-attach the debugger?`,
      'Re-attach',
      'Stop It',
    );
    if (pick === 'Re-attach') await reattach(session);
    else if (pick === 'Stop It') await api.kill(session.id).catch((e) => vscode.window.showErrorMessage(`Ember: ${e.message}`));
  }

  async function afterDebugStop(d) {
    if (deactivating) return; // the window is going away: the debuggee keeps running
    const mode = dcfg().get('onStop');
    if (mode === 'keep') return;
    // The adapter often notices a debuggee's exit before node reports it.
    await new Promise((r) => setTimeout(r, 500));
    const info = await api.get(d.termId).catch(() => null);
    if (deactivating || !info || info.state !== 'running') return;
    if (mode !== 'kill') {
      const pick = await vscode.window.showInformationMessage(
        `Ember: "${d.name}" keeps running in a persistent terminal after the debugger detached.`,
        'Stop It',
        'Keep Running',
      );
      if (pick !== 'Stop It') return;
    }
    await api.kill(d.termId).catch(() => {});
  }

  async function resolveDebuggee(folder, config) {
    const debugId = randomId();
    const plan = planDebugLaunch(config, {
      project: (folder && folder.uri.path) || project(),
      device,
      debugId,
      debugpyPath: dcfg().get('debugpyPath') || undefined,
      enabled: dcfg().get('persistent'),
    });
    if (plan.skip) return config;
    debugOffered.add(debugId);
    pendingCreates++;
    let settled = false;
    try {
      return await launchDebuggee(plan, {
        api,
        debugId,
        sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
        show: (term) => {
          pendingCreates--;
          settled = true;
          showSession(term, { passive: false, focus: 'preserve' });
        },
      });
    } catch (e) {
      if (!settled) {
        // No session was started (ember node or the proxy is not there): debug as usual.
        vscode.window.showWarningMessage(`Ember: "${config.name}" runs without a persistent terminal: ${e.message}`);
        return config;
      }
      vscode.window.showErrorMessage(`Ember: could not start "${config.name}" in a persistent terminal: ${e.message}`);
      return undefined;
    } finally {
      if (!settled) pendingCreates--;
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

    // Every debug type: only `launch` + `console: integratedTerminal` of node/pwa-node and
    // debugpy/python are rewritten (planDebugLaunch); everything else passes through unchanged.
    vscode.debug.registerDebugConfigurationProvider('*', {
      resolveDebugConfigurationWithSubstitutedVariables: (folder, config) => resolveDebuggee(folder, config),
    }),

    vscode.debug.onDidStartDebugSession((s) => {
      const c = s.configuration || {};
      if (c.__emberDebugId) {
        debugSessions.set(s.id, { debugId: c.__emberDebugId, termId: c.__emberTermId, name: c.name });
        debugOffered.add(c.__emberDebugId);
      }
    }),

    vscode.debug.onDidTerminateDebugSession((s) => {
      const d = debugSessions.get(s.id);
      if (!d) return;
      debugSessions.delete(s.id);
      afterDebugStop(d);
    }),

    vscode.commands.registerCommand('ember.debug.reattach', async () => {
      let sessions;
      try {
        sessions = await api.list({ project: project(), running: true });
      } catch (e) {
        vscode.window.showErrorMessage(`Ember: could not list terminals: ${e.message}`);
        return;
      }
      const attached = new Set([...debugSessions.values()].map((d) => d.debugId));
      const items = planReattach(sessions, { project: project(), attached, offered: new Set() }).map((s) => ({
        label: s.tags[DEBUG_TAG.name] || s.title,
        description: s.tags[DEBUG_TAG.type],
        detail: `started on ${s.tags['vscode.device'] || 'another device'}`,
        session: s,
      }));
      if (!items.length) return vscode.window.showInformationMessage('Ember: no running debuggee to re-attach to.');
      const pick = await vscode.window.showQuickPick(items, { placeHolder: 'Running debuggees in persistent terminals' });
      if (pick) await reattach(pick.session);
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
  shQuote,
  loginShellArgv,
  planDebugLaunch,
  parseDebugEndpoint,
  attachFor,
  reattachConfig,
  planReattach,
  launchDebuggee,
  DEBUG_TAG,
  bytesToBase64,
  base64ToBytes,
  wsUrl,
};
