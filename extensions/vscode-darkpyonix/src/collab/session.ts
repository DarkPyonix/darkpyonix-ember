// Collaboration on one kernel's document (SPEC FR-S1..S8). Editor-agnostic: the VS Code layer
// implements `NotebookHost`; tests implement it with plain arrays.
//
// Echo-loop rule: the model always holds the server's version of each cell. Remote changes are
// written to the model first and then to the editor, so the editor-change callback that follows
// finds editor == model and sends nothing. Only differences between the editor and the model are
// sent, and only for cells this client may edit.
import { canonicalType } from "../format/parser";
import { bodyToView, effectiveType, viewToSource, type ViewCell } from "../format/cells";
import { ApiError, type ManagerClient } from "../manager/client";
import type { EventStream } from "../manager/sse";
import type { Cursor, Document, DocumentCell, KernelEvent, Lock } from "../manager/types";
import { DocModel, who, type ModelOp } from "./model";

/** One editor cell. `handle` must keep its identity while the cell exists. */
export interface HostCell {
  handle: unknown;
  cellId?: string;
  view: ViewCell;
}

export type NotifyLevel = "info" | "warning" | "error";

export interface NotebookHost {
  getCells(): HostCell[];
  insertCell(position: number, cellId: string, view: ViewCell): Promise<void>;
  /** Replace text and format metadata of the cell at `position`; keeps its id and outputs. */
  replaceCell(position: number, view: ViewCell): Promise<void>;
  deleteCell(position: number): Promise<void>;
  moveCell(from: number, to: number): Promise<void>;
  /** Replace all cells; keep outputs of cells whose id and text are unchanged. */
  resetCells(cells: Array<{ cellId: string; view: ViewCell }>): Promise<void>;
  bindCells(pairs: Array<{ handle: unknown; cellId: string }>): Promise<void>;
  /** Locks, presence, conflicts or stale marks changed. */
  refreshState(): void;
  notify(level: NotifyLevel, message: string, ...actions: string[]): Promise<string | undefined>;
}

export interface SessionOptions {
  /** Debounce for sending typed text (ms). */
  editDebounceMs?: number;
  /** Release the lock after this long without typing (ms). */
  lockIdleMs?: number;
  /** Renew held locks this often (ms); the kernel drops them after 3 min (FR-S3). */
  lockRenewMs?: number;
  /** Minimum interval between presence updates (ms). */
  presenceThrottleMs?: number;
  /**
   * How long a cell must stay unbound (or missing) in the editor before it is sent as a create
   * (or delete). VS Code reloads a clean notebook when the kernel rewrites the file, replacing
   * cells with new, unbound ones; this gives `doc.reloaded` time to arrive so those cells are
   * adopted instead of re-created (ms).
   */
  structureSettleMs?: number;
  /** Run, cell and output events go here. */
  onRunEvent?: (ev: KernelEvent) => void;
  /** The SSE stream reported `replay_truncated` or reconnected after a gap: resync. */
  onResync?: () => void;
  log?: (line: string) => void;
}

interface Pending {
  source: string;
  type: string;
  timer?: ReturnType<typeof setTimeout>;
}

export class CollabSession {
  readonly model: DocModel;
  private stream: EventStream | null = null;
  private chain: Promise<void> = Promise.resolve();
  private syncTimer: ReturnType<typeof setTimeout> | null = null;
  private pending = new Map<string, Pending>();
  private myLocks = new Set<string>();
  private locking = new Map<string, Promise<boolean>>();
  private idleTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private renewTimer: ReturnType<typeof setInterval> | null = null;
  private pendingCreates = 0;
  private presenceTimer: ReturnType<typeof setTimeout> | null = null;
  private presenceLast = 0;
  private presenceNext: { focused_cell_id: string | null; cursor: Cursor | null } | null = null;
  private focusedCellId: string | null = null;
  private firstUnbound = new WeakMap<object, number>();
  private firstMissing = new Map<string, number>();
  private settleTimer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;
  private readonly o: Required<Omit<SessionOptions, "onRunEvent" | "onResync" | "log">> & SessionOptions;

