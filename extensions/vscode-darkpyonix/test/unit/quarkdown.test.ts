import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { convertOutputWithQuarkdown, decodeText, QUARKDOWN_MIME } from "../../src/run/outputs";
import { renderQuarkdown } from "../../src/run/quarkdown";

function makeFakeCli(scriptBody: string): { dir: string; cliPath: string } {
  const dir = mkdtempSync(join(tmpdir(), "dpx-qd-fake-"));
  const cliPath = join(dir, "fake-quarkdown.sh");
  writeFileSync(cliPath, `#!/bin/sh\n${scriptBody}\n`, "utf8");
  chmodSync(cliPath, 0o755);
  return { dir, cliPath };
}

describe("FR-F14 Quarkdown separate-process renderer", () => {
  it("renders Quarkdown via a separate CLI process and puts text/html first", async () => {
    const fake = makeFakeCli(`
      # Args: c <input.qd> --out <outDir>
      input="$2"
      outdir="$4"
      mkdir -p "$outdir/doc"
      printf '<article class="qd">%s</article>' "$(cat "$input")" > "$outdir/doc/index.html"
    `);
    try {
      const converted = await convertOutputWithQuarkdown(
        {
          output_type: "display_data",
          data: {
            [QUARKDOWN_MIME]: ".doctype {paged}\n# Hello",
            "text/plain": ".doctype {paged}\n# Hello",
          },
          metadata: { darkpyonix: { markup: "quarkdown" } },
        },
        (src) => renderQuarkdown(src, { cliPath: fake.cliPath }),
      );
      expect(converted.items[0].mime).toBe("text/html");
      expect(decodeText(converted.items[0])).toBe('<article class="qd">.doctype {paged}\n# Hello</article>');
      expect(converted.items.map((i) => i.mime)).toEqual(["text/html", QUARKDOWN_MIME, "text/plain"]);
      expect(converted.metadata.darkpyonix).toMatchObject({
        markup: "quarkdown",
        renderer: "quarkdown",
      });
    } finally {
      rmSync(fake.dir, { recursive: true, force: true });
    }
  });

  it("falls back cleanly to text/plain when the Quarkdown CLI is not installed", async () => {
    const converted = await convertOutputWithQuarkdown(
      {
        output_type: "display_data",
        data: {
          [QUARKDOWN_MIME]: "# Fallback",
          "text/plain": "# Fallback",
        },
        metadata: { darkpyonix: { markup: "quarkdown" } },
      },
      (src) => renderQuarkdown(src, { cliPath: "/nonexistent/darkpyonix-quarkdown-missing" }),
    );
    expect(converted.items[0].mime).toBe("text/plain");
    expect(decodeText(converted.items[0])).toBe("# Fallback");
    expect(converted.metadata.darkpyonix).toMatchObject({
      markup: "quarkdown",
      fallback: "not_installed",
    });
  });

  it("falls back cleanly to text/plain when the Quarkdown CLI exits non-zero", async () => {
    const fake = makeFakeCli(`
      echo "syntax error" >&2
      exit 2
    `);
    try {
      const converted = await convertOutputWithQuarkdown(
        {
          output_type: "display_data",
          data: {
            [QUARKDOWN_MIME]: ".bad {",
          },
          metadata: {},
        },
        (src) => renderQuarkdown(src, { cliPath: fake.cliPath }),
      );
      expect(converted.items[0].mime).toBe("text/plain");
      expect(decodeText(converted.items[0])).toBe(".bad {");
      expect(converted.metadata.darkpyonix).toMatchObject({
        fallback: "failed",
      });
    } finally {
      rmSync(fake.dir, { recursive: true, force: true });
    }
  });
});
