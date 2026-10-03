// The kernel's shared document as this client knows it (SPEC FR-S1..S5, PROTOCOL §4):
// a snapshot plus the `doc.*` and `presence.*` events applied in order. Pure and synchronous;
// `apply` returns what the editor has to change, without touching any editor.
import type { Attribution, Document, DocumentCell, KernelEvent, Lock, Presence } from "../manager/types";

export type ModelOp =
  /** A cell appeared after `afterId` (null: at the start). */
  | { op: "insert"; cellId: string; afterId: string | null; by?: Attribution }
  | { op: "update"; cellId: string; by?: Attribution }
  | { op: "delete"; cellId: string; by?: Attribution }
  | { op: "move"; cellId: string; afterId: string | null; by?: Attribution }
  /** Every cell may have changed (`doc.reloaded`). */
  | { op: "reset" }
  /** Locks or presence changed: redraw badges. */
  | { op: "state" }
  | { op: "conflict"; cellId: string; local: { source: string; version: number; by?: Attribution }; disk: { source: string } }
  /** A lock this client held was released by the kernel (idle or disconnect). */
  | { op: "lockLost"; cellId: string; reason: string };

export class DocModel {
  cells: DocumentCell[] = [];
  docVersion = 0;
  readonly presence = new Map<string, Presence>();

  constructor(readonly me: string) {}

  load(doc: Document): void {
    this.cells = doc.cells.map((c) => ({ ...c }));
    this.cells.sort((a, b) => a.index - b.index);
    this.reindex();
    this.docVersion = doc.doc_version;
    this.presence.clear();
    for (const p of doc.presence ?? []) this.presence.set(p.client_id, p);
  }

  byId(id: string): DocumentCell | undefined {
    return this.cells.find((c) => c.cell_id === id);
  }

  indexOf(id: string): number {
    return this.cells.findIndex((c) => c.cell_id === id);
  }

  idAt(index: number): string | undefined {
    return this.cells[index]?.cell_id;
  }

  /** The id of the cell before `id` in document order, or null. */
  prevId(id: string): string | null {
    const i = this.indexOf(id);
    return i > 0 ? this.cells[i - 1].cell_id : null;
  }

  /** A cell the editor shows: everything but an empty preamble. */
  isVisible(c: DocumentCell): boolean {
    return !(c.index === 0 && c.type === "preamble" && c.source === "");
  }

  visibleCells(): DocumentCell[] {
    return this.cells.filter((c) => this.isVisible(c));
  }

  lockedByOther(id: string): Lock | undefined {
    const lock = this.byId(id)?.lock;
    return lock && lock.locked_by !== this.me ? lock : undefined;
  }

  /** Merge a cell returned by an HTTP call (create/update/move/unlock). Returns true if new. */
  upsert(cell: DocumentCell): boolean {
    const i = this.indexOf(cell.cell_id);
    if (i >= 0) {
      const prev = this.cells[i];
      if (cell.version < prev.version) return false;
      const merged = { ...prev, ...cell };
      if (!("lock" in cell)) merged.lock = prev.lock;
      this.cells.splice(i, 1);
      this.cells.splice(clamp(cell.index ?? i, 0, this.cells.length), 0, merged);
      this.reindex();
      return false;
    }
    this.cells.splice(clamp(cell.index, 0, this.cells.length), 0, { ...cell });
    this.reindex();
    return true;
  }

  remove(id: string): void {
    const i = this.indexOf(id);
    if (i >= 0) {
      this.cells.splice(i, 1);
      this.reindex();
    }
  }

  moveTo(id: string, toIndex: number): void {
    const i = this.indexOf(id);
    if (i < 0) return;
    const [c] = this.cells.splice(i, 1);
    this.cells.splice(clamp(toIndex, 0, this.cells.length), 0, c);
    this.reindex();
  }

  setLock(id: string, lock: Lock | null): void {
    const c = this.byId(id);
    if (c) c.lock = lock;
  }

  private reindex(): void {
    this.cells.forEach((c, i) => { c.index = i; });
  }

  private stale(d: Record<string, any>): boolean {
    return typeof d.doc_version === "number" && d.doc_version <= this.docVersion;
  }

  private bump(d: Record<string, any>): void {
    if (typeof d.doc_version === "number" && d.doc_version > this.docVersion) this.docVersion = d.doc_version;
  }

