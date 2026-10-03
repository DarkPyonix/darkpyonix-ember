// Server-Sent Events: an incremental parser (WHATWG "event stream interpretation") and a
// reconnecting reader for the manager's events endpoint that resumes with Last-Event-ID.
import type { KernelEvent } from "./types";

export interface SseMessage {
  id: string | null;
  event: string;
  data: string;
}

/** Feed text chunks; complete messages come out of `push`. */
export class SseParser {
  private buf = "";
  private data: string[] = [];
  private event = "";
  private id: string | null = null;
  private sawId = false;

  push(chunk: string): SseMessage[] {
    this.buf += chunk;
    const out: SseMessage[] = [];
    for (;;) {
      const m = /\r\n|\r|\n/.exec(this.buf);
      if (!m) break;
      // A lone "\r" at the very end may be the first half of "\r\n".
      if (m[0] === "\r" && m.index === this.buf.length - 1) break;
      const line = this.buf.slice(0, m.index);
      this.buf = this.buf.slice(m.index + m[0].length);
      const msg = this.line(line);
      if (msg) out.push(msg);
    }
    return out;
  }

  private line(line: string): SseMessage | null {
    if (line === "") {
      if (this.data.length === 0) {
        this.event = "";
        this.sawId = false;
        return null;
      }
      const msg = { id: this.sawId ? this.id : null, event: this.event || "message", data: this.data.join("\n") };
      this.data = [];
      this.event = "";
      this.sawId = false;
      return msg;
    }
    if (line.startsWith(":")) return null;
    const colon = line.indexOf(":");
    const field = colon < 0 ? line : line.slice(0, colon);
    let value = colon < 0 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    switch (field) {
      case "data": this.data.push(value); break;
      case "event": this.event = value; break;
      case "id": if (!value.includes("\0")) { this.id = value; this.sawId = true; } break;
      default: break;
    }
    return null;
  }
}

export function toKernelEvent(msg: SseMessage): KernelEvent | null {
  let data: Record<string, any>;
  try {
    data = msg.data ? JSON.parse(msg.data) : {};
  } catch {
    return null;
  }
  const seq = msg.id !== null && /^\d+$/.test(msg.id) ? Number(msg.id) : null;
  return { seq, type: msg.event, data };
}

export interface EventStreamOptions {
  url: (since: number | null) => string;
  headers: Record<string, string>;
  /** Highest seq already applied; the stream resumes after it. */
  since: number | null;
  onEvent: (ev: KernelEvent) => void;
  onState?: (state: "connecting" | "open" | "closed", err?: unknown) => void;
  /** Reconnect delays in ms (last value repeats). */
  backoff?: number[];
}

/** A reconnecting SSE reader built on fetch streaming. */
export class EventStream {
  private controller: AbortController | null = null;
  private closed = false;
  private lastSeq: number | null;
  private attempt = 0;

  constructor(private readonly opts: EventStreamOptions) {
    this.lastSeq = opts.since;
    void this.loop();
  }

  get seq(): number | null {
    return this.lastSeq;
  }

  close(): void {
    this.closed = true;
    this.controller?.abort();
    this.opts.onState?.("closed");
  }

  private async loop(): Promise<void> {
    const backoff = this.opts.backoff ?? [250, 500, 1000, 2000, 5000];
    while (!this.closed) {
      this.opts.onState?.("connecting");
      try {
        await this.once();
        this.attempt = 0;
      } catch (err) {
        if (this.closed) return;
        this.opts.onState?.("closed", err);
      }
      if (this.closed) return;
      const delay = backoff[Math.min(this.attempt++, backoff.length - 1)];
      await new Promise((r) => setTimeout(r, delay));
    }
  }

  private async once(): Promise<void> {
    this.controller = new AbortController();
    const headers: Record<string, string> = { ...this.opts.headers, Accept: "text/event-stream" };
    if (this.lastSeq !== null) headers["Last-Event-ID"] = String(this.lastSeq);
    const res = await fetch(this.opts.url(this.lastSeq), { headers, signal: this.controller.signal });
    if (!res.ok || !res.body) throw new Error(`events: HTTP ${res.status}`);
    this.opts.onState?.("open");
    const parser = new SseParser();
    const decoder = new TextDecoder();
    const reader = res.body.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      for (const msg of parser.push(decoder.decode(value, { stream: true }))) {
        const ev = toKernelEvent(msg);
        if (!ev) continue;
        if (ev.seq !== null) {
          if (this.lastSeq !== null && ev.seq <= this.lastSeq) continue;
          this.lastSeq = ev.seq;
        }
        try {
          this.opts.onEvent(ev);
        } catch (err) {
          console.error("darkpyonix: event handler failed", err);
        }
      }
    }
  }
}
