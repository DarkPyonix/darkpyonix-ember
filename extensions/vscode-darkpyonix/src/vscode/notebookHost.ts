// VS Code implementations of NotebookHost (cells), OutputsHost (stored outputs) and ExecSink
// (live outputs) for one open notebook.
import * as vscode from "vscode";
import type { ViewCell } from "../format/cells";
import type { HostCell, NotebookHost, NotifyLevel } from "../collab/session";
import type { OutputsHost } from "../connection";
import type { DocumentCell, Kernel, NbOutput } from "../manager/types";
import { convertOutput, type ConvertedOutput } from "../run/outputs";
import type { ExecHandle, ExecSink, RunInfo } from "../run/tracker";
import { META_KEY, bindCellId, cellData, cellIdOf, viewOfCell } from "./serializer";

export function toCellOutput(c: ConvertedOutput): vscode.NotebookCellOutput {
  return new vscode.NotebookCellOutput(
    c.items.map((i) => new vscode.NotebookCellOutputItem(i.data, i.mime)),
    c.metadata,
  );
}

/** Badges and kernel state for one notebook, read by the status bar provider. */
export class NotebookUiState {
  stale = new WeakSet<vscode.NotebookCell>();
  runs = new Map<string, RunInfo>();
  kernel: Kernel | null = null;
  kernelState = "not connected";
  constructor(readonly onChange: () => void) {}
}

export class VsNotebookHost implements NotebookHost, OutputsHost {
  /** Snapshot outputs waiting for the controller to be selected for this notebook. */
  private pendingOutputs: Array<{ cellId?: string; position: number; cell: DocumentCell }> = [];

  constructor(
    readonly notebook: vscode.NotebookDocument,
    readonly controllerFor: () => vscode.NotebookController | undefined,
    readonly ui: NotebookUiState,
  ) {}

  getCells(): HostCell[] {
    return this.notebook.getCells().map((cell) => ({ handle: cell, cellId: cellIdOf(cell), view: viewOfCell(cell) }));
  }

  private async apply(edits: vscode.NotebookEdit[]): Promise<void> {
    const we = new vscode.WorkspaceEdit();
    we.set(this.notebook.uri, edits);
    await vscode.workspace.applyEdit(we);
  }

  async insertCell(position: number, cellId: string, view: ViewCell): Promise<void> {
    const at = Math.min(position, this.notebook.cellCount);
    await this.apply([vscode.NotebookEdit.insertCells(at, [cellData(view)])]);
    bindCellId(this.notebook.cellAt(at), cellId);
  }

  async replaceCell(position: number, view: ViewCell): Promise<void> {
    const cell = this.notebook.cellAt(position);
    const kind = view.kind === "markup" ? vscode.NotebookCellKind.Markup : vscode.NotebookCellKind.Code;
    const metadata = { ...cell.metadata, [META_KEY]: view.meta };
    if (cell.kind === kind) {
      const doc = cell.document;
      if (doc.getText() !== view.value) {
        const we = new vscode.WorkspaceEdit();
        we.replace(doc.uri, new vscode.Range(0, 0, doc.lineCount, 0), view.value);
        await vscode.workspace.applyEdit(we);
      }
      if (JSON.stringify(cell.metadata?.[META_KEY]) !== JSON.stringify(view.meta)) {
        await this.apply([vscode.NotebookEdit.updateCellMetadata(cell.index, metadata)]);
      }
      return;
    }
    const id = cellIdOf(cell);
    const data = cellData(view);
    data.outputs = [...cell.outputs];
    await this.apply([vscode.NotebookEdit.replaceCells(new vscode.NotebookRange(position, position + 1), [data])]);
    if (id) bindCellId(this.notebook.cellAt(position), id);
  }

  async deleteCell(position: number): Promise<void> {
    await this.apply([vscode.NotebookEdit.deleteCells(new vscode.NotebookRange(position, position + 1))]);
  }

  async moveCell(from: number, to: number): Promise<void> {
    const cell = this.notebook.cellAt(from);
    const id = cellIdOf(cell);
    const data = cellData(viewOfCell(cell));
    data.outputs = [...cell.outputs];
    data.executionSummary = cell.executionSummary;
    await this.apply([
      vscode.NotebookEdit.deleteCells(new vscode.NotebookRange(from, from + 1)),
      vscode.NotebookEdit.insertCells(to, [data]),
    ]);
    if (id) bindCellId(this.notebook.cellAt(to), id);
  }

  async resetCells(cells: Array<{ cellId: string; view: ViewCell }>): Promise<void> {
    const old = new Map(this.notebook.getCells().filter((c) => cellIdOf(c)).map((c) => [cellIdOf(c)!, c]));
    const datas = cells.map(({ cellId, view }) => {
      const data = cellData(view);
      const prev = old.get(cellId);
      if (prev && prev.document.getText() === view.value) {
        data.outputs = [...prev.outputs];
        data.executionSummary = prev.executionSummary;
      }
      return data;
    });
    await this.apply([vscode.NotebookEdit.replaceCells(new vscode.NotebookRange(0, this.notebook.cellCount), datas)]);
    cells.forEach(({ cellId }, i) => bindCellId(this.notebook.cellAt(i), cellId));
    this.ui.onChange();
  }

