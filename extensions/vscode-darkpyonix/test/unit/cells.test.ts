// Editor view of format cells: what the notebook shows, and byte-exact save.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  bodyToView, splitTrailer, textToView, unwrapMarkdown, viewToSource, viewToText, wrapMarkdown,
  type ViewNotebook,
} from "../../src/format/cells";
import { parse } from "../../src/format/parser";
import { ROUNDTRIP_CASES } from "../corpus/cases";

const REFERENCE = readFileSync(join(__dirname, "..", "corpus", "darkpyonix_format.py"), "utf8");

const EXTRA_CASES = [
  '# %% [markdown]\ndarkpyonix.markdown("""\n# T\n""")\n',
  "# %% [markdown]\r\ndarkpyonix.markdown('''\r\nA\r\nB\r\n''', silent=True)\r\n\r\n# %% [code]\r\nx\r\n",
  '# %% [markdown]\ndarkpyonix.markdown("not triple")\n',
  '# %% [markdown]\n# comment first\ndarkpyonix.markdown("""\nx\n""")\n',
  "pre\n\n\n# %% [code]\nmixed\r\nendings\n\n",
  "# %% [code]\n   \n\t\n",
  "# %% [code]\nx   \n  \n",
];

describe("editor view", () => {
  it("round-trips every case without edits", () => {
    for (const text of [REFERENCE, ...ROUNDTRIP_CASES, ...EXTRA_CASES]) {
      expect(viewToText(textToView(text)), JSON.stringify(text)).toBe(text);
    }
  });

  it("shows the reference file as expected cells", () => {
    const nb = textToView(REFERENCE);
    expect(nb.cells).toHaveLength(23);
    expect(nb.cells[0].meta?.type).toBe("preamble");
    const md = nb.cells.filter((c) => c.kind === "markup");
    expect(md).toHaveLength(2);
    expect(md[0].value.startsWith("# ")).toBe(true);
    expect(md[0].value).toContain("Python support in Starboard Notebook");
    expect(md[0].value.endsWith("for an overview of Starboard itself.")).toBe(true);
    expect(md[0].meta?.md?.suffix).toBe('\n""", silent=True)');
    expect(md[1].meta?.md?.suffix).toBe('\n""", slient=False)');
    // Blank lines between cells are kept out of the editor.
    expect(nb.cells[1].value).toBe("import torch\nimport torch.nn as nn\nimport torch.distributed as dist");
    expect(nb.cells[1].meta?.trailer).toBe("\n\n\n");
    expect(nb.cells.find((c) => c.meta?.rawType === "concorrunt")?.meta?.type).toBe("concurrent");
  });

  it("hides an empty preamble", () => {
    expect(textToView("# %% [code]\nx\n").cells).toHaveLength(1);
    expect(textToView("").cells).toHaveLength(0);
  });

  it("saves edits with the original layout", () => {
    const nb = textToView(REFERENCE);
    nb.cells[1].value = "import torch";
    const md = nb.cells.find((c) => c.kind === "markup")!;
    md.value = "# Changed";
    const out = viewToText(nb);
    expect(out).toContain('# %% [code]\nimport torch\n\n\n# %% [argparse]');
    expect(out).toContain('# %% [markdown]\ndarkpyonix.markdown("""\n# Changed\n""", silent=True)\n\n\n# %% [code]');
    const doc = parse(out);
    expect(doc.cells).toHaveLength(23);
  });

  it("writes new cells with generated headers", () => {
    const nb: ViewNotebook = textToView("# %% [code]\nx = 1\n");
    nb.cells.push({ kind: "markup", value: "## Notes", language: "markdown" });
    nb.cells.push({ kind: "code", value: "y = 2", language: "python" });
    expect(viewToText(nb)).toBe(
      '# %% [code]\nx = 1\n# %% [markdown]\ndarkpyonix.markdown("""\n## Notes\n""")\n\n\n# %% [code]\ny = 2\n',
    );
    const empty: ViewNotebook = { cells: [{ kind: "code", value: "a", language: "python" }], bom: false };
    expect(viewToText(empty)).toBe("# %% [code]\na\n");
  });

  it("keeps CRLF files CRLF", () => {
    const text = "import a\r\n\r\n# %% [code]\r\nx = 1\r\ny = 2\r\n";
    const nb = textToView(text);
    expect(nb.cells[1].value).toBe("x = 1\ny = 2");
    nb.cells[1].value = "x = 1\ny = 3\nz = 4";
    expect(viewToText(nb)).toBe("import a\r\n\r\n# %% [code]\r\nx = 1\r\ny = 3\r\nz = 4\r\n");
  });

  it("switching a cell's kind rewrites its marker", () => {
    const nb = textToView('# %% [code]\nx\n\n\n# %% [markdown]\ndarkpyonix.markdown("""\nhi\n""")\n');
    nb.cells[0] = { ...nb.cells[0], kind: "markup", language: "markdown" };
    nb.cells[1] = { ...nb.cells[1], kind: "code", language: "python", value: "print('hi')" };
    expect(viewToText(nb)).toBe('# %% [markdown]\ndarkpyonix.markdown("""\nx\n""")\n\n\n# %% [code]\nprint(\'hi\')\n');
  });

  it("escapes triple quotes typed into Markdown", () => {
    const un = unwrapMarkdown('darkpyonix.markdown("""\nx\n""")')!;
    expect(un.text).toBe("x");
    expect(wrapMarkdown(un.md, 'say """hi"""')).toBe('darkpyonix.markdown("""\nsay \\"\\"\\"hi\\"\\"\\"\n""")');
    expect(unwrapMarkdown('darkpyonix.markdown("""a""" + b)')).toBeNull();
  });

  it("splits trailing blank lines", () => {
    expect(splitTrailer("x\n\n\n")).toEqual(["x", "\n\n\n"]);
    expect(splitTrailer("x")).toEqual(["x", ""]);
    expect(splitTrailer("x\r\n  \r\n")).toEqual(["x", "\r\n  \r\n"]);
    expect(splitTrailer("\n\n")).toEqual(["", "\n\n"]);
    expect(splitTrailer("a\n b\n")).toEqual(["a\n b", "\n"]);
  });

  it("maps manager cells (source with trailer) to the same view", () => {
    const v = bodyToView("markdown", 'darkpyonix.markdown("""\n# T\n""", silent=True)\n\n\n', { title: null });
    expect(v.kind).toBe("markup");
    expect(v.value).toBe("# T");
    expect(viewToSource(v, 1, false)).toBe('darkpyonix.markdown("""\n# T\n""", silent=True)\n\n\n');
  });
});
