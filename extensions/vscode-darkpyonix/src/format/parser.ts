/**
 * Parser and serializer for the DarkPyonix notebook format (darkpyonix-core docs/FORMAT.md §2,
 * SPEC FR-F1). A line-for-line port of the reference implementation
 * `kernel/darkpyonix/format/_parser.py` so that this extension and the kernel cut a file into
 * exactly the same cells.
 *
 * Layout of a parsed file: the text is cut at every marker line. Each cell owns
 *  - `header`: its marker line plus the metadata lines right after it, line endings included;
 *  - `source`: every following line up to (not including) the next marker, line endings
 *    included. Blank lines between cells therefore belong to the preceding cell's source.
 *
 * `serialize` concatenates `header + source` for every cell, so an unmodified document is
 * reproduced byte for byte (CRLF, a missing trailing newline and a leading BOM included).
 *
 * Like Jupytext, the parser is line based: a marker line inside a triple-quoted string still
 * opens a new cell.
 */
import { createHash } from "node:crypto";

/** FORMAT §2.2, verbatim. Applied to a line without its line ending. */
export const MARKER_RE =
  /^# %%(?:[ \t]+(?<title>[^[\n]*?))?(?:[ \t]*\[(?<type>[A-Za-z_][A-Za-z0-9_-]*)\])?[ \t]*$/;
/** FORMAT §2.3: `# @key: value`. */
export const METADATA_RE = /^# @(?<key>[A-Za-z_][A-Za-z0-9_.-]*):(?:[ \t]*(?<value>.*?))?[ \t]*$/;

export const BOM = "\ufeff";
export const PREAMBLE = "preamble";
export const CODE = "code";
export const MARKDOWN = "markdown";
export const KNOWN_TYPES = [
  "preamble", "code", "markdown", "argparse", "binding", "shell",
  "parallel", "concurrent", "cinterop", "cppinterop", "rustinterop",
  "sql", "toml", "yaml", "json",
] as const;
export const TYPE_ALIASES: Record<string, string> = { concorrunt: "concurrent" };

export type MetadataValue = unknown;

export interface Cell {
  /** 0 is the preamble (text before the first marker). */
  index: number;
  /** Canonical type, aliases resolved and lower-cased. */
  type: string;
  /** Type as written between `[ ]`, or null. */
  rawType: string | null;
  title: string | null;
  metadata: Record<string, MetadataValue>;
  /** Body without marker and metadata lines (trailing blank lines included). */
  source: string;
  /** Exact marker + metadata lines; "" for the preamble and for cells built by a client. */
  header: string;
  sourceSha256: string;
  /** Metadata `id` if present (stringified). */
  id: string | null;
}

export interface NotebookDocument {
  /** `cells[0]` is always the preamble (possibly empty). */
  cells: Cell[];
  /** SHA-256 of the whole text (UTF-8, BOM included as written). */
  fileSha256: string;
  /** The text started with U+FEFF; kept out of the preamble. */
  bom: boolean;
}

/** Split on `\n` only, keeping line endings (like the reference `_split_lines`). */
export function splitLines(text: string): string[] {
  const lines = text.split("\n");
  const out: string[] = [];
  for (let i = 0; i < lines.length - 1; i++) out.push(lines[i] + "\n");
  const last = lines[lines.length - 1];
  if (last) out.push(last);
  return out;
}

function content(line: string): string {
  if (line.endsWith("\r\n")) return line.slice(0, -2);
  if (line.endsWith("\n")) return line.slice(0, -1);
  return line;
}

