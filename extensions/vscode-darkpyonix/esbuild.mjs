// Bundles the extension into dist/extension.js (CommonJS, `vscode` external).
// --e2e also bundles the VS Code integration suite into dist/e2e/suite.js.
import * as esbuild from "esbuild";

const watch = process.argv.includes("--watch");
const e2e = process.argv.includes("--e2e");

const common = {
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node18",
  external: ["vscode"],
  sourcemap: true,
  logLevel: "info",
};

const builds = [{ ...common, entryPoints: ["src/extension.ts"], outfile: "dist/extension.js" }];
if (e2e) {
  builds.push({ ...common, entryPoints: ["test/e2e/suite.ts"], outfile: "dist/e2e/suite.js" });
}

if (watch) {
  for (const b of builds) await (await esbuild.context(b)).watch();
} else {
  await Promise.all(builds.map((b) => esbuild.build(b)));
}
