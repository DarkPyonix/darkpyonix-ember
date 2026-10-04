// FR-C1: find a live manager in <home>/managers/*.json, else spawn one and wait for its registry.
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { findLiveManager, pidAlive, resolveManager } from "../../src/manager/discovery";
import { FakeManager } from "../fake/fakeManager";

const SCRATCH = join(__dirname, "..", "..", "..", "..", ".scratch");
let home: string;
let fm: FakeManager;
const spawned: number[] = [];

beforeEach(async () => {
  mkdirSync(SCRATCH, { recursive: true });
  home = mkdtempSync(join(SCRATCH, "dphome-"));
  fm = new FakeManager("# %% [code]\nx\n");
  await fm.start();
});

afterEach(async () => {
  for (const pid of spawned.splice(0)) {
    try { process.kill(pid, "SIGTERM"); } catch { /* gone */ }
  }
  await fm.stop();
  rmSync(home, { recursive: true, force: true });
});

function register(name: string, rec: object): void {
  mkdirSync(join(home, "managers"), { recursive: true });
  writeFileSync(join(home, "managers", name), JSON.stringify(rec));
}

describe("manager discovery (FR-C1)", () => {
  it("picks a live, healthy manager and skips dead or unhealthy ones", async () => {
    register("999999.json", { pid: 999999, url: fm.url, token: "dead", mode: "ephemeral", started_at: "2026-10-03T09:00:00Z" });
    register("1.json", { pid: process.pid, url: "http://127.0.0.1:1", token: "nohealth", mode: "ephemeral", started_at: "2026-10-03T08:00:00Z" });
    register(`${process.pid}.json`, { pid: process.pid, url: fm.url, token: fm.token, mode: "ephemeral", started_at: "2026-10-03T07:00:00Z" });
    register("junk.json", { nope: true });
    expect(pidAlive(999999)).toBe(false);
    const rec = await findLiveManager(home);
    expect(rec).toMatchObject({ url: fm.url, token: fm.token });
  });

  it("returns undefined when the registry is empty", async () => {
    expect(await findLiveManager(home)).toBeUndefined();
  });

  it("uses a configured dedicated manager as is", async () => {
    const ep = await resolveManager({ remoteUrl: "https://main.example", remoteToken: "t", home, passHome: true, cliPath: "nope", managerArgs: [] });
    expect(ep).toEqual({ url: "https://main.example", token: "t", source: "dedicated" });
  });

  it("spawns `darkpyonix manager` and waits for its registry file", async () => {
    // A stand-in CLI: serves /health and writes <home>/managers/<pid>.json like the Rust manager.
    const cli = join(home, "fake-darkpyonix.mjs");
    writeFileSync(cli, `
      import { createServer } from "node:http";
      import { mkdirSync, writeFileSync } from "node:fs";
      import { join } from "node:path";
      if (process.argv[2] !== "manager" || process.argv[3] !== "--ephemeral") process.exit(2);
      const s = createServer((q, r) => { r.writeHead(200, {"Content-Type": "application/json"}); r.end('{"status":"ok","version":"x"}'); });
      s.listen(0, "127.0.0.1", () => {
        const home = process.env.DARKPYONIX_HOME;
        mkdirSync(join(home, "managers"), { recursive: true });
        setTimeout(() => writeFileSync(join(home, "managers", process.pid + ".json"), JSON.stringify({
          pid: process.pid, url: "http://127.0.0.1:" + s.address().port, token: "spawned-token", mode: "ephemeral",
          started_at: new Date().toISOString(), version: "x" })), 200);
      });
      setTimeout(() => process.exit(0), 20000);
    `);
    const ep = await resolveManager({
      home, passHome: true, cliPath: process.execPath, managerArgs: [cli, "manager", "--ephemeral"],
    });
    if (ep.pid) spawned.push(ep.pid);
    expect(ep.source).toBe("spawned");
    expect(ep.token).toBe("spawned-token");
    expect(pidAlive(ep.pid!)).toBe(true);
  });

  it("reports why a spawn failed", async () => {
    await expect(resolveManager({
      home, passHome: true, cliPath: process.execPath, managerArgs: ["-e", "console.error('boom'); process.exit(3)"],
    })).rejects.toThrow(/exit code 3.*boom/);
  });
});
