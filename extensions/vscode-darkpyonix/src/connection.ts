// One open notebook ↔ its file-bound kernel. Editor-agnostic; the VS Code layer supplies the
// host (cells), the execution sink (outputs) and a way to show snapshot outputs.
import { viewToSource } from "./format/cells";
import { sourceSha256 } from "./format/parser";
import { CollabSession, type NotebookHost, type SessionOptions } from "./collab/session";
import { ApiError, type ManagerClient } from "./manager/client";
import type { Document, DocumentCell, Kernel, NbOutput } from "./manager/types";
import { RunTracker, type ExecSink } from "./run/tracker";

export interface OutputsHost {
  /** Show stored outputs on an editor cell (by position): FR-R4 latest outputs on open. */
  showOutputs(position: number, cell: DocumentCell): void;
  kernelChanged(kernel: Kernel | null, state: string): void;
}

export interface ConnectionOptions extends SessionOptions {
  python?: string;
}

export class NotebookConnection {
  kernel: Kernel | null = null;
  session: CollabSession | null = null;
  tracker: RunTracker | null = null;
  private attaching: Promise<void> | null = null;
  private disposed = false;

  constructor(
    public client: ManagerClient,
    /** The file's path on the manager's machine. */
    readonly path: string,
    readonly host: NotebookHost & OutputsHost,
    readonly sink: ExecSink,
    readonly opts: ConnectionOptions = {},
  ) {}

  get me(): string {
    return this.client.identity.clientId;
  }

  /** On open: attach to a running kernel (or start one when `autoStart`), else show stored outputs. */
  async open(autoStart: boolean): Promise<void> {
    const running = await this.findKernel();
    if (running || autoStart) {
      await this.ensureAttached(running ?? undefined);
      return;
    }
    try {
      const doc = await this.client.getDocumentByPath(this.path);
      this.showSnapshotOutputs(doc, false);
    } catch (err) {
      if (!(err instanceof ApiError && (err.status === 404 || err.status === 403))) throw err;
    }
    this.host.kernelChanged(null, "no kernel");
  }

  async findKernel(): Promise<Kernel | null> {
    const kernels = await this.client.listKernels();
    return kernels.find((k) => k.path === this.path) ?? null;
  }

  /** Start (idempotent, FR-M2) or attach to the file's kernel and join its document. */
  ensureAttached(existing?: Kernel): Promise<void> {
    if (this.session) return Promise.resolve();
    if (!this.attaching) {
      this.attaching = (async () => {
        this.host.kernelChanged(null, "starting");
        const kernel = existing ?? (await this.client.startKernel({ path: this.path, ...(this.opts.python ? { python: this.opts.python } : {}) }));
        if (this.disposed) return;
        this.kernel = kernel;
        const tracker = new RunTracker(this.me, (index) => this.session?.model.idAt(index), this.sink);
        const session = new CollabSession(this.client, kernel.kernel_id, this.host, {
          ...this.opts,
          onRunEvent: (ev) => {
            tracker.handle(ev);
            if (ev.type === "kernel.status" && this.kernel) {
              this.kernel = { ...this.kernel, status: ev.data.status };
              this.host.kernelChanged(this.kernel, ev.data.status);
            }
          },
        });
        this.tracker = tracker;
        this.session = session;
        const doc = await session.start();
        this.showSnapshotOutputs(doc, true);
        this.host.kernelChanged(kernel, kernel.status);
      })().finally(() => {
        this.attaching = null;
      });
    }
    return this.attaching;
  }

  /** FR-R4: put each cell's latest outputs (with stale marks) on the matching editor cell. */
  showSnapshotOutputs(doc: Document, bound: boolean): void {
    const cells = this.host.getCells();
    if (bound) {
      cells.forEach((hc, pos) => {
        const c = hc.cellId ? this.session?.model.byId(hc.cellId) : undefined;
        const snap = c && doc.cells.find((d) => d.cell_id === c.cell_id);
        if (snap && (snap.outputs?.length || snap.execution_count != null || snap.stale)) this.host.showOutputs(pos, snap);
      });
      return;
    }
    // Without a kernel: cells by index (preamble 0; hidden when empty).
    const hidden = doc.cells.length > 0 && doc.cells[0].index === 0 && doc.cells[0].type === "preamble" && doc.cells[0].source === "" ? 1 : 0;
    for (const snap of doc.cells) {
      const pos = snap.index - hidden;
      if (pos < 0 || pos >= cells.length) continue;
      if (!(snap.outputs?.length || snap.execution_count != null || snap.stale)) continue;
      // The editor may hold unsaved text: mark stale when it no longer matches.
      const editorSha = sourceSha256(viewToSource(cells[pos].view, pos, pos === cells.length - 1));
      this.host.showOutputs(pos, { ...snap, stale: !!snap.stale || editorSha !== snap.source_sha256 });
    }
  }

  /** Run cells by editor position (FR-S6: by cell_id; this client is `started_by`). */
  async run(positions: number[]): Promise<string> {
    await this.ensureAttached();
    const session = this.session!;
    await session.flushNow();
    const cells = this.host.getCells();
    const ids = positions.map((p) => cells[p]?.cellId).filter((x): x is string => !!x);
    if (ids.length === 0) throw new Error("These cells are not in the kernel's document yet.");
    const accepted = await this.client.startRun(this.kernel!.kernel_id, { mode: "cells", cell_ids: ids, on_busy: "queue" });
    this.tracker!.expect(accepted.run_id, ids, { client_id: this.me, nickname: this.client.identity.nickname });
    return accepted.run_id;
  }

  async runAll(): Promise<string> {
    await this.ensureAttached();
    await this.session!.flushNow();
    const accepted = await this.client.startRun(this.kernel!.kernel_id, { mode: "all", on_busy: "queue" });
    return accepted.run_id;
  }

  /** "Stop" is interrupt: KeyboardInterrupt in the running cell, namespace kept (FR-X4). */
  async interrupt(): Promise<boolean> {
    if (!this.kernel) return false;
    return (await this.client.interrupt(this.kernel.kernel_id)).interrupted;
  }

  async restart(hard = false): Promise<void> {
    if (!this.kernel) return;
    this.kernel = await this.client.restart(this.kernel.kernel_id, hard);
    this.host.kernelChanged(this.kernel, this.kernel.status);
  }

  /** Graceful shutdown (never `force`). */
  async shutdown(): Promise<void> {
    if (!this.kernel) return;
    await this.client.shutdownKernel(this.kernel.kernel_id);
    await this.detach();
    this.host.kernelChanged(null, "shut down");
  }

  async detach(): Promise<void> {
    this.tracker?.abandonAll();
    const s = this.session;
    this.session = null;
    this.tracker = null;
    this.kernel = null;
    await s?.dispose();
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    await this.detach();
  }
}

export type { NbOutput };
