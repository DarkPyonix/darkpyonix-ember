// nbformat 4 outputs → notebook output items (MIME + bytes). Pure; the VS Code layer wraps the
// result in NotebookCellOutput / NotebookCellOutputItem.
import type { NbOutput } from "../manager/types";

export const STDOUT_MIME = "application/vnd.code.notebook.stdout";
export const STDERR_MIME = "application/vnd.code.notebook.stderr";
export const ERROR_MIME = "application/vnd.code.notebook.error";

export interface OutputItem {
  mime: string;
  data: Uint8Array;
}

export interface ConvertedOutput {
  items: OutputItem[];
  metadata: Record<string, unknown>;
  /** `stdout`/`stderr` for stream outputs, which the UI appends to instead of adding a new output. */
  stream?: "stdout" | "stderr";
}

const enc = new TextEncoder();

export function joinText(t: string | string[] | undefined): string {
  if (t === undefined) return "";
  return Array.isArray(t) ? t.join("") : t;
}

function isJsonMime(mime: string): boolean {
  return mime === "application/json" || mime.endsWith("+json");
}

function isBinaryMime(mime: string): boolean {
  return mime.startsWith("image/") && mime !== "image/svg+xml";
}

export function mimeItem(mime: string, value: unknown): OutputItem {
  if (isJsonMime(mime)) {
    return { mime, data: enc.encode(typeof value === "string" ? value : JSON.stringify(value)) };
  }
  const text = typeof value === "string" || Array.isArray(value) ? joinText(value as string | string[]) : JSON.stringify(value);
  if (isBinaryMime(mime)) return { mime, data: Uint8Array.from(Buffer.from(text.replace(/\s+/g, ""), "base64")) };
  return { mime, data: enc.encode(text) };
}

/** Rich MIME types first, plain text last, as notebook renderers prefer. */
function mimeRank(mime: string): number {
  if (mime === "text/plain") return 9;
  if (mime.startsWith("application/vnd.")) return 0;
  if (mime === "text/html" || mime === "image/svg+xml") return 1;
  if (mime.startsWith("image/")) return 2;
  if (mime === "text/markdown" || mime === "text/latex") return 3;
  return 5;
}

export function convertOutput(o: NbOutput): ConvertedOutput {
  switch (o.output_type) {
    case "stream": {
      const stream = o.name === "stderr" ? "stderr" : "stdout";
      return { items: [{ mime: stream === "stderr" ? STDERR_MIME : STDOUT_MIME, data: enc.encode(joinText(o.text)) }], metadata: {}, stream };
    }
    case "error": {
      const tb = (o.traceback ?? []).join("\n");
      const err = { name: o.ename ?? "Error", message: o.evalue ?? "", stack: tb || `${o.ename}: ${o.evalue}` };
      return { items: [{ mime: ERROR_MIME, data: enc.encode(JSON.stringify(err)) }], metadata: {} };
    }
    default: {
      const data = o.data ?? {};
      const mimes = Object.keys(data).sort((a, b) => mimeRank(a) - mimeRank(b));
      const metadata: Record<string, unknown> = { ...(o.metadata ?? {}) };
      if (o.output_type === "execute_result") metadata.executionCount = o.execution_count ?? null;
      metadata.outputType = o.output_type;
      return { items: mimes.map((m) => mimeItem(m, data[m])), metadata };
    }
  }
}

export function decodeText(item: OutputItem): string {
  return new TextDecoder().decode(item.data);
}
