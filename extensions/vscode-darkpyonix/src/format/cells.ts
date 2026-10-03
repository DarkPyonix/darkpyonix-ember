/**
 * Mapping between parsed format cells and what a notebook editor shows.
 *
 * Pure (no `vscode` import) so it is unit-tested directly. The editor shows a cleaned-up view of
 * each cell body; everything needed to restore the exact bytes travels in `DpxCellMeta`:
 *
 *  - `header`  exact marker + metadata lines ("" for cells created in the editor);
 *  - `trailer` blank lines after the code, before the next marker (layout, not code);
 *  - `eol`     "\r\n" when the body uses CRLF throughout (shown with "\n");
 *  - `md`      for `[markdown]` cells, the `darkpyonix.markdown("""` … `""", silent=…)` wrapper
 *              around the Markdown text shown in a markup cell.
 */
import {
  BOM, CODE, MARKDOWN, PREAMBLE, canonicalType, parse, serialize,
  type Cell, type NotebookDocument,
} from "./parser";

export interface MarkdownWrapper {
  prefix: string;
  suffix: string;
}

export interface DpxCellMeta {
  /** Canonical type ("preamble" for the text before the first marker). */
  type: string;
  rawType: string | null;
  title: string | null;
  metadata: Record<string, unknown>;
  header: string;
  trailer: string;
  eol: "\n" | "\r\n";
  md?: MarkdownWrapper;
}

export interface ViewCell {
  kind: "code" | "markup";
  /** Editor text. */
  value: string;
  language: "python" | "markdown";
  /** Absent for cells the user created in the editor. */
  meta?: DpxCellMeta;
}

export interface ViewNotebook {
  cells: ViewCell[];
  bom: boolean;
}

export const DEFAULT_MD: MarkdownWrapper = { prefix: 'darkpyonix.markdown("""\n', suffix: '\n""")' };

/** Split a body into the code and its trailing blank lines (from the first line ending after which only whitespace follows). */
export function splitTrailer(body: string): [string, string] {
  const m = /\r?\n[ \t\r\n]*$/.exec(body);
  if (!m) return [body, ""];
  return [body.slice(0, m.index), body.slice(m.index)];
}

function detectEol(main: string): "\n" | "\r\n" {
  if (!main.includes("\r\n")) return "\n";
  const lf = (main.match(/\n/g) ?? []).length;
  const crlf = (main.match(/\r\n/g) ?? []).length;
  return lf === crlf ? "\r\n" : "\n";
}

