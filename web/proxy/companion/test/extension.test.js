// Unit tests for the companion's vscode-free parts: the /__terms client, the Pseudoterminal
// adapter against a fake attach socket, and the auto-attach plan.
//
//     cd web/proxy/companion && node --test
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const ext = require('../extension.js');

const enc = new TextEncoder();
const b64 = (s) => ext.bytesToBase64(typeof s === 'string' ? enc.encode(s) : s);

class FakeSocket {
  constructor(url) {
    this.url = url;
    this.readyState = 0;
    this.sent = [];
    FakeSocket.all.push(this);
  }
  send(s) {
    this.sent.push(JSON.parse(s));
  }
  close() {
    this.readyState = 3;
    if (this.onclose) this.onclose({});
  }
  // test side
  open() {
    this.readyState = 1;
    this.onopen();
  }
  emit(ev) {
    this.onmessage({ data: JSON.stringify(ev) });
  }
}
FakeSocket.all = [];

function fakeFetch(routes, calls) {
  return async (url, init) => {
    calls.push({ url, method: init.method, body: init.body ? JSON.parse(init.body) : undefined, credentials: init.credentials });
    const key = `${init.method} ${url.replace(/^https?:\/\/[^/]+/, '')}`;
    const [status, body] = routes[key] || [404, { error: 'no route ' + key }];
    return { ok: status < 300, status, text: async () => (body === undefined ? '' : JSON.stringify(body)) };
  };
}

const info = (over = {}) =>
  Object.assign(
    {
      id: 's1', title: 'zsh', origin: 'ide-vscode', project: '/p', state: 'running', tags: {}, clients: [],
      controller: null, has_screen: true, finished_ms: null,
    },
    over,
  );

function setup(opts = {}) {
  FakeSocket.all = [];
  const calls = [];
  const api = new ext.TermApi({
    baseUrl: 'https://proxy.example:8888/',
    fetch: fakeFetch({ 'POST /__terms': [201, { created: true, term: info() }], ...(opts.routes || {}) }, calls),
    WebSocket: FakeSocket,
  });
  const notices = [];
  const timers = [];
  let now = 1000;
  const pty = new ext.EmberPty({
    api,
    device: 'laptop',
    notify: (m) => notices.push(m),
    now: () => now,
    setTimeout: (fn, ms) => timers.push({ fn, ms }),
    ...opts.pty,
  });
  const out = [];
  const names = [];
  const closes = [];
  pty.onDidWrite((s) => out.push(s));
  pty.onDidChangeName((n) => names.push(n));
  pty.onDidClose((c) => closes.push(c));
  return { api, pty, calls, notices, timers, out, names, closes, tick: (ms) => (now += ms) };
}

test('api: urls, credentials, errors', async () => {
  const calls = [];
  const api = new ext.TermApi({
    baseUrl: 'http://h:1/',
    fetch: fakeFetch({ 'GET /__terms?project=%2Fp&running=true': [200, []], 'GET /__terms/x': [404, { error: 'no terminal session x' }] }, calls),
    WebSocket: FakeSocket,
  });
  assert.deepEqual(await api.list({ project: '/p', running: true, origin: undefined }), []);
  assert.equal(calls[0].credentials, 'include');
  await assert.rejects(api.get('x'), (e) => e.status === 404 && /no terminal session/.test(e.message));
  assert.equal(api.connect('a b').url, 'ws://h:1/__terms/a%20b/attach');
  assert.equal(ext.wsUrl('https://h', '/x'), 'wss://h/x');
});

