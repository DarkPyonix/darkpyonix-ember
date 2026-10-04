// Event application on the shared document model (PROTOCOL §4) and run event tracking (§3.4).
import { describe, expect, it } from "vitest";
import { DocModel, who } from "../../src/collab/model";
import type { Document, DocumentCell } from "../../src/manager/types";
import { RunTracker } from "../../src/run/tracker";
import { FakeSink } from "../fake/fakeHost";

const ME = "me-client-0001";
const ALICE = { client_id: "alice-0001", user: "alice", nickname: "desk" };

function cell(id: string, index: number, source = `${id}\n`, version = 1, type = "code"): DocumentCell {
  return { cell_id: id, index, type, source, source_sha256: "", version, metadata: {}, title: null, lock: null };
}

function doc(): Document {
  return {
    path: "/x.pynb", doc_version: 10, seq: 50, presence: [],
    cells: [cell("pre", 0, "", 1, "preamble"), cell("a", 1), cell("b", 2), cell("c", 3)],
  };
}

function noLock(c: DocumentCell): DocumentCell {
  const { lock: _drop, ...rest } = c;
  return rest as DocumentCell;
}

const ev = (type: string, data: any, seq = 1) => ({ seq, type, data });

describe("DocModel", () => {
  it("inserts, updates, moves and deletes", () => {
    const m = new DocModel(ME);
    m.load(doc());
    expect(m.apply(ev("doc.cell.created", { doc_version: 11, cell: cell("n", 2), by: ALICE }))).toEqual([
      { op: "insert", cellId: "n", afterId: "a", by: ALICE },
    ]);
    expect(m.cells.map((c) => c.cell_id)).toEqual(["pre", "a", "n", "b", "c"]);
    expect(m.cells.map((c) => c.index)).toEqual([0, 1, 2, 3, 4]);

    expect(m.apply(ev("doc.cell.updated", { doc_version: 12, cell: cell("b", 3, "B\n", 2), by: ALICE }))).toEqual([
      { op: "update", cellId: "b", by: ALICE },
    ]);
    expect(m.byId("b")!.source).toBe("B\n");

    expect(m.apply(ev("doc.cell.moved", { doc_version: 13, cell: cell("c", 1, "c\n", 2), by: ALICE }))).toEqual([
      { op: "move", cellId: "c", afterId: "pre", by: ALICE },
    ]);
    expect(m.cells.map((c) => c.cell_id)).toEqual(["pre", "c", "a", "n", "b"]);

    expect(m.apply(ev("doc.cell.deleted", { doc_version: 14, cell_id: "n", by: ALICE }))).toEqual([
      { op: "delete", cellId: "n", by: ALICE },
    ]);
    expect(m.cells.map((c) => c.index)).toEqual([0, 1, 2, 3]);
    expect(m.docVersion).toBe(14);
  });

  it("drops events the snapshot already contains", () => {
    const m = new DocModel(ME);
    m.load(doc());
    expect(m.apply(ev("doc.cell.updated", { doc_version: 10, cell: cell("a", 1, "old\n", 1) }))).toEqual([]);
    expect(m.apply(ev("doc.cell.deleted", { doc_version: 9, cell_id: "a" }))).toEqual([]);
    expect(m.byId("a")!.source).toBe("a\n");
  });

  it("an update of a known cell with the same id is not an insert", () => {
    const m = new DocModel(ME);
    m.load(doc());
    m.upsert(cell("z", 4)); // our POST response arrived first
    expect(m.apply(ev("doc.cell.created", { doc_version: 11, cell: cell("z", 4), by: { client_id: ME } }))[0].op).toBe("update");
    expect(m.cells.filter((c) => c.cell_id === "z")).toHaveLength(1);
  });

  it("ignores stale versions from HTTP responses", () => {
    const m = new DocModel(ME);
    m.load(doc());
    m.upsert(cell("a", 1, "v3\n", 3));
    m.upsert(cell("a", 1, "v2\n", 2));
    expect(m.byId("a")!.source).toBe("v3\n");
  });

  it("tracks locks and reports a lost lock", () => {
    const m = new DocModel(ME);
    m.load(doc());
    const lock = { cell_id: "a", locked_by: ALICE.client_id, user: "alice", nickname: "desk", locked_at: "", last_activity: "" };
    m.apply(ev("doc.lock", { doc_version: 10, cell_id: "a", lock, by: ALICE }));
    expect(m.lockedByOther("a")).toEqual(lock);
    expect(who(m.lockedByOther("a"))).toBe("alice@desk");
    m.apply(ev("doc.unlock", { doc_version: 10, cell_id: "a", by: ALICE, reason: "released" }));
    expect(m.lockedByOther("a")).toBeUndefined();

    m.setLock("b", { ...lock, cell_id: "b", locked_by: ME });
    expect(m.lockedByOther("b")).toBeUndefined();
    expect(m.apply(ev("doc.unlock", { doc_version: 10, cell_id: "b", reason: "idle" }))).toContainEqual({ op: "lockLost", cellId: "b", reason: "idle" });
  });

  it("replaces everything on doc.reloaded but keeps known locks", () => {
    const m = new DocModel(ME);
    m.load(doc());
    m.setLock("a", { cell_id: "a", locked_by: ME, user: "me", locked_at: "", last_activity: "" });
    const ops = m.apply(ev("doc.reloaded", { doc_version: 20, cells: [cell("pre", 0, "", 1, "preamble"), noLock(cell("a", 1, "A\n", 2))], cause: "external" }));
    expect(ops[0]).toEqual({ op: "reset" });
    expect(m.cells.map((c) => c.cell_id)).toEqual(["pre", "a"]);
    expect(m.byId("a")!.lock?.locked_by).toBe(ME);
  });

  it("keeps presence and focus", () => {
    const m = new DocModel(ME);
    m.load(doc());
    m.apply(ev("presence.update", { client_id: ALICE.client_id, user: "alice", nickname: "desk", focused_cell_id: "b" }));
    m.apply(ev("presence.update", { client_id: ME, user: "me", nickname: "lap", focused_cell_id: "b" }));
    expect(m.focusedBy("b").map((p) => p.client_id)).toEqual([ALICE.client_id]);
    m.apply(ev("presence.update", { client_id: ALICE.client_id, focused_cell_id: null }));
    expect(m.focusedBy("b")).toEqual([]);
    expect(m.presence.get(ALICE.client_id)?.user).toBe("alice");
    m.apply(ev("presence.leave", { client_id: ALICE.client_id }));
    expect(m.presence.has(ALICE.client_id)).toBe(false);
  });

  it("hides only an empty preamble", () => {
    const m = new DocModel(ME);
    m.load(doc());
    expect(m.visibleCells().map((c) => c.cell_id)).toEqual(["a", "b", "c"]);
    m.apply(ev("doc.cell.updated", { doc_version: 11, cell: cell("pre", 0, "import os\n", 2, "preamble") }));
    expect(m.visibleCells().map((c) => c.cell_id)).toEqual(["pre", "a", "b", "c"]);
  });
});

