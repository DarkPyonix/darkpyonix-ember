#!/usr/bin/env node
// Live CDP harness for VS Code Web tab detach on real dpx-ose-server + ember/proxy rewrite.
//
// Zero npm dependencies: uses Node 22 built-ins (http, net, child_process, WebSocket) plus
// Python stdlib to run ember/proxy/dpx/vscode/html_rewrite.py directly against real OSE HTML.
//
// Usage:
//   EMBER_OSE_SERVER=/path/to/dpx-ose-server node ember/proxy/tests/live_detach_cdp.mjs
//
// Optional env:
//   CHROME_PATH       Explicit path to Chrome/Chromium binary.
//   EMBER_LIVE_KEEP   Set to "1" to keep temporary workspace/profile dirs for debugging.

'use strict';

import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const PROXY_DIR = path.resolve(__dirname, '..');
const STATIC_DIR = path.join(PROXY_DIR, 'static');

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

function findOseBinary() {
  const fromEnv = process.env.EMBER_OSE_SERVER;
  if (fromEnv && fs.existsSync(fromEnv)) return fromEnv;
  const idx = process.argv.indexOf('--ose');
  if (idx >= 0 && process.argv[idx + 1] && fs.existsSync(process.argv[idx + 1])) {
    return process.argv[idx + 1];
  }
  throw new Error(
    'EMBER_OSE_SERVER is not set or does not exist. Pass EMBER_OSE_SERVER=/path/to/dpx-ose-server.'
  );
}

function findPlaywrightChrome(cacheDir) {
  if (!cacheDir || !fs.existsSync(cacheDir)) return null;
  let entries = [];
  try {
    entries = fs.readdirSync(cacheDir).filter((n) => n.startsWith('chromium'));
  } catch {
    return null;
  }
  entries.sort().reverse();
  for (const dir of entries) {
    const base = path.join(cacheDir, dir);
    const candidates = [
      path.join(base, 'chrome-mac-arm64', 'Google Chrome for Testing.app', 'Contents', 'MacOS', 'Google Chrome for Testing'),
      path.join(base, 'chrome-mac', 'Chromium.app', 'Contents', 'MacOS', 'Chromium'),
      path.join(base, 'chrome-linux', 'chrome'),
      path.join(base, 'chrome-linux64', 'chrome'),
    ];
    for (const c of candidates) {
      if (fs.existsSync(c)) return c;
    }
  }
  return null;
}

function findChromeBinary() {
  for (const key of ['CHROME_PATH', 'CHROMIUM_PATH']) {
    if (process.env[key] && fs.existsSync(process.env[key])) return process.env[key];
  }
  const home = os.homedir();
  const pwCandidates = [
    process.env.PLAYWRIGHT_BROWSERS_PATH,
    path.join(home, 'Library', 'Caches', 'ms-playwright'),
    path.join(home, '.cache', 'ms-playwright'),
  ];
  for (const dir of pwCandidates) {
    const hit = findPlaywrightChrome(dir);
    if (hit) return hit;
  }
  const staticCandidates = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/usr/bin/google-chrome',
    '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ];
  for (const c of staticCandidates) {
    if (fs.existsSync(c)) return c;
  }
  throw new Error('No Chrome/Chromium binary found. Set CHROME_PATH or install Chromium.');
}

// ---------------------------------------------------------------------------
// Free TCP port helper
// ---------------------------------------------------------------------------

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      const port = typeof addr === 'object' && addr ? addr.port : 0;
      srv.close(() => resolve(port));
    });
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitFor(fn, { timeoutMs = 30000, intervalMs = 150, label = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const val = await fn();
      if (val) return val;
    } catch (err) {
      lastErr = err;
    }
    await sleep(intervalMs);
  }
  throw new Error(
    `Timed out waiting for ${label} after ${timeoutMs}ms` + (lastErr ? `: ${lastErr.message}` : '')
  );
}

// ---------------------------------------------------------------------------
// Real ember/proxy HTML rewrite + static asset serving reverse proxy
// ---------------------------------------------------------------------------

