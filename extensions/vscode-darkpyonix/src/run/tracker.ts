// Turns run events (PROTOCOL §3.4: run.*, cell.*, output, output.clear) into per-cell executions,
// for this client's runs and for runs started by anyone else on the same kernel (FR-S6).
import type { Attribution, KernelEvent, NbOutput } from "../manager/types";

export interface ExecHandle {
  start(executionCount: number | null): void;
  output(o: NbOutput): void;
  clear(wait: boolean): void;
  end(status: string | undefined): void;
}

export interface RunInfo {
  runId: string;
  by?: Attribution | null;
  mine: boolean;
  state: "queued" | "running";
}

export interface ExecSink {
  /** A new execution for the cell, or undefined if the editor cannot show one. */
  create(cellId: string): ExecHandle | undefined;
  /** Who runs the cell (null: not running any more). */
  attribution(cellId: string, info: RunInfo | null): void;
  /** The cell's outputs are current again (it ran). */
  ran?(cellId: string): void;
}

interface RunState {
  by?: Attribution | null;
  mine: boolean;
  /** cellId → execution, created up front for our runs or on cell.started. */
  execs: Map<string, ExecHandle>;
  started: Set<string>;
  /** index → cellId as of cell.started (indexes may shift while a run is going). */
  indexes: Map<number, string>;
}

export class RunTracker {
  private runs = new Map<string, RunState>();

  constructor(
    private readonly me: string,
    /** Document index → cell id (preamble is 0). */
    private readonly resolve: (index: number) => string | undefined,
    private readonly sink: ExecSink,
  ) {}

  private run(runId: string, by?: Attribution | null): RunState {
    let r = this.runs.get(runId);
    if (!r) {
      r = { by, mine: !!by && by.client_id === this.me, execs: new Map(), started: new Set(), indexes: new Map() };
      this.runs.set(runId, r);
    } else if (by && !r.by) {
      r.by = by;
      r.mine = by.client_id === this.me;
    }
    return r;
  }

  /** Our own run was accepted: show the cells as pending right away. */
  expect(runId: string, cellIds: string[], by: Attribution): void {
    const r = this.run(runId, by);
    r.mine = true;
    for (const id of cellIds) {
      if (r.execs.has(id)) continue;
      const h = this.sink.create(id);
      if (h) r.execs.set(id, h);
      this.sink.attribution(id, { runId, by, mine: true, state: "queued" });
    }
  }

  isActive(runId: string): boolean {
    return this.runs.has(runId);
  }

  private cellFor(r: RunState, index: number): string | undefined {
    return r.indexes.get(index) ?? this.resolve(index);
  }

  private exec(r: RunState, runId: string, cellId: string): ExecHandle | undefined {
    let h = r.execs.get(cellId);
    if (!h) {
      h = this.sink.create(cellId);
      if (h) r.execs.set(cellId, h);
    }
    if (h && !r.started.has(cellId)) {
      r.started.add(cellId);
      h.start(null);
      this.sink.attribution(cellId, { runId, by: r.by, mine: r.mine, state: "running" });
    }
    return h;
  }

  handle(ev: KernelEvent): void {
    const d = ev.data ?? {};
    const runId: string | undefined = d.run_id;
    switch (ev.type) {
      case "run.queued":
      case "run.started": {
        if (!runId) return;
        const r = this.run(runId, d.started_by);
        if (Array.isArray(d.cells)) {
          for (const index of d.cells as number[]) {
            const id = this.cellFor(r, index);
            if (id && !r.started.has(id)) this.sink.attribution(id, { runId, by: r.by, mine: r.mine, state: "queued" });
          }
        }
        return;
      }
      case "cell.started": {
        if (!runId) return;
        const r = this.run(runId, d.started_by);
        const id = this.cellFor(r, d.index);
        if (!id) return;
        r.indexes.set(d.index, id);
        let h = r.execs.get(id);
        if (!h) {
          h = this.sink.create(id);
          if (h) r.execs.set(id, h);
        }
        if (h && !r.started.has(id)) {
          r.started.add(id);
          h.start(typeof d.execution_count === "number" ? d.execution_count : null);
        }
        this.sink.attribution(id, { runId, by: r.by, mine: r.mine, state: "running" });
        return;
      }
      case "output": {
        if (!runId) return;
        const r = this.run(runId);
        const id = this.cellFor(r, d.index);
        if (!id || !d.output) return;
        this.exec(r, runId, id)?.output(d.output as NbOutput);
        return;
      }
      case "output.clear": {
        if (!runId) return;
        const r = this.run(runId);
        const id = this.cellFor(r, d.index);
        if (!id) return;
        this.exec(r, runId, id)?.clear(!!d.wait);
        return;
      }
      case "cell.finished": {
        if (!runId) return;
        const r = this.runs.get(runId);
        if (!r) return;
        const id = this.cellFor(r, d.index);
        if (!id) return;
        r.execs.get(id)?.end(d.status);
        r.execs.delete(id);
        this.sink.attribution(id, null);
        this.sink.ran?.(id);
        return;
      }
      case "run.finished": {
        if (!runId) return;
        const r = this.runs.get(runId);
        if (!r) return;
        // Cells that never started (cancelled, interrupted before them) or never reported finish.
        for (const [id, h] of r.execs) {
          h.end(r.started.has(id) ? d.status : "cancelled");
          this.sink.attribution(id, null);
        }
        this.runs.delete(runId);
        return;
      }
      default:
        return;
    }
  }

  /** End every execution (connection lost / kernel gone). */
  abandonAll(): void {
    for (const r of this.runs.values()) {
      for (const [id, h] of r.execs) {
        h.end(undefined);
        this.sink.attribution(id, null);
      }
    }
    this.runs.clear();
  }
}
