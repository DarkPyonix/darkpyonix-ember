// The extension's editor-agnostic modules against the REAL DarkPyonix manager and kernel
// (darkpyonix-core FR-C1, FR-M2, FR-X4, FR-R4, FR-S6, PROTOCOL §3.4). Runs only when
// DARKPYONIX_CLI names a built `darkpyonix` binary (CI builds it from a pinned core commit).
// Every test runs in a temporary DARKPYONIX_HOME under .scratch/ and every process it starts
// (the spawned manager, the kernel) is ended in afterAll.
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { NotebookConnection } from "../../src/connection";
import { ManagerClient } from "../../src/manager/client";
import { pidAlive, resolveManager, type ManagerEndpoint } from "../../src/manager/discovery";
import { FakeHost, FakeSink, until } from "../fake/fakeHost";

const CLI = process.env.DARKPYONIX_CLI;
const SCRATCH = join(__dirname, "..", "..", "..", "..", ".scratch");
const ME = { clientId: "vsc-real-test-0001", nickname: "ci" };

const TEXT =
  '"""real manager test"""\n\n\n' +
  "# %% [code]\nx = 1\nprint(\"hello\", x)\n\n\n" +
  "# %% [code]\nimport time\nfor i in range(3):\n    print(\"tick\", i, flush=True)\n    time.sleep(0.5)\n\n\n" +
  "# %% [code]\nimport time\ntime.sleep(60)\n\n\n" +
  "# %% [code]\nprint(\"alive\", x)\n";

let dir: string;
let file: string;
let ep: ManagerEndpoint;
let client: ManagerClient;
const conns: NotebookConnection[] = [];
const kernelPids = new Set<number>();

async function connect(autoStart: boolean, text = readFileSync(file, "utf8"), via = client) {
  const host = new FakeHost(text);
  const sink = new FakeSink();
  const conn = new NotebookConnection(via, file, host, sink, { editDebounceMs: 50, structureSettleMs: 200 });
  conns.push(conn);
  await conn.open(autoStart);
  if (conn.kernel) kernelPids.add(conn.kernel.pid);
  return { host, sink, conn };
}

const outText = (o: { text?: string | string[] }) => (Array.isArray(o.text) ? o.text.join("") : o.text ?? "");

async function waitGone(pid: number, ms: number): Promise<boolean> {
  const end = Date.now() + ms;
  while (pidAlive(pid) && Date.now() < end) await new Promise((r) => setTimeout(r, 100));
  return !pidAlive(pid);
}