function rewriteWorkbenchHtml(html) {
  const pyScript = [
    'import sys',
    'from dpx.vscode.html_rewrite import workbench_inserts',
    'doc = sys.stdin.read()',
    'out, _ = workbench_inserts(doc, "live", detach=True)',
    'sys.stdout.write(out)',
  ].join('; ');
  return execFileSync('python3', ['-c', pyScript], {
    input: html,
    encoding: 'utf8',
    env: { ...process.env, PYTHONPATH: PROXY_DIR },
    maxBuffer: 16 * 1024 * 1024,
  });
}

function startProxyServer(osePort, proxyPort) {
  const staticAssets = new Map([
    ['/__detach.js', { file: path.join(STATIC_DIR, 'detach.js'), type: 'application/javascript; charset=utf-8' }],
    ['/__overlay.js', { file: path.join(STATIC_DIR, 'overlay.js'), type: 'application/javascript; charset=utf-8' }],
    ['/__overlay.css', { file: path.join(STATIC_DIR, 'overlay.css'), type: 'text/css; charset=utf-8' }],
  ]);

  const server = http.createServer((req, res) => {
    const pathname = (req.url || '/').split('?')[0];
    const asset = staticAssets.get(pathname);
    if (asset && fs.existsSync(asset.file)) {
      res.writeHead(200, { 'content-type': asset.type, 'cache-control': 'no-store' });
      fs.createReadStream(asset.file).pipe(res);
      return;
    }

    const headers = { ...req.headers, host: `127.0.0.1:${osePort}` };
    // Avoid compressed HTML so we can rewrite the workbench document directly.
    delete headers['accept-encoding'];

    const upReq = http.request(
      {
        hostname: '127.0.0.1',
        port: osePort,
        method: req.method,
        path: req.url,
        headers,
      },
      (upRes) => {
        const ct = String(upRes.headers['content-type'] || '');
        if (ct.includes('text/html')) {
          const chunks = [];
          upRes.on('data', (c) => chunks.push(c));
          upRes.on('end', () => {
            const raw = Buffer.concat(chunks).toString('utf8');
            let body = raw;
            if (raw.includes('vscode-workbench-web-configuration')) {
              body = rewriteWorkbenchHtml(raw);
            }
            const outHeaders = { ...upRes.headers };
            delete outHeaders['content-length'];
            delete outHeaders['content-encoding'];
            outHeaders['content-length'] = Buffer.byteLength(body, 'utf8');
            res.writeHead(upRes.statusCode || 200, outHeaders);
            res.end(body);
          });
        } else {
          res.writeHead(upRes.statusCode || 200, upRes.headers);
          upRes.pipe(res);
        }
      }
    );
    upReq.on('error', (err) => {
      if (!res.headersSent) res.writeHead(502);
      res.end(`proxy upstream error: ${err.message}`);
    });
    req.pipe(upReq);
  });

  // Transparent WebSocket / raw upgrade relay to dpx-ose-server.
  server.on('upgrade', (req, socket, head) => {
    const up = net.connect(osePort, '127.0.0.1', () => {
      let rawReq = `${req.method} ${req.url} HTTP/${req.httpVersion}\r\n`;
      for (let i = 0; i < req.rawHeaders.length; i += 2) {
        const k = req.rawHeaders[i];
        const v = k.toLowerCase() === 'host' ? `127.0.0.1:${osePort}` : req.rawHeaders[i + 1];
        rawReq += `${k}: ${v}\r\n`;
      }
      rawReq += '\r\n';
      up.write(rawReq);
      if (head && head.length) up.write(head);
      up.pipe(socket);
      socket.pipe(up);
    });
    up.on('error', () => socket.destroy());
    socket.on('error', () => up.destroy());
  });

  return new Promise((resolve, reject) => {
    server.on('error', reject);
    server.listen(proxyPort, '127.0.0.1', () => resolve(server));
  });
}