  constructor(
    readonly client: ManagerClient,
    readonly kernelId: string,
    readonly host: NotebookHost,
    opts: SessionOptions = {},
  ) {
    this.model = new DocModel(client.identity.clientId);
    this.o = { editDebounceMs: 300, lockIdleMs: 20000, lockRenewMs: 60000, presenceThrottleMs: 200, structureSettleMs: 1000, ...opts };
  }

  get me(): string {
    return this.client.identity.clientId;
  }

  isLockedByMe(cellId: string): boolean {
    return this.myLocks.has(cellId);
  }

  /** Load the snapshot, align the editor with it and subscribe from its `seq` (FR-S1). */
  async start(doc?: Document): Promise<Document> {
    const snapshot = doc ?? (await this.client.getKernelDocument(this.kernelId));
    this.model.load(snapshot);
    await this.enqueue(() => this.reconcile());
    this.host.refreshState();
    // `seq` is "the event number right after the snapshot"; ask from one before it and let the
    // doc_version checks drop what the snapshot already contains.
    const since = snapshot.seq > 0 ? snapshot.seq - 1 : 0;
    this.stream = this.client.events(this.kernelId, since, (ev) => this.onEvent(ev), (state, err) => {
      if (state === "closed" && err) this.o.log?.(`events: ${(err as Error).message ?? err}`);
    });
    this.renewTimer = setInterval(() => void this.renewLocks(), this.o.lockRenewMs);
    return snapshot;
  }

  /** Re-read the snapshot (after `replay_truncated` or on demand) and realign the editor. */
  async resync(): Promise<void> {
    const snapshot = await this.client.getKernelDocument(this.kernelId);
    this.model.load(snapshot);
    for (const id of [...this.myLocks]) {
      if (this.model.byId(id)?.lock?.locked_by !== this.me) this.myLocks.delete(id);
    }
    await this.enqueue(() => this.reconcile(true));
    this.host.refreshState();
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    if (this.syncTimer) clearTimeout(this.syncTimer);
    if (this.settleTimer) clearTimeout(this.settleTimer);
    if (this.renewTimer) clearInterval(this.renewTimer);
    if (this.presenceTimer) clearTimeout(this.presenceTimer);
    for (const t of this.idleTimers.values()) clearTimeout(t);
    try {
      await this.flushAll();
      await Promise.all([...this.myLocks].map((id) => this.release(id)));
    } catch {
      // best effort
    }
    this.stream?.close();
    await this.client.leavePresence(this.kernelId).catch(() => undefined);
  }

  // ---------------------------------------------------------------- editor ← server

  private enqueue(fn: () => Promise<void> | void): Promise<void> {
    const next = this.chain.then(fn).catch((err) => {
      this.o.log?.(`collab: ${(err as Error).stack ?? err}`);
    });
    this.chain = next;
    return next;
  }

  private onEvent(ev: KernelEvent): void {
    if (this.disposed) return;
    if (ev.type === "replay_truncated") {
      void this.resync();
      this.o.onResync?.();
      return;
    }
    if (!ev.type.startsWith("doc.") && !ev.type.startsWith("presence.")) {
      this.o.onRunEvent?.(ev);
      return;
    }
    // Apply to the model in arrival order, then let the editor catch up in the queue.
    const ops = this.model.apply(ev);
    if (ops.length) void this.enqueue(() => this.applyOps(ops));
  }

  /** Editor view of a model cell, keeping the exact header of an editor cell when it still fits. */
  viewFor(c: DocumentCell, existing?: ViewCell): ViewCell {
    const type = canonicalType(c.type === "preamble" ? null : c.type);
    const realType = c.type === "preamble" ? "preamble" : type;
    const m = existing?.meta;
    const keepHeader = m && m.type === realType && (m.title ?? null) === (c.title ?? null)
      && JSON.stringify(m.metadata) === JSON.stringify(c.metadata ?? {});
    return bodyToView(realType, c.source, {
      rawType: keepHeader ? m!.rawType : realType === "preamble" ? null : c.type,
      title: c.title ?? null,
      metadata: c.metadata ?? {},
      header: keepHeader ? m!.header : "",
    });
  }

