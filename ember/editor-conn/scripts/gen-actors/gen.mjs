// Generates ember/editor-conn/src/actors/generated.rs from Code-OSS's extHost.protocol.ts.
//
// Usage:
//   node gen.mjs <extHost.protocol.ts> <commit> > ../../src/actors/generated.rs
//   node gen.mjs --types <extHost.protocol.ts>    (lists every parameter type and its Rust mapping)
//
// What it reads: the `MainContext` object (`createProxyIdentifier<Shape>('Name')` entries, in
// declaration order) and, for each entry, the `Shape` interface with the members of the interfaces
// it extends inside the same file. Every `$method` of a shape becomes a method id, an argument
// struct with a decoder and an encoder, and a trait method whose default says "not implemented".
// Behaviour is written by hand by implementing the trait; nothing in the output is edited.
//
// The output is deterministic for a given input file, so CI regenerates it at the pinned commit
// and fails on any difference (scripts/check-actors.sh).

import fs from 'node:fs';
import ts from 'typescript';

const args = process.argv.slice(2);
const typesOnly = args[0] === '--types';
if (typesOnly) args.shift();
const [protoPath, commit] = args;
if (!protoPath || (!typesOnly && !commit)) {
  console.error('usage: node gen.mjs <extHost.protocol.ts> <commit> | --types <extHost.protocol.ts>');
  process.exit(64);
}

const text = fs.readFileSync(protoPath, 'utf8');
const sf = ts.createSourceFile('extHost.protocol.ts', text, ts.ScriptTarget.Latest, true);
const lineOf = (node) => sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
const die = (msg) => {
  console.error(`gen-actors: ${msg}`);
  process.exit(2);
};

// ---- read the file -----------------------------------------------------------------------------

const interfaces = new Map();
const typeAliases = new Map();
let mainContext = null;
// Every createProxyIdentifier call in file order: the nid of each is its 1-based position.
const proxyOrder = [];

function visit(node) {
  if (ts.isInterfaceDeclaration(node)) interfaces.set(node.name.text, node);
  if (ts.isTypeAliasDeclaration(node)) typeAliases.set(node.name.text, node);
  if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'createProxyIdentifier') {
    const a = node.arguments[0];
    if (!a || !ts.isStringLiteral(a)) die(`createProxyIdentifier without a string literal at line ${lineOf(node)}`);
    proxyOrder.push(a.text);
  }
  if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === 'MainContext') {
    mainContext = node;
  }
  ts.forEachChild(node, visit);
}
visit(sf);
if (!mainContext) die('MainContext not found');

const init = mainContext.initializer;
if (!init || !ts.isObjectLiteralExpression(init)) die('MainContext is not an object literal');

const actors = [];
for (const prop of init.properties) {
  if (!ts.isPropertyAssignment(prop)) die(`MainContext member at line ${lineOf(prop)} is not a property`);
  const call = prop.initializer;
  if (!ts.isCallExpression(call) || call.expression.getText(sf) !== 'createProxyIdentifier') {
    die(`MainContext.${prop.name.getText(sf)} is not createProxyIdentifier(...)`);
  }
  const shapeName = call.typeArguments?.[0]?.getText(sf);
  // The MainContext key is the name Ember uses (rpc_ids.rs); the string literal is the
  // identifier's sid, which upstream sometimes spells differently ('MainThreadDiaglogs').
  const proxy = prop.name.getText(sf);
  const sid = call.arguments[0].text;
  if (!shapeName) die(`MainContext.${proxy} has no shape type`);
  const nid = proxyOrder.indexOf(sid) + 1;
  if (nid < 1 || proxyOrder.lastIndexOf(sid) + 1 !== nid) die(`${sid} is not declared exactly once`);
  actors.push({ proxy, sid, shapeName, nid, line: lineOf(prop) });
}

// Members of an interface and of the interfaces it extends in this file (base members first, so a
// redeclaration in the derived interface wins). IDisposable and other outside bases add nothing
// callable over RPC.
function members(name, seen = new Set()) {
  if (seen.has(name)) return [];
  seen.add(name);
  const decl = interfaces.get(name);
  if (!decl) return [];
  const out = [];
  for (const h of decl.heritageClauses ?? []) {
    for (const t of h.types) out.push(...members(t.expression.getText(sf), seen));
  }
  for (const m of decl.members) {
    if (!(ts.isMethodSignature(m) || ts.isPropertySignature(m))) continue;
    const n = m.name.getText(sf);
    if (!n.startsWith('$')) continue;
    const i = out.findIndex((x) => x.name.getText(sf) === n);
    if (i >= 0) out.splice(i, 1);
    out.push(m);
  }
  return out;
}

