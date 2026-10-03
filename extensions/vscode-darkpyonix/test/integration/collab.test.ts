// Module-level integration: the real client, SSE reader, collaboration session, run tracker and
// connection against the fake manager over HTTP. Only the editor is simulated (FakeHost).
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { NotebookConnection } from "../../src/connection";
import { ManagerClient } from "../../src/manager/client";
import { FakeManager, type Client } from "../fake/fakeManager";
import { FakeHost, FakeSink, until } from "../fake/fakeHost";

const TEXT =
  '"""doc"""\nimport darkpyonix\n\n\n' +
  "# %% [code]\nx = 1\n\n\n" +
  '# %% [markdown]\ndarkpyonix.markdown("""\n# Title\n""", silent=True)\n\n\n' +
  "# %% [argparse]\nN = darkpyonix.params.get(\"n\", default=1)\n\n\n" +
  "# %% [code]\nprint(x)\n";

const ME = { clientId: "vsc-test-me-0001", nickname: "laptop" };
const ALICE: Client = { client_id: "alice-device-01", nickname: "desktop", user: "alice" };

let fm: FakeManager;
let host: FakeHost;
let sink: FakeSink;
let conn: NotebookConnection;

async function attach(opts: Record<string, number> = {}): Promise<void> {
  const client = new ManagerClient(fm.url, fm.token, ME);
  conn = new NotebookConnection(client, fm.path, host, sink, { editDebounceMs: 20, lockIdleMs: 300, presenceThrottleMs: 20, structureSettleMs: 100, ...opts });
  await conn.open(true);
  await until(() => fm.presence.has(ME.clientId), 3000, "SSE presence join");
}

function mine(method: string, pathPart: string) {
  return fm.requests.filter((r) => r.client === ME.clientId && r.method === method && r.path.includes(pathPart));
}

beforeEach(async () => {
  fm = new FakeManager(TEXT);
  await fm.start();
  host = new FakeHost(TEXT);
  sink = new FakeSink();
});

afterEach(async () => {
  await conn?.dispose();
  await fm.stop();
});

