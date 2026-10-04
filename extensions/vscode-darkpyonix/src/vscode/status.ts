// Cell badges (type, stale outputs, locks, focus, run attribution, conflicts), remote cursors and
// the kernel status bar item.
import * as vscode from "vscode";
import type { DpxCellMeta } from "../format/cells";
import { who } from "../collab/model";
import type { NotebookConnection } from "../connection";
import type { NotebookUiState } from "./notebookHost";
import { META_KEY, NOTEBOOK_TYPES, cellIdOf } from "./serializer";

export interface NotebookEntry {
  notebook: vscode.NotebookDocument;
  conn: NotebookConnection | null;
  ui: NotebookUiState;
}

export type EntryLookup = (nb: vscode.NotebookDocument) => NotebookEntry | undefined;

const PLAIN_TYPES = new Set(["code", "markdown"]);

export class CellStatusProvider implements vscode.NotebookCellStatusBarItemProvider {
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChangeCellStatusBarItems = this.emitter.event;

  constructor(private readonly lookup: EntryLookup, private readonly nickname: () => string) {}

  refresh(): void {
    this.emitter.fire();
  }

  provideCellStatusBarItems(cell: vscode.NotebookCell): vscode.NotebookCellStatusBarItem[] {
    const items: vscode.NotebookCellStatusBarItem[] = [];
    const meta = cell.metadata?.[META_KEY] as DpxCellMeta | undefined;
    const L = vscode.NotebookCellStatusBarAlignment.Left;
    const R = vscode.NotebookCellStatusBarAlignment.Right;

    if (meta && (!PLAIN_TYPES.has(meta.type) || (meta.type === "markdown" && cell.kind === vscode.NotebookCellKind.Code))) {
      const label = meta.rawType && meta.rawType !== meta.type ? `${meta.type} (${meta.rawType})` : meta.type;
      const item = new vscode.NotebookCellStatusBarItem(`$(symbol-field) ${label}`, R);
      item.tooltip = meta.type === "preamble"
        ? "Preamble: lines before the first `# %%` marker; runs once before any cell."
        : `DarkPyonix cell type [${meta.rawType ?? meta.type}]`;
      items.push(item);
    }
    if (meta?.title) items.push(new vscode.NotebookCellStatusBarItem(`$(tag) ${meta.title}`, R));

    const entry = this.lookup(cell.notebook);
    if (!entry) return items;
    const { ui, conn } = entry;
    if (ui.stale.has(cell)) {
      const item = new vscode.NotebookCellStatusBarItem("$(history) outputs from previous source", L);
      item.tooltip = "These outputs come from the latest run, but the cell's source has changed since (FR-R4).";
      items.push(item);
    }

    const id = cellIdOf(cell);
    if (!id) return items;
    const run = ui.runs.get(id);
    if (run) {
      const by = run.mine ? `${run.by?.user || "you"}@${run.by?.nickname || this.nickname()}` : who(run.by);
      const item = new vscode.NotebookCellStatusBarItem(`${run.state === "running" ? "$(sync~spin)" : "$(clock)"} ${run.state === "running" ? "run" : "queued"} by ${by}`, L);
      item.tooltip = `Run ${run.runId}`;
      items.push(item);
    }

    const session = conn?.session;
    if (!session) return items;
    const c = session.model.byId(id);
    if (c?.lock) {
      const mine = c.lock.locked_by === session.me;
      const item = new vscode.NotebookCellStatusBarItem(mine ? "$(edit) you are editing" : `$(lock) ${who(c.lock)} is editing`, L);
      item.tooltip = mine ? "You hold this cell's edit lock; it is released when you leave the cell or stop typing." : "Another client holds this cell's edit lock (FR-S3). Your edits here would be undone.";
      items.push(item);
    }
    for (const p of session.model.focusedBy(id)) {
      const item = new vscode.NotebookCellStatusBarItem(`$(eye) ${who(p)}`, L);
      item.tooltip = `${who(p)} is on this cell`;
      items.push(item);
    }
    if (c?.conflict) {
      const item = new vscode.NotebookCellStatusBarItem("$(warning) differs from file on disk", L);
      item.tooltip = "The file changed on disk while this cell was locked (doc.conflict).";
      items.push(item);
    }
    return items;
  }
}