// ---- TypeScript type to Rust type --------------------------------------------------------------

// A number is an integer on the wire (handles, ids, counts, lines) unless the parameter is one of
// the few that carry a JS double: timings, sizes, priorities and free values. Decoding an integer
// field fails on a fraction, so a parameter added upstream that carries one shows up in a test.
const FLOAT_NAME = /(Time|^duration|^height|^priority|^value|^result|Threshold)$/;

const KNOWN = {
  string: 'String',
  boolean: 'bool',
  UriComponents: 'UriComponents',
  VSBuffer: 'VsBuffer',
};

function stripParens(t) {
  while (ts.isParenthesizedTypeNode(t)) t = t.type;
  return t;
}

// Returns { rust, optional } where optional means the TS type admits undefined or null.
function mapType(t, paramName) {
  t = stripParens(t);
  if (ts.isUnionTypeNode(t)) {
    const rest = t.types.filter((u) => {
      const s = stripParens(u);
      return !(s.kind === ts.SyntaxKind.UndefinedKeyword
        || (ts.isLiteralTypeNode(s) && s.literal.kind === ts.SyntaxKind.NullKeyword)
        || s.kind === ts.SyntaxKind.VoidKeyword);
    });
    const optional = rest.length !== t.types.length;
    if (rest.length === 1) {
      const inner = mapType(rest[0], paramName);
      return { rust: inner.rust, optional: optional || inner.optional };
    }
    // A union of string literals is a string on the wire; a union of numbers is a number.
    if (rest.every((u) => ts.isLiteralTypeNode(stripParens(u)) && ts.isStringLiteral(stripParens(u).literal))) {
      return { rust: 'String', optional };
    }
    return { rust: 'Value', optional };
  }
  switch (t.kind) {
    case ts.SyntaxKind.StringKeyword: return { rust: 'String', optional: false };
    case ts.SyntaxKind.BooleanKeyword: return { rust: 'bool', optional: false };
    case ts.SyntaxKind.NumberKeyword:
      return { rust: FLOAT_NAME.test(paramName) ? 'f64' : 'i64', optional: false };
    case ts.SyntaxKind.AnyKeyword:
    case ts.SyntaxKind.UnknownKeyword:
    case ts.SyntaxKind.ObjectKeyword:
      return { rust: 'Value', optional: false };
    case ts.SyntaxKind.UndefinedKeyword:
      return { rust: 'Value', optional: true };
  }
  if (ts.isLiteralTypeNode(t)) {
    if (ts.isStringLiteral(t.literal)) return { rust: 'String', optional: false };
    if (t.literal.kind === ts.SyntaxKind.TrueKeyword || t.literal.kind === ts.SyntaxKind.FalseKeyword) return { rust: 'bool', optional: false };
    return { rust: 'Value', optional: false };
  }
  if (ts.isArrayTypeNode(t)) {
    const el = mapType(t.elementType, '');
    return { rust: el.optional ? 'Vec<Option<' + el.rust + '>>' : `Vec<${el.rust}>`, optional: false };
  }
  if (ts.isTypeReferenceNode(t)) {
    const name = t.typeName.getText(sf);
    if (name in KNOWN) return { rust: KNOWN[name], optional: false };
    if ((name === 'Array' || name === 'ReadonlyArray') && t.typeArguments?.length === 1) {
      return mapType(ts.factory.createArrayTypeNode(t.typeArguments[0]), paramName);
    }
    if (name === 'SerializableObjectWithBuffers') return { rust: 'ObjectWithBuffers', optional: false };
    // An alias for a plain primitive or a string-literal union maps like its target.
    const alias = typeAliases.get(name);
    if (alias && !alias.typeParameters) {
      const target = stripParens(alias.type);
      const primitive = [ts.SyntaxKind.StringKeyword, ts.SyntaxKind.NumberKeyword, ts.SyntaxKind.BooleanKeyword].includes(target.kind);
      const strUnion = ts.isUnionTypeNode(target)
        && target.types.every((u) => ts.isLiteralTypeNode(stripParens(u)) && ts.isStringLiteral(stripParens(u).literal));
      if (primitive || strUnion) return mapType(target, paramName);
    }
    return { rust: 'Value', optional: false };
  }
  return { rust: 'Value', optional: false };
}

