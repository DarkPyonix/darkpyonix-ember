// Finding a manager the way the darkpyonix CLI does (darkpyonix-core SPEC FR-C1, INTENT D4):
// a given manager (settings, else DARKPYONIX_MANAGER_URL + DARKPYONIX_TOKEN), else spawn our own
// `darkpyonix manager --ephemeral` and read `{url, token, pid}` from the first line of its stdout.
// No registry file is read or written. The spawned manager is left running when we leave; it
// ends by itself after its idle_timeout (FR-M3).
import { spawn } from "node:child_process";
import { chmodSync, closeSync, mkdirSync, openSync, readFileSync, renameSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export const URL_ENV = "DARKPYONIX_MANAGER_URL";
export const TOKEN_ENV = "DARKPYONIX_TOKEN";
/** Rotate manager.log past this size, like the CLI (FR-C1). */
export const MANAGER_LOG_LIMIT = 1024 * 1024;

/** The one JSON line a manager prints on stdout once it listens (FR-M3, FR-C1). */
export interface ManagerLine {
  url: string;
  token: string;
  pid: number;
}

export interface ManagerEndpoint {
  url: string;
  token: string | undefined;
  /** "settings" (configured URL), "env" (DARKPYONIX_MANAGER_URL) or "spawned" (we started it). */
  source: "settings" | "env" | "spawned";
  pid?: number;
}

export function darkpyonixHome(configured?: string, env: NodeJS.ProcessEnv = process.env): string {
  if (configured) return expandHome(configured);
  if (env.DARKPYONIX_HOME) return expandHome(env.DARKPYONIX_HOME);
  return join(homedir(), ".darkpyonix");
}

function expandHome(p: string): string {
  return p === "~" || p.startsWith("~/") ? join(homedir(), p.slice(1)) : p;
}

export function pidAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === "EPERM";
  }
}

export async function checkHealth(url: string, timeoutMs = 2000): Promise<boolean> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    const res = await fetch(url.replace(/\/+$/, "") + "/health", { signal: ctl.signal });
    return res.ok;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

/** Open `<home>/manager.log` for appending (0600), rotating it to `.1` past `limit` bytes. */
export function openManagerLog(path: string, limit = MANAGER_LOG_LIMIT): number {
  try {
    if (statSync(path).size > limit) renameSync(path, path + ".1");
  } catch {
    // No log yet.
  }
  const fd = openSync(path, "a", 0o600);
  if (process.platform !== "win32") {
    try { chmodSync(path, 0o600); } catch { /* best effort */ }
  }
  return fd;
}

function logTail(path: string, lines = 5): string {
  try {
    return readFileSync(path, "utf8").split(/\r?\n/).filter((l) => l !== "").slice(-lines).join("\n");
  } catch {
    return "";
  }
}

export interface SpawnOptions {
  cliPath: string;
  args: string[];
  home: string;
  timeoutMs?: number;
  /** Set DARKPYONIX_HOME for the child (when the home was configured explicitly). */
  passHome?: boolean;
  log?: (line: string) => void;
}

/**
 * Spawn `darkpyonix manager --ephemeral` in its own process group, read `{url, token, pid}` from
 * the first stdout line and wait for `/health`. Its stderr goes to `<home>/manager.log` (a file, not
 * a pipe, so the manager can keep writing after this process is gone) and the failure message shows
 * the end of that log.
 */