/** Other clients' cursors as inline labels, and a left border on cells locked by someone else. */
export class RemoteDecorations implements vscode.Disposable {
  private readonly cursor = vscode.window.createTextEditorDecorationType({
    borderStyle: "solid",
    borderWidth: "0 0 0 2px",
    borderColor: new vscode.ThemeColor("editorCursor.foreground"),
  });
  private readonly lockedLine = vscode.window.createTextEditorDecorationType({
    isWholeLine: true,
    borderStyle: "solid",
    borderWidth: "0 0 0 3px",
    borderColor: new vscode.ThemeColor("editorWarning.foreground"),
  });

  constructor(private readonly lookup: EntryLookup) {}

  update(): void {
    for (const editor of vscode.window.visibleTextEditors) {
      const doc = editor.document;
      if (doc.uri.scheme !== "vscode-notebook-cell") continue;
      const nb = vscode.workspace.notebookDocuments.find((n) => NOTEBOOK_TYPES.includes(n.notebookType as never) && n.uri.fsPath === doc.uri.fsPath);
      const cell = nb?.getCells().find((c) => c.document === doc);
      const session = nb && this.lookup(nb)?.conn?.session;
      const id = cell && cellIdOf(cell);
      if (!session || !id) {
        editor.setDecorations(this.cursor, []);
        editor.setDecorations(this.lockedLine, []);
        continue;
      }
      const cursors: vscode.DecorationOptions[] = [];
      for (const p of session.model.presence.values()) {
        if (p.client_id === session.me || !p.cursor || p.cursor.cell_id !== id) continue;
        const pos = doc.validatePosition(new vscode.Position(p.cursor.line, p.cursor.column));
        cursors.push({
          range: new vscode.Range(pos, pos),
          hoverMessage: who(p),
          renderOptions: { after: { contentText: ` ${who(p)}`, color: new vscode.ThemeColor("editorCodeLens.foreground"), fontStyle: "italic" } },
        });
      }
      editor.setDecorations(this.cursor, cursors);
      const lock = session.model.lockedByOther(id);
      editor.setDecorations(this.lockedLine, lock ? [{ range: new vscode.Range(0, 0, Math.max(0, doc.lineCount - 1), 0), hoverMessage: `${who(lock)} is editing this cell` }] : []);
    }
  }

  dispose(): void {
    this.cursor.dispose();
    this.lockedLine.dispose();
  }
}

export class KernelStatusItem implements vscode.Disposable {
  private readonly item = vscode.window.createStatusBarItem("darkpyonix.kernel", vscode.StatusBarAlignment.Left, 100);

  constructor(private readonly lookup: EntryLookup) {
    this.item.name = "DarkPyonix Kernel";
    this.item.command = "darkpyonix.connect";
  }

  update(): void {
    const nb = vscode.window.activeNotebookEditor?.notebook;
    const entry = nb && NOTEBOOK_TYPES.includes(nb.notebookType as never) ? this.lookup(nb) : undefined;
    if (!entry) {
      this.item.hide();
      return;
    }
    const { ui, conn } = entry;
    const others = conn?.session ? [...conn.session.model.presence.values()].filter((p) => p.client_id !== conn.session!.me) : [];
    const state = ui.kernel ? ui.kernel.status : ui.kernelState;
    const icon = state === "busy" ? "$(loading~spin)" : ui.kernel ? "$(server-process)" : "$(debug-disconnect)";
    this.item.text = `${icon} DarkPyonix: ${state}${others.length ? ` · $(person) ${others.length}` : ""}`;
    const lines = [
      ui.kernel ? `Kernel ${ui.kernel.kernel_id} (pid ${ui.kernel.pid}, Python ${ui.kernel.python?.version ?? "?"})` : "No kernel attached. Run a cell or use “Connect” to start one.",
      conn ? `Manager ${conn.client.base}` : "",
      ...others.map((p) => `• ${who(p)}${p.focused_cell_id ? " (focused)" : ""}`),
    ].filter(Boolean);
    this.item.tooltip = lines.join("\n");
    this.item.show();
  }

  dispose(): void {
    this.item.dispose();
  }
}