// ---- collect -----------------------------------------------------------------------------------

const snake = (s) => s
  .replace(/^\$/, '')
  .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
  .replace(/([A-Z]+)([A-Z][a-z])/g, '$1_$2')
  .toLowerCase();
const pascal = (s) => s.replace(/^\$/, '').replace(/^./, (c) => c.toUpperCase());
const RUST_KEYWORDS = new Set(['type', 'ref', 'self', 'match', 'move', 'mod', 'loop', 'impl', 'fn', 'use', 'where', 'in', 'as', 'box', 'crate', 'dyn', 'enum', 'extern', 'final', 'macro', 'override', 'priv', 'pub', 'struct', 'super', 'trait', 'try', 'unsafe', 'virtual', 'yield', 'async', 'await', 'const', 'static', 'let', 'mut', 'return', 'break', 'continue', 'else', 'if', 'for', 'while', 'true', 'false', 'abstract', 'become', 'do', 'gen', 'typeof', 'unsized']);
const field = (s) => {
  const f = snake(s);
  return RUST_KEYWORDS.has(f) ? `r#${f}` : f;
};
const oneLine = (s) => s.replace(/\s+/g, ' ').trim();
const rustStr = (s) => JSON.stringify(s);

const typeInventory = new Map();
for (const a of actors) {
  if (!interfaces.has(a.shapeName)) die(`${a.proxy}: interface ${a.shapeName} not found`);
  a.module = snake(a.proxy);
  a.traitName = a.proxy.replace(/^MainThread/, '') || a.proxy;
  a.methods = [];
  for (const m of members(a.shapeName)) {
    const name = m.name.getText(sf);
    let params = [];
    let returns = 'void';
    if (ts.isMethodSignature(m)) {
      params = [...m.parameters];
      returns = m.type ? oneLine(m.type.getText(sf)) : 'void';
    } else if (m.type && ts.isFunctionTypeNode(m.type)) {
      params = [...m.type.parameters];
      returns = oneLine(m.type.type.getText(sf));
    } else {
      die(`${a.shapeName}.${name} at line ${lineOf(m)} is neither a method nor a function property`);
    }
    // A trailing CancellationToken is not sent: it selects the cancellable message type.
    let cancellable = false;
    const last = params[params.length - 1];
    if (last && last.type && last.type.getText(sf) === 'CancellationToken') {
      cancellable = true;
      params = params.slice(0, -1);
    }
    const ps = params.map((p, i) => {
      // A destructured parameter has no name of its own; it is the i-th argument.
      const pname = ts.isIdentifier(p.name) ? p.name.text : `arg${i}`;
      if (p.dotDotDotToken) {
        return { name: pname, field: field(pname), ts: oneLine(p.type?.getText(sf) ?? 'any'), rust: 'Vec<Value>', optional: false, rest: true, index: i };
      }
      const tsType = p.type ? oneLine(p.type.getText(sf)) : 'any';
      const mapped = p.type ? mapType(p.type, pname) : { rust: 'Value', optional: false };
      const optional = mapped.optional || !!p.questionToken;
      const entry = typeInventory.get(tsType) ?? { rust: mapped.rust, n: 0 };
      entry.n++;
      typeInventory.set(tsType, entry);
      return { name: pname, field: field(pname), ts: tsType, rust: mapped.rust, optional, rest: false, index: i };
    });
    if (ps.some((p) => p.rest && p.index !== ps.length - 1)) die(`${a.shapeName}.${name}: rest parameter is not last`);
    a.methods.push({
      name,
      variant: pascal(name),
      fn: field(name),
      argsStruct: `${pascal(name)}Args`,
      params: ps,
      returns,
      cancellable,
      line: lineOf(m),
    });
  }
}

if (typesOnly) {
  const rows = [...typeInventory.entries()].sort((x, y) => y[1].n - x[1].n);
  for (const [t, { rust, n }] of rows) console.log(`${String(n).padStart(4)}  ${rust.padEnd(22)} ${t}`);
  console.error(`${actors.length} actors, ${actors.reduce((s, a) => s + a.methods.length, 0)} methods, ${rows.length} parameter types`);
  process.exit(0);
}

