// Array-backed NotebookHost + ExecSink: an editor without VS Code.
import { textToView, type ViewCell } from "../../src/format/cells";
import type { HostCell, NotebookHost, NotifyLevel } from "../../src/collab/session";
import type { OutputsHost } from "../../src/connection";
import type { DocumentCell, Kernel, NbOutput } from "../../src/manager/types";
import type { ExecHandle, ExecSink, RunInfo } from "../../src/run/tracker";

export class FakeHost implements NotebookHost, OutputsHost {
  cells: HostCell[];
  notes: Array<{ level: NotifyLevel; message: string; actions: string[] }> = [];
  answer: string | undefined = undefined;
  shown = new Map<number, DocumentCell>();
  kernelStates: string[] = [];
  resets = 0;
  refreshes = 0;

  constructor(text: string) {
    this.cells = textToView(text).cells.map((view) => ({ handle: {}, view }));
  }

  getCells(): HostCell[] {
    return this.cells.map((c) => ({ ...c, view: { ...c.view } }));
  }
  private at(handle: unknown): HostCell {
    return this.cells.find((c) => c.handle === handle)!;
  }
  async insertCell(position: number, cellId: string, view: ViewCell): Promise<void> {
    this.cells.splice(position, 0, { handle: {}, cellId, view });
  }
  async replaceCell(position: number, view: ViewCell): Promise<void> {
    this.cells[position] = { ...this.cells[position], view };
  }
  async deleteCell(position: number): Promise<void> {
    this.cells.splice(position, 1);
  }
  async moveCell(from: number, to: number): Promise<void> {
    const [c] = this.cells.splice(from, 1);
    this.cells.splice(to, 0, c);
  }
  async resetCells(cells: Array<{ cellId: string; view: ViewCell }>): Promise<void> {
    this.resets++;
    this.cells = cells.map((c) => ({ handle: {}, cellId: c.cellId, view: c.view }));
  }
  async bindCells(pairs: Array<{ handle: unknown; cellId: string }>): Promise<void> {
    for (const p of pairs) this.at(p.handle).cellId = p.cellId;
  }
  refreshState(): void {
    this.refreshes++;
  }
  async notify(level: NotifyLevel, message: string, ...actions: string[]): Promise<string | undefined> {
    this.notes.push({ level, message, actions });
    return actions.length ? this.answer : undefined;
  }
  showOutputs(position: number, cell: DocumentCell): void {
    this.shown.set(position, cell);
  }
  kernelChanged(_kernel: Kernel | null, state: string): void {
    this.kernelStates.push(state);
  }

  // Simulated typing.
  type(position: number, value: string): void {
    this.cells[position] = { ...this.cells[position], view: { ...this.cells[position].view, value } };
  }
  texts(): string[] {
    return this.cells.map((c) => c.view.value);
  }
}

export interface ExecRecord {
  cellId: string;
  started: number | null | undefined;
  outputs: NbOutput[];
  cleared: number;
  ended?: string;
  done: boolean;
}

export class FakeSink implements ExecSink {
  execs: ExecRecord[] = [];
  attributions: Array<{ cellId: string; info: RunInfo | null }> = [];
  ranCells: string[] = [];

  create(cellId: string): ExecHandle {
    const rec: ExecRecord = { cellId, started: undefined, outputs: [], cleared: 0, done: false };
    this.execs.push(rec);
    return {
      start: (n) => { rec.started = n; },
      output: (o) => { rec.outputs.push(o); },
      clear: () => { rec.cleared++; rec.outputs = []; },
      end: (s) => { rec.ended = s; rec.done = true; },
    };
  }
  attribution(cellId: string, info: RunInfo | null): void {
    this.attributions.push({ cellId, info });
  }
  ran(cellId: string): void {
    this.ranCells.push(cellId);
  }
}

export async function until(cond: () => boolean, ms = 3000, what = "condition"): Promise<void> {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}