  private hostSource(cells: HostCell[], pos: number): string {
    return viewToSource(cells[pos].view, pos, pos === cells.length - 1);
  }

  private positionOf(cells: HostCell[], cellId: string): number {
    return cells.findIndex((c) => c.cellId === cellId);
  }

  /** Position right after `afterId` in the editor (0 if it is absent, e.g. a hidden preamble). */
  private positionAfter(cells: HostCell[], afterId: string | null): number {
    let id = afterId;
    while (id) {
      const p = this.positionOf(cells, id);
      if (p >= 0) return p + 1;
      id = this.model.prevId(id);
    }
    return 0;
  }

  private async applyOps(ops: ModelOp[]): Promise<void> {
    for (const op of ops) {
      const cells = this.host.getCells();
      switch (op.op) {
        case "insert": {
          if (this.positionOf(cells, op.cellId) >= 0) break;
          // Our own create: the HTTP response binds the editor cell that is already there.
          if (op.by?.client_id === this.me && this.pendingCreates > 0) break;
          const c = this.model.byId(op.cellId);
          if (!c || !this.model.isVisible(c)) break;
          await this.host.insertCell(this.positionAfter(cells, op.afterId), c.cell_id, this.viewFor(c));
          break;
        }
        case "update": {
          const c = this.model.byId(op.cellId);
          if (!c) break;
          const pos = this.positionOf(cells, op.cellId);
          if (pos < 0) {
            if (this.model.isVisible(c)) await this.host.insertCell(this.positionAfter(cells, this.model.prevId(c.cell_id)), c.cell_id, this.viewFor(c));
            break;
          }
          if (!this.model.isVisible(c)) {
            await this.host.deleteCell(pos);
            break;
          }
          // The user is typing here and holds the lock: this is our own edit coming back.
          if (this.myLocks.has(op.cellId) || this.pending.has(op.cellId)) break;
          if (this.hostSource(cells, pos) !== c.source || effectiveType(cells[pos].view, pos) !== this.viewFor(c).meta!.type || cells[pos].view.meta?.title !== (c.title ?? null)) {
            await this.host.replaceCell(pos, this.viewFor(c, cells[pos].view));
          }
          break;
        }
        case "delete": {
          this.dropLocal(op.cellId);
          const pos = this.positionOf(cells, op.cellId);
          if (pos >= 0) await this.host.deleteCell(pos);
          break;
        }
        case "move": {
          const from = this.positionOf(cells, op.cellId);
          if (from < 0) break;
          let to = this.positionAfter(cells, op.afterId);
          if (to > from) to -= 1;
          if (to !== from) await this.host.moveCell(from, to);
          break;
        }
        case "reset":
          await this.reconcile(true);
          break;
        case "state":
          this.host.refreshState();
          break;
        case "conflict":
          void this.onConflict(op.cellId, op.local, op.disk);
          break;
        case "lockLost":
          this.myLocks.delete(op.cellId);
          if (this.pending.has(op.cellId)) void this.lockAndFlushLater(op.cellId);
          break;
      }
    }
  }

  /**
   * Align the editor with the model. When the cells already match (the usual case right after
   * opening the file the kernel parsed), only bind ids; otherwise replace the editor's cells,
   * keeping the text of cells this client is still editing.
   */
  private async reconcile(force = false): Promise<void> {
    const cells = this.host.getCells();
    const visible = this.model.visibleCells();
    const sameShape = cells.length === visible.length && visible.every((c, i) => {
      const view = cells[i].view;
      return effectiveType(view, i) === (c.type === "preamble" ? "preamble" : canonicalType(c.type))
        && this.hostSource(cells, i) === c.source;
    });
    if (sameShape) {
      const unbound = cells.some((c, i) => c.cellId !== visible[i].cell_id);
      if (unbound) await this.host.bindCells(cells.map((c, i) => ({ handle: c.handle, cellId: visible[i].cell_id })));
      return;
    }
    if (!force && cells.length === 0 && visible.length === 0) return;
    const byId = new Map(cells.filter((c) => c.cellId).map((c) => [c.cellId!, c]));
    await this.host.resetCells(visible.map((c) => {
      const mine = byId.get(c.cell_id);
      if (mine && (this.myLocks.has(c.cell_id) || this.pending.has(c.cell_id))) return { cellId: c.cell_id, view: mine.view };
      return { cellId: c.cell_id, view: this.viewFor(c, mine?.view) };
    }));
  }