const MD_OPEN = /^darkpyonix\.markdown\(\s*([rRuU]?)("""|''')\n?/;
const MD_CLOSE = /^\n?("""|''')\s*(?:,[^"']*)?\)[ \t]*$/;

/**
 * Split `darkpyonix.markdown("""…""", silent=…)` into wrapper and Markdown text. Returns null
 * when the cell does not have exactly that shape; it is then shown as a code cell.
 */
export function unwrapMarkdown(code: string): { md: MarkdownWrapper; text: string } | null {
  const open = MD_OPEN.exec(code);
  if (!open) return null;
  const quote = open[2];
  const rest = code.slice(open[0].length);
  const close = rest.lastIndexOf(quote);
  if (close < 0) return null;
  let bodyEnd = close;
  if (bodyEnd > 0 && rest[bodyEnd - 1] === "\n") bodyEnd -= 1;
  const text = rest.slice(0, bodyEnd);
  const suffix = rest.slice(bodyEnd);
  if (!MD_CLOSE.test(suffix) || text.includes(quote)) return null;
  // A body ending in a backslash would escape the closing quote; leave it as code.
  if (text.endsWith("\\")) return null;
  return { md: { prefix: open[0], suffix }, text };
}

export function wrapMarkdown(md: MarkdownWrapper, text: string): string {
  const quote = md.prefix.includes("'''") ? "'''" : '"""';
  const escaped = text.split(quote).join(quote === '"""' ? '\\"\\"\\"' : "\\'\\'\\'");
  return md.prefix + escaped + (escaped.endsWith("\\") && !md.suffix.startsWith("\n") ? " " : "") + md.suffix;
}

/** View of one cell body of a given type (used for parsed cells and for cells from the manager). */
export function bodyToView(
  type: string, source: string,
  rest: { rawType?: string | null; title?: string | null; metadata?: Record<string, unknown>; header?: string },
): ViewCell {
  const [main, trailer] = splitTrailer(source);
  const eol = detectEol(main);
  const display = eol === "\r\n" ? main.replace(/\r\n/g, "\n") : main;
  const meta: DpxCellMeta = {
    type,
    rawType: rest.rawType ?? (type === PREAMBLE ? null : type),
    title: rest.title ?? null,
    metadata: rest.metadata ?? {},
    header: rest.header ?? "",
    trailer,
    eol,
  };
  if (type === MARKDOWN) {
    const un = unwrapMarkdown(display);
    if (un) {
      meta.md = un.md;
      return { kind: "markup", value: un.text, language: "markdown", meta };
    }
  }
  return { kind: "code", value: display, language: "python", meta };
}

export function cellToView(cell: Cell): ViewCell {
  return bodyToView(cell.type, cell.source, {
    rawType: cell.rawType, title: cell.title, metadata: cell.metadata, header: cell.header,
  });
}

/** Parse file text into editor cells. An empty preamble is not shown. */
export function textToView(text: string): ViewNotebook {
  const doc = parse(text);
  const cells: ViewCell[] = [];
  for (const cell of doc.cells) {
    if (cell.type === PREAMBLE && cell.source === "") continue;
    cells.push(cellToView(cell));
  }
  return { cells, bom: doc.bom };
}

/** Resolve the effective type of an editor cell (the user may have switched code ↔ markup). */
export function effectiveType(view: ViewCell, position: number): string {
  const meta = view.meta;
  if (view.kind === "markup") return MARKDOWN;
  if (!meta) return CODE;
  if (meta.type === PREAMBLE) return position === 0 ? PREAMBLE : CODE;
  if (meta.type === MARKDOWN && !meta.md) return MARKDOWN; // unparseable markdown shown as code
  if (meta.type === MARKDOWN) return CODE; // was markup, switched to code
  return meta.type;
}

/** The exact cell body (`Cell.source`) an editor cell stands for. */
export function viewToSource(view: ViewCell, position: number, isLast: boolean): string {
  const meta = view.meta;
  const type = effectiveType(view, position);
  let main = view.value.replace(/\r\n/g, "\n");
  if (type === MARKDOWN && view.kind === "markup") {
    main = wrapMarkdown(meta?.md ?? DEFAULT_MD, main);
  }
  const eol = meta?.eol ?? "\n";
  if (eol === "\r\n") main = main.replace(/\n/g, "\r\n");
  let trailer = meta?.trailer;
  if (trailer === undefined) trailer = isLast ? eol : eol + eol + eol;
  // Code that ends without a line ending would glue onto the next marker; serialize() adds the
  // "\n" in that case, as the reference serializer does.
  return main + trailer;
}

/** Header for an editor cell; "" means "generate from type/title/metadata". */
function headerFor(view: ViewCell, position: number): { header: string; rawType: string | null } {
  const meta = view.meta;
  const type = effectiveType(view, position);
  if (!meta) return { header: "", rawType: type };
  if (type === PREAMBLE) return { header: "", rawType: null };
  if (canonicalType(meta.rawType) === type && meta.type !== PREAMBLE) return { header: meta.header, rawType: meta.rawType };
  return { header: "", rawType: type };
}

/** Editor cells → format cells. */
export function viewToDocument(nb: ViewNotebook): NotebookDocument {
  const cells: Cell[] = [];
  const views = nb.cells;
  const hasPreamble = views.length > 0 && effectiveType(views[0], 0) === PREAMBLE;
  if (!hasPreamble) {
    cells.push({ index: 0, type: PREAMBLE, rawType: null, title: null, metadata: {}, source: "", header: "", sourceSha256: "", id: null });
  }
  views.forEach((view, position) => {
    const type = effectiveType(view, position);
    const { header, rawType } = headerFor(view, position);
    const meta = view.meta;
    const metadata = meta?.metadata ?? {};
    cells.push({
      index: cells.length,
      type,
      rawType,
      title: meta?.title ?? null,
      metadata,
      source: viewToSource(view, position, position === views.length - 1),
      header,
      sourceSha256: "",
      id: typeof metadata.id === "string" ? metadata.id : null,
    });
  });
  return { cells, fileSha256: "", bom: nb.bom };
}

export function viewToText(nb: ViewNotebook): string {
  return serialize(viewToDocument(nb));
}

export { BOM };