export function metadataValue(raw: string): MetadataValue {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

export function sha256(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

/**
 * SHA-256 of a cell body (FORMAT §2.4): `\r\n` → `\n`, trailing blank (empty or
 * whitespace-only) lines dropped together with the final line ending.
 */
export function sourceSha256(source: string): string {
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  return sha256(lines.join("\n"));
}

interface Chunk {
  match: RegExpMatchArray | null;
  items: Array<[string, MetadataValue]>;
  header: string;
  source: string;
}

function parseChunks(text: string): { bom: boolean; chunks: Chunk[] } {
  const bom = text.startsWith(BOM);
  if (bom) text = text.slice(BOM.length);
  const chunks: Chunk[] = [];
  let match: RegExpMatchArray | null = null;
  let items: Array<[string, MetadataValue]> = [];
  let header: string[] = [];
  let body: string[] = [];
  let inMetadata = false;
  for (const line of splitLines(text)) {
    const c = content(line);
    const m = MARKER_RE.exec(c);
    if (m) {
      chunks.push({ match, items, header: header.join(""), source: body.join("") });
      match = m;
      items = [];
      header = [line];
      body = [];
      inMetadata = true;
      continue;
    }
    if (inMetadata) {
      const md = METADATA_RE.exec(c);
      if (md) {
        items.push([md.groups!.key, metadataValue(md.groups!.value ?? "")]);
        header.push(line);
        continue;
      }
      inMetadata = false;
    }
    body.push(line);
  }
  chunks.push({ match, items, header: header.join(""), source: body.join("") });
  return { bom, chunks };
}

export function canonicalType(rawType: string | null): string {
  if (rawType === null) return CODE;
  const lowered = rawType.toLowerCase();
  return TYPE_ALIASES[lowered] ?? lowered;
}

/** Parse notebook source text. Never throws on unknown types or metadata. */
export function parse(text: string): NotebookDocument {
  const { bom, chunks } = parseChunks(text);
  const cells: Cell[] = chunks.map((chunk, index) => {
    if (chunk.match === null) {
      return {
        index: 0, type: PREAMBLE, rawType: null, title: null, metadata: {},
        source: chunk.source, header: chunk.header, sourceSha256: sourceSha256(chunk.source), id: null,
      };
    }
    const rawType = chunk.match.groups?.type ?? null;
    const metadata: Record<string, MetadataValue> = {};
    for (const [k, v] of chunk.items) metadata[k] = v;
    const cid = metadata.id;
    return {
      index,
      type: canonicalType(rawType),
      rawType,
      title: chunk.match.groups?.title || null,
      metadata,
      source: chunk.source,
      header: chunk.header,
      sourceSha256: sourceSha256(chunk.source),
      id: cid === undefined || cid === null ? null : typeof cid === "string" ? cid : pyStr(cid),
    };
  });
  return { cells, fileSha256: sha256(text), bom };
}

/** `str(value)` for the JSON scalars a metadata id can hold. */
function pyStr(v: unknown): string {
  if (v === true) return "True";
  if (v === false) return "False";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

/** Python `json.dumps` with default separators (", ", ": "), for metadata written by clients. */
export function pyJsonDumps(value: unknown): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "string") {
    return JSON.stringify(value).replace(/[^\x00-\x7f]/g, (ch) => "\\u" + ch.charCodeAt(0).toString(16).padStart(4, "0"));
  }
  if (typeof value === "number" || typeof value === "boolean") return JSON.stringify(value);
  if (Array.isArray(value)) return "[" + value.map(pyJsonDumps).join(", ") + "]";
  return "{" + Object.entries(value as Record<string, unknown>)
    .map(([k, v]) => pyJsonDumps(k) + ": " + pyJsonDumps(v)).join(", ") + "}";
}

/** Build a marker + metadata block for a cell that has no recorded header. */
export function formatHeader(title: string | null, type: string | null, metadata: Record<string, MetadataValue>): string {
  let marker = "# %%";
  if (title) marker += " " + title;
  if (type) marker += " [" + type + "]";
  const lines = [marker];
  for (const [key, value] of Object.entries(metadata)) {
    const text = typeof value === "string" && metadataValue(value) === value ? value : pyJsonDumps(value);
    lines.push(`# @${key}: ${text}`);
  }
  return lines.join("\n") + "\n";
}

/**
 * Inverse of `parse`: byte-identical for an unmodified document. A cell without a recorded
 * header (one built by a client) gets a marker and metadata lines generated from its title,
 * type and metadata.
 */
export function serialize(doc: NotebookDocument): string {
  const parts: string[] = [];
  let last = "";
  for (const cell of doc.cells) {
    let header = cell.header;
    if (!header && cell.index !== 0 && cell.type !== PREAMBLE) {
      header = formatHeader(cell.title, cell.rawType ?? cell.type, cell.metadata);
    }
    if (header && last && !last.endsWith("\n")) parts.push("\n");
    for (const part of [header, cell.source]) {
      if (part) {
        parts.push(part);
        last = part;
      }
    }
  }
  return (doc.bom ? BOM : "") + parts.join("");
}