  // ---------------------------------------------------------------- editor → server

  /** Call after any editor change (debounced here). */
  editorChanged(): void {
    if (this.disposed) return;
    if (this.syncTimer) clearTimeout(this.syncTimer);
    this.syncTimer = setTimeout(() => {
      this.syncTimer = null;
      void this.enqueue(() => this.syncFromHost());
    }, 30);
  }

  /** Diff editor against model and send creates, deletes, moves and text edits. */
  async syncFromHost(): Promise<void> {
    if (this.disposed) return;
    let cells = this.host.getCells();
    const hostIds = new Set(cells.map((c) => c.cellId).filter((x): x is string => !!x));

    // 0. Unbound editor cells that match a model cell missing from the editor (same type and
    //    text, in order) are the same cell: VS Code reloaded it from disk, or the user undid a
    //    delete. Bind them instead of sending a delete and a create.
    const missing = this.model.visibleCells().filter((c) => !hostIds.has(c.cell_id));
    const adopt: Array<{ handle: unknown; cellId: string }> = [];
    cells.forEach((hc, pos) => {
      if (hc.cellId) return;
      const type = effectiveType(hc.view, pos);
      const source = this.hostSource(cells, pos);
      const i = missing.findIndex((c) => c.source === source && (c.type === "preamble" ? "preamble" : canonicalType(c.type)) === type);
      if (i < 0) return;
      adopt.push({ handle: hc.handle, cellId: missing[i].cell_id });
      hostIds.add(missing[i].cell_id);
      missing.splice(i, 1);
    });
    if (adopt.length) {
      await this.host.bindCells(adopt);
      cells = this.host.getCells();
    }

    // Structural changes wait until they have been stable for `structureSettleMs`.
    const now = Date.now();
    const settle = this.o.structureSettleMs;
    let unsettled = false;
    const settled = (since: number) => {
      if (now - since >= settle) return true;
      unsettled = true;
      return false;
    };
    for (const id of [...this.firstMissing.keys()]) if (hostIds.has(id) || !this.model.byId(id)) this.firstMissing.delete(id);

    // 1. Deleted in the editor.
    for (const c of this.model.visibleCells()) {
      if (hostIds.has(c.cell_id)) continue;
      if (!this.firstMissing.has(c.cell_id)) this.firstMissing.set(c.cell_id, now);
      if (!settled(this.firstMissing.get(c.cell_id)!)) continue;
      this.firstMissing.delete(c.cell_id);
      const lock = this.model.lockedByOther(c.cell_id);
      if (lock) {
        await this.restore(c.cell_id);
        void this.host.notify("warning", `This cell is being edited by ${who(lock)}; it was not deleted.`);
        continue;
      }
      try {
        this.dropLocal(c.cell_id);
        await this.client.deleteCell(this.kernelId, c.cell_id, c.version);
        this.model.remove(c.cell_id);
      } catch (err) {
        await this.restore(c.cell_id);
        this.reportError("Delete", err);
      }
    }

    // 2. Created in the editor.
    cells = this.host.getCells();
    for (let pos = 0; pos < cells.length; pos++) {
      const hc = cells[pos];
      if (hc.cellId) continue;
      const key = hc.handle as object;
      if (!this.firstUnbound.has(key)) this.firstUnbound.set(key, now);
      if (!settled(this.firstUnbound.get(key)!)) continue;
      const type = effectiveType(hc.view, pos);
      const source = this.hostSource(cells, pos);
      const prev = pos > 0 ? cells[pos - 1].cellId : this.model.cells[0]?.type === "preamble" && type !== "preamble" ? this.model.cells[0].cell_id : undefined;
      const next = cells.slice(pos + 1).find((c) => c.cellId)?.cellId;
      const where = prev ? { after: prev } : next ? { before: next } : {};
      this.pendingCreates++;
      try {
        const created = await this.client.createCell(this.kernelId, {
          type: type === "preamble" ? "code" : type, source, metadata: hc.view.meta?.metadata ?? {}, ...where,
        });
        this.model.upsert(created);
        await this.host.bindCells([{ handle: hc.handle, cellId: created.cell_id }]);
      } catch (err) {
        this.reportError("Create cell", err);
      } finally {
        this.pendingCreates--;
      }
      cells = this.host.getCells();
    }

    if (unsettled && !this.settleTimer) {
      this.settleTimer = setTimeout(() => {
        this.settleTimer = null;
        void this.enqueue(() => this.syncFromHost());
      }, settle + 20);
    }

    // 3. Moved in the editor.
    cells = this.host.getCells();
    const order = cells.map((c) => c.cellId).filter((x): x is string => !!x && !!this.model.byId(x));
    const offset = this.model.cells.length > 0 && !this.model.isVisible(this.model.cells[0]) ? 1 : 0;
    for (let i = 0; i < order.length; i++) {
      const want = i + offset;
      if (this.model.idAt(want) === order[i]) continue;
      try {
        const moved = await this.client.moveCell(this.kernelId, order[i], want);
        this.model.upsert(moved);
        this.model.moveTo(order[i], want);
      } catch (err) {
        this.reportError("Move cell", err);
        await this.enqueueReconcileLater();
        break;
      }
    }

    // 4. Text and type edits.
    cells = this.host.getCells();
    for (let pos = 0; pos < cells.length; pos++) {
      const hc = cells[pos];
      if (!hc.cellId) continue;
      const c = this.model.byId(hc.cellId);
      if (!c) continue;
      const source = this.hostSource(cells, pos);
      const type = effectiveType(hc.view, pos);
      const modelType = c.type === "preamble" ? "preamble" : canonicalType(c.type);
      const p = this.pending.get(c.cell_id);
      if (source === c.source && type === modelType) {
        if (p) { clearTimeout(p.timer); this.pending.delete(c.cell_id); }
        continue;
      }
      if (p && p.source === source && p.type === type) continue;
      await this.localEdit(c.cell_id, source, type);
    }
  }

