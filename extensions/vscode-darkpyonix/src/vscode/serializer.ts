// NotebookSerializer for *.pynb (default) and *.py ("Open as DarkPyonix Notebook").
// Bytes in = bytes out for unedited notebooks (FORMAT §2, FR-F1); outputs are never written.
import * as vscode from "vscode";
import { textToView, viewToText, type DpxCellMeta, type ViewCell } from "../format/cells";

export const NOTEBOOK_TYPES = ["darkpyonix-notebook", "darkpyonix-notebook-py"] as const;

/** Persisted (round-trip) cell metadata key; changing it is a content change. */
export const META_KEY = "darkpyonix";
export const SERIALIZER_OPTIONS: vscode.NotebookDocumentContentOptions = {
  transientOutputs: true,
};

/**
 * Kernel cell_id of each editor cell, kept by cell identity rather than in cell metadata:
 * a metadata edit marks the notebook dirty, and a copied cell would carry the id along.
 */
const cellIds = new WeakMap<vscode.NotebookCell, string>();

export function bindCellId(cell: vscode.NotebookCell, cellId: string): void {
  cellIds.set(cell, cellId);
}

export function cellIdOf(cell: vscode.NotebookCell): string | undefined {
  return cellIds.get(cell);
}

export function cellData(view: ViewCell): vscode.NotebookCellData {
  const data = new vscode.NotebookCellData(
    view.kind === "markup" ? vscode.NotebookCellKind.Markup : vscode.NotebookCellKind.Code,
    view.value,
    view.language,
  );
  const metadata: Record<string, unknown> = {};
  if (view.meta) metadata[META_KEY] = view.meta;
  data.metadata = metadata;
  return data;
}

export function viewOfData(kind: vscode.NotebookCellKind, value: string, languageId: string, metadata: Record<string, any> | undefined): ViewCell {
  const markup = kind === vscode.NotebookCellKind.Markup;
  return {
    kind: markup ? "markup" : "code",
    value,
    language: markup || languageId === "markdown" ? "markdown" : "python",
    meta: metadata?.[META_KEY] as DpxCellMeta | undefined,
  };
}

export function viewOfCell(cell: vscode.NotebookCell): ViewCell {
  return viewOfData(cell.kind, cell.document.getText(), cell.document.languageId, cell.metadata);
}

export class DarkPyonixSerializer implements vscode.NotebookSerializer {
  deserializeNotebook(content: Uint8Array): vscode.NotebookData {
    // Keep a BOM (ignoreBOM: do not strip it) so it can be written back.
    const text = new TextDecoder("utf-8", { ignoreBOM: true }).decode(content);
    const nb = textToView(text);
    const data = new vscode.NotebookData(nb.cells.map((v) => cellData(v)));
    data.metadata = { [META_KEY]: { bom: nb.bom } };
    return data;
  }

  serializeNotebook(data: vscode.NotebookData): Uint8Array {
    const cells = data.cells.map((c) => viewOfData(c.kind, c.value, c.languageId, c.metadata));
    const bom = !!(data.metadata?.[META_KEY] as { bom?: boolean } | undefined)?.bom;
    return new TextEncoder().encode(viewToText({ cells, bom }));
  }
}
