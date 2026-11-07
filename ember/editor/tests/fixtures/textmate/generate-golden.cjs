// Regenerates golden/*.json: the tokens vscode-textmate (the engine VS Code runs) produces for each sample
// with the vendored grammars. The Rust tests compare the interpreter against these files, so they only
// need re-generating when a grammar or a sample changes.
//
//   OSE=<unpacked Code-OSS server dir>   (it holds node, node_modules/vscode-textmate and vscode-oniguruma)
//   "$OSE/node" ember/editor/tests/fixtures/textmate/generate-golden.cjs "$OSE"
//
// The files record vscode-textmate and vscode-oniguruma versions so a change of engine shows in review.
const fs = require("fs");
const path = require("path");

const ose = process.argv[2];
if (!ose) {
  console.error("usage: node generate-golden.cjs <unpacked OSE server dir>");
  process.exit(2);
}
const textmate = require(path.join(ose, "node_modules/vscode-textmate"));
const oniguruma = require(path.join(ose, "node_modules/vscode-oniguruma"));
const versions = {
  "vscode-textmate": require(path.join(ose, "node_modules/vscode-textmate/package.json")).version,
  "vscode-oniguruma": require(path.join(ose, "node_modules/vscode-oniguruma/package.json")).version,
};

const here = __dirname;
const grammarsDir = path.join(here, "grammars");
const byScope = {};
for (const file of fs.readdirSync(grammarsDir).filter((f) => f.endsWith(".json") || f.endsWith(".tmLanguage"))) {
  const full = path.join(grammarsDir, file);
  byScope[textmate.parseRawGrammar(fs.readFileSync(full, "utf8"), full).scopeName] = full;
}

// Grammars injected into a scope (what an extension's `injectTo` says).
const injections = {
  "source.ts": ["documentation.injection.ts"],
  "source.js": ["documentation.injection.js.jsx"],
  "source.host": ["source.guest"],
};

const samples = {
  "sample.ts": "source.ts",
  "sample.js": "source.js",
  "sample.rs": "source.rust",
  "sample.py": "source.python",
  "sample.json": "source.json",
  "sample.md": "text.html.markdown",
  "sample.host": "source.host",
};

async function main() {
  const wasm = fs.readFileSync(path.join(ose, "node_modules/vscode-oniguruma/release/onig.wasm")).buffer;
  await oniguruma.loadWASM(wasm);
  const registry = new textmate.Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (sources) => new oniguruma.OnigScanner(sources),
      createOnigString: (s) => new oniguruma.OnigString(s),
    }),
    getInjections: (scope) => injections[scope],
    loadGrammar: async (scope) => {
      const file = byScope[scope];
      if (!file) return null;
      return textmate.parseRawGrammar(fs.readFileSync(file, "utf8"), file);
    },
  });
  fs.mkdirSync(path.join(here, "golden"), { recursive: true });
  for (const [sample, scope] of Object.entries(samples)) {
    const grammar = await registry.loadGrammar(scope);
    const text = fs.readFileSync(path.join(here, "samples", sample), "utf8");
    const lines = text.split("\n");
    if (lines[lines.length - 1] === "") lines.pop();
    let state = null;
    const out = [];
    for (const line of lines) {
      const result = grammar.tokenizeLine(line, state);
      state = result.ruleStack;
      // Offsets are UTF-16 code units. Clamp to the line, drop empty tokens, merge neighbours with the same scopes.
      const merged = [];
      for (const t of result.tokens) {
        const start = Math.min(t.startIndex, line.length);
        const end = Math.min(t.endIndex, line.length);
        if (start >= end) continue;
        const last = merged[merged.length - 1];
        if (last && last[1] === start && JSON.stringify(last[2]) === JSON.stringify(t.scopes)) last[1] = end;
        else merged.push([start, end, t.scopes]);
      }
      out.push(merged);
    }
    const golden = { grammar: scope, sample, engine: versions, lines: out };
    fs.writeFileSync(path.join(here, "golden", sample.replace("sample.", "") + ".json"), JSON.stringify(golden));
    console.log(sample, scope, lines.length, "lines", out.reduce((n, l) => n + l.length, 0), "tokens");
  }
}
main().catch((e) => { console.error(e); process.exit(1); });