  private async enqueueReconcileLater(): Promise<void> {
    const snapshot = await this.client.getKernelDocument(this.kernelId).catch(() => null);
    if (snapshot) {
      this.model.load(snapshot);
      await this.reconcile(true);
    }
  }

  /** Put the model's version of a cell back into the editor. */
  private async restore(cellId: string): Promise<void> {
    const c = this.model.byId(cellId);
    if (!c) return;
    const cells = this.host.getCells();
    const pos = this.positionOf(cells, cellId);
    if (pos >= 0) {
      await this.host.replaceCell(pos, this.viewFor(c, cells[pos].view));
    } else if (this.model.isVisible(c)) {
      await this.host.insertCell(this.positionAfter(cells, this.model.prevId(cellId)), cellId, this.viewFor(c));
    }
  }

  private dropLocal(cellId: string): void {
    const p = this.pending.get(cellId);
    if (p?.timer) clearTimeout(p.timer);
    this.pending.delete(cellId);
    const t = this.idleTimers.get(cellId);
    if (t) clearTimeout(t);
    this.idleTimers.delete(cellId);
    this.myLocks.delete(cellId);
  }

  private async localEdit(cellId: string, source: string, type: string): Promise<void> {
    const lock = this.model.lockedByOther(cellId);
    if (lock) {
      await this.restore(cellId);
      void this.host.notify("warning", `This cell is being edited by ${who(lock)}. Your change was undone.`);
      return;
    }
    const prev = this.pending.get(cellId);
    if (prev?.timer) clearTimeout(prev.timer);
    const p: Pending = { source, type };
    this.pending.set(cellId, p);
    if (!(await this.ensureLock(cellId))) return;
    p.timer = setTimeout(() => void this.enqueue(() => this.flush(cellId)), this.o.editDebounceMs);
    this.touchIdle(cellId);
  }

