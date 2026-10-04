// Unit tests for routing debuggees into persistent sessions (FR-P5): which launch
// configurations are rewritten, the session they create, the endpoint parsing, the attach
// configuration, and the re-attach plan.
//
//     cd ember/proxy/companion && node --test
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const ext = require('../extension.js');

const ctx = { project: '/p', device: 'laptop', debugId: 'd1' };
const T = ext.DEBUG_TAG;

test('shell quoting and the login-shell wrapper', () => {
  assert.equal(ext.shQuote('/usr/bin/node'), '/usr/bin/node');
  assert.equal(ext.shQuote('a b'), "'a b'");
  assert.equal(ext.shQuote("it's"), "'it'\\''s'");
  assert.equal(ext.shQuote(''), "''");
  const argv = ext.loginShellArgv(['node', 'my app.js']);
  assert.deepEqual(argv.slice(0, 4), ['/bin/sh', '-c', 'exec "${SHELL:-/bin/sh}" -l -i -c "$1"', 'ember-debug']);
  assert.equal(argv[4], "node 'my app.js'");
});

test('node launch in the integrated terminal becomes a session + attach', () => {
  const plan = ext.planDebugLaunch(
    {
      type: 'pwa-node', request: 'launch', name: 'server', console: 'integratedTerminal',
      program: '/p/server.js', args: ['--port', '3000'], runtimeArgs: ['--enable-source-maps'],
      env: { A: '1', B: 2, C: null }, skipFiles: ['<node_internals>/**'], outFiles: ['/p/dist/**'],
    },
    ctx,
  );
  assert.equal(plan.kind, 'node-inspect');
  const c = plan.create;
  assert.equal(c.origin, 'ide-vscode');
  assert.equal(c.project, '/p');
  assert.equal(c.cwd, '/p');
  assert.equal(c.title, 'server');
  assert.equal(c.key, 'vscode:debug:d1');
  assert.deepEqual(c.env, { A: '1', B: '2', EMBER_PROJECT: '/p' });
  assert.equal(c.program.argv.at(-1), 'node --inspect-brk=127.0.0.1:0 --enable-source-maps /p/server.js --port 3000');
  assert.equal(c.tags[T.id], 'd1');
  assert.equal(c.tags[T.kind], 'node-inspect');
  assert.equal(c.tags[T.type], 'pwa-node');
  assert.equal(c.tags['vscode.device'], 'laptop');
  assert.deepEqual(JSON.parse(c.tags[T.attach]), plan.attach);
  assert.deepEqual(plan.attach, {
    type: 'pwa-node', request: 'attach', name: 'server', continueOnAttach: true,
    skipFiles: ['<node_internals>/**'], outFiles: ['/p/dist/**'],
  });
  for (const v of Object.values(c.tags)) assert.equal(typeof v, 'string');

  const brk = ext.planDebugLaunch({ type: 'node', request: 'launch', console: 'integratedTerminal', program: 'a.js', stopOnEntry: true, cwd: '/p/sub', runtimeExecutable: '/opt/node/bin/node' }, ctx);
  assert.equal(brk.attach.continueOnAttach, false);
  assert.equal(brk.attach.type, 'node');
  assert.equal(brk.create.cwd, '/p/sub');
  assert.match(brk.create.program.argv.at(-1), /^\/opt\/node\/bin\/node --inspect-brk/);
});

