// A tiny in-process stand-in for the DarkPyonix manager + kernel: the endpoints this extension
// uses (manager.openapi.yaml 1.0.0-draft.3) with document state, locks, presence, runs and SSE.
// Behaviour follows SPEC FR-S1..S7 closely enough to exercise the client; it is not a reference.
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { parse, sourceSha256 } from "../../src/format/parser";
import type { DocumentCell, Lock, NbOutput, Presence } from "../../src/manager/types";

export interface Client {
  client_id: string;
  nickname: string;
  user: string;
}

export interface Recorded {
  method: string;
  path: string;
  client?: string;
  body?: any;
}

export class FakeManager {
  readonly token = "test-token";
  readonly kernelId = "k_0123456789abcdef0123";
  server!: Server;
  url = "";
  path: string;
  cells: DocumentCell[] = [];
  docVersion = 1;
  seq = 0;
  events: Array<{ seq: number; type: string; data: any }> = [];
  presence = new Map<string, Presence>();
  requests: Recorded[] = [];
  kernelStarted = false;
  /** Outputs to attach to the snapshot by cell index (FR-R4). */
  snapshotOutputs = new Map<number, { outputs: NbOutput[]; execution_count: number; stale?: boolean }>();
  /** What a run prints per cell. */
  runOutput: (cell: DocumentCell) => NbOutput[] = (c) => [{ output_type: "stream", name: "stdout", text: `ran ${c.cell_id}\n` }];
  private streams = new Set<ServerResponse>();
  private nextId = 100;
  private runCounter = 0;
  /** Hold runs until released (to test interrupt / in-flight state). */
  holdRuns = false;
  private held: Array<() => void> = [];

  constructor(text: string, path = "/work/nb.pynb") {
    this.path = path;
    const doc = parse(text);
    this.cells = doc.cells.map((c, i) => ({
      cell_id: c.id ?? `c_${i.toString(16).padStart(4, "0")}`,
      index: i,
      type: c.rawType ?? c.type,
      title: c.title,
      source: c.source,
      source_sha256: c.sourceSha256,
      version: 1,
      metadata: c.metadata,
      outputs: [],
      execution_count: null,
      lock: null,
    }));
  }

  async start(): Promise<void> {
    this.server = createServer((req, res) => void this.handle(req, res));
    await new Promise<void>((r) => this.server.listen(0, "127.0.0.1", r));
    this.url = `http://127.0.0.1:${(this.server.address() as AddressInfo).port}`;
  }

  async stop(): Promise<void> {
    for (const s of this.streams) s.destroy();
    this.server.closeAllConnections?.();
    await new Promise<void>((r) => this.server.close(() => r()));
  }

  // ------------------------------------------------------------ helpers for tests

  emit(type: string, data: any): void {
    const ev = { seq: ++this.seq, type, data };
    this.events.push(ev);
    for (const s of this.streams) s.write(`id: ${ev.seq}\nevent: ${type}\ndata: ${JSON.stringify(data)}\n\n`);
  }

  byId(id: string): DocumentCell | undefined {
    return this.cells.find((c) => c.cell_id === id);
  }

  private reindex(): void {
    this.cells.forEach((c, i) => { c.index = i; });
  }

  private pub(c: DocumentCell): DocumentCell {
    return { ...c, outputs: undefined } as DocumentCell;
  }

  /** Another client edits a cell (as if through its own HTTP call). */
  editAs(by: Client, cellId: string, source: string): DocumentCell {
    const c = this.byId(cellId)!;
    c.source = source;
    c.source_sha256 = sourceSha256(source);
    c.version++;
    this.docVersion++;
    this.emit("doc.cell.updated", { doc_version: this.docVersion, cell: this.pub(c), by });
    return c;
  }

  lockAs(by: Client, cellId: string): Lock {
    const now = new Date().toISOString();
    const lock: Lock = { cell_id: cellId, locked_by: by.client_id, user: by.user, nickname: by.nickname, locked_at: now, last_activity: now };
    this.byId(cellId)!.lock = lock;
    this.emit("doc.lock", { doc_version: this.docVersion, cell_id: cellId, lock, by });
    return lock;
  }

  unlockAs(by: Client, cellId: string, reason = "released"): void {
    this.byId(cellId)!.lock = null;
    this.emit("doc.unlock", { doc_version: this.docVersion, cell_id: cellId, by, reason });
  }

