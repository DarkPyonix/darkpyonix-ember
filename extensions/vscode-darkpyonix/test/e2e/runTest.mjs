// Runs test/e2e/suite.ts inside a real VS Code with @vscode/test-electron.
// Uses an installed VS Code (VSCODE_PATH, default: the macOS app); nothing is downloaded.
// The test workspace lives under <worktree>/.scratch/vscode-e2e; the profile in <worktree>/.scratch/u
// (short: VS Code puts its IPC socket there and macOS limits socket paths to 103 chars).
import { runTests } from "@vscode/test-electron";
import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const ext = resolve(here, "..", "..");
const scratch = resolve(ext, "..", "..", ".scratch", "vscode-e2e");
const exe = process.env.VSCODE_PATH ?? "/Applications/Visual Studio Code.app/Contents/MacOS/Code";
if (!existsSync(exe)) {
  console.error(`No VS Code at ${exe}; set VSCODE_PATH. (Not downloading one.)`);
  process.exit(2);
}

rmSync(join(scratch, "ws"), { recursive: true, force: true });
mkdirSync(join(scratch, "ws"), { recursive: true });
cpSync(join(ext, "test", "corpus", "darkpyonix_format.py"), join(scratch, "ws", "nb.pynb"));
cpSync(join(ext, "test", "corpus", "darkpyonix_format.py"), join(scratch, "ws", "plain.py"));

try {
  await runTests({
    vscodeExecutablePath: exe,
    extensionDevelopmentPath: ext,
    extensionTestsPath: join(ext, "dist", "e2e", "suite.js"),
    extensionTestsEnv: { DPX_E2E_WS: join(scratch, "ws") },
    launchArgs: [
      join(scratch, "ws"),
      "--disable-extensions",
      "--disable-workspace-trust",
      "--skip-welcome",
      "--skip-release-notes",
      `--user-data-dir=${join(scratch, "..", "u")}`,
      `--extensions-dir=${join(scratch, "extensions")}`,
    ],
  });
} catch (err) {
  console.error("e2e failed:", err);
  process.exit(1);
}