// ---- emit --------------------------------------------------------------------------------------

const out = [];
const w = (s = '') => out.push(s);
const totalMethods = actors.reduce((s, a) => s + a.methods.length, 0);

const rustType = (p) => (p.optional ? `Option<${p.rust}>` : p.rust);

w('// @generated by ember/editor-conn/scripts/gen-actors/gen.mjs. Do not edit by hand.');
w(`// Source: src/vs/workbench/api/common/extHost.protocol.ts at microsoft/vscode ${commit}.`);
w('// Regenerate with ember/editor-conn/scripts/gen-actors/gen.sh; CI fails when this file');
w('// differs from what the generator writes for the pinned commit.');
w();
w('#![allow(clippy::all, unused_variables)]');
w();
w('use super::codec::*;');
w('use super::{ActorInfo, Ctx, MethodInfo, Outcome, ParamInfo};');
w('use crate::rpc::Arg;');
w('use crate::Result;');
w();
w('/// The Code-OSS commit this file was generated from.');
w(`pub const GENERATED_FROM: &str = ${rustStr(commit)};`);
w();
w(`/// Number of \`MainThread*\` actors in \`MainContext\`.`);
w(`pub const ACTOR_COUNT: usize = ${actors.length};`);
w(`/// Number of \`$\` methods across all actors.`);
w(`pub const METHOD_COUNT: usize = ${totalMethods};`);
w();

for (const a of actors) {
  w(`/// \`${a.proxy}\` (nid ${a.nid}): \`${a.shapeName}\`, extHost.protocol.ts line ${interfaces.get(a.shapeName) ? lineOf(interfaces.get(a.shapeName)) : 0}.`);
  w(`pub mod ${a.module} {`);
  w('    use super::*;');
  w();
  w(`    pub const PROXY: &str = ${rustStr(a.proxy)};`);
  w(`    /// The identifier's own string (\`ProxyIdentifier.sid\`), as upstream spells it.`);
  w(`    pub const SID: &str = ${rustStr(a.sid)};`);
  w(`    pub const NID: u8 = ${a.nid};`);
  w();
  w('    /// The methods of this actor, as named on the wire.');
  w('    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]');
  w('    pub enum Method {');
  for (const m of a.methods) w(`        ${m.variant},`);
  w('    }');
  w();
  w('    impl Method {');
  w(`        pub const ALL: &'static [Method] = &[${a.methods.map((m) => `Method::${m.variant}`).join(', ')}];`);
  w();
  w("        pub fn name(self) -> &'static str {");
  if (a.methods.length === 0) {
    w('        match self {}');
  } else {
    w('            match self {');
    for (const m of a.methods) w(`                Method::${m.variant} => ${rustStr(m.name)},`);
    w('            }');
  }
  w('        }');
  w();
  w('        pub fn from_name(name: &str) -> Option<Method> {');
  w('            match name {');
  for (const m of a.methods) w(`                ${rustStr(m.name)} => Some(Method::${m.variant}),`);
  w('                _ => None,');
  w('            }');
  w('        }');
  w('    }');
  w();
  for (const m of a.methods) {
    w(`    /// Arguments of \`${a.proxy}.${m.name}\` (line ${m.line}), returns \`${m.returns}\`${m.cancellable ? '; cancellable' : ''}.`);
    w('    #[derive(Debug, Clone, PartialEq)]');
    if (m.params.length === 0) {
      w(`    pub struct ${m.argsStruct};`);
    } else {
      w(`    pub struct ${m.argsStruct} {`);
      for (const p of m.params) {
        w(`        /// \`${p.name}${p.optional && !p.rest ? '?' : ''}: ${p.ts.replace(/`/g, "'")}\``);
        w(`        pub ${p.field}: ${rustType(p)},`);
      }
      w('    }');
    }
    w();
    w(`    impl ${m.argsStruct} {`);
    w('        pub fn decode(args: &[Arg]) -> Result<Self> {');
    if (m.params.length === 0) {
      w(`        Ok(${m.argsStruct})`);
    } else {
      w(`            Ok(${m.argsStruct} {`);
      for (const p of m.params) {
        const how = p.rest ? `rest(args, ${p.index})` : p.optional ? `opt::<${p.rust}>(args, ${p.index}, ${rustStr(p.name)})?` : `req::<${p.rust}>(args, ${p.index}, ${rustStr(p.name)})?`;
        w(`                ${p.field}: ${how},`);
      }
      w('            })');
    }
    w('        }');
    w();
    w('        pub fn encode(&self) -> Result<Vec<Arg>> {');
    if (m.params.length === 0) {
      w('            Ok(Vec::new())');
    } else {
      w('            let mut out = Vec::new();');
      for (const p of m.params) {
        if (p.rest) w(`            put_rest(&mut out, &self.${p.field});`);
        else if (p.optional) w(`            put_opt(&mut out, &self.${p.field})?;`);
        else w(`            put(&mut out, &self.${p.field})?;`);
      }
      w('            trim_undefined(&mut out);');
      w('            Ok(out)');
    }
    w('        }');
    w('    }');
    w();
  }
  w(`    /// Ember's side of \`${a.proxy}\`. Every method defaults to [\`Outcome::Unimplemented\`]; an`);
  w('    /// implementation overrides the methods it supports.');
  w(`    pub trait ${a.traitName} {`);
  for (const m of a.methods) {
    w(`        /// \`${m.name}\`, returns \`${m.returns}\`.`);
    w(`        fn ${m.fn}(&mut self, cx: &Ctx, args: ${m.argsStruct}) -> Outcome {`);
    w('            Outcome::Unimplemented');
    w('        }');
  }
  w('    }');
  w();
  w('    /// Decodes `args` for `method` and calls the matching trait method. `Ok(None)` means this');
  w('    /// actor has no such method at the pinned commit.');
  w(`    pub fn dispatch<A: ${a.traitName} + ?Sized>(actor: &mut A, cx: &Ctx, method: &str, args: &[Arg]) -> Result<Option<Outcome>> {`);
  if (a.methods.length === 0) {
    w('        let _ = (actor, cx, method, args);');
    w('        Ok(None)');
  } else {
    w('        Ok(Some(match Method::from_name(method) {');
    for (const m of a.methods) w(`            Some(Method::${m.variant}) => actor.${m.fn}(cx, ${m.argsStruct}::decode(args)?),`);
    w('            None => return Ok(None),');
    w('        }))');
  }
  w('    }');
  w();
  w(`    pub const METHODS: &[MethodInfo] = &[`);
  for (const m of a.methods) {
    const params = m.params.map((p) => `ParamInfo { name: ${rustStr(p.name)}, ts_type: ${rustStr(p.ts)}, optional: ${p.optional}, rest: ${p.rest} }`);
    w(`        MethodInfo { name: ${rustStr(m.name)}, line: ${m.line}, returns: ${rustStr(m.returns)}, cancellable: ${m.cancellable}, params: &[${params.join(', ')}] },`);
  }
  w('    ];');
  w('}');
  w();
}