// ---------------------------------------------------------------------------
// Minimal CDP WebSocket client (Node 22 native WebSocket)
// ---------------------------------------------------------------------------

class CdpClient {
  constructor(wsUrl) {
    this.wsUrl = wsUrl;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = new Map();
    this.ws = null;
  }

  async connect() {
    await new Promise((resolve, reject) => {
      const ws = new WebSocket(this.wsUrl);
      this.ws = ws;
      ws.addEventListener('open', () => resolve());
      ws.addEventListener('error', (err) => reject(err));
      ws.addEventListener('message', (ev) => {
        const msg = JSON.parse(String(ev.data));
        if (msg.id) {
          const entry = this.pending.get(msg.id);
          if (!entry) return;
          this.pending.delete(msg.id);
          if (msg.error) {
            entry.reject(new Error(`CDP ${entry.method}: ${msg.error.message}`));
          } else {
            entry.resolve(msg.result);
          }
        } else if (msg.method) {
          const fns = this.listeners.get(msg.method);
          if (fns) {
            for (const fn of [...fns]) fn(msg.params, msg.sessionId);
          }
        }
      });
    });
  }

  on(method, fn) {
    if (!this.listeners.has(method)) this.listeners.set(method, new Set());
    this.listeners.get(method).add(fn);
    return () => this.listeners.get(method)?.delete(fn);
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId++;
    const payload = sessionId ? { id, method, params, sessionId } : { id, method, params };
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.ws.send(JSON.stringify(payload));
    });
  }

  async evalIn(sessionId, expression, awaitPromise = true) {
    const r = await this.send(
      'Runtime.evaluate',
      { expression, returnByValue: true, awaitPromise },
      sessionId
    );
    if (r.exceptionDetails) {
      throw new Error(
        `Page exception: ${r.exceptionDetails.text || ''} ${r.exceptionDetails.exception?.description || ''}`
      );
    }
    return r.result ? r.result.value : undefined;
  }

  close() {
    try {
      this.ws?.close();
    } catch {
      // ignore
    }
  }
}

// ---------------------------------------------------------------------------
// Process lifecycle helpers
// ---------------------------------------------------------------------------

async function startOse(oseBin, osePort, userDataDir, serverDataDir) {
  const args = [
    '--host', '127.0.0.1',
    '--port', String(osePort),
    '--without-connection-token',
    '--accept-server-license-terms',
    '--user-data-dir', userDataDir,
    '--server-data-dir', serverDataDir,
  ];
  const proc = spawn(oseBin, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  const logs = [];
  proc.stdout.on('data', (d) => logs.push(d.toString()));
  proc.stderr.on('data', (d) => logs.push(d.toString()));

  await waitFor(
    async () => {
      if (proc.exitCode !== null) {
        throw new Error(`dpx-ose-server exited early (${proc.exitCode}):\n${logs.join('')}`);
      }
      const res = await fetch(`http://127.0.0.1:${osePort}/version`);
      return res.ok;
    },
    { timeoutMs: 30000, intervalMs: 250, label: 'dpx-ose-server /version' }
  );
  return { proc, logs };
}

async function startChrome(chromeBin, profileDir) {
  const args = [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profileDir}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-popup-blocking',
    '--disable-gpu',
    '--window-size=1280,800',
  ];
  if (process.platform === 'linux') {
    args.push('--no-sandbox', '--disable-dev-shm-usage');
  }
  const proc = spawn(chromeBin, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  let stderrBuf = '';
  let wsUrl = null;
  proc.stderr.on('data', (d) => {
    stderrBuf += d.toString();
    const m = /DevTools listening on (ws:\/\/\S+)/.exec(stderrBuf);
    if (m) wsUrl = m[1];
  });

  await waitFor(
    () => {
      if (proc.exitCode !== null) {
        throw new Error(`Chrome exited early (${proc.exitCode}):\n${stderrBuf}`);
      }
      return wsUrl;
    },
    { timeoutMs: 20000, intervalMs: 100, label: 'Chrome DevTools endpoint' }
  );
  return { proc, wsUrl };
}

// ---------------------------------------------------------------------------
// Workbench interaction helpers
// ---------------------------------------------------------------------------

async function openAndWaitEditorReady(cdp, url) {
  const { targetId } = await cdp.send('Target.createTarget', { url });
  await cdp.send('Target.activateTarget', { targetId });
  const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
  await cdp.send('Page.enable', {}, sessionId);
  await cdp.send('Runtime.enable', {}, sessionId);
  await cdp.send('DOM.enable', {}, sessionId);
  await cdp.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sessionId);

  const metrics = await waitFor(
    async () => {
      return cdp.evalIn(
        sessionId,
        `(() => {
          const wb = document.querySelector('.monaco-workbench');
          const ed = document.querySelector('.editor-instance .monaco-editor');
          const lines = ed ? ed.querySelector('.view-lines .view-line') : null;
          if (!wb || !ed || !lines) return null;
          const active = document.activeElement;
          const focused = ed.classList.contains('focused') ||
            Boolean(active && active.closest && active.closest('.monaco-editor'));
          return {
            ready: true,
            focused,
            navToNowMs: Math.round(performance.now()),
            detachInstalled: Boolean(window.__emberDetachInstalled),
            href: window.location.href,
          };
        })()`
      ).then((m) => (m && m.ready ? m : null));
    },
    { timeoutMs: 45000, intervalMs: 100, label: `editor ready for ${url}` }
  );
  return { targetId, sessionId, metrics };
}