  /** PUT lock unless held (FR-S3, 2025 `start_typing`). On `409 locked`, undo and say who has it. */
  private ensureLock(cellId: string): Promise<boolean> {
    if (this.myLocks.has(cellId)) return Promise.resolve(true);
    const inflight = this.locking.get(cellId);
    if (inflight) return inflight;
    const attempt = (async () => {
      try {
        const lock = await this.client.lockCell(this.kernelId, cellId);
        this.myLocks.add(cellId);
        this.model.setLock(cellId, lock);
        this.host.refreshState();
        return true;
      } catch (err) {
        this.dropLocal(cellId);
        if (err instanceof ApiError && err.code === "locked") {
          const holder = (err.data.locked_by ?? err.data.lock) as Lock | string | undefined;
          const lock = typeof holder === "object" ? holder : this.model.byId(cellId)?.lock;
          if (lock) this.model.setLock(cellId, lock);
          await this.restore(cellId);
          this.host.refreshState();
          void this.host.notify("warning", `This cell is being edited by ${who(lock ?? { user: typeof holder === "string" ? holder : undefined })}. Your change was undone.`);
        } else {
          await this.restore(cellId);
          this.reportError("Lock cell", err);
        }
        return false;
      } finally {
        this.locking.delete(cellId);
      }
    })();
    this.locking.set(cellId, attempt);
    return attempt;
  }

  private async lockAndFlushLater(cellId: string): Promise<void> {
    if (await this.ensureLock(cellId)) void this.enqueue(() => this.flush(cellId));
  }

  /** PATCH the pending text with `base_version` (FR-S2). */
  private async flush(cellId: string): Promise<void> {
    const p = this.pending.get(cellId);
    const c = this.model.byId(cellId);
    if (!p || !c) return;
    clearTimeout(p.timer);
    const modelType = c.type === "preamble" ? "preamble" : canonicalType(c.type);
    const edit: { source: string; base_version: number; type?: string } = { source: p.source, base_version: c.version };
    if (p.type !== modelType && p.type !== "preamble") edit.type = p.type;
    try {
      const updated = await this.client.updateCell(this.kernelId, cellId, edit);
      this.model.upsert(updated);
      if (this.pending.get(cellId) === p) this.pending.delete(cellId);
    } catch (err) {
      if (err instanceof ApiError && err.code === "conflict") {
        const current = err.data.cell as DocumentCell | undefined;
        if (current) this.model.upsert(current);
        void this.askConflict(cellId, current);
      } else if (err instanceof ApiError && err.code === "locked") {
        this.myLocks.delete(cellId);
        this.pending.delete(cellId);
        const lock = (typeof err.data.locked_by === "object" ? err.data.locked_by : undefined) as Lock | undefined;
        if (lock) this.model.setLock(cellId, lock);
        await this.restore(cellId);
        this.host.refreshState();
        void this.host.notify("warning", `This cell is now being edited by ${who(lock)}. Your change was undone.`);
      } else {
        this.reportError("Save cell", err);
      }
    }
  }

  private async askConflict(cellId: string, current: DocumentCell | undefined): Promise<void> {
    const pick = await this.host.notify(
      "warning",
      `This cell was changed by someone else while you were editing (now version ${current?.version ?? "?"}).`,
      "Keep mine", "Use theirs",
    );
    await this.enqueue(async () => {
      if (pick === "Keep mine" && this.pending.has(cellId)) {
        await this.flush(cellId);
      } else {
        this.dropLocal(cellId);
        await this.restore(cellId);
        if (this.myLocks.has(cellId)) await this.release(cellId);
      }
    });
  }

  private async onConflict(cellId: string, local: { source: string; version: number }, disk: { source: string }): Promise<void> {
    const pick = await this.host.notify(
      "warning",
      "The file changed on disk while this cell was being edited (doc.conflict). Which version should the cell keep?",
      "Keep editor version", "Use disk version",
    );
    if (!pick) return;
    await this.enqueue(async () => {
      const c = this.model.byId(cellId);
      if (!c) return;
      if (pick === "Use disk version") {
        c.source = disk.source;
        const cells = this.host.getCells();
        const pos = this.positionOf(cells, cellId);
        if (pos >= 0) await this.host.replaceCell(pos, this.viewFor(c, cells[pos].view));
      }
      const cells = this.host.getCells();
      const pos = this.positionOf(cells, cellId);
      const source = pos >= 0 ? this.hostSource(cells, pos) : local.source;
      try {
        if (!(await this.ensureLock(cellId))) return;
        const updated = await this.client.updateCell(this.kernelId, cellId, { source, base_version: c.version });
        this.model.upsert(updated);
      } catch (err) {
        this.reportError("Resolve conflict", err);
      }
    });
  }

