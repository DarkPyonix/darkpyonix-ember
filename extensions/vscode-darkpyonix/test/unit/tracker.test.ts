// PROTOCOL §3.4 (manager OpenAPI draft.4): run events carry the shared document's cell_id beside
// the file index; the cell is found by cell_id even when cells moved during the run.
import { describe, expect, it } from "vitest";
import { RunTracker, type ExecHandle, type ExecSink } from "../../src/run/tracker";

function sink() {
  const log: string[] = [];
  const s: ExecSink = {
    create: (id) => ({
      start: () => log.push(`start ${id}`),
      output: (o) => log.push(`out ${id} ${String(o.text)}`),
      clear: () => log.push(`clear ${id}`),
      end: (st) => log.push(`end ${id} ${st}`),
    }) as ExecHandle,
    attribution: () => {},
  };
  return { s, log };
}

describe("run events by cell_id (PROTOCOL §3.4)", () => {
  it("test_protocol_3_4_output_goes_to_the_event_cell_id_not_the_index", () => {
    const { s, log } = sink();
    // The index now points at another cell (a cell was inserted above during the run).
    const t = new RunTracker("me", (i) => ["c_pre", "c_new", "c_ran"][i], s);
    const by = { client_id: "other" };
    t.handle({ seq: 1, type: "run.started", data: { run_id: "r", cells: [1], cell_ids: ["c_ran"], started_by: by } });
    t.handle({ seq: 2, type: "cell.started", data: { run_id: "r", index: 1, cell_id: "c_ran", execution_count: 1 } });
    t.handle({ seq: 3, type: "output", data: { run_id: "r", index: 1, cell_id: "c_ran", output: { output_type: "stream", text: "hi" } } });
    t.handle({ seq: 4, type: "output.clear", data: { run_id: "r", index: 1, cell_id: "c_ran", wait: false } });
    t.handle({ seq: 5, type: "cell.finished", data: { run_id: "r", index: 1, cell_id: "c_ran", status: "ok" } });
    expect(log).toEqual(["start c_ran", "out c_ran hi", "clear c_ran", "end c_ran ok"]);
  });

  it("test_protocol_3_4_null_cell_id_falls_back_to_the_index", () => {
    const { s, log } = sink();
    const t = new RunTracker("me", (i) => ["c_pre", "c_one"][i], s);
    t.handle({ seq: 1, type: "cell.started", data: { run_id: "r", index: 1, cell_id: null, execution_count: 1 } });
    t.handle({ seq: 2, type: "output", data: { run_id: "r", index: 1, cell_id: null, output: { output_type: "stream", text: "x" } } });
    expect(log).toEqual(["start c_one", "out c_one x"]);
  });
});