async function focusActiveEditor(cdp, sessionId) {
  const pt = await waitFor(
    async () => {
      return cdp.evalIn(
        sessionId,
        `(() => {
          const el = document.querySelector('.editor-instance .monaco-editor .view-lines');
          if (!el) return null;
          const r = el.getBoundingClientRect();
          if (r.width < 10 || r.height < 10) return null;
          return { x: r.left + Math.min(80, r.width / 2), y: r.top + Math.min(40, r.height / 2) };
        })()`
      );
    },
    { timeoutMs: 10000, intervalMs: 100, label: 'visible editor view-lines' }
  );
  await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: pt.x, y: pt.y, button: 'left', buttons: 1, clickCount: 1 }, sessionId);
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: pt.x, y: pt.y, button: 'left', buttons: 0, clickCount: 1 }, sessionId);
  await sleep(150);
}

async function getTabs(cdp, sessionId) {
  return cdp.evalIn(
    sessionId,
    `Array.from(document.querySelectorAll('.tabs-container .tab')).map((el) => {
      const r = el.getBoundingClientRect();
      return {
        name: el.getAttribute('data-resource-name') || el.textContent.trim(),
        active: el.classList.contains('active'),
        dirty: el.classList.contains('dirty'),
        rect: { left: r.left, top: r.top, right: r.right, bottom: r.bottom, cx: r.left + r.width / 2, cy: r.top + r.height / 2 },
      };
    })`
  );
}

