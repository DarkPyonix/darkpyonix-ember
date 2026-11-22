// node --test ember/editor-conn/scripts/gen-actors/compare.test.mjs
// FR-E10: compare.mjs admits a commit only with the same proxy ids and the same signature of every
// bridged method; line numbers and non-bridged methods may differ.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const compare = join(dirname(fileURLToPath(import.meta.url)), 'compare.mjs');

function generated(lines) {
  return `pub mod main_thread_a {
    pub const PROXY: &str = "MainThreadA";
    pub const SID: &str = "MainThreadA";
    pub const NID: u8 = 1;
    pub const METHODS: &[MethodInfo] = &[
${lines.map((l) => `        ${l},`).join('\n')}
    ];
}
`;
}

const bridgedV1 = 'MethodInfo { name: "$b", line: 10, returns: "void", cancellable: false, params: &[ParamInfo { name: "x", ts_type: "string", optional: false, rest: false }] }';
const todoV1 = 'MethodInfo { name: "$t", line: 11, returns: "void", cancellable: false, params: &[] }';
const rpcIds = 'pub const PROXY_IDS: &[&str] = &[\n    "MainThreadA", // 1\n    "ExtHostB", // 2\n];\n';
const proto = (a = 'MainThreadA', b = 'ExtHostB') =>
  `export const MainContext = {\n\t${a}: createProxyIdentifier<X>('${a}'),\n};\nexport const ExtHostContext = {\n\t${b}: createProxyIdentifier<Y>('${b}'),\n};\n`;
const ledger = '[MainThreadA]\n"$b" = "bridged"\n"$t" = "todo"\n';

function run(candidateLines, candidateProto) {
  const dir = mkdtempSync(join(tmpdir(), 'compare-'));
  try {
    const f = (name, text) => {
      const p = join(dir, name);
      writeFileSync(p, text);
      return p;
    };
    const args = [
      f('pinned.rs', generated([bridgedV1, todoV1])),
      f('candidate.rs', generated(candidateLines)),
      f('candidate.ts', candidateProto),
      f('rpc_ids.rs', rpcIds),
      f('coverage.toml', ledger),
    ];
    return spawnSync(process.execPath, [compare, ...args], { encoding: 'utf8' });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('fr_e10_admits_moved_lines_and_changed_todo_methods', () => {
  const moved = bridgedV1.replace('line: 10', 'line: 99');
  const todoChanged = 'MethodInfo { name: "$t", line: 12, returns: "Promise<string>", cancellable: true, params: &[] }';
  const extra = 'MethodInfo { name: "$new", line: 13, returns: "void", cancellable: false, params: &[] }';
  const r = run([moved, todoChanged, extra], proto());
  assert.equal(r.status, 0, r.stdout + r.stderr);
  assert.match(r.stdout, /admitted/);
});

test('fr_e10_refuses_a_changed_bridged_signature', () => {
  const changed = bridgedV1.replace('ts_type: "string"', 'ts_type: "number"');
  const r = run([changed, todoV1], proto());
  assert.equal(r.status, 1, r.stdout + r.stderr);
  assert.match(r.stdout, /MainThreadA\.\$b: signature changed/);
});

test('fr_e10_refuses_a_missing_bridged_method', () => {
  const r = run([todoV1], proto());
  assert.equal(r.status, 1, r.stdout + r.stderr);
  assert.match(r.stdout, /MainThreadA\.\$b: bridged, missing at the candidate commit/);
});

test('fr_e10_refuses_moved_proxy_ids', () => {
  const r = run([bridgedV1, todoV1], proto('MainThreadA', 'ExtHostC'));
  assert.equal(r.status, 1, r.stdout + r.stderr);
  assert.match(r.stdout, /proxy id 2: pinned ExtHostB, candidate ExtHostC/);
});