describe("collaboration (FR-S1..S5)", () => {
  it("binds the editor to the snapshot without rewriting it", async () => {
    await attach();
    expect(host.resets).toBe(0);
    expect(host.cells.map((c) => c.cellId)).toEqual(fm.cells.map((c) => c.cell_id));
    expect(host.kernelStates).toContain("idle");
    expect(fm.requests.some((r) => r.path.startsWith("/api/v1/kernels") && r.method === "POST")).toBe(true);
  });

  it("applies another client's edit without echoing it back", async () => {
    await attach();
    fm.editAs(ALICE, fm.cells[1].cell_id, "x = 42\n\n\n");
    await until(() => host.cells[1].view.value === "x = 42", 3000, "remote edit");
    conn.session!.editorChanged();
    await new Promise((r) => setTimeout(r, 100));
    expect(mine("PATCH", "/cells/")).toHaveLength(0);
    expect(mine("PUT", "/lock")).toHaveLength(0);
  });

  it("locks on typing, sends edits with base_version, releases with the final source", async () => {
    await attach();
    const id = fm.cells[1].cell_id;
    host.type(1, "x = 2");
    conn.session!.editorChanged();
    await until(() => mine("PATCH", `/cells/${id}`).length === 1, 3000, "PATCH");
    expect(mine("PUT", `/cells/${id}/lock`)).toHaveLength(1);
    expect(mine("PATCH", `/cells/${id}`)[0].body).toEqual({ source: "x = 2\n\n\n", base_version: 1 });
    expect(fm.byId(id)!.source).toBe("x = 2\n\n\n");
    expect(fm.byId(id)!.lock?.locked_by).toBe(ME.clientId);

    host.type(1, "x = 3");
    conn.session!.editorChanged();
    // Moving to another cell releases the lock with the final source (2025 cell_unlocked_with_code).
    await new Promise((r) => setTimeout(r, 5));
    conn.session!.focusChanged(fm.cells[4].cell_id);
    await until(() => mine("DELETE", `/cells/${id}/lock`).length === 1, 3000, "unlock");
    await until(() => fm.byId(id)!.lock === null, 3000, "unlocked");
    expect(fm.byId(id)!.source).toBe("x = 3\n\n\n");
    // Focus is reported as presence (FR-S4).
    await until(() => fm.presence.get(ME.clientId)?.focused_cell_id === fm.cells[4].cell_id, 3000, "presence");
  });

  it("releases an idle lock", async () => {
    await attach({ lockIdleMs: 150 });
    const id = fm.cells[4].cell_id;
    host.type(4, "print(x + 1)");
    conn.session!.editorChanged();
    await until(() => fm.byId(id)!.lock?.locked_by === ME.clientId, 3000, "lock");
    await until(() => fm.byId(id)!.lock === null, 3000, "idle release");
    expect(fm.byId(id)!.source).toBe("print(x + 1)\n");
  });

  it("undoes typing in a cell someone else is editing and says who", async () => {
    await attach();
    const id = fm.cells[1].cell_id;
    fm.lockAs(ALICE, id);
    await until(() => conn.session!.model.byId(id)?.lock?.locked_by === ALICE.client_id, 3000, "lock event");
    host.type(1, "x = 999");
    conn.session!.editorChanged();
    await until(() => host.cells[1].view.value === "x = 1", 3000, "revert");
    expect(host.notes.some((n) => n.message.includes("alice@desktop"))).toBe(true);
    expect(mine("PATCH", "/cells/")).toHaveLength(0);
  });

  it("handles 409 locked from the server (lock race) the same way", async () => {
    await attach();
    const id = fm.cells[1].cell_id;
    // Locked on the server, but this client has not seen the event yet.
    fm.byId(id)!.lock = { cell_id: id, locked_by: ALICE.client_id, user: "alice", nickname: "desktop", locked_at: "", last_activity: "" };
    host.type(1, "x = 5");
    conn.session!.editorChanged();
    await until(() => host.notes.some((n) => n.message.includes("alice@desktop")), 3000, "locked notice");
    expect(host.cells[1].view.value).toBe("x = 1");
  });

  it("offers Keep mine / Use theirs on 409 conflict", async () => {
    await attach();
    const id = fm.cells[4].cell_id;
    host.answer = "Use theirs";
    // Server version moves on without an event reaching us (simulated by bumping directly).
    const c = fm.byId(id)!;
    c.source = "print('server')\n";
    c.version = 5;
    host.type(4, "print('mine')");
    conn.session!.editorChanged();
    await until(() => host.notes.some((n) => n.actions.includes("Keep mine")), 3000, "conflict prompt");
    await until(() => host.cells[4].view.value === "print('server')", 3000, "theirs applied");

    host.answer = "Keep mine";
    c.version = 9;
    host.type(4, "print('mine again')");
    conn.session!.editorChanged();
    await until(() => fm.byId(id)!.source === "print('mine again')\n", 3000, "mine kept");
  });

  it("follows remote create, delete and move", async () => {
    await attach();
    const created = fm.createAs(ALICE, fm.cells[1].cell_id, "code", "y = 2\n\n\n");
    await until(() => host.cells.length === 6, 3000, "insert");
    expect(host.cells[2].cellId).toBe(created.cell_id);
    expect(host.cells[2].view.value).toBe("y = 2");

    fm.moveAs(ALICE, created.cell_id, 4);
    await until(() => host.cells[4]?.cellId === created.cell_id, 3000, "move");

    fm.deleteAs(ALICE, created.cell_id);
    await until(() => host.cells.length === 5, 3000, "delete");
    expect(host.cells.map((c) => c.cellId)).toEqual(fm.cells.map((c) => c.cell_id));
    expect(mine("POST", "/cells")).toHaveLength(0);
  });

  it("sends local create, delete and move", async () => {
    await attach();
    host.cells.splice(2, 0, { handle: {}, view: { kind: "code", value: "z = 3", language: "python" } });
    conn.session!.editorChanged();
    await until(() => fm.cells.length === 6, 3000, "create");
    expect(mine("POST", "/cells")[0].body).toMatchObject({ type: "code", source: "z = 3\n\n\n", after: fm.cells[1].cell_id });
    await until(() => !!host.cells[2].cellId, 3000, "bind");
    expect(host.cells[2].cellId).toBe(fm.cells[2].cell_id);
    expect(host.cells).toHaveLength(6); // not inserted twice by the echo

    const moving = host.cells[2];
    host.cells.splice(2, 1);
    host.cells.splice(4, 0, moving);
    conn.session!.editorChanged();
    await until(() => fm.cells[4].cell_id === moving.cellId, 3000, "move");

    const gone = host.cells[4].cellId!;
    host.cells.splice(4, 1);
    conn.session!.editorChanged();
    await until(() => !fm.byId(gone), 3000, "delete");
    expect(host.cells.map((c) => c.cellId)).toEqual(fm.cells.map((c) => c.cell_id));
  });

  it("adopts cells VS Code reloaded from disk instead of re-creating them", async () => {
    await attach({ structureSettleMs: 300 });
    const ids = host.cells.map((c) => c.cellId);
    // A reload replaces every editor cell with a new, unbound one with the same text.
    host.cells = host.cells.map((c) => ({ handle: {}, view: c.view }));
    conn.session!.editorChanged();
    await until(() => host.cells.every((c) => c.cellId), 3000, "adopted");
    expect(host.cells.map((c) => c.cellId)).toEqual(ids);
    // The external edit itself arrives a moment later; nothing was deleted or created meanwhile.
    host.cells[1] = { handle: {}, view: { ...host.cells[1].view, value: "x = 8" } };
    conn.session!.editorChanged();
    fm.reloadFromText(TEXT.replace("x = 1", "x = 8"));
    await until(() => host.cells[1].cellId === fm.cells[1].cell_id, 3000, "adopted after reload");
    await new Promise((r) => setTimeout(r, 500));
    expect(mine("POST", "/cells")).toHaveLength(0);
    expect(mine("DELETE", "/cells/")).toHaveLength(0);
  });

  it("applies doc.reloaded after an external edit", async () => {
    await attach();
    fm.reloadFromText(TEXT.replace("x = 1", "x = 7"));
    await until(() => host.cells[1].view.value === "x = 7", 3000, "reload");
    expect(host.cells.map((c) => c.cellId)).toEqual(fm.cells.map((c) => c.cell_id));
  });

  it("shows doc.conflict and resolves to the disk version on request", async () => {
    await attach();
    const id = fm.cells[1].cell_id;
    host.answer = "Use disk version";
    fm.emit("doc.conflict", { cell_id: id, local: { source: "x = 1\n\n\n", version: 1, by: ME }, disk: { source: "x = 100\n\n\n" } });
    await until(() => host.cells[1].view.value === "x = 100", 3000, "disk version");
    await until(() => fm.byId(id)!.source === "x = 100\n\n\n", 3000, "saved");
  });

  it("tracks other clients' presence", async () => {
    await attach();
    fm.emit("presence.update", { client_id: ALICE.client_id, nickname: "desktop", user: "alice", focused_cell_id: fm.cells[4].cell_id });
    await until(() => conn.session!.model.focusedBy(fm.cells[4].cell_id).length === 1, 3000, "presence");
    fm.emit("presence.leave", { client_id: ALICE.client_id });
    await until(() => conn.session!.model.focusedBy(fm.cells[4].cell_id).length === 0, 3000, "leave");
  });
});