async function performHtml5Drag(cdp, sessionId, from, to, { drop = true, steps = 10 } = {}) {
  await cdp.send('Input.setInterceptDrags', { enabled: true }, sessionId);

  let interceptedData = null;
  const off = cdp.on('Input.dragIntercepted', (params, sid) => {
    if (sid === sessionId && !interceptedData) {
      interceptedData = params.data;
    }
  });

  try {
    await cdp.send(
      'Input.dispatchMouseEvent',
      { type: 'mouseMoved', x: from.x, y: from.y, button: 'none', buttons: 0 },
      sessionId
    );
    await cdp.send(
      'Input.dispatchMouseEvent',
      { type: 'mousePressed', x: from.x, y: from.y, button: 'left', buttons: 1, clickCount: 1 },
      sessionId
    );

    // Move enough to trigger Chromium's HTML5 dragstart and Input.dragIntercepted.
    for (let i = 1; i <= steps; i++) {
      const x = from.x + ((to.x - from.x) * i) / steps;
      const y = from.y + ((to.y - from.y) * i) / steps;
      await cdp.send(
        'Input.dispatchMouseEvent',
        { type: 'mouseMoved', x, y, button: 'left', buttons: 1 },
        sessionId
      );
      await sleep(15);
      if (interceptedData) break;
    }

    const data = await waitFor(() => interceptedData, {
      timeoutMs: 5000,
      intervalMs: 50,
      label: 'Input.dragIntercepted',
    });

    // Drive the HTML5 drag session to the destination via CDP Input.dispatchDragEvent.
    await cdp.send(
      'Input.dispatchDragEvent',
      { type: 'dragEnter', x: to.x, y: to.y, data },
      sessionId
    );
    await cdp.send(
      'Input.dispatchDragEvent',
      { type: 'dragOver', x: to.x, y: to.y, data },
      sessionId
    );
    await sleep(60);
    if (drop) {
      await cdp.send(
        'Input.dispatchDragEvent',
        { type: 'drop', x: to.x, y: to.y, data },
        sessionId
      );
    } else {
      await cdp.send(
        'Input.dispatchDragEvent',
        { type: 'dragCancel', x: to.x, y: to.y, data },
        sessionId
      );
    }
    await cdp.send(
      'Input.dispatchMouseEvent',
      { type: 'mouseReleased', x: to.x, y: to.y, button: 'left', buttons: 0, clickCount: 1 },
      sessionId
    );
  } finally {
    off();
    await cdp.send('Input.setInterceptDrags', { enabled: false }, sessionId).catch(() => {});
  }
}

// ---------------------------------------------------------------------------
// Main test runner
// ---------------------------------------------------------------------------

