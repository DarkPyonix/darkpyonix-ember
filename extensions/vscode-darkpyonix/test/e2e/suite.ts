// End-to-end inside VS Code (run by test/e2e/runTest.mjs): the real extension against the fake
// manager, which runs in the same extension host process.
import * as assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import * as vscode from "vscode";
import type { DarkPyonixApi } from "../../src/extension";
import { FakeManager } from "../fake/fakeManager";

const EXT_ID = "darkpyonix.vscode-darkpyonix";

async function until(cond: () => boolean, ms = 10000, what = "condition"): Promise<void> {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

const tests: Array<[string, () => Promise<void>]> = [];
const test = (name: string, fn: () => Promise<void>) => tests.push([name, fn]);

export async function run(): Promise<void> {
  const ws = process.env.DPX_E2E_WS!;
  const file = join(ws, "nb.pynb");
  const original = readFileSync(file, "utf8");
  const fm = new FakeManager(original, file);
  await fm.start();
  const cfg = vscode.workspace.getConfiguration("darkpyonix");
  await cfg.update("manager.url", fm.url, vscode.ConfigurationTarget.Global);
  await cfg.update("manager.token", fm.token, vscode.ConfigurationTarget.Global);
  await cfg.update("autoStartKernel", true, vscode.ConfigurationTarget.Global);
  await cfg.update("nickname", "e2e-box", vscode.ConfigurationTarget.Global);

  let nb!: vscode.NotebookDocument;
  let api!: DarkPyonixApi;

  test("opens .pynb as a DarkPyonix notebook with the format's cells", async () => {
    nb = await vscode.workspace.openNotebookDocument(vscode.Uri.file(file));
    await vscode.window.showNotebookDocument(nb);
    assert.equal(nb.notebookType, "darkpyonix-notebook");
    assert.equal(nb.cellCount, 23);
    const markup = nb.getCells().filter((c) => c.kind === vscode.NotebookCellKind.Markup);
    assert.equal(markup.length, 2);
    assert.ok(markup[0].document.getText().includes("Python support in Starboard Notebook"));
    assert.equal(nb.cellAt(1).document.getText(), "import torch\nimport torch.nn as nn\nimport torch.distributed as dist");
    const ext = vscode.extensions.getExtension<DarkPyonixApi>(EXT_ID)!;
    api = ext.isActive ? ext.exports : await ext.activate();
  });

  test("attaches to the kernel and binds cells (FR-S1)", async () => {
    await until(() => !!api.connectionFor(nb)?.session, 10000, "session");
    await until(() => nb.getCells().every((c) => !!api.cellIdOf(c)), 10000, "bound cells");
    assert.equal(nb.isDirty, false, "binding ids must not dirty the notebook");
    await until(() => fm.presence.size > 0, 5000, "presence");
  });

  test("applies a remote edit to the open notebook (FR-S2)", async () => {
    fm.editAs({ client_id: "alice-device-01", nickname: "desk", user: "alice" }, fm.cells[3].cell_id, 'print("from alice")\n\n\n');
    // Document index 3 is editor cell 3 (the preamble is shown).
    await until(() => nb.cellAt(3).document.getText() === 'print("from alice")', 5000, "remote edit");
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(fm.requests.filter((r) => r.method === "PATCH").length, 0, "no echo");
  });

  test("typing locks the cell and sends the edit (FR-S3)", async () => {
    const cell = nb.cellAt(5);
    const we = new vscode.WorkspaceEdit();
    we.insert(cell.document.uri, new vscode.Position(0, 0), "# edited in VS Code\n");
    await vscode.workspace.applyEdit(we);
    const id = api.cellIdOf(cell)!;
    await until(() => fm.byId(id)!.source.startsWith("# edited in VS Code\n"), 5000, "PATCH");
    assert.ok(fm.requests.some((r) => r.method === "PUT" && r.path.endsWith(`/cells/${id}/lock`)));
  });

  test("runs a cell and shows streamed outputs with attribution (FR-X, FR-S6)", async () => {
    await vscode.commands.executeCommand("notebook.cell.execute", { ranges: [{ start: 5, end: 6 }] }, nb.uri);
    const cell = nb.cellAt(5);
    await until(() => cell.outputs.length > 0 && cell.executionSummary?.success === true, 10000, "outputs");
    const text = new TextDecoder().decode(cell.outputs[0].items[0].data);
    assert.equal(text, `ran ${api.cellIdOf(cell)}\n`);
    const run = fm.requests.find((r) => r.method === "POST" && r.path.endsWith("/runs"))!;
    assert.deepEqual(run.body.cell_ids, [api.cellIdOf(cell)]);
  });

  test("saves byte-exact except for the edited cells", async () => {
    await nb.save();
    const saved = readFileSync(file, "utf8");
    const expected = original
      .replace('# %% [code]\nprint("Running preprocessor for Python support...")', '# %% [code]\nprint("from alice")');
    const lines = expected.split("\n");
    const at = lines.indexOf("# When you first run this cell it will load the Python runtime.");
    lines.splice(at, 0, "# edited in VS Code");
    assert.equal(saved, lines.join("\n"));
  });

  test("Open as DarkPyonix Notebook opens a .py file as a notebook", async () => {
    await vscode.commands.executeCommand("darkpyonix.openAsNotebook", vscode.Uri.file(join(ws, "plain.py")));
    await until(() => vscode.window.activeNotebookEditor?.notebook.notebookType === "darkpyonix-notebook-py", 10000, "py notebook");
    assert.equal(vscode.window.activeNotebookEditor!.notebook.cellCount, 23);
  });

  const failures: string[] = [];
  for (const [name, fn] of tests) {
    try {
      await fn();
      console.log(`  ok   ${name}`);
    } catch (err) {
      console.log(`  FAIL ${name}\n       ${(err as Error).stack ?? err}`);
      failures.push(name);
      if (name.startsWith("opens") || name.startsWith("attaches")) break;
    }
  }
  await fm.stop();
  if (failures.length) throw new Error(`${failures.length} e2e test(s) failed: ${failures.join("; ")}`);
}