test('debugpy launch becomes a session running the bootstrap + attach', () => {
  const plan = ext.planDebugLaunch(
    { type: 'debugpy', request: 'launch', name: 'app', console: 'integratedTerminal', program: '/p/app.py', args: ['-v'], python: '/p/.venv/bin/python', justMyCode: false },
    Object.assign({ debugpyPath: '/ext/bundled/libs' }, ctx),
  );
  assert.equal(plan.kind, 'debugpy');
  const cmd = plan.create.program.argv.at(-1);
  assert.match(cmd, /^\/p\/\.venv\/bin\/python -c '/);
  assert.match(cmd, /debugpy\.listen\(\(\\?'127\.0\.0\.1\\?', 0\)\)|debugpy\.listen/);
  assert.ok(cmd.endsWith(' -f /p/app.py -v'));
  assert.equal(plan.create.env.EMBER_DEBUGPY_PATH, '/ext/bundled/libs');
  assert.deepEqual(plan.attach, { type: 'debugpy', request: 'attach', name: 'app', justMyCode: false });

  const mod = ext.planDebugLaunch({ type: 'python', request: 'launch', console: 'integratedTerminal', module: 'pkg.main', python: ['/usr/bin/python3', '-X', 'dev'] }, ctx);
  assert.match(mod.create.program.argv.at(-1), /^\/usr\/bin\/python3 -X dev -c '.*' -m pkg\.main$/s);
  assert.equal(mod.create.env.EMBER_DEBUGPY_PATH, undefined);
});

test('the debugpy bootstrap is valid Python', { skip: !hasPython() }, () => {
  const { execFileSync } = require('node:child_process');
  const plan = ext.planDebugLaunch({ type: 'debugpy', request: 'launch', console: 'integratedTerminal', program: 'x.py' }, ctx);
  const argv = plan.create.program.argv;
  // Recover the -c argument through a real shell, then compile it without running it.
  const code = execFileSync('/bin/sh', ['-c', `eval "set -- $1"; printf %s "$3"`, "sh", argv[4]]).toString();
  execFileSync('python3', ['-c', 'import sys; compile(sys.stdin.read(), "boot", "exec")'], { input: code });
});

function hasPython() {
  try {
    require('node:child_process').execFileSync('python3', ['-c', '0']);
    return true;
  } catch {
    return false;
  }
}

test('what is left alone', () => {
  const base = { type: 'pwa-node', request: 'launch', console: 'integratedTerminal', program: 'a.js' };
  const skip = (over, extra = {}) => ext.planDebugLaunch(Object.assign({}, base, over), Object.assign({}, ctx, extra)).skip;
  assert.ok(!skip({}));
  assert.match(skip({}, { enabled: false }), /disabled/);
  assert.match(skip({ request: 'attach' }), /not a launch/);
  assert.match(skip({ console: 'internalConsole' }), /integratedTerminal/);
  assert.match(skip({ console: undefined }), /integratedTerminal/);
  assert.match(skip({ emberPersistent: false }), /opted out/);
  assert.match(skip({ __emberDebugId: 'x' }), /opted out/);
  assert.match(skip({ runtimeExecutable: 'npm', runtimeArgs: ['run', 'dev'] }), /not node/);
  assert.match(skip({ program: undefined }), /no program/);
  assert.match(skip({ args: '--a --b' }), /no program/);
  assert.match(skip({ type: 'lldb' }), /not supported/);
  assert.match(skip({ type: 'go' }), /not supported/);
  assert.match(skip({ type: 'debugpy', program: undefined }), /no program or module/);
});

test('endpoint parsing', () => {
  const node = '\x1b[0mDebugger listening on ws://127.0.0.1:41234/1b2c-3d\r\nFor help, see: https://nodejs.org/en/docs/inspector\r\n';
  assert.deepEqual(ext.parseDebugEndpoint('node-inspect', node), { host: '127.0.0.1', port: 41234 });
  // A restart prints a new line: the last one wins.
  assert.deepEqual(ext.parseDebugEndpoint('node-inspect', node + 'Debugger listening on ws://127.0.0.1:5000/x\n'), { host: '127.0.0.1', port: 5000 });
  assert.deepEqual(ext.parseDebugEndpoint('node-inspect', 'Debugger listening on ws://[::1]:9229/x'), { host: '::1', port: 9229 });
  assert.equal(ext.parseDebugEndpoint('node-inspect', 'Debugger attached.'), null);
  assert.deepEqual(ext.parseDebugEndpoint('debugpy', '$ \x1b[2m[ember] debugpy listening on 127.0.0.1:50123\x1b[0m\n'), { host: '127.0.0.1', port: 50123 });
  assert.equal(ext.parseDebugEndpoint('debugpy', ''), null);
});

test('attach configurations', () => {
  assert.deepEqual(ext.attachFor('node-inspect', { type: 'pwa-node', request: 'attach' }, { host: '127.0.0.1', port: 9 }, { debugId: 'd', termId: 't' }), {
    type: 'pwa-node', request: 'attach', address: '127.0.0.1', port: 9, __emberDebugId: 'd', __emberTermId: 't',
  });
  assert.deepEqual(ext.attachFor('debugpy', { type: 'debugpy', request: 'attach' }, { host: '127.0.0.1', port: 9 }, { debugId: 'd', termId: 't' }), {
    type: 'debugpy', request: 'attach', connect: { host: '127.0.0.1', port: 9 }, __emberDebugId: 'd', __emberTermId: 't',
  });
});

const session = (over = {}) =>
  Object.assign(
    {
      id: 's1', title: 'server', origin: 'ide-vscode', project: '/p', state: 'running',
      tags: { [T.id]: 'd1', [T.kind]: 'node-inspect', [T.name]: 'server', [T.attach]: JSON.stringify({ type: 'pwa-node', request: 'attach', name: 'server', continueOnAttach: true }) },
    },
    over,
  );

test('re-attach plan and configuration', () => {
  const sessions = [
    session(),
    session({ id: 's2', tags: Object.assign({}, session().tags, { [T.id]: 'attached' }) }),
    session({ id: 's3', tags: Object.assign({}, session().tags, { [T.id]: 'offered' }) }),
    session({ id: 's4', state: 'exited' }),
    session({ id: 's5', project: '/q' }),
    session({ id: 's6', tags: {} }),
    session({ id: 's7', origin: 'agent' }),
  ];
  const plan = ext.planReattach(sessions, { project: '/p', attached: new Set(['attached']), offered: new Set(['offered']) });
  assert.deepEqual(plan.map((s) => s.id), ['s1']);

  const cfg = ext.reattachConfig(session(), 'Debugger listening on ws://127.0.0.1:41234/x\nlots of output\n');
  assert.deepEqual(cfg, { type: 'pwa-node', request: 'attach', name: 'server', address: '127.0.0.1', port: 41234, __emberDebugId: 'd1', __emberTermId: 's1' });
  assert.equal(ext.reattachConfig(session(), 'scrolled away'), null);
  assert.equal(ext.reattachConfig(session({ tags: {} }), 'Debugger listening on ws://127.0.0.1:1/x'), null);
});

// A fake TermApi: the session prints its endpoint after `readyAfter` snapshots, or exits.
function fakeApi({ readyAfter = 2, text = 'Debugger listening on ws://127.0.0.1:41234/x', exitAfter = Infinity, createError } = {}) {
  const calls = [];
  let snaps = 0;
  return {
    calls,
    async create(body) {
      calls.push(['create', body]);
      if (createError) throw createError;
      return { created: true, term: { id: 't1', title: body.title, tags: body.tags } };
    },
    async request(method, path) {
      calls.push([method, path]);
      snaps++;
      if (snaps === 1) throw Object.assign(new Error('no screen'), { status: 409 });
      return { text: snaps > readyAfter ? text : '$ ' };
    },
    async get(id) {
      calls.push(['get', id]);
      return { id, state: snaps >= exitAfter ? 'exited' : 'running' };
    },
    async kill(id) {
      calls.push(['kill', id]);
    },
  };
}

test('launchDebuggee: creates, shows, waits for the endpoint, returns the attach configuration', async () => {
  const plan = ext.planDebugLaunch({ type: 'pwa-node', request: 'launch', name: 'server', console: 'integratedTerminal', program: 'a.js' }, ctx);
  const api = fakeApi();
  const shown = [];
  const cfg = await ext.launchDebuggee(plan, { api, debugId: 'd1', show: (t) => shown.push(t.id), sleep: async () => {} });
  assert.deepEqual(shown, ['t1']);
  assert.equal(cfg.request, 'attach');
  assert.equal(cfg.port, 41234);
  assert.equal(cfg.__emberTermId, 't1');
  assert.equal(cfg.__emberDebugId, 'd1');
  assert.equal(api.calls[0][1].tags[T.id], 'd1');
  assert.ok(!api.calls.some((c) => c[0] === 'kill'));
});

test('launchDebuggee: the debuggee ends first, or never reports a port', async () => {
  const plan = ext.planDebugLaunch({ type: 'debugpy', request: 'launch', name: 'app', console: 'integratedTerminal', program: 'a.py' }, ctx);
  await assert.rejects(
    ext.launchDebuggee(plan, { api: fakeApi({ readyAfter: 99, exitAfter: 2, text: '' }), debugId: 'd', sleep: async () => {} }),
    /ended before the debugger could attach/,
  );

  let t = 0;
  const api = fakeApi({ readyAfter: Infinity });
  await assert.rejects(
    ext.launchDebuggee(plan, { api, debugId: 'd', sleep: async (ms) => (t += ms), now: () => t, timeoutMs: 1000 }),
    /did not report a debugger port/,
  );
  assert.deepEqual(api.calls.at(-1), ['kill', 't1']);

  // No node: the error reaches the caller before anything was shown (it falls back to a plain launch).
  const shown = [];
  await assert.rejects(
    ext.launchDebuggee(plan, { api: fakeApi({ createError: Object.assign(new Error('ember node is not running'), { status: 503 }) }), debugId: 'd', show: (x) => shown.push(x), sleep: async () => {} }),
    /not running/,
  );
  assert.deepEqual(shown, []);
});