  createAs(by: Client, after: string, type: string, source: string): DocumentCell {
    const i = this.cells.findIndex((c) => c.cell_id === after);
    const cell: DocumentCell = {
      cell_id: `c_${(this.nextId++).toString(16)}`, index: i + 1, type, title: null, source,
      source_sha256: sourceSha256(source), version: 1, metadata: {}, lock: null,
    };
    this.cells.splice(i + 1, 0, cell);
    this.reindex();
    this.docVersion++;
    this.emit("doc.cell.created", { doc_version: this.docVersion, cell: this.pub(cell), by });
    return cell;
  }

  deleteAs(by: Client, cellId: string): void {
    this.cells = this.cells.filter((c) => c.cell_id !== cellId);
    this.reindex();
    this.docVersion++;
    this.emit("doc.cell.deleted", { doc_version: this.docVersion, cell_id: cellId, by });
  }

  moveAs(by: Client, cellId: string, to: number): void {
    const i = this.cells.findIndex((c) => c.cell_id === cellId);
    const [c] = this.cells.splice(i, 1);
    this.cells.splice(to, 0, c);
    this.reindex();
    c.version++;
    this.docVersion++;
    this.emit("doc.cell.moved", { doc_version: this.docVersion, cell: this.pub(c), by });
  }

  /** The file changed on disk (FR-S5). */
  reloadFromText(text: string): void {
    const doc = parse(text);
    const old = this.cells;
    this.cells = doc.cells.map((c, i) => {
      const match = old.find((o) => o.source_sha256 === c.sourceSha256) ?? old[i];
      return {
        cell_id: match?.cell_id ?? `c_${(this.nextId++).toString(16)}`, index: i, type: c.rawType ?? c.type,
        title: c.title, source: c.source, source_sha256: c.sourceSha256,
        version: (match?.version ?? 0) + (match && match.source === c.source ? 0 : 1), metadata: c.metadata, lock: match?.lock ?? null,
      };
    });
    this.docVersion++;
    this.emit("doc.reloaded", { doc_version: this.docVersion, cells: this.cells.map((c) => this.pub(c)), cause: "external" });
  }

  releaseRuns(): void {
    const h = this.held;
    this.held = [];
    for (const f of h) f();
  }

  // ------------------------------------------------------------ HTTP

  private async body(req: IncomingMessage): Promise<any> {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const text = Buffer.concat(chunks).toString("utf8");
    return text ? JSON.parse(text) : undefined;
  }

  private send(res: ServerResponse, status: number, body?: unknown): void {
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(body === undefined ? "" : JSON.stringify(body));
  }

  private error(res: ServerResponse, status: number, code: string, message: string, data?: any): void {
    this.send(res, status, { error: { code, message, ...(data ? { data } : {}) } });
  }

  private client(req: IncomingMessage, url: URL): Client {
    const id = (req.headers["x-darkpyonix-client"] as string) ?? url.searchParams.get("client_id") ?? "anon";
    const nick = decodeURIComponent((req.headers["x-darkpyonix-nickname"] as string) ?? url.searchParams.get("nickname") ?? "");
    return { client_id: id, nickname: nick, user: "me" };
  }

  private kernel() {
    return {
      kernel_id: this.kernelId, path: this.path, pid: process.pid, status: "idle",
      python: { version: "3.12.0", implementation: "CPython", executable: "/usr/bin/python3" },
      started_at: new Date().toISOString(),
    };
  }

  private snapshot() {
    return {
      path: this.path, kernel_id: this.kernelId, doc_version: this.docVersion, seq: this.seq + 1,
      presence: [...this.presence.values()],
      cells: this.cells.map((c) => {
        const o = this.snapshotOutputs.get(c.index);
        return { ...c, outputs: o?.outputs ?? [], execution_count: o?.execution_count ?? null, stale: o?.stale ?? false };
      }),
    };
  }

  private async handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = new URL(req.url!, this.url);
    const p = url.pathname;
    const method = req.method!;
    const body = method === "GET" ? undefined : await this.body(req).catch(() => undefined);
    const who = this.client(req, url);
    this.requests.push({ method, path: p + url.search, client: who.client_id, body });

    if (p === "/health") return this.send(res, 200, { status: "ok", version: "0.1.0-fake" });
    const auth = req.headers.authorization === `Bearer ${this.token}` || url.searchParams.get("token") === this.token;
    if (!auth) return this.error(res, 401, "unauthorized", "token required");

