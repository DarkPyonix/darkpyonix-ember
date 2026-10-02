// HTTP client for the DarkPyonix Kernel Manager API (manager.openapi.yaml 1.0.0-draft.3).
import { EventStream, type EventStreamOptions } from "./sse";
import type {
  ApiErrorBody, Document, DocumentCell, Kernel, KernelEvent, Lock, Presence, RunAccepted, RunRequest, Cursor,
} from "./types";

export interface ClientIdentity {
  /** Stable per-device id (FR-S4), `^[A-Za-z0-9_-]{8,64}$`. */
  clientId: string;
  /** Device name shown to other clients. */
  nickname: string;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly data: Record<string, any> = {},
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export interface CellEdit {
  type?: string;
  source?: string;
  metadata?: Record<string, unknown>;
}

const API = "/api/v1";

export class ManagerClient {
  readonly base: string;

  constructor(base: string, private readonly token: string | undefined, readonly identity: ClientIdentity) {
    this.base = base.replace(/\/+$/, "");
  }

  private headers(json: boolean): Record<string, string> {
    const h: Record<string, string> = {
      "X-DarkPyonix-Client": this.identity.clientId,
      // Header values must be ByteStrings; nicknames may be non-ASCII.
      "X-DarkPyonix-Nickname": encodeHeader(this.identity.nickname),
    };
    if (this.token) h.Authorization = `Bearer ${this.token}`;
    if (json) h["Content-Type"] = "application/json";
    return h;
  }

  async request<T>(method: string, path: string, body?: unknown, timeoutMs = 30000): Promise<T> {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), timeoutMs);
    let res: Response;
    try {
      res = await fetch(this.base + path, {
        method,
        headers: this.headers(body !== undefined),
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: ctl.signal,
      });
    } catch (err) {
      throw new ApiError(0, "unreachable", `${method} ${path}: ${(err as Error).message}`);
    } finally {
      clearTimeout(timer);
    }
    const text = await res.text();
    let parsed: unknown = undefined;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        parsed = undefined;
      }
    }
    if (!res.ok) {
      const e = (parsed as ApiErrorBody | undefined)?.error;
      throw new ApiError(res.status, e?.code ?? `http_${res.status}`, e?.message ?? `${method} ${path}: HTTP ${res.status}`, e?.data ?? {});
    }
    return parsed as T;
  }

  health(): Promise<{ status: string; version: string }> {
    return this.request("GET", "/health", undefined, 3000);
  }
  manager(): Promise<{ version: string; mode: string; pid: number; permission: string }> {
    return this.request("GET", `${API}/manager`);
  }
  async listKernels(): Promise<Kernel[]> {
    return (await this.request<{ kernels: Kernel[] }>("GET", `${API}/kernels`)).kernels;
  }
  startKernel(req: { path: string; python?: string; cwd?: string; env?: Record<string, string> }): Promise<Kernel> {
    return this.request("POST", `${API}/kernels`, req, 60000);
  }
  getKernel(id: string): Promise<Kernel> {
    return this.request("GET", `${API}/kernels/${id}`);
  }
  /** Graceful shutdown. This client never sends `force=true` (stopping is interrupting). */
  shutdownKernel(id: string): Promise<{ shutting_down: boolean }> {
    return this.request("DELETE", `${API}/kernels/${id}`);
  }
  interrupt(id: string): Promise<{ interrupted: boolean; run_id?: string }> {
    return this.request("POST", `${API}/kernels/${id}/interrupt`);
  }
  restart(id: string, hard = false): Promise<Kernel> {
    return this.request("POST", `${API}/kernels/${id}/restart`, { hard }, 60000);
  }
  getKernelDocument(id: string): Promise<Document> {
    return this.request("GET", `${API}/kernels/${id}/document`);
  }
  getDocumentByPath(path: string): Promise<Document> {
    return this.request("GET", `${API}/documents?path=${encodeURIComponent(path)}`);
  }
  startRun(id: string, req: RunRequest): Promise<RunAccepted> {
    return this.request("POST", `${API}/kernels/${id}/runs`, req);
  }
  createCell(id: string, edit: CellEdit & { after?: string; before?: string }): Promise<DocumentCell> {
    return this.request("POST", `${API}/kernels/${id}/cells`, edit);
  }
  updateCell(id: string, cellId: string, edit: CellEdit & { base_version: number }): Promise<DocumentCell> {
    return this.request("PATCH", `${API}/kernels/${id}/cells/${encodeURIComponent(cellId)}`, edit);
  }
  async deleteCell(id: string, cellId: string, baseVersion: number): Promise<void> {
    await this.request("DELETE", `${API}/kernels/${id}/cells/${encodeURIComponent(cellId)}?base_version=${baseVersion}`);
  }
  moveCell(id: string, cellId: string, toIndex: number): Promise<DocumentCell> {
    return this.request("POST", `${API}/kernels/${id}/cells/${encodeURIComponent(cellId)}/move`, { to_index: toIndex });
  }
  lockCell(id: string, cellId: string): Promise<Lock> {
    return this.request("PUT", `${API}/kernels/${id}/cells/${encodeURIComponent(cellId)}/lock`);
  }
  unlockCell(id: string, cellId: string, final?: { source: string; base_version: number }): Promise<DocumentCell> {
    return this.request("DELETE", `${API}/kernels/${id}/cells/${encodeURIComponent(cellId)}/lock`, final);
  }
  async updatePresence(id: string, p: { focused_cell_id: string | null; cursor: Cursor | null }): Promise<Presence[]> {
    return (await this.request<{ presence: Presence[] }>("PUT", `${API}/kernels/${id}/presence`, p)).presence;
  }
  async leavePresence(id: string): Promise<void> {
    await this.request("DELETE", `${API}/kernels/${id}/presence`, undefined, 3000);
  }

  /** Subscribe to kernel events; joins presence with this client's id and nickname (FR-S4). */
  events(
    id: string, since: number | null, onEvent: (ev: KernelEvent) => void,
    onState?: EventStreamOptions["onState"],
  ): EventStream {
    const q = (s: number | null) => {
      const p = new URLSearchParams({ client_id: this.identity.clientId, nickname: this.identity.nickname });
      if (s !== null) p.set("since", String(s));
      return `${this.base}${API}/kernels/${id}/events?${p.toString()}`;
    };
    return new EventStream({ url: q, headers: this.headers(false), since, onEvent, onState });
  }
}

/** Percent-encode non-ASCII so the value is a valid HTTP header (servers may decode it). */
export function encodeHeader(v: string): string {
  return /^[\x20-\x7e]*$/.test(v) ? v : encodeURIComponent(v);
}