  async bindCells(pairs: Array<{ handle: unknown; cellId: string }>): Promise<void> {
    for (const { handle, cellId } of pairs) bindCellId(handle as vscode.NotebookCell, cellId);
    this.flushPendingOutputs();
    this.ui.onChange();
  }

  refreshState(): void {
    this.ui.onChange();
  }

  async notify(level: NotifyLevel, message: string, ...actions: string[]): Promise<string | undefined> {
    const show = level === "error" ? vscode.window.showErrorMessage : level === "warning" ? vscode.window.showWarningMessage : vscode.window.showInformationMessage;
    return show(message, ...actions);
  }

  kernelChanged(kernel: Kernel | null, state: string): void {
    this.ui.kernel = kernel;
    this.ui.kernelState = state;
    this.ui.onChange();
  }

  /** FR-R4: stored outputs and stale marks, written through an execution (no dirty state). */
  showOutputs(position: number, cell: DocumentCell): void {
    const nbCell = this.notebook.cellAt(position);
    if (!this.writeOutputs(nbCell, cell)) {
      this.pendingOutputs.push({ cellId: cellIdOf(nbCell), position, cell });
    }
  }

  flushPendingOutputs(): void {
    const pending = this.pendingOutputs;
    this.pendingOutputs = [];
    for (const p of pending) {
      const nbCell = (p.cellId && this.notebook.getCells().find((c) => cellIdOf(c) === p.cellId)) || (p.position < this.notebook.cellCount ? this.notebook.cellAt(p.position) : undefined);
      if (nbCell && !this.writeOutputs(nbCell, p.cell)) this.pendingOutputs.push(p);
    }
  }

  private writeOutputs(nbCell: vscode.NotebookCell, cell: DocumentCell): boolean {
    const controller = this.controllerFor();
    if (!controller) return false;
    let exec: vscode.NotebookCellExecution;
    try {
      exec = controller.createNotebookCellExecution(nbCell);
    } catch {
      return false;
    }
    exec.start();
    if (typeof cell.execution_count === "number") exec.executionOrder = cell.execution_count;
    void exec.replaceOutput((cell.outputs ?? []).map((o) => toCellOutput(convertOutput(o))));
    exec.end(cell.status === "ok" ? true : cell.status ? false : undefined);
    if (cell.stale) this.ui.stale.add(nbCell);
    else this.ui.stale.delete(nbCell);
    this.ui.onChange();
    return true;
  }
}

/** Live executions driven by run events. */
export class VsExecSink implements ExecSink {
  constructor(
    readonly notebook: vscode.NotebookDocument,
    readonly controllerFor: () => vscode.NotebookController | undefined,
    readonly ui: NotebookUiState,
  ) {}

  private cell(cellId: string): vscode.NotebookCell | undefined {
    return this.notebook.getCells().find((c) => cellIdOf(c) === cellId);
  }

  create(cellId: string): ExecHandle | undefined {
    const cell = this.cell(cellId);
    const controller = this.controllerFor();
    if (!cell || !controller) return undefined;
    let exec: vscode.NotebookCellExecution;
    try {
      exec = controller.createNotebookCellExecution(cell);
    } catch {
      return undefined; // already executing (or controller not selected)
    }
    let started = false;
    let clearOnNext = false;
    let outputs: vscode.NotebookCellOutput[] = [];
    let last: { out: vscode.NotebookCellOutput; stream?: string } | null = null;
    const ui = this.ui;
    return {
      start(executionCount) {
        if (started) return;
        started = true;
        exec.start(Date.now());
        if (executionCount !== null) exec.executionOrder = executionCount;
        void exec.clearOutput();
        outputs = [];
        last = null;
        ui.stale.delete(cell);
      },
      output(o: NbOutput) {
        if (!started) this.start(null);
        const conv = convertOutput(o);
        if (o.output_type === "execute_result" && typeof o.execution_count === "number") exec.executionOrder = o.execution_count;
        if (clearOnNext) {
          clearOnNext = false;
          outputs = [];
          last = null;
          const out = toCellOutput(conv);
          outputs.push(out);
          last = { out, stream: conv.stream };
          void exec.replaceOutput(out);
          return;
        }
        if (conv.stream && last && last.stream === conv.stream) {
          void exec.appendOutputItems(conv.items.map((i) => new vscode.NotebookCellOutputItem(i.data, i.mime)), last.out);
          return;
        }
        const out = toCellOutput(conv);
        outputs.push(out);
        last = { out, stream: conv.stream };
        void exec.appendOutput(out);
      },
      clear(wait: boolean) {
        if (wait) {
          clearOnNext = true;
          return;
        }
        outputs = [];
        last = null;
        void exec.clearOutput();
      },
      end(status) {
        if (!started) exec.start(Date.now());
        exec.end(status === "ok" ? true : status === undefined ? undefined : false, Date.now());
      },
    };
  }

  attribution(cellId: string, info: RunInfo | null): void {
    if (info) this.ui.runs.set(cellId, info);
    else this.ui.runs.delete(cellId);
    this.ui.onChange();
  }

  ran(cellId: string): void {
    const cell = this.cell(cellId);
    if (cell) this.ui.stale.delete(cell);
  }
}