    const K = `/api/kernels/${this.kernelId}`;
    if (p === "/api/manager") return this.send(res, 200, { version: "0.1.0-fake", mode: "ephemeral", pid: process.pid, started_at: new Date().toISOString(), permission: "admin" });
    if (p === "/api/kernels" && method === "GET") return this.send(res, 200, { kernels: this.kernelStarted ? [this.kernel()] : [] });
    if (p === "/api/kernels" && method === "POST") {
      if (body?.path !== this.path) return this.error(res, 400, "bad_request", "unknown file");
      const existed = this.kernelStarted;
      this.kernelStarted = true;
      return this.send(res, existed ? 200 : 201, this.kernel());
    }
    if (p === "/api/documents" && method === "GET") {
      if (url.searchParams.get("path") !== this.path) return this.error(res, 404, "not_found", "no such file");
      return this.send(res, 200, this.snapshot());
    }
    if (!p.startsWith(K) || !this.kernelStarted) return this.error(res, 404, "not_found", "no such kernel");
    const rest = p.slice(K.length);

    if (rest === "" && method === "GET") return this.send(res, 200, this.kernel());
    if (rest === "" && method === "DELETE") {
      if (url.searchParams.get("force") === "true") return this.error(res, 400, "bad_request", "tests never force");
      this.kernelStarted = false;
      return this.send(res, 202, { shutting_down: true });
    }
    if (rest === "/document" && method === "GET") return this.send(res, 200, this.snapshot());
    if (rest === "/interrupt" && method === "POST") {
      const running = this.held.length > 0;
      this.releaseRuns();
      return this.send(res, 200, { interrupted: running });
    }
    if (rest === "/restart" && method === "POST") return this.send(res, 200, this.kernel());
    if (rest === "/events" && method === "GET") return this.sse(req, res, url, who);
    if (rest === "/presence" && method === "PUT") {
      const prev = this.presence.get(who.client_id);
      const pr: Presence = { ...(prev ?? { ...who, permission: "admin", last_seen: "" }), ...body, focused_at: new Date().toISOString(), last_seen: new Date().toISOString() };
      this.presence.set(who.client_id, pr);
      this.emit("presence.update", pr);
      return this.send(res, 200, { presence: [...this.presence.values()] });
    }
    if (rest === "/presence" && method === "DELETE") {
      this.presence.delete(who.client_id);
      this.emit("presence.leave", { ...who });
      res.writeHead(204).end();
      return;
    }
    if (rest === "/runs" && method === "POST") return this.run(res, body, who);
    if (rest === "/cells" && method === "POST") {
      const at = body.after ? this.cells.findIndex((c) => c.cell_id === body.after) + 1
        : body.before ? this.cells.findIndex((c) => c.cell_id === body.before) : this.cells.length;
      const cell: DocumentCell = {
        cell_id: `c_${(this.nextId++).toString(16)}`, index: at, type: body.type ?? "code", title: null,
        source: body.source ?? "", source_sha256: sourceSha256(body.source ?? ""), version: 1, metadata: body.metadata ?? {}, lock: null,
      };
      this.cells.splice(at, 0, cell);
      this.reindex();
      this.docVersion++;
      this.emit("doc.cell.created", { doc_version: this.docVersion, cell: this.pub(cell), by: who });
      return this.send(res, 201, this.pub(cell));
    }
    const m = /^\/cells\/([^/]+)(\/lock|\/move)?$/.exec(rest);
    if (m) {
      const id = decodeURIComponent(m[1]);
      const c = this.byId(id);
      if (!c) return this.error(res, 404, "not_found", "no such cell");
      const lockedByOther = c.lock && c.lock.locked_by !== who.client_id;
      if (!m[2] && method === "PATCH") {
        if (lockedByOther) return this.error(res, 409, "locked", "locked", { locked_by: c.lock });
        if (body.base_version !== c.version) return this.error(res, 409, "conflict", "stale base_version", { cell: this.pub(c) });
        if (body.source !== undefined) { c.source = body.source; c.source_sha256 = sourceSha256(body.source); }
        if (body.type !== undefined) c.type = body.type;
        c.version++;
        if (c.lock) c.lock.last_activity = new Date().toISOString();
        this.docVersion++;
        this.emit("doc.cell.updated", { doc_version: this.docVersion, cell: this.pub(c), by: who });
        return this.send(res, 200, this.pub(c));
      }
      if (!m[2] && method === "DELETE") {
        if (lockedByOther) return this.error(res, 409, "locked", "locked", { locked_by: c.lock });
        if (Number(url.searchParams.get("base_version")) !== c.version) return this.error(res, 409, "conflict", "stale", { cell: this.pub(c) });
        this.cells = this.cells.filter((x) => x !== c);
        this.reindex();
        this.docVersion++;
        this.emit("doc.cell.deleted", { doc_version: this.docVersion, cell_id: id, by: who });
        res.writeHead(204).end();
        return;
      }
      if (m[2] === "/move" && method === "POST") {
        this.cells = this.cells.filter((x) => x !== c);
        this.cells.splice(body.to_index, 0, c);
        this.reindex();
        c.version++;
        this.docVersion++;
        this.emit("doc.cell.moved", { doc_version: this.docVersion, cell: this.pub(c), by: who });
        return this.send(res, 200, this.pub(c));
      }
      if (m[2] === "/lock" && method === "PUT") {
        if (lockedByOther) return this.error(res, 409, "locked", "locked", { locked_by: c.lock });
        const now = new Date().toISOString();
        const fresh = !c.lock;
        c.lock = { cell_id: id, locked_by: who.client_id, user: who.user, nickname: who.nickname, locked_at: c.lock?.locked_at ?? now, last_activity: now };
        if (fresh) this.emit("doc.lock", { doc_version: this.docVersion, cell_id: id, lock: c.lock, by: who });
        return this.send(res, 200, c.lock);
      }
      if (m[2] === "/lock" && method === "DELETE") {
        if (lockedByOther) return this.error(res, 409, "locked", "locked", { locked_by: c.lock });
        if (body?.source !== undefined && body.source !== c.source) {
          if (body.base_version !== c.version) return this.error(res, 409, "conflict", "stale", { cell: this.pub(c) });
          c.source = body.source;
          c.source_sha256 = sourceSha256(body.source);
          c.version++;
          this.docVersion++;
          this.emit("doc.cell.updated", { doc_version: this.docVersion, cell: this.pub(c), by: who });
        }
        c.lock = null;
        this.emit("doc.unlock", { doc_version: this.docVersion, cell_id: id, by: who, reason: "released" });
        return this.send(res, 200, this.pub(c));
      }
    }
    return this.error(res, 404, "not_found", `no route ${method} ${p}`);
  }

  private sse(req: IncomingMessage, res: ServerResponse, url: URL, who: Client): void {
    res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache", Connection: "keep-alive" });
    const since = Number(req.headers["last-event-id"] ?? url.searchParams.get("since") ?? this.seq);
    for (const ev of this.events) if (ev.seq > since) res.write(`id: ${ev.seq}\nevent: ${ev.type}\ndata: ${JSON.stringify(ev.data)}\n\n`);
    res.write(": open\n\n");
    this.streams.add(res);
    if (url.searchParams.get("client_id") && !this.presence.has(who.client_id)) {
      const pr: Presence = { ...who, permission: "admin", last_seen: new Date().toISOString() };
      this.presence.set(who.client_id, pr);
      this.emit("presence.update", pr);
    }
    req.on("close", () => this.streams.delete(res));
  }

  /** A run started by `who` over `cell_ids` (or `all`), streamed as events. */
  private run(res: ServerResponse, body: any, who: Client): void {
    const ids: string[] = body.mode === "all" ? this.cells.map((c) => c.cell_id) : body.cell_ids ?? body.cells.map((i: number) => this.cells[i].cell_id);
    const runId = `20261003-142233-${(this.runCounter++).toString(16).padStart(4, "0")}`;
    this.send(res, 202, { run_id: runId, state: "running" });
    this.runCells(runId, ids, who);
  }

  /** Also used by tests to simulate another client's run. */
  runCells(runId: string, ids: string[], who: Client): void {
    const started_by = who;
    const go = () => {
      const indexes = ids.map((id) => this.byId(id)!.index);
      this.emit("run.started", { run_id: runId, mode: "cells", cells: indexes, params: {}, started_by });
      let n = 0;
      for (const id of ids) {
        const c = this.byId(id)!;
        this.emit("cell.started", { run_id: runId, index: c.index, execution_count: ++n, started_by });
        for (const o of this.runOutput(c)) this.emit("output", { run_id: runId, index: c.index, output: o });
        this.emit("cell.finished", { run_id: runId, index: c.index, status: "ok", duration: 0.01, started_by });
      }
      this.emit("run.finished", { run_id: runId, status: "ok", duration: 0.02, started_by });
    };
    setTimeout(() => (this.holdRuns ? this.held.push(go) : go()), 10);
  }
}