test('creates a session on open, sends hello, writes snapshot and output, queues early input', async () => {
  const t = setup({ pty: { create: { cwd: '/p', origin: 'ide-vscode', key: 'vscode:1' } } });
  await t.pty.open({ rows: 30, columns: 100 });
  assert.deepEqual(t.calls[0].body, { cwd: '/p', origin: 'ide-vscode', key: 'vscode:1', size: { rows: 30, cols: 100 } });
  const ws = FakeSocket.all[0];
  assert.equal(ws.url, 'wss://proxy.example:8888/__terms/s1/attach');

  t.pty.handleInput('ls\r'); // before the socket is ready: queued
  ws.open();
  assert.deepEqual(ws.sent[0], { device: 'laptop', kind: 'vscode-companion', size: { rows: 30, cols: 100 }, active: true, read_only: false, snapshot: true });
  ws.emit({ type: 'attached', client: 7, term: info() });
  assert.deepEqual(ws.sent[1], { type: 'input', data: b64('ls\r') });
  assert.deepEqual(t.names, ['zsh']);

  ws.emit({ type: 'snapshot', size: { rows: 30, cols: 100 }, data: b64('\x1bcold screen') });
  // A UTF-8 character split across two output events is decoded whole.
  const bytes = enc.encode('한');
  ws.emit({ type: 'output', data: ext.bytesToBase64(bytes.subarray(0, 1)) });
  ws.emit({ type: 'output', data: ext.bytesToBase64(bytes.subarray(1)) });
  assert.equal(t.out.join(''), '\x1bcold screen한');

  t.pty.setDimensions({ rows: 20, columns: 70 });
  assert.deepEqual(ws.sent.at(-1), { type: 'resize', size: { rows: 20, cols: 70 } });
});

test('control changes the name; refusals notify, throttled', async () => {
  const t = setup({ pty: { sessionId: 's1', passive: true } });
  await t.pty.open({ rows: 24, columns: 80 });
  const ws = FakeSocket.all[0];
  ws.open();
  assert.equal(ws.sent[0].active, false, 'a background re-show is passive');
  ws.emit({ type: 'attached', client: 2, term: info() });
  ws.emit({ type: 'control', controller: { client: 9, device: 'phone' } });
  assert.equal(t.names.at(-1), 'zsh — controlled by phone');
  t.pty.handleInput('x');
  ws.emit({ type: 'refused', reason: 'controlled', controller: { client: 9, device: 'phone' } });
  ws.emit({ type: 'refused', reason: 'controlled', controller: { client: 9, device: 'phone' } });
  assert.equal(t.notices.length, 1);
  assert.match(t.notices[0], /controlled by phone/);
  t.tick(5000);
  ws.emit({ type: 'refused', reason: 'controlled', controller: { client: 9, device: 'phone' } });
  assert.equal(t.notices.length, 2);

  t.pty.takeControl();
  assert.deepEqual(ws.sent.at(-1), { type: 'take_control' });
  ws.emit({ type: 'control', controller: { client: 2, device: 'laptop' } });
  assert.ok(t.pty.hasControl());
  assert.equal(t.names.at(-1), 'zsh — in control');
  t.pty.releaseControl();
  assert.deepEqual(ws.sent.at(-1), { type: 'release_control' });
  ws.emit({ type: 'control', controller: null });
  assert.equal(t.names.at(-1), 'zsh');
});

test('reconnects passively after a dropped socket and redraws from the snapshot', async () => {
  const t = setup({ pty: { sessionId: 's1' } });
  await t.pty.open({ rows: 24, columns: 80 });
  let ws = FakeSocket.all[0];
  ws.open();
  ws.emit({ type: 'attached', client: 1, term: info() });
  ws.close(); // proxy restart, network change...
  assert.equal(t.timers.length, 1);
  t.timers[0].fn();
  ws = FakeSocket.all[1];
  ws.open();
  assert.equal(ws.sent[0].active, false, 'a reconnect does not take the PTY size');
  ws.emit({ type: 'attached', client: 3, term: info() });
  ws.emit({ type: 'snapshot', size: { rows: 24, cols: 80 }, data: b64('\x1bcredrawn') });
  assert.ok(t.out.join('').endsWith('\x1bcredrawn'));
});

