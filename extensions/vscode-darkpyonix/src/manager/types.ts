// Shapes of the DarkPyonix Kernel Manager API (darkpyonix-core docs/api/manager.openapi.yaml,
// version 1.0.0-draft.3) used by this extension. Fields the extension does not read are omitted.

export type Permission = "admin" | "editor" | "viewer3" | "viewer2" | "viewer1";

export interface Kernel {
  kernel_id: string;
  path: string;
  pid: number;
  status: "starting" | "idle" | "busy" | "stopping";
  run_id?: string | null;
  queue?: string[];
  execution_count?: number;
  python: { version: string; implementation: string; executable: string };
  started_at: string;
  host?: string;
}

/** nbformat 4 output object. */
export interface NbOutput {
  output_type: "stream" | "display_data" | "execute_result" | "error";
  name?: string;
  text?: string | string[];
  data?: Record<string, unknown>;
  metadata?: Record<string, unknown>;
  execution_count?: number | null;
  ename?: string;
  evalue?: string;
  traceback?: string[];
  [k: string]: unknown;
}

export interface Lock {
  cell_id: string;
  locked_by: string;
  user: string;
  nickname?: string;
  locked_at: string;
  last_activity: string;
  expires_at?: string;
}

export interface Cursor {
  cell_id: string;
  line: number;
  column: number;
  selection?: [[number, number], [number, number]] | null;
}

export interface Presence {
  client_id: string;
  nickname: string;
  user: string;
  avatar?: string | null;
  permission?: Permission;
  focused_cell_id?: string | null;
  focused_at?: string | null;
  cursor?: Cursor | null;
  last_seen?: string;
}

export interface DocumentCell {
  cell_id: string;
  index: number;
  type: string;
  title?: string | null;
  source: string;
  source_sha256: string;
  version: number;
  metadata: Record<string, unknown>;
  outputs?: NbOutput[];
  execution_count?: number | null;
  status?: "ok" | "error" | "interrupted" | null;
  stale?: boolean;
  run_id?: string | null;
  lock?: Lock | null;
  conflict?: { disk_source?: string; [k: string]: unknown } | null;
}

export interface RunSummary {
  run_id: string;
  status: string;
  mode: "all" | "cells";
  started_at: string;
  started_by?: Attribution | null;
  interrupted_by?: Attribution | null;
}

export interface Document {
  path: string;
  kernel_id?: string;
  file_sha256?: string;
  latest_run?: RunSummary | null;
  cells: DocumentCell[];
  doc_version: number;
  seq: number;
  presence: Presence[];
}

export interface RunRequest {
  mode: "all" | "cells";
  cells?: number[];
  cell_ids?: string[];
  source?: string;
  params?: Record<string, unknown>;
  on_busy?: "reject" | "queue";
}

export interface RunAccepted {
  run_id: string;
  state: "running" | "queued";
  position?: number;
}

/** `started_by` / `interrupted_by` / `by` (SPEC FR-S2, FR-S6). */
export interface Attribution {
  client_id?: string;
  user?: string;
  nickname?: string;
}

export interface ApiErrorBody {
  error: { code: string; message: string; data?: Record<string, unknown> };
}

/** One event from `GET /kernels/{id}/events` (PROTOCOL §3.4, §4). */
export interface KernelEvent {
  seq: number | null;
  type: string;
  data: Record<string, any>;
}
