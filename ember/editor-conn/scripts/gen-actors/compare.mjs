// Decide whether a Code-OSS commit may join the supported set (SPEC FR-E10, INTENT D50).
//
// Usage: node compare.mjs <pinned generated.rs> <candidate generated.rs> <candidate extHost.protocol.ts>
//                         <rpc_ids.rs> <coverage.toml>
//
// A commit is admitted when
//   1. its proxy identifiers (MainThread* and ExtHost*, in declaration order, which is the nid
//      order) are exactly src/rpc_ids.rs PROXY_IDS, so every id Ember sends or receives means
//      the same actor; and
//   2. every method the coverage ledger marks `bridged` has the same signature (name, return
//      type, cancellable, parameters) as in the pinned generated.rs. Source line numbers may differ.
// Prints a report and exits 0 (admitted) or 1 (refused); 2 on bad input.

import { readFileSync } from 'node:fs';

const [pinnedRs, candidateRs, candidateProto, rpcIdsRs, ledgerToml] = process.argv.slice(2);
if (!ledgerToml) {
  console.error('usage: node compare.mjs <pinned generated.rs> <candidate generated.rs> <candidate extHost.protocol.ts> <rpc_ids.rs> <coverage.toml>');
  process.exit(2);
}
const read = (p) => readFileSync(p, 'utf8');

/** proxy -> { nid, methods: Map(name -> signature without the line number) } */
function actors(rs) {
  const out = new Map();
  const mod = /pub const PROXY: &str = "(\w+)";[\s\S]*?pub const NID: u8 = (\d+);[\s\S]*?pub const METHODS: &\[MethodInfo\] = &\[([\s\S]*?)\n    \];/g;
  for (const m of rs.matchAll(mod)) {
    const methods = new Map();
    for (const line of m[3].split('\n')) {
      const mm = line.match(/MethodInfo \{ name: "([^"]+)", line: \d+, (.*) \},?\s*$/);
      if (mm) methods.set(mm[1], mm[2]);
    }
    out.set(m[1], { nid: Number(m[2]), methods });
  }
  if (out.size === 0) {
    console.error('no actors found in a generated.rs');
    process.exit(2);
  }
  return out;
}

function proxyIdsFromProto(ts) {
  return ts
    .split('\n')
    .filter((l) => l.includes('createProxyIdentifier<'))
    .map((l) => l.replace(/^\s*([A-Za-z0-9_]+):.*$/, '$1').trim());
}

function proxyIdsFromRs(rs) {
  const body = rs.match(/pub const PROXY_IDS: &\[&str\] = &\[([\s\S]*?)\n\];/);
  if (!body) {
    console.error('PROXY_IDS not found in rpc_ids.rs');
    process.exit(2);
  }
  return [...body[1].matchAll(/"([A-Za-z0-9_]+)"/g)].map((m) => m[1]);
}

function bridged(toml) {
  const out = [];
  let actor = null;
  for (const raw of toml.split('\n')) {
    const line = raw.trim();
    const h = line.match(/^\[(\w+)\]$/);
    if (h) {
      actor = h[1];
      continue;
    }
    const m = line.match(/^"(\$\w+)" = "bridged"$/);
    if (m && actor) out.push([actor, m[1]]);
  }
  return out;
}

const problems = [];
const pinnedIds = proxyIdsFromRs(read(rpcIdsRs));
const candidateIds = proxyIdsFromProto(read(candidateProto));
const n = Math.max(pinnedIds.length, candidateIds.length);
for (let i = 0; i < n; i++) {
  if (pinnedIds[i] !== candidateIds[i]) {
    problems.push(`proxy id ${i + 1}: pinned ${pinnedIds[i] ?? '(none)'}, candidate ${candidateIds[i] ?? '(none)'}`);
  }
}

const pinned = actors(read(pinnedRs));
const candidate = actors(read(candidateRs));
const methods = bridged(read(ledgerToml));
for (const [proxy, method] of methods) {
  const p = pinned.get(proxy)?.methods.get(method);
  const c = candidate.get(proxy)?.methods.get(method);
  if (p === undefined) {
    problems.push(`${proxy}.${method}: bridged in the ledger but not in the pinned generated.rs`);
  } else if (c === undefined) {
    problems.push(`${proxy}.${method}: bridged, missing at the candidate commit`);
  } else if (p !== c) {
    problems.push(`${proxy}.${method}: signature changed\n    pinned:    ${p}\n    candidate: ${c}`);
  }
}

console.log(`proxy ids: pinned ${pinnedIds.length}, candidate ${candidateIds.length}; bridged methods compared: ${methods.length}`);
if (problems.length) {
  console.log(`refused (${problems.length}):`);
  for (const p of problems) console.log(`  ${p}`);
  process.exit(1);
}
console.log('admitted: same proxy ids, same signature for every bridged method');