  apply(ev: KernelEvent): ModelOp[] {
    const d = ev.data ?? {};
    const by: Attribution | undefined = d.by;
    switch (ev.type) {
      case "doc.cell.created": {
        if (this.stale(d) && this.byId(d.cell?.cell_id)) return [];
        this.bump(d);
        const cell = d.cell as DocumentCell;
        const isNew = this.upsert(cell);
        return isNew
          ? [{ op: "insert", cellId: cell.cell_id, afterId: this.prevId(cell.cell_id), by }]
          : [{ op: "update", cellId: cell.cell_id, by }];
      }
      case "doc.cell.updated": {
        if (this.stale(d)) return [];
        this.bump(d);
        const cell = d.cell as DocumentCell;
        const known = this.byId(cell.cell_id);
        if (!known) {
          this.upsert(cell);
          return [{ op: "insert", cellId: cell.cell_id, afterId: this.prevId(cell.cell_id), by }];
        }
        const moved = cell.index !== undefined && cell.index !== known.index;
        this.upsert(cell);
        const ops: ModelOp[] = [{ op: "update", cellId: cell.cell_id, by }];
        if (moved) ops.push({ op: "move", cellId: cell.cell_id, afterId: this.prevId(cell.cell_id), by });
        return ops;
      }
      case "doc.cell.deleted": {
        if (this.stale(d)) return [];
        this.bump(d);
        const id: string | undefined = d.cell_id ?? d.cell?.cell_id;
        if (!id || !this.byId(id)) return [];
        this.remove(id);
        return [{ op: "delete", cellId: id, by }];
      }
      case "doc.cell.moved": {
        if (this.stale(d)) return [];
        this.bump(d);
        const cell = d.cell as DocumentCell;
        if (!this.byId(cell.cell_id)) {
          this.upsert(cell);
          return [{ op: "insert", cellId: cell.cell_id, afterId: this.prevId(cell.cell_id), by }];
        }
        this.upsert(cell);
        return [{ op: "move", cellId: cell.cell_id, afterId: this.prevId(cell.cell_id), by }];
      }
      case "doc.lock": {
        this.bump(d);
        const lock: Lock | undefined = d.lock;
        if (d.cell_id && lock) this.setLock(d.cell_id, lock);
        return [{ op: "state" }];
      }
      case "doc.unlock": {
        this.bump(d);
        const prev = this.byId(d.cell_id)?.lock;
        this.setLock(d.cell_id, null);
        const ops: ModelOp[] = [{ op: "state" }];
        if (prev && prev.locked_by === this.me && d.reason && d.reason !== "released") {
          ops.push({ op: "lockLost", cellId: d.cell_id, reason: d.reason });
        }
        return ops;
      }
      case "doc.reloaded": {
        if (this.stale(d)) return [];
        this.bump(d);
        const locks = new Map(this.cells.filter((c) => c.lock).map((c) => [c.cell_id, c.lock!]));
        this.cells = (d.cells as DocumentCell[]).map((c) => ({ ...c, lock: "lock" in c ? c.lock : locks.get(c.cell_id) ?? null }));
        this.cells.sort((a, b) => a.index - b.index);
        this.reindex();
        return [{ op: "reset" }, { op: "state" }];
      }
      case "doc.conflict": {
        const c = this.byId(d.cell_id);
        if (c) c.conflict = { disk_source: d.disk?.source };
        return [{ op: "conflict", cellId: d.cell_id, local: d.local ?? { source: c?.source ?? "", version: c?.version ?? 0 }, disk: d.disk ?? { source: "" } }, { op: "state" }];
      }
      case "presence.update": {
        if (!d.client_id) return [];
        const prev = this.presence.get(d.client_id);
        this.presence.set(d.client_id, { ...(prev ?? {}), ...d } as Presence);
        return [{ op: "state" }];
      }
      case "presence.leave": {
        if (!d.client_id) return [];
        this.presence.delete(d.client_id);
        return [{ op: "state" }];
      }
      default:
        return [];
    }
  }

  /** Other clients focused on a cell (FR-S4). */
  focusedBy(cellId: string): Presence[] {
    return [...this.presence.values()].filter((p) => p.client_id !== this.me && p.focused_cell_id === cellId);
  }
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, n));
}

export function who(a: { user?: string; nickname?: string } | null | undefined): string {
  if (!a) return "someone";
  if (a.user && a.nickname) return `${a.user}@${a.nickname}`;
  return a.user || a.nickname || "someone";
}
