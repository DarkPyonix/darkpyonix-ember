// Finding a manager the way the darkpyonix CLI does (SPEC FR-C1, FR-M3):
// a live ephemeral manager from `<home>/managers/<pid>.json`, else spawn one and wait for its
// registry file; or a configured dedicated manager (URL + token).
import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface RegistryRecord {
  pid: number;
  url: string;
  token: string;
  mode: string;
  started_at?: string;
  version?: string;
}

export interface ManagerEndpoint {
  url: string;
  token: string | undefined;
  /** "ephemeral" (local registry), "spawned" (we started it) or "dedicated" (configured URL). */
  source: "ephemeral" | "spawned" | "dedicated";
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
    if (!res.ok) return false;
    const body = (await res.json()) as { status?: string };
    return body.status === "ok";
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

export async function readRegistry(home: string): Promise<RegistryRecord[]> {
  const dir = join(home, "managers");
  let names: string[];
  try {
    names = await fs.readdir(dir);
  } catch {
    return [];
  }
  const out: RegistryRecord[] = [];
  for (const name of names) {
    if (!name.endsWith(".json")) continue;
    try {
      const rec = JSON.parse(await fs.readFile(join(dir, name), "utf8")) as RegistryRecord;
      if (typeof rec.url === "string" && typeof rec.pid === "number") out.push(rec);
    } catch {
      // Being written or corrupt; skip.
    }
  }
  // Newest first.
  out.sort((a, b) => String(b.started_at ?? "").localeCompare(String(a.started_at ?? "")));
  return out;
}

export interface DiscoveryDeps {
  pidAlive?: (pid: number) => boolean;
  checkHealth?: (url: string) => Promise<boolean>;
}

/** A live ephemeral manager of this OS user: pid alive and `/health` answers. */
export async function findLiveManager(home: string, deps: DiscoveryDeps = {}): Promise<RegistryRecord | undefined> {
  const alive = deps.pidAlive ?? pidAlive;
  const healthy = deps.checkHealth ?? checkHealth;
  for (const rec of await readRegistry(home)) {
    if (!alive(rec.pid)) continue;
    if (await healthy(rec.url)) return rec;
  }
  return undefined;
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

/** Spawn `darkpyonix manager` detached and wait until its registry file appears and is healthy. */
export async function spawnManager(opts: SpawnOptions, deps: DiscoveryDeps = {}): Promise<RegistryRecord> {
  const env = { ...process.env };
  if (opts.passHome) env.DARKPYONIX_HOME = opts.home;
  const child = spawn(opts.cliPath, opts.args, { detached: true, stdio: ["ignore", "ignore", "pipe"], env });
  let stderr = "";
  let exited: { code: number | null; error?: Error } | null = null;
  child.stderr?.on("data", (d: Buffer) => {
    stderr += d.toString();
    if (stderr.length > 8192) stderr = stderr.slice(-8192);
  });
  child.on("error", (error) => { exited = { code: null, error }; });
  child.on("exit", (code) => { exited = exited ?? { code }; });
  opts.log?.(`spawned ${opts.cliPath} ${opts.args.join(" ")} (pid ${child.pid})`);

  const deadline = Date.now() + (opts.timeoutMs ?? 15000);
  try {
    while (Date.now() < deadline) {
      const recs = await readRegistry(opts.home);
      const mine = recs.find((r) => r.pid === child.pid) ?? (await findLiveManager(opts.home, deps));
      if (mine && (await (deps.checkHealth ?? checkHealth)(mine.url))) return mine;
      if (exited) {
        const why = (exited as { error?: Error }).error?.message ?? `exit code ${(exited as { code: number | null }).code}`;
        throw new Error(`darkpyonix manager did not start (${why})${stderr ? ": " + stderr.trim() : ""}`);
      }
      await new Promise((r) => setTimeout(r, 150));
    }
    throw new Error(`darkpyonix manager did not write ${join(opts.home, "managers")}/<pid>.json within ${(opts.timeoutMs ?? 15000) / 1000}s`);
  } finally {
    child.stderr?.removeAllListeners("data");
    child.stderr?.destroy();
    child.unref();
  }
}

export interface ConnectSettings {
  remoteUrl?: string;
  remoteToken?: string;
  home: string;
  passHome: boolean;
  cliPath: string;
  managerArgs: string[];
  log?: (line: string) => void;
}

export async function resolveManager(s: ConnectSettings, deps: DiscoveryDeps = {}): Promise<ManagerEndpoint> {
  if (s.remoteUrl) return { url: s.remoteUrl, token: s.remoteToken || undefined, source: "dedicated" };
  const live = await findLiveManager(s.home, deps);
  if (live) return { url: live.url, token: live.token, source: "ephemeral", pid: live.pid };
  const rec = await spawnManager({ cliPath: s.cliPath, args: s.managerArgs, home: s.home, passHome: s.passHome, log: s.log }, deps);
  return { url: rec.url, token: rec.token, source: "spawned", pid: rec.pid };
}

/** Map a local path to the manager machine's path using prefix rules (dedicated managers). */
export function mapPath(local: string, pathMap: Record<string, string>): string {
  let best = "";
  for (const prefix of Object.keys(pathMap)) {
    if (local.startsWith(prefix) && prefix.length > best.length) best = prefix;
  }
  return best ? pathMap[best] + local.slice(best.length) : local;
}