  private touchIdle(cellId: string): void {
    const prev = this.idleTimers.get(cellId);
    if (prev) clearTimeout(prev);
    this.idleTimers.set(cellId, setTimeout(() => void this.enqueue(() => this.release(cellId)), this.o.lockIdleMs));
  }

  private async flushAll(): Promise<void> {
    for (const id of [...this.pending.keys()]) await this.flush(id);
  }

  /** DELETE lock with the final source (2025 `cell_unlocked_with_code`). */
  async release(cellId: string): Promise<void> {
    if (!this.myLocks.has(cellId)) return;
    if (this.pending.has(cellId)) await this.flush(cellId);
    const t = this.idleTimers.get(cellId);
    if (t) clearTimeout(t);
    this.idleTimers.delete(cellId);
    const c = this.model.byId(cellId);
    const cells = this.host.getCells();
    const pos = this.positionOf(cells, cellId);
    this.myLocks.delete(cellId);
    try {
      const final = c && pos >= 0 ? { source: this.hostSource(cells, pos), base_version: c.version } : undefined;
      const cell = await this.client.unlockCell(this.kernelId, cellId, final);
      if (cell && typeof cell === "object" && "cell_id" in cell) this.model.upsert({ ...cell, lock: null });
      else this.model.setLock(cellId, null);
    } catch (err) {
      if (err instanceof ApiError && err.code === "conflict") {
        const current = err.data.cell as DocumentCell | undefined;
        if (current) this.model.upsert(current);
        await this.restore(cellId);
      } else if (!(err instanceof ApiError && err.status === 404)) {
        this.reportError("Release lock", err);
      }
      this.model.setLock(cellId, null);
    }
    this.host.refreshState();
  }

  private async renewLocks(): Promise<void> {
    for (const id of [...this.myLocks]) {
      try {
        const lock = await this.client.lockCell(this.kernelId, id);
        this.model.setLock(id, lock);
      } catch {
        this.myLocks.delete(id);
      }
    }
  }

  // ---------------------------------------------------------------- focus and presence

  /** The user moved to another cell (or left the notebook): release locks elsewhere, report focus. */
  focusChanged(cellId: string | null, cursor: Cursor | null = null): void {
    if (this.disposed) return;
    if (cellId !== this.focusedCellId) {
      for (const id of [...this.myLocks]) if (id !== cellId) void this.enqueue(() => this.release(id));
    }
    this.focusedCellId = cellId;
    this.reportPresence({ focused_cell_id: cellId, cursor });
  }

  /** Throttled PUT presence (FR-S4); the last value always goes out. */
  reportPresence(p: { focused_cell_id: string | null; cursor: Cursor | null }): void {
    this.presenceNext = p;
    if (this.presenceTimer) return;
    const wait = Math.max(0, this.presenceLast + this.o.presenceThrottleMs - Date.now());
    this.presenceTimer = setTimeout(() => {
      this.presenceTimer = null;
      const next = this.presenceNext;
      this.presenceNext = null;
      if (!next || this.disposed) return;
      this.presenceLast = Date.now();
      this.client.updatePresence(this.kernelId, next).catch((err) => this.o.log?.(`presence: ${(err as Error).message}`));
    }, wait);
  }

  /** Flush pending edits now (before a run). */
  async flushNow(): Promise<void> {
    if (this.syncTimer) {
      clearTimeout(this.syncTimer);
      this.syncTimer = null;
    }
    await this.enqueue(() => this.syncFromHost());
    await this.enqueue(() => this.flushAll());
  }

  private reportError(what: string, err: unknown): void {
    const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : String((err as Error)?.message ?? err);
    this.o.log?.(`${what} failed: ${msg}`);
    void this.host.notify("error", `DarkPyonix: ${what} failed (${msg}).`);
  }
}