describe.skipIf(!CLI)("real manager (FR-C1, FR-X4, FR-R4, PROTOCOL §3.4)", () => {
  beforeAll(async () => {
    mkdirSync(SCRATCH, { recursive: true });
    dir = realpathSync(mkdtempSync(join(SCRATCH, "real-")));
    file = join(dir, "nb.pynb");
    writeFileSync(file, TEXT);
    ep = await resolveManager({
      home: join(dir, "home"), passHome: true, cliPath: CLI!, managerArgs: ["manager", "--ephemeral"], env: {},
    });
    client = new ManagerClient(ep.url, ep.token, ME);
  });

  afterAll(async () => {
    for (const c of conns.splice(0)) await c.dispose().catch(() => {});
    if (client) {
      for (const k of await client.listKernels().catch(() => [])) {
        kernelPids.add(k.pid);
        await client.shutdownKernel(k.kernel_id).catch(() => {});
      }
    }
    for (const pid of kernelPids) {
      if (!(await waitGone(pid, 10000))) {
        try { process.kill(pid, "SIGKILL"); } catch { /* gone */ }
      }
    }
    if (ep?.pid) {
      try { process.kill(ep.pid, "SIGTERM"); } catch { /* gone */ }
      if (!(await waitGone(ep.pid, 5000))) {
        try { process.kill(ep.pid, "SIGKILL"); } catch { /* gone */ }
      }
    }
    if (dir) rmSync(dir, { recursive: true, force: true });
  });

  it("test_fr_c1_spawned_real_manager_is_found_from_its_stdout_line", async () => {
    expect(ep.source).toBe("spawned");
    expect(pidAlive(ep.pid!)).toBe(true);
    expect((await client.health()).status).toBe("ok");
    const info = await client.manager();
    expect(info.mode).toBe("ephemeral");
    expect([...info.capabilities].sort()).toEqual(["edit", "execute", "history", "manage", "read"]);
  });

  it("test_fr_m2_opening_the_pynb_starts_the_kernel_and_binds_its_cells", async () => {
    const { host, conn } = await connect(true);
    expect(conn.kernel?.path).toBe(file);
    expect(host.cells.length).toBe(5);
    expect(host.cells.every((c) => !!c.cellId)).toBe(true);
  });

  it("test_protocol_3_4_run_streams_live_outputs_over_sse", async () => {
    const conn = conns[0];
    const sink = conn.sink as FakeSink;
    await conn.run([1]);
    await until(() => sink.execs.some((e) => e.done && outText(e.outputs[0] ?? {}).includes("hello 1")), 30000, "hello output");
    // A cell that prints over time: the first output arrives while the cell is still running.
    await conn.run([2]);
    const tick = () => sink.execs.find((e) => e.cellId === conn.host.getCells()[2].cellId);
    await until(() => (tick()?.outputs.length ?? 0) > 0, 30000, "first tick");
    expect(tick()!.done).toBe(false);
    await until(() => !!tick()?.done, 30000, "tick cell done");
    expect(tick()!.ended).toBe("ok");
    expect(tick()!.outputs.map(outText).join("")).toBe("tick 0\ntick 1\ntick 2\n");
  });

  it("test_fr_x4_interrupt_stops_a_slow_cell_and_the_kernel_survives", async () => {
    const conn = conns[0];
    const sink = conn.sink as FakeSink;
    const pid = conn.kernel!.pid;
    const slowId = conn.host.getCells()[3].cellId!;
    await conn.run([3]);
    await until(() => sink.attributions.some((a) => a.cellId === slowId && a.info?.state === "running"), 30000, "slow cell running");
    await new Promise((r) => setTimeout(r, 300));
    expect(await conn.interrupt()).toBe(true);
    const slow = () => sink.execs.filter((e) => e.cellId === slowId).at(-1);
    await until(() => !!slow()?.done, 15000, "slow cell interrupted");
    expect(slow()!.ended).toBe("interrupted");
    // Same process, namespace kept.
    expect(pidAlive(pid)).toBe(true);
    const aliveId = conn.host.getCells()[4].cellId!;
    await conn.run([4]);
    await until(() => sink.execs.some((e) => e.cellId === aliveId && e.done), 30000, "alive cell");
    expect(outText(sink.execs.find((e) => e.cellId === aliveId)!.outputs[0])).toBe("alive 1\n");
    expect((await client.getKernel(conn.kernel!.kernel_id)).pid).toBe(pid);
  });

  it("test_fr_r4_reopening_shows_the_latest_outputs_per_cell", async () => {
    // The latest run covers cells 1 and 4 (FR-R4 maps the latest run onto the file).
    const conn = conns[0];
    const sink = conn.sink as FakeSink;
    const ids = [1, 4].map((p) => conn.host.getCells()[p].cellId!);
    const before = sink.execs.length;
    await conn.run([1, 4]);
    await until(() => ids.every((id) => sink.execs.slice(before).some((e) => e.cellId === id && e.done)), 30000, "run of 1 and 4");
    // Another client (another device) opens the same file.
    const other = new ManagerClient(ep.url, ep.token, { clientId: "vsc-real-test-0002", nickname: "ci-2" });
    const { host } = await connect(false, readFileSync(file, "utf8"), other);
    await until(() => host.shown.has(1) && host.shown.has(4), 10000, "snapshot outputs");
    expect(outText(host.shown.get(1)!.outputs![0])).toBe("hello 1\n");
    expect(host.shown.get(1)!.stale).toBeFalsy();
    expect(outText(host.shown.get(4)!.outputs![0])).toBe("alive 1\n");
  });

  it("test_fr_r4_an_edited_cell_is_marked_stale", async () => {
    const conn = conns[0];
    const kid = conn.kernel!.kernel_id;
    const cellId = conn.host.getCells()[1].cellId!;
    (conn.host as FakeHost).type(1, "x = 2\nprint(\"hello\", x)");
    conn.session!.editorChanged();
    await conn.session!.flushNow();
    await until(() => readFileSync(file, "utf8").includes("x = 2"), 10000, "edit saved by the kernel");
    let doc = await client.getKernelDocument(kid);
    for (let i = 0; i < 50 && !doc.cells.find((c) => c.cell_id === cellId)?.stale; i++) {
      await new Promise((r) => setTimeout(r, 100));
      doc = await client.getKernelDocument(kid);
    }
    const edited = doc.cells.find((c) => c.cell_id === cellId)!;
    expect(edited.stale).toBe(true);
    expect(outText(edited.outputs![0])).toBe("hello 1\n");
    expect(doc.cells.find((c) => c.cell_id === conn.host.getCells()[4].cellId)!.stale).toBeFalsy();
  });
});