w('/// Every actor in `MainContext` order (= nid order among the `MainThread*` identifiers).');
w('pub const ACTORS: &[ActorInfo] = &[');
for (const a of actors) {
  w(`    ActorInfo { proxy: ${a.module}::PROXY, nid: ${a.module}::NID, shape: ${rustStr(a.shapeName)}, methods: ${a.module}::METHODS },`);
}
w('];');
w();
w('/// One field per actor: `None` (the default) answers every call to that actor with');
w('/// [`Outcome::Unimplemented`]. Ember fills in the actors it implements.');
w('#[derive(Default)]');
w("pub struct Actors<'a> {");
for (const a of actors) w(`    pub ${a.module}: Option<&'a mut dyn ${a.module}::${a.traitName}>,`);
w('}');
w();
w("impl Actors<'_> {");
w('    /// Routes a request by proxy name. `Ok(None)`: the proxy or the method is not in the pinned');
w('    /// protocol.');
w('    pub fn dispatch(&mut self, proxy: &str, cx: &Ctx, method: &str, args: &[Arg]) -> Result<Option<Outcome>> {');
w('        match proxy {');
for (const a of actors) {
  w(`            ${a.module}::PROXY => match self.${a.module}.as_deref_mut() {`);
  w(`                Some(actor) => ${a.module}::dispatch(actor, cx, method, args),`);
  w(`                None => Ok(${a.module}::Method::from_name(method).map(|_| Outcome::Unimplemented)),`);
  w('            },');
}
w('            _ => Ok(None),');
w('        }');
w('    }');
w('}');

process.stdout.write(out.join('\n') + '\n');