describe("RunTracker", () => {
  const ids = ["pre", "a", "b", "c"];

  it("creates executions for our run up front and finishes them", () => {
    const sink = new FakeSink();
    const t = new RunTracker(ME, (i) => ids[i], sink);
    t.expect("r1", ["a", "c"], { client_id: ME });
    expect(sink.execs.map((e) => e.cellId)).toEqual(["a", "c"]);
    t.handle(ev("run.started", { run_id: "r1", cells: [1, 3], started_by: { client_id: ME } }));
    t.handle(ev("cell.started", { run_id: "r1", index: 1, execution_count: 7 }));
    t.handle(ev("output", { run_id: "r1", index: 1, output: { output_type: "stream", name: "stdout", text: "x" } }));
    t.handle(ev("output.clear", { run_id: "r1", index: 1, wait: false }));
    t.handle(ev("output", { run_id: "r1", index: 1, output: { output_type: "stream", name: "stdout", text: "y" } }));
    t.handle(ev("cell.finished", { run_id: "r1", index: 1, status: "ok" }));
    t.handle(ev("run.finished", { run_id: "r1", status: "interrupted" }));
    const [a, c] = sink.execs;
    expect(a).toMatchObject({ started: 7, cleared: 1, ended: "ok", done: true });
    expect(a.outputs).toHaveLength(1);
    // "c" never started: cancelled by the interrupt.
    expect(c).toMatchObject({ started: undefined, ended: "cancelled", done: true });
    expect(sink.ranCells).toEqual(["a"]);
    expect(t.isActive("r1")).toBe(false);
  });

  it("does not double-create when events beat the POST response", () => {
    const sink = new FakeSink();
    const t = new RunTracker(ME, (i) => ids[i], sink);
    t.handle(ev("cell.started", { run_id: "r2", index: 2, execution_count: 1, started_by: { client_id: ME } }));
    t.expect("r2", ["b"], { client_id: ME });
    expect(sink.execs).toHaveLength(1);
  });

  it("attributes other clients' runs and keeps the cell of a started index", () => {
    const sink = new FakeSink();
    const order = [...ids];
    const t = new RunTracker(ME, (i) => order[i], sink);
    t.handle(ev("cell.started", { run_id: "r3", index: 3, execution_count: 1, started_by: ALICE }));
    order.splice(1, 0, "inserted"); // a cell was inserted above while running
    t.handle(ev("output", { run_id: "r3", index: 3, output: { output_type: "stream", name: "stdout", text: "z" } }));
    expect(sink.execs.map((e) => e.cellId)).toEqual(["c"]);
    expect(sink.execs[0].outputs).toHaveLength(1);
    expect(sink.attributions[0].info).toMatchObject({ runId: "r3", mine: false, by: ALICE, state: "running" });
  });
});
