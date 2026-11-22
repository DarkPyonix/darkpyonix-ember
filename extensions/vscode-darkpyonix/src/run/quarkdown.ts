// Separate-process Quarkdown renderer (SPEC FR-F14, INTENT D24).
// Invokes the user's local `quarkdown` CLI in a child process to keep GPL-3.0 isolated from
// Ember (Apache-2.0). Never links or embeds Quarkdown; returns a clean fallback when absent.
import { execFile } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const QUARKDOWN_MIME = "text/vnd.darkpyonix.quarkdown";
const DEFAULT_TIMEOUT_MS = 15_000;

export interface QuarkdownRenderOptions {
  cliPath?: string;
  timeoutMs?: number;
}

export type QuarkdownRenderResult =
  | { ok: true; html: string }
  | { ok: false; reason: "not_installed" | "failed"; message: string };

function findHtmlOutput(dir: string): string | null {
  const entries = readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name));
  // Prefer index.html at this level, then any .html file, then recurse into subdirectories.
  for (const e of entries) {
    if (e.isFile() && e.name.toLowerCase() === "index.html") {
      return join(dir, e.name);
    }
  }
  for (const e of entries) {
    if (e.isFile() && e.name.toLowerCase().endsWith(".html")) {
      return join(dir, e.name);
    }
  }
  for (const e of entries) {
    if (e.isDirectory()) {
      const found = findHtmlOutput(join(dir, e.name));
      if (found) return found;
    }
  }
  return null;
}

export function renderQuarkdown(
  source: string,
  options: QuarkdownRenderOptions = {},
): Promise<QuarkdownRenderResult> {
  const cli = (options.cliPath || process.env.DARKPYONIX_QUARKDOWN_CLI || "quarkdown").trim() || "quarkdown";
  const timeout = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const workDir = mkdtempSync(join(tmpdir(), "dpx-quarkdown-"));
  const inputPath = join(workDir, "cell.qd");
  const outDir = join(workDir, "out");

  try {
    writeFileSync(inputPath, source, "utf8");
  } catch (err) {
    rmSync(workDir, { recursive: true, force: true });
    return Promise.resolve({
      ok: false,
      reason: "failed",
      message: `Failed to stage Quarkdown input: ${String(err)}`,
    });
  }

  return new Promise((resolve) => {
    execFile(
      cli,
      ["c", inputPath, "--out", outDir],
      { timeout, maxBuffer: 8 * 1024 * 1024 },
      (err, _stdout, stderr) => {
        try {
          if (err) {
            const code = (err as NodeJS.ErrnoException).code;
            if (code === "ENOENT") {
              resolve({
                ok: false,
                reason: "not_installed",
                message: `Quarkdown CLI ('${cli}') is not installed; showing text/plain fallback.`,
              });
              return;
            }
            const detail = (stderr || err.message || String(err)).trim();
            resolve({
              ok: false,
              reason: "failed",
              message: `Quarkdown CLI failed: ${detail}`,
            });
            return;
          }
          const htmlPath = findHtmlOutput(outDir);
          if (!htmlPath) {
            resolve({
              ok: false,
              reason: "failed",
              message: "Quarkdown CLI exited 0 but produced no HTML output.",
            });
            return;
          }
          const html = readFileSync(htmlPath, "utf8");
          resolve({ ok: true, html });
        } catch (readErr) {
          resolve({
            ok: false,
            reason: "failed",
            message: `Failed to read Quarkdown HTML output: ${String(readErr)}`,
          });
        } finally {
          rmSync(workDir, { recursive: true, force: true });
        }
      },
    );
  });
}