test('close detaches and never reconnects; fatal errors stop', async () => {
  const t = setup({ pty: { sessionId: 's1' } });
  await t.pty.open(undefined);
  const ws = FakeSocket.all[0];
  ws.open();
  ws.emit({ type: 'attached', client: 1, term: info() });
  t.pty.close();
  assert.deepEqual(ws.sent.at(-1), { type: 'detach' });
  assert.equal(t.timers.length, 0);

  const u = setup({ pty: { sessionId: 'gone' } });
  await u.pty.open(undefined);
  const ws2 = FakeSocket.all[0];
  ws2.open();
  ws2.emit({ type: 'error', message: 'terminal session gone was lost when ember node restarted' });
  ws2.close();
  assert.equal(u.timers.length, 0);
  assert.match(u.out.join(''), /lost when ember node restarted/);
});

test('exit: an interactive shell closes on code 0, a task stays open with its output', async () => {
  const t = setup({ pty: { sessionId: 's1' } });
  await t.pty.open(undefined);
  let ws = FakeSocket.all[0];
  ws.open();
  ws.emit({ type: 'attached', client: 1, term: info() });
  ws.emit({ type: 'exit', code: 0, signal: null });
  assert.deepEqual(t.closes, [0]);

  const u = setup({ pty: { sessionId: 's2' } });
  await u.pty.open(undefined);
  ws = FakeSocket.all[0];
  ws.open();
  ws.emit({ type: 'attached', client: 1, term: info({ id: 's2', tags: { 'ember-term.mode': 'task' } }) });
  ws.emit({ type: 'exit', code: 0, signal: null });
  assert.deepEqual(u.closes, []);
  assert.match(u.out.join(''), /process ended \(exit code 0\)/);
  u.pty.handleInput('x');
  assert.equal(ws.sent.filter((m) => m.type === 'input').length, 0);
});

test('create failure is reported and settles', async () => {
  let settled = 0;
  const t = setup({
    routes: { 'POST /__terms': [403, { code: 'forbidden_path', error: 'outside the allowed roots' }] },
    pty: { create: { cwd: '/' }, onCreateSettled: () => settled++ },
  });
  await t.pty.open({ rows: 24, columns: 80 });
  assert.equal(settled, 1);
  assert.equal(FakeSocket.all.length, 0);
  assert.match(t.out.join(''), /outside the allowed roots/);
});

test('auto-attach plan', () => {
  const now = 10_000_000;
  const sessions = [
    info({ id: 'mine' }),
    info({ id: 'shown' }),
    info({ id: 'other-project', project: '/q' }),
    info({ id: 'agent', origin: 'agent' }),
    info({ id: 'task-here', tags: { 'ember-term.pid': '4242', 'ember-term.mode': 'task' } }),
    info({ id: 'task-elsewhere', tags: { 'ember-term.pid': '1', 'ember-term.mode': 'task' }, clients: [{ kind: 'ember-term', pid: 1 }] }),
    info({ id: 'attached-here', clients: [{ kind: 'ember-term', pid: 4243 }] }),
    info({ id: 'done-task', state: 'exited', tags: { 'ember-term.mode': 'task' }, finished_ms: now - 60_000 }),
    info({ id: 'old-task', state: 'exited', tags: { 'ember-term.mode': 'task' }, finished_ms: now - 3_600_000 }),
    info({ id: 'done-shell', state: 'exited', finished_ms: now - 1000 }),
    info({ id: 'lost', state: 'lost', has_screen: false }),
    info({ id: 'dismissed' }),
  ];
  const plan = ext.planAutoAttach(sessions, {
    project: '/p',
    shown: new Map([['shown', {}]]),
    dismissed: new Set(['dismissed']),
    localPids: new Set([4242, 4243]),
    now,
  });
  assert.deepEqual(plan.map((s) => s.id), ['mine', 'task-elsewhere', 'done-task']);
});

test('device label and refusal text', () => {
  assert.equal(ext.deviceLabel('desk', 'x', 'abcdef123'), 'desk');
  assert.equal(ext.deviceLabel('', 'Mozilla/5.0 (Linux; Android 14)', 'abcdef123'), 'VS Code on Android (abcdef)');
  assert.equal(ext.deviceLabel('', 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', ''), 'VS Code on Mac');
  assert.match(ext.refusalText('controlled', { device: 'phone' }), /controlled by phone/);
  assert.match(ext.refusalText('read_only'), /read-only/);
});