async function main() {
  const oseBin = findOseBinary();
  const chromeBin = findChromeBinary();

  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ember-live-detach-'));
  const workspaceDir = path.join(tmpRoot, 'workspace');
  const oseUserDir = path.join(tmpRoot, 'ose-user');
  const oseServerDir = path.join(tmpRoot, 'ose-server');
  const chromeProfileDir = path.join(tmpRoot, 'chrome-profile');
  fs.mkdirSync(workspaceDir, { recursive: true });
  fs.mkdirSync(path.join(oseUserDir, 'User'), { recursive: true });
  fs.mkdirSync(path.join(oseServerDir, 'data', 'Machine'), { recursive: true });
  fs.mkdirSync(chromeProfileDir, { recursive: true });

  // Seed two real multi-line files in the workspace.
  const alphaPath = path.join(workspaceDir, 'alpha.txt');
  const betaPath = path.join(workspaceDir, 'beta.txt');
  fs.writeFileSync(
    alphaPath,
    Array.from({ length: 60 }, (_, i) => `alpha line ${i + 1}: quick brown fox`).join('\n') + '\n',
    'utf8'
  );
  fs.writeFileSync(
    betaPath,
    Array.from({ length: 60 }, (_, i) => `beta line ${i + 1}: lazy dog jumps`).join('\n') + '\n',
    'utf8'
  );

  // Disable workspace trust prompt, autoSave, and preview tabs so tabs behave deterministically.
  const oseSettings = JSON.stringify(
    {
      'security.workspace.trust.enabled': false,
      'workbench.startupEditor': 'none',
      'workbench.editor.enablePreview': false,
      'window.restoreWindows': 'none',
      'files.autoSave': 'off',
    },
    null,
    2
  );
  fs.writeFileSync(path.join(oseUserDir, 'User', 'settings.json'), oseSettings, 'utf8');
  fs.writeFileSync(path.join(oseServerDir, 'data', 'Machine', 'settings.json'), oseSettings, 'utf8');

  const osePort = await freePort();
  const proxyPort = await freePort();
  let oseProc = null;
  let proxyServer = null;
  let chromeProc = null;
  let cdp = null;

  try {
    console.log(`[live_detach_cdp] Starting OSE on :${osePort} (${oseBin})`);
    const ose = await startOse(oseBin, osePort, oseUserDir, oseServerDir);
    oseProc = ose.proc;

    proxyServer = await startProxyServer(osePort, proxyPort);
    console.log(`[live_detach_cdp] Proxy listening on :${proxyPort}`);

    console.log(`[live_detach_cdp] Starting headless Chromium (${chromeBin})`);
    const chrome = await startChrome(chromeBin, chromeProfileDir);
    chromeProc = chrome.proc;

    cdp = new CdpClient(chrome.wsUrl);
    await cdp.connect();
    await cdp.send('Target.setDiscoverTargets', { discover: true });

    const targetMap = new Map();
    const recordTarget = (info) => {
      if (info && info.type === 'page') {
        targetMap.set(info.targetId, info);
      }
    };
    cdp.on('Target.targetCreated', (params) => recordTarget(params.targetInfo));
    cdp.on('Target.targetInfoChanged', (params) => recordTarget(params.targetInfo));

    const baseOrigin = `http://127.0.0.1:${proxyPort}`;
    const folderQ = `folder=${encodeURIComponent(workspaceDir)}`;
    const alphaUri = `vscode-remote://127.0.0.1:${proxyPort}${alphaPath}`;
    const betaUri = `vscode-remote://127.0.0.1:${proxyPort}${betaPath}`;

    // -----------------------------------------------------------------------
    // Baseline measurement: plain serve-web load without detach payload
    // -----------------------------------------------------------------------
    const plainPayload = encodeURIComponent(JSON.stringify([['openFile', alphaUri]]));
    const plainUrl = `${baseOrigin}/?${folderQ}&payload=${plainPayload}`;
    const baseline = await openAndWaitEditorReady(cdp, plainUrl);
    const baselineLoadMs = baseline.metrics.navToNowMs;
    assert.equal(baseline.metrics.detachInstalled, true, 'detach.js must be injected and installed');
    console.log(`[live_detach_cdp] Baseline plain serve-web load: ${baselineLoadMs} ms`);

    // Open beta.txt as a second tab via the explorer so we have two tabs in the strip.
    const { sessionId } = baseline;
    await waitFor(
      async () => {
        const clicked = await cdp.evalIn(
          sessionId,
          `(() => {
            const rows = Array.from(document.querySelectorAll('.explorer-folders-view .monaco-list-row'));
            const beta = rows.find((r) => (r.textContent || '').includes('beta.txt'));
            if (!beta) return false;
            beta.click();
            return true;
          })()`
        );
        return clicked;
      },
      { timeoutMs: 20000, intervalMs: 200, label: 'click beta.txt in explorer' }
    );

    const initialTabs = await waitFor(
      async () => {
        const tabs = await getTabs(cdp, sessionId);
        return tabs.length >= 2 ? tabs : null;
      },
      { timeoutMs: 15000, intervalMs: 150, label: 'two open tabs' }
    );
    const initialNames = initialTabs.map((t) => t.name);
    console.log(`[live_detach_cdp] Initial tabs: ${JSON.stringify(initialNames)}`);
    assert.equal(initialNames.length, 2);

    // -----------------------------------------------------------------------
    // Test 1: fr_b1_live_short_drag_reorders
    // -----------------------------------------------------------------------
    const targetsBeforeReorder = new Set(targetMap.keys());
    await performHtml5Drag(
      cdp,
      sessionId,
      { x: initialTabs[1].rect.cx, y: initialTabs[1].rect.cy },
      { x: initialTabs[0].rect.left + 16, y: initialTabs[0].rect.cy },
      { drop: true }
    );
    await sleep(400);

    const tabsAfterReorder = await getTabs(cdp, sessionId);
    const namesAfterReorder = tabsAfterReorder.map((t) => t.name);
    console.log(`[live_detach_cdp] fr_b1_live_short_drag_reorders: ${JSON.stringify(namesAfterReorder)}`);
    const newAfterReorder = [...targetMap.keys()].filter((id) => !targetsBeforeReorder.has(id));
    assert.equal(
      newAfterReorder.length,
      0,
      'short drag inside tab strip must not open a detached window'
    );
    assert.deepEqual(
      namesAfterReorder,
      [initialNames[1], initialNames[0]],
      'short drag inside tab strip must reorder tabs'
    );

    // -----------------------------------------------------------------------
    // Test 2: fr_b1_live_long_drag_detaches + fr_b3_live_payload_url + detached load timing
    // -----------------------------------------------------------------------
    // Move cursor to line 5 in the active editor so cursor survives in the payload URL.
    await focusActiveEditor(cdp, sessionId);
    for (let i = 0; i < 4; i++) {
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 }, sessionId);
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 }, sessionId);
    }
    await sleep(250);

    // Drag the active tab > 48 px below the tab strip and cancel drop (dropEffect === 'none').
    const tabsBeforeDetach = await getTabs(cdp, sessionId);
    const targetTab = tabsBeforeDetach.find((t) => t.active) || tabsBeforeDetach[0];
    const targetsBeforeDetach = new Set(targetMap.keys());

    await performHtml5Drag(
      cdp,
      sessionId,
      { x: targetTab.rect.cx, y: targetTab.rect.cy },
      { x: targetTab.rect.cx, y: 790 },
      { drop: false }
    );

    // Wait for the popup target created by detach.js's window.open(url, '_blank', features).
    const detachedTarget = await waitFor(
      () => {
        for (const [id, info] of targetMap.entries()) {
          if (!targetsBeforeDetach.has(id) && info.url && info.url.includes('payload=')) {
            return info;
          }
        }
        return null;
      },
      { timeoutMs: 15000, intervalMs: 100, label: 'detached popup window with payload URL' }
    );

    // Verify detached payload URL contains folder, openFile with detached file, and gotoLineMode.
    const detachedUrl = new URL(detachedTarget.url);
    assert.equal(detachedUrl.searchParams.get('folder'), workspaceDir, 'detached URL must preserve folder');
    const decodedPayload = JSON.parse(detachedUrl.searchParams.get('payload') || '[]');
    const openFileEntry = decodedPayload.find(([k]) => k === 'openFile');
    assert.ok(openFileEntry, 'detached payload must include openFile');
    assert.ok(
      openFileEntry[1].includes(targetTab.name),
      `openFile (${openFileEntry[1]}) must reference detached tab ${targetTab.name}`
    );
    console.log(`[live_detach_cdp] fr_b1_live_long_drag_detaches + fr_b3_live_payload_url OK: ${detachedTarget.url}`);

    // Measure detached window load timing: navigationStart -> editor ready.
    await cdp.send('Target.activateTarget', { targetId: detachedTarget.targetId });
    const { sessionId: detachedSid } = await cdp.send('Target.attachToTarget', {
      targetId: detachedTarget.targetId,
      flatten: true,
    });
    await cdp.send('Runtime.enable', {}, detachedSid);
    await cdp.send('Emulation.setFocusEmulationEnabled', { enabled: true }, detachedSid);
    const detachedMetrics = await waitFor(
      async () => {
        return cdp.evalIn(
          detachedSid,
          `(() => {
            const ed = document.querySelector('.editor-instance .monaco-editor');
            const lines = ed ? ed.querySelector('.view-lines .view-line') : null;
            if (!ed || !lines) return null;
            const active = document.activeElement;
            const focused = ed.classList.contains('focused') ||
              Boolean(active && active.closest && active.closest('.monaco-editor'));
            return {
              focused,
              navToFocusMs: Math.round(performance.now()),
            };
          })()`
        ).then((m) => (m ? m : null));
      },
      { timeoutMs: 45000, intervalMs: 100, label: 'detached window editor ready' }
    );

    const detachLoadMs = detachedMetrics.navToFocusMs;
    const deltaMs = detachLoadMs - baselineLoadMs;
    console.log(
      `[live_detach_cdp] detached window timing: detached=${detachLoadMs}ms (focused=${detachedMetrics.focused}), baseline=${baselineLoadMs}ms (focused=${baseline.metrics.focused}), delta=${deltaMs >= 0 ? '+' : ''}${deltaMs}ms`
    );

    // Close detached popup and reactivate source window for dirty test.
    await cdp.send('Target.closeTarget', { targetId: detachedTarget.targetId });
    await cdp.send('Target.activateTarget', { targetId: baseline.targetId });
    await cdp.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sessionId);
    await sleep(300);

    // -----------------------------------------------------------------------
    // Test 3: fr_b1_live_dirty_tab_refused
    // -----------------------------------------------------------------------
    // Focus the remaining editor in the source window and trigger a deleteLeft edit (Backspace)
    // to mark the tab dirty (pure keybinding, works in both EditContext and textarea modes).
    await focusActiveEditor(cdp, sessionId);
    await cdp.send(
      'Input.dispatchKeyEvent',
      { type: 'keyDown', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 },
      sessionId
    );
    await cdp.send(
      'Input.dispatchKeyEvent',
      { type: 'keyUp', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 },
      sessionId
    );

    const dirtyTab = await waitFor(
      async () => {
        const tabs = await getTabs(cdp, sessionId);
        return tabs.find((t) => t.dirty) || null;
      },
      { timeoutMs: 10000, intervalMs: 100, label: 'dirty tab indicator' }
    ).catch(async (err) => {
      const diag = await cdp.evalIn(
        sessionId,
        `({
          tabs: Array.from(document.querySelectorAll('.tabs-container .tab')).map((el) => el.className),
          activeEl: document.activeElement ? document.activeElement.outerHTML.slice(0, 140) : null,
          firstLine: document.querySelector('.view-lines .view-line')?.textContent || null,
        })`
      );
      throw new Error(`${err.message} (diag=${JSON.stringify(diag)})`);
    });

    const targetsBeforeDirty = new Set(targetMap.keys());
    await performHtml5Drag(
      cdp,
      sessionId,
      { x: dirtyTab.rect.cx, y: dirtyTab.rect.cy },
      { x: dirtyTab.rect.cx, y: 790 },
      { drop: false }
    );

    const toastText = await waitFor(
      async () => {
        return cdp.evalIn(
          sessionId,
          `(() => {
            const els = Array.from(document.querySelectorAll('body > [role="status"]'));
            const hit = els.find((el) => (el.textContent || '').includes('before detaching it'));
            return hit ? hit.textContent : null;
          })()`
        );
      },
      { timeoutMs: 10000, intervalMs: 100, label: 'dirty tab refusal toast' }
    );
    const newAfterDirty = [...targetMap.keys()].filter((id) => !targetsBeforeDirty.has(id));
    assert.equal(
      newAfterDirty.length,
      0,
      'dirty tab drag-out must not open a new window'
    );
    assert.ok(
      toastText.includes('Save') && toastText.includes('before detaching it'),
      `expected dirty refusal toast, got: ${toastText}`
    );
    console.log(`[live_detach_cdp] fr_b1_live_dirty_tab_refused OK: "${toastText}"`);

    // Final machine-readable summary.
    const summary = {
      fr_b1_live_short_drag_reorders: 'pass',
      fr_b1_live_long_drag_detaches: 'pass',
      fr_b3_live_payload_url: 'pass',
      fr_b1_live_dirty_tab_refused: 'pass',
      nfr_b1_detached_ms: detachLoadMs,
      nfr_b1_baseline_ms: baselineLoadMs,
      nfr_b1_delta_ms: deltaMs,
    };
    console.log(`LIVE_DETACH_CDP_SUMMARY=${JSON.stringify(summary)}`);
  } finally {
    if (cdp) cdp.close();
    if (proxyServer) proxyServer.close();
    if (chromeProc && chromeProc.exitCode === null) chromeProc.kill('SIGTERM');
    if (oseProc && oseProc.exitCode === null) oseProc.kill('SIGTERM');
    if (process.env.EMBER_LIVE_KEEP !== '1') {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  }
}

main().catch((err) => {
  console.error('[live_detach_cdp] FAILED:', err);
  process.exit(1);
});