describe("runs and outputs (FR-X, FR-R4, FR-S6)", () => {
  it("runs cells by cell_id and streams outputs into executions", async () => {
    await attach();
    fm.runOutput = (c) => [
      { output_type: "stream", name: "stdout", text: "hello\n" },
      { output_type: "execute_result", data: { "text/plain": "42" }, metadata: {}, execution_count: 1 },
      ...(c.index === 4 ? [{ output_type: "error" as const, ename: "E", evalue: "v", traceback: ["tb"] }] : []),
    ];
    const runId = await conn.run([1, 4]);
    expect(mine("POST", "/runs")[0].body).toEqual({ mode: "cells", cell_ids: [fm.cells[1].cell_id, fm.cells[4].cell_id], on_busy: "queue" });
    await until(() => sink.execs.length === 2 && sink.execs.every((e) => e.done), 3000, "run done");
    expect(sink.execs.map((e) => e.cellId)).toEqual([fm.cells[1].cell_id, fm.cells[4].cell_id]);
    expect(sink.execs[0].started).toBe(1);
    expect(sink.execs[0].outputs.map((o) => o.output_type)).toEqual(["stream", "execute_result"]);
    expect(sink.execs[1].outputs.map((o) => o.output_type)).toEqual(["stream", "execute_result", "error"]);
    expect(sink.execs.every((e) => e.ended === "ok")).toBe(true);
    const running = sink.attributions.find((a) => a.info?.state === "running");
    expect(running?.info).toMatchObject({ runId, mine: true });
    expect(sink.attributions.at(-1)?.info).toBeNull();
  });

  it("flushes typed text before running", async () => {
    await attach();
    host.type(1, "x = 10");
    conn.session!.editorChanged();
    await conn.run([1]);
    expect(fm.byId(fm.cells[1].cell_id)!.source).toBe("x = 10\n\n\n");
  });

  it("shows other clients' runs with attribution", async () => {
    await attach();
    fm.runCells("20261003-000000-beef", [fm.cells[4].cell_id], ALICE);
    await until(() => sink.execs.length === 1 && sink.execs[0].done, 3000, "foreign run");
    const a = sink.attributions.find((x) => x.info?.state === "running")!;
    expect(a.info).toMatchObject({ mine: false, by: { user: "alice", nickname: "desktop" } });
  });

  it("interrupts, never kills", async () => {
    await attach();
    fm.holdRuns = true;
    await conn.run([1]);
    await new Promise((r) => setTimeout(r, 50)); // the fake queues the held run after 10 ms
    expect(await conn.interrupt()).toBe(true);
    await conn.shutdown();
    expect(fm.requests.some((r) => r.path.includes("force=true"))).toBe(false);
    expect(fm.requests.some((r) => r.method === "DELETE" && r.path === `/api/v1/kernels/${fm.kernelId}`)).toBe(true);
  });

  it("shows the latest outputs and stale marks from the snapshot", async () => {
    fm.snapshotOutputs.set(1, { outputs: [{ output_type: "stream", name: "stdout", text: "old\n" }], execution_count: 3, stale: true });
    fm.snapshotOutputs.set(4, { outputs: [{ output_type: "stream", name: "stdout", text: "1\n" }], execution_count: 4 });
    await attach();
    expect(host.shown.get(1)?.stale).toBe(true);
    expect(host.shown.get(4)?.stale).toBe(false);
    expect(host.shown.get(4)?.outputs?.[0]).toMatchObject({ text: "1\n" });
  });

  it("shows stored outputs without a kernel (GET /documents)", async () => {
    fm.snapshotOutputs.set(4, { outputs: [{ output_type: "stream", name: "stdout", text: "1\n" }], execution_count: 4 });
    const client = new ManagerClient(fm.url, fm.token, ME);
    conn = new NotebookConnection(client, fm.path, host, sink, {});
    host.type(1, "x = 'unsaved'");
    await conn.open(false);
    expect(fm.kernelStarted).toBe(false);
    expect(host.shown.get(4)?.outputs?.[0]).toMatchObject({ text: "1\n" });
    expect(host.kernelStates).toEqual(["no kernel"]);
  });
});
