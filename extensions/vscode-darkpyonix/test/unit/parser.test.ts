// Port of darkpyonix-core tests/test_fr_f1_format.py (FR-F1) against the TypeScript parser.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { PREAMBLE, parse, serialize, sha256 } from "../../src/format/parser";

const CORPUS = join(__dirname, "..", "corpus");
const REFERENCE = join(CORPUS, "darkpyonix_format.py");
import { BOM, ROUNDTRIP_CASES } from "../corpus/cases";


function count<T>(xs: T[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const x of xs) out[String(x)] = (out[String(x)] ?? 0) + 1;
  return out;
}

describe("FR-F1 parser", () => {
  it("parses the reference file", () => {
    const raw = readFileSync(REFERENCE);
    const doc = parse(raw.toString("utf8"));
    expect(doc.fileSha256).toBe(sha256(raw.toString("utf8")));

    const pre = doc.cells[0];
    expect(pre.index).toBe(0);
    expect(pre.type).toBe(PREAMBLE);
    expect(pre.header).toBe("");
    expect(pre.source.startsWith('"""Starboard Notebook: Python support"""\nimport darkpyonix\n')).toBe(true);

    const cells = doc.cells.slice(1);
    expect(cells).toHaveLength(22);
    expect(doc.cells.map((c) => c.index)).toEqual([...Array(23).keys()]);
    expect(count(cells.map((c) => c.type))).toEqual({
      code: 11, markdown: 2, binding: 2, argparse: 1, shell: 1, parallel: 1,
      concurrent: 1, cinterop: 1, cppinterop: 1, rustinterop: 1,
    });
    const conc = cells.filter((c) => c.type === "concurrent");
    expect(conc).toHaveLength(1);
    expect(conc[0].rawType).toBe("concorrunt");

    const widths = cells.filter((c) => Object.keys(c.metadata).length);
    expect(widths).toHaveLength(2);
    for (const c of widths) {
      expect(c.type).toBe("code");
      expect(c.metadata).toEqual({ width: "1fr" });
      expect(c.source).not.toContain("# @width");
      expect(c.header.startsWith("# %% [code]")).toBe(true);
      expect(c.header.endsWith("# @width: 1fr\n")).toBe(true);
    }
    for (const c of cells) {
      expect(c.title).toBeNull();
      expect(c.id).toBeNull();
      expect(c.header.startsWith("# %%")).toBe(true);
      expect(c.source).not.toContain("# %%");
      expect(c.sourceSha256).toMatch(/^[0-9a-f]{64}$/);
    }
    expect(cells[0].source.startsWith("import torch\n")).toBe(true);
    expect(cells[1].type).toBe("argparse");
  });

  it("round-trips byte for byte", () => {
    const raw = readFileSync(REFERENCE);
    expect(Buffer.from(serialize(parse(raw.toString("utf8"))), "utf8").equals(raw)).toBe(true);
    for (const text of ROUNDTRIP_CASES) {
      const doc = parse(text);
      expect(serialize(doc), JSON.stringify(text)).toBe(text);
      expect(doc.fileSha256).toBe(sha256(text));
    }
  });

  it("follows the marker grammar", () => {
    const text =
      "# %% Load data [code]\n" +
      "# %% Title only\n" +
      "# %%\n" +
      "# %% [Markdown]\n" +
      "# %% [concorrunt]\n" +
      "# %% [mystery-type]   \n" +
      "  # %% [code]\n" +
      "# %%% [code]\n" +
      "# %%[code]\n" +
      "#%% [code]\n";
    const cells = parse(text).cells.slice(1);
    expect(cells.map((c) => [c.title, c.type, c.rawType])).toEqual([
      ["Load data", "code", "code"],
      ["Title only", "code", null],
      [null, "code", null],
      [null, "markdown", "Markdown"],
      [null, "concurrent", "concorrunt"],
      [null, "mystery-type", "mystery-type"],
      [null, "code", "code"],
    ]);
    expect(cells[5].source).toBe("  # %% [code]\n# %%% [code]\n");
    expect(cells[6].source).toBe("#%% [code]\n");
  });

  it("reads metadata lines", () => {
    const text =
      "# %% [code]\n" +
      '# @id: "c-3f2a"\n' +
      "# @width: 1fr\n" +
      "# @collapsed: true\n" +
      "# @n: 3\n" +
      '# @data: {"a": [1, null]}\n' +
      "# @empty:\n" +
      "x = 1\n" +
      "# @after: 1\n";
    const c = parse(text).cells[1];
    expect(c.metadata).toEqual({ id: "c-3f2a", width: "1fr", collapsed: true, n: 3, data: { a: [1, null] }, empty: "" });
    expect(c.id).toBe("c-3f2a");
    expect(c.source).toBe("x = 1\n# @after: 1\n");
    expect(c.header).toBe(text.slice(0, text.indexOf("x = 1")));

    const d = parse("# %% [code]\n\n# @width: 1fr\n").cells[1];
    expect(d.metadata).toEqual({});
    expect(d.source).toBe("\n# @width: 1fr\n");
    expect(parse("# %%\n# @id: 7\n").cells[1].id).toBe("7");
  });

  it("hashes sources ignoring line endings and trailing blank lines", () => {
    const a = parse("# %% [code]\nx = 1\ny = 2\n\n\n# %% [code]\nz\n").cells[1];
    const b = parse("# %% [code]\r\nx = 1\r\ny = 2\r\n# %% [code]\r\nz\r\n").cells[1];
    const c = parse("# %% [code]\nx = 1\ny = 2").cells[1];
    const d = parse("# %% [code]\nx = 1\n\ny = 2\n").cells[1];
    expect(a.source).toBe("x = 1\ny = 2\n\n\n");
    expect(a.sourceSha256).toBe(sha256("x = 1\ny = 2"));
    expect(b.sourceSha256).toBe(a.sourceSha256);
    expect(c.sourceSha256).toBe(a.sourceSha256);
    expect(d.sourceSha256).not.toBe(a.sourceSha256);
    expect(parse("# %% Title [code]\n# @width: 1fr\nx = 1\ny = 2\n").cells[1].sourceSha256).toBe(a.sourceSha256);
    expect(parse("").cells[0].sourceSha256).toBe(sha256(""));
  });

  it("treats markers inside strings as markers", () => {
    const text = 's = """\n# %% [code]\n"""\n';
    const doc = parse(text);
    expect(doc.cells).toHaveLength(2);
    expect(doc.cells[0].source).toBe('s = """\n');
    expect(doc.cells[1].source).toBe('"""\n');
    expect(serialize(doc)).toBe(text);
  });

  it("keeps the BOM out of the preamble", () => {
    let doc = parse(BOM + "# %% [code]\nx\n");
    expect(doc.cells[0].source).toBe("");
    expect(doc.cells).toHaveLength(2);
    doc = parse(BOM + "import os\n");
    expect(doc.cells[0].source).toBe("import os\n");
  });

  it("generates a header for a new cell", () => {
    const doc = parse("# %% [code]\nx = 1");
    doc.cells.push({
      index: 2, type: "markdown", rawType: null, title: "Notes", metadata: { width: "1fr" },
      source: "m()\n", header: "", sourceSha256: "", id: null,
    });
    expect(serialize(doc)).toBe("# %% [code]\nx = 1\n# %% Notes [markdown]\n# @width: 1fr\nm()\n");
    expect(parse(serialize(doc)).cells.map((c) => c.type)).toEqual(["preamble", "code", "markdown"]);
  });

  it("always has a preamble", () => {
    for (const text of ["# %% [code]\n", "x\n# %% [code]\ny\n"]) expect(parse(text).cells[0].type).toBe(PREAMBLE);
  });
});
