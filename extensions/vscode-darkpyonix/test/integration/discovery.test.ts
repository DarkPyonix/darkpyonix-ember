// FR-C1 (darkpyonix-core SPEC): a given manager (settings, else DARKPYONIX_MANAGER_URL +
// DARKPYONIX_TOKEN), else spawn `darkpyonix manager --ephemeral` and read its first stdout line.
import { existsSync, mkdtempSync, mkdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { givenManager, pidAlive, resolveManager } from "../../src/manager/discovery";

const SCRATCH = join(__dirname, "..", "..", "..", "..", ".scratch");
const CLI = join(__dirname, "..", "fake", "fakeManagerCli.mjs");
let home: string;
const spawned: number[] = [];

beforeEach(() => {
  mkdirSync(SCRATCH, { recursive: true });
  home = mkdtempSync(join(SCRATCH, "dphome-"));
});

afterEach(() => {
  for (const pid of spawned.splice(0)) {
    try { process.kill(pid, "SIGTERM"); } catch { /* gone */ }
  }
  rmSync(home, { recursive: true, force: true });
});

const base = () => ({ home, passHome: true, cliPath: process.execPath, managerArgs: [CLI, "manager", "--ephemeral"], env: {} });

describe("manager discovery (FR-C1)", () => {
  it("test_fr_c1_uses_the_manager_from_the_settings", async () => {
    const ep = await resolveManager({ ...base(), url: "https://main.example", token: "t", cliPath: "nope" });
    expect(ep).toEqual({ url: "https://main.example", token: "t", source: "settings" });
  });

  it("test_fr_c1_uses_the_manager_from_the_environment_without_spawning", async () => {
    const ep = await resolveManager({ ...base(), cliPath: "nope", env: { DARKPYONIX_MANAGER_URL: "http://127.0.0.1:9", DARKPYONIX_TOKEN: "envtok" } });
    expect(ep).toEqual({ url: "http://127.0.0.1:9", token: "envtok", source: "env" });
  });

  it("test_fr_c1_settings_take_precedence_over_the_environment", () => {
    const ep = givenManager({ url: "https://s", token: "st", env: { DARKPYONIX_MANAGER_URL: "http://e", DARKPYONIX_TOKEN: "et" } });
    expect(ep?.source).toBe("settings");
  });

  it("test_fr_c1_only_one_of_url_and_token_is_an_error", () => {
    expect(() => givenManager({ env: { DARKPYONIX_MANAGER_URL: "http://e" } })).toThrow(/DARKPYONIX_MANAGER_URL and DARKPYONIX_TOKEN/);
    expect(() => givenManager({ env: { DARKPYONIX_TOKEN: "t" } })).toThrow(/set both/);
    expect(() => givenManager({ url: "https://s", env: {} })).toThrow(/set both/);
    expect(givenManager({ env: {} })).toBeUndefined();
  });

  it("test_fr_c1_spawns_its_own_manager_and_reads_stdout", async () => {
    const ep = await resolveManager(base());
    if (ep.pid) spawned.push(ep.pid);
    expect(ep.source).toBe("spawned");
    expect(ep.token).toBe("spawned-token");
    expect(pidAlive(ep.pid!)).toBe(true);
    const health = (await (await fetch(ep.url + "/health")).json()) as Record<string, unknown>;
    expect(health.home).toBe(home);
    // No registry file; stderr goes to manager.log (0600).
    expect(existsSync(join(home, "managers"))).toBe(false);
    if (process.platform !== "win32") expect(statSync(join(home, "manager.log")).mode & 0o777).toBe(0o600);
  });

  it("test_fr_c1_spawned_manager_does_not_inherit_a_partial_url", async () => {
    process.env.DARKPYONIX_MANAGER_URL = "http://leak";
    try {
      const ep = await resolveManager(base());
      if (ep.pid) spawned.push(ep.pid);
      const health = (await (await fetch(ep.url + "/health")).json()) as Record<string, unknown>;
      expect(health.inherited_url).toBeNull();
    } finally {
      delete process.env.DARKPYONIX_MANAGER_URL;
    }
  });

  it("test_fr_c1_spawn_failure_is_reported_with_the_log_tail", async () => {
    await expect(resolveManager({ ...base(), managerArgs: [CLI, "--fail"] })).rejects.toThrow(/exit code 3[\s\S]*boom/);
  });

  it("test_fr_c1_no_address_within_the_timeout_is_an_error", async () => {
    await expect(resolveManager({ ...base(), managerArgs: ["-e", "setTimeout(() => {}, 5000)"], spawnTimeoutMs: 500 })).rejects.toThrow(/no address on stdout/);
  });
});