export async function spawnManager(opts: SpawnOptions): Promise<ManagerLine> {
  mkdirSync(opts.home, { recursive: true });
  const logPath = join(opts.home, "manager.log");
  const logFd = openManagerLog(logPath);
  const env = { ...process.env };
  if (opts.passHome) env.DARKPYONIX_HOME = opts.home;
  // A manager we spawn must not inherit a URL/token meant for us.
  delete env[URL_ENV];
  delete env[TOKEN_ENV];
  const timeoutMs = opts.timeoutMs ?? 15000;
  const child = spawn(opts.cliPath, opts.args, { detached: true, stdio: ["ignore", "pipe", logFd], env });
  closeSync(logFd);
  opts.log?.(`spawned ${opts.cliPath} ${opts.args.join(" ")} (pid ${child.pid})`);
  const fail = (what: string) => {
    const tail = logTail(logPath);
    return new Error(`could not start an ephemeral manager: ${what}${tail ? "\n" + tail : ""}`);
  };

  const line = await new Promise<string>((resolve, reject) => {
    let buf = "";
    let done = false;
    const finish = (err: Error | null, value = "") => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      if (err) reject(err);
      else resolve(value);
    };
    const timer = setTimeout(() => finish(fail(`no address on stdout within ${timeoutMs / 1000} s`)), timeoutMs);
    child.stdout!.setEncoding("utf8");
    child.stdout!.on("data", (d: string) => {
      buf += d;
      const nl = buf.indexOf("\n");
      if (nl >= 0) finish(null, buf.slice(0, nl));
    });
    child.on("error", (e) => finish(fail(`cannot start ${opts.cliPath}: ${e.message}`)));
    child.on("exit", (code, signal) => {
      // Give the log a moment to be flushed by the exiting child.
      setTimeout(() => finish(fail(`${opts.cliPath} ${opts.args.join(" ")} exited with ${signal ?? `exit code ${code}`} before printing its address`)), 50);
    });
  }).catch((err) => {
    try { if (child.pid) process.kill(child.pid, "SIGTERM"); } catch { /* gone */ }
    throw err;
  });

  // Keep draining stdout (the manager prints nothing more) but never hold the event loop for it.
  child.stdout!.removeAllListeners("data");
  child.stdout!.resume();
  child.removeAllListeners("exit");
  child.unref();
  (child.stdout as unknown as { unref?: () => void }).unref?.();

  let rec: ManagerLine;
  try {
    rec = JSON.parse(line.trim()) as ManagerLine;
  } catch {
    throw fail(`unexpected first line on stdout: ${line.trim()}`);
  }
  if (typeof rec.url !== "string" || typeof rec.token !== "string") throw fail(`unexpected first line on stdout: ${line.trim()}`);
  if (!(await checkHealth(rec.url))) throw fail(`${rec.url} did not answer /health`);
  return { url: rec.url, token: rec.token, pid: typeof rec.pid === "number" ? rec.pid : child.pid ?? 0 };
}

export interface ConnectSettings {
  /** `darkpyonix.manager.url` (and the token from the secret store or settings). */
  url?: string;
  token?: string;
  home: string;
  passHome: boolean;
  cliPath: string;
  managerArgs: string[];
  env?: NodeJS.ProcessEnv;
  spawnTimeoutMs?: number;
  log?: (line: string) => void;
}

/** The given manager (settings, else environment), else a freshly spawned ephemeral one (FR-C1). */
export async function resolveManager(s: ConnectSettings): Promise<ManagerEndpoint> {
  const given = givenManager(s);
  if (given) return given;
  const rec = await spawnManager({ cliPath: s.cliPath, args: s.managerArgs, home: s.home, passHome: s.passHome, timeoutMs: s.spawnTimeoutMs, log: s.log });
  return { url: rec.url, token: rec.token, source: "spawned", pid: rec.pid };
}

/** URL and token from the settings, else from the environment; only one of the two is an error. */
export function givenManager(s: Pick<ConnectSettings, "url" | "token" | "env">): ManagerEndpoint | undefined {
  const pick = (url: string | undefined, token: string | undefined, source: "settings" | "env", names: string): ManagerEndpoint | undefined => {
    if (url && token) return { url, token, source };
    if (url || token) throw new Error(`set both ${names}, or neither`);
    return undefined;
  };
  const fromSettings = pick(s.url || undefined, s.token || undefined, "settings", "darkpyonix.manager.url and its token (DarkPyonix: Connect)");
  if (fromSettings) return fromSettings;
  const env = s.env ?? process.env;
  return pick(env[URL_ENV] || undefined, env[TOKEN_ENV] || undefined, "env", `${URL_ENV} and ${TOKEN_ENV}`);
}

/** Map a local path to the manager machine's path using prefix rules (dedicated managers). */
export function mapPath(local: string, pathMap: Record<string, string>): string {
  let best = "";
  for (const prefix of Object.keys(pathMap)) {
    if (local.startsWith(prefix) && prefix.length > best.length) best = prefix;
  }
  return best ? pathMap[best] + local.slice(best.length) : local;
}
