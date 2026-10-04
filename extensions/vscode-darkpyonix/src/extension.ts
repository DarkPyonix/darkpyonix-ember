// DarkPyonix notebooks for VS Code: serializer, the "DarkPyonix" controller, collaboration and
// kernel commands. The logic lives in editor-agnostic modules (format/, manager/, collab/, run/,
// connection.ts); this file wires them to the VS Code API.
import * as vscode from "vscode";
import { NotebookConnection } from "./connection";
import { ApiError } from "./manager/client";
import { NotebookUiState, VsExecSink, VsNotebookHost } from "./vscode/notebookHost";
import { ManagerService, TOKEN_SECRET } from "./vscode/managerService";
import { DarkPyonixSerializer, NOTEBOOK_TYPES, SERIALIZER_OPTIONS, cellIdOf } from "./vscode/serializer";
import { CellStatusProvider, KernelStatusItem, RemoteDecorations, type NotebookEntry } from "./vscode/status";

const CONTROLLER_LABEL = "DarkPyonix";

interface Entry extends NotebookEntry {
  host: VsNotebookHost;
  sink: VsExecSink;
  opening: Promise<void> | null;
}

export interface DarkPyonixApi {
  /** For tests: the connection of an open notebook. */
  connectionFor(nb: vscode.NotebookDocument): NotebookConnection | null;
  cellIdOf(cell: vscode.NotebookCell): string | undefined;
}

export function activate(ctx: vscode.ExtensionContext): DarkPyonixApi {
  const log = vscode.window.createOutputChannel("DarkPyonix", { log: true });
  const service = new ManagerService(ctx, log);
  const entries = new Map<string, Entry>();
  const controllers = new Map<string, vscode.NotebookController>();
  const selected = new Set<string>(); // notebook uris with our controller selected

  const isOurs = (nb: vscode.NotebookDocument) => (NOTEBOOK_TYPES as readonly string[]).includes(nb.notebookType);
  const lookup = (nb: vscode.NotebookDocument) => entries.get(nb.uri.toString());
  const statusProvider = new CellStatusProvider(lookup, () => service.identity().nickname);
  const decorations = new RemoteDecorations(lookup);
  const kernelItem = new KernelStatusItem(lookup);
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  const refreshUi = () => {
    if (refreshTimer) return;
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      statusProvider.refresh();
      decorations.update();
      kernelItem.update();
    }, 50);
  };

  ctx.subscriptions.push(log, decorations, kernelItem);
  for (const type of NOTEBOOK_TYPES) {
    ctx.subscriptions.push(vscode.workspace.registerNotebookSerializer(type, new DarkPyonixSerializer(), SERIALIZER_OPTIONS));
    ctx.subscriptions.push(vscode.notebooks.registerNotebookCellStatusBarItemProvider(type, statusProvider));

    const controller = vscode.notebooks.createNotebookController(`darkpyonix-kernel-${type}`, type, CONTROLLER_LABEL);
    controller.description = "File-bound DarkPyonix kernel";
    controller.detail = "Runs cells through the DarkPyonix manager; stop = interrupt";
    controller.supportedLanguages = ["python", "markdown"];
    controller.supportsExecutionOrder = true;
    controller.executeHandler = (cells, nb) => execute(cells, nb);
    controller.interruptHandler = (nb) => interrupt(nb);
    controller.onDidChangeSelectedNotebooks(({ notebook, selected: on }) => {
      if (on) selected.add(notebook.uri.toString());
      else selected.delete(notebook.uri.toString());
      if (on) lookup(notebook)?.host.flushPendingOutputs();
    }, null, ctx.subscriptions);
    controllers.set(type, controller);
    ctx.subscriptions.push(controller);
  }

  function controllerFor(nb: vscode.NotebookDocument): vscode.NotebookController | undefined {
    return controllers.get(nb.notebookType);
  }

  function entryFor(nb: vscode.NotebookDocument): Entry {
    let e = entries.get(nb.uri.toString());
    if (!e) {
      const ui = new NotebookUiState(refreshUi);
      const getController = () => (selected.has(nb.uri.toString()) ? controllerFor(nb) : undefined);
      e = {
        notebook: nb, conn: null, ui, opening: null,
        host: new VsNotebookHost(nb, getController, ui),
        sink: new VsExecSink(nb, getController, ui),
      };
      entries.set(nb.uri.toString(), e);
      controllerFor(nb)?.updateNotebookAffinity(nb, vscode.NotebookControllerAffinity.Preferred);
    }
    return e;
  }

  async function connect(nb: vscode.NotebookDocument, autoStart: boolean): Promise<Entry> {
    const e = entryFor(nb);
    if (e.conn) {
      if (autoStart) await e.conn.ensureAttached();
      return e;
    }
    if (!e.opening) {
      e.opening = (async () => {
        const client = await service.client();
        const cfg = vscode.workspace.getConfiguration("darkpyonix");
        const conn = new NotebookConnection(client, service.managerPath(nb.uri), e.host, e.sink, {
          python: cfg.get<string>("python") || undefined,
          lockIdleMs: Math.max(3, cfg.get<number>("lockIdleSeconds") ?? 20) * 1000,
          log: (l) => log.info(l),
        });
        e.conn = conn;
        await conn.open(autoStart);
      })().catch((err) => {
        e.conn = null;
        throw err;
      }).finally(() => {
        e.opening = null;
      });
    }
    await e.opening;
    if (autoStart) await e.conn!.ensureAttached();
    refreshUi();
    return e;
  }

  async function onOpen(nb: vscode.NotebookDocument): Promise<void> {
    if (!isOurs(nb) || nb.uri.scheme !== "file") return;
    const auto = vscode.workspace.getConfiguration("darkpyonix").get<boolean>("autoStartKernel") ?? false;
    try {
      await connect(nb, auto);
    } catch (err) {
      const e = entryFor(nb);
      e.ui.kernelState = "manager unavailable";
      refreshUi();
      log.warn(`open ${nb.uri.fsPath}: ${describe(err)}`);
    }
  }

  async function execute(cells: vscode.NotebookCell[], nb: vscode.NotebookDocument): Promise<void> {
    try {
      let e = entryFor(nb);
      if (!e.conn?.session && nb.isDirty) await nb.save(); // the new kernel parses the file on disk
      e = await connect(nb, true);
      await e.conn!.run(cells.map((c) => c.index));
    } catch (err) {
      log.error(`run: ${describe(err)}`);
      void vscode.window.showErrorMessage(`DarkPyonix: could not run (${describe(err)}).`, "Show Log").then((a) => a && log.show());
    }
  }

  async function interrupt(nb: vscode.NotebookDocument): Promise<void> {
    const conn = lookup(nb)?.conn;
    if (!conn?.kernel) return;
    try {
      const interrupted = await conn.interrupt();
      if (!interrupted) void vscode.window.setStatusBarMessage("DarkPyonix: nothing was running", 3000);
    } catch (err) {
      void vscode.window.showErrorMessage(`DarkPyonix: interrupt failed (${describe(err)}).`);
    }
  }

  function activeNotebook(): vscode.NotebookDocument | undefined {
    const nb = vscode.window.activeNotebookEditor?.notebook;
    return nb && isOurs(nb) ? nb : undefined;
  }

  async function withKernel(what: string, fn: (conn: NotebookConnection) => Promise<void>): Promise<void> {
    const nb = activeNotebook();
    const conn = nb && lookup(nb)?.conn;
    if (!conn?.kernel) {
      void vscode.window.showInformationMessage("DarkPyonix: no kernel is attached to this notebook.");
      return;
    }
    try {
      await fn(conn);
    } catch (err) {
      void vscode.window.showErrorMessage(`DarkPyonix: ${what} failed (${describe(err)}).`);
    }
  }

  ctx.subscriptions.push(
    vscode.commands.registerCommand("darkpyonix.openAsNotebook", async (uri?: vscode.Uri) => {
      const target = uri ?? vscode.window.activeTextEditor?.document.uri;
      if (!target) return;
      const type = target.path.endsWith(".pynb") ? "darkpyonix-notebook" : "darkpyonix-notebook-py";
      await vscode.commands.executeCommand("vscode.openWith", target, type);
    }),
    vscode.commands.registerCommand("darkpyonix.interruptKernel", async () => {
      const nb = activeNotebook();
      if (nb) await interrupt(nb);
    }),
    vscode.commands.registerCommand("darkpyonix.restartKernel", () => withKernel("restart", async (conn) => {
      await conn.restart(false);
      void vscode.window.setStatusBarMessage("DarkPyonix: kernel restarted (namespace cleared)", 3000);
    })),
    vscode.commands.registerCommand("darkpyonix.hardRestartKernel", () => withKernel("restart", async (conn) => {
      await conn.restart(true);
      await conn.session?.resync();
    })),
    vscode.commands.registerCommand("darkpyonix.shutdownKernel", () => withKernel("shutdown", async (conn) => {
      const ok = await vscode.window.showWarningMessage(
        "Shut down this file's kernel? Other clients attached to it lose their session too.", { modal: true }, "Shut Down",
      );
      if (ok === "Shut Down") await conn.shutdown();
    })),
    vscode.commands.registerCommand("darkpyonix.reloadDocument", () => withKernel("reload", async (conn) => {
      await conn.session?.resync();
    })),
    vscode.commands.registerCommand("darkpyonix.showOutputLog", () => log.show()),
    vscode.commands.registerCommand("darkpyonix.connect", async () => {
      const cfg = vscode.workspace.getConfiguration("darkpyonix");
      const pick = await vscode.window.showQuickPick([
        { label: "$(plug) Start or attach this notebook's kernel", id: "attach" },
        { label: "$(vm) Use a local manager (find or spawn)", id: "local" },
        { label: "$(remote) Use a dedicated manager (URL + token)…", id: "remote" },
      ], { placeHolder: cfg.get<string>("manager.url") ? `Current: ${cfg.get<string>("manager.url")}` : "Current: local manager" });
      if (!pick) return;
      if (pick.id === "remote") {
        const url = await vscode.window.showInputBox({ prompt: "Dedicated manager URL", value: cfg.get<string>("manager.url") || "https://" });
        if (!url) return;
        const token = await vscode.window.showInputBox({ prompt: "Token (master or share token)", password: true });
        if (token === undefined) return;
        await cfg.update("manager.url", url, vscode.ConfigurationTarget.Global);
        await ctx.secrets.store(TOKEN_SECRET, token);
        await reconnectAll();
      } else if (pick.id === "local") {
        await cfg.update("manager.url", "", vscode.ConfigurationTarget.Global);
        await reconnectAll();
      }
      const nb = activeNotebook();
      if (nb && pick.id === "attach") {
        try {
          if (!lookup(nb)?.conn?.session && nb.isDirty) await nb.save();
          await connect(nb, true);
        } catch (err) {
          void vscode.window.showErrorMessage(`DarkPyonix: ${describe(err)}`, "Show Log").then((a) => a && log.show());
        }
      }
    }),
  );

  async function reconnectAll(): Promise<void> {
    service.reset();
    for (const e of entries.values()) {
      const attached = !!e.conn?.session;
      await e.conn?.dispose();
      e.conn = null;
      try {
        await connect(e.notebook, attached);
      } catch (err) {
        log.warn(`reconnect ${e.notebook.uri.fsPath}: ${describe(err)}`);
      }
    }
  }

  // Editor → server: edits, focus and cursor.
  ctx.subscriptions.push(
    vscode.workspace.onDidOpenNotebookDocument((nb) => void onOpen(nb)),
    vscode.workspace.onDidCloseNotebookDocument(async (nb) => {
      const e = lookup(nb);
      if (!e) return;
      entries.delete(nb.uri.toString());
      await e.conn?.dispose();
      refreshUi();
    }),
    vscode.workspace.onDidChangeNotebookDocument((ev) => {
      lookup(ev.notebook)?.conn?.session?.editorChanged();
    }),
    vscode.window.onDidChangeTextEditorSelection((ev) => {
      const doc = ev.textEditor.document;
      if (doc.uri.scheme !== "vscode-notebook-cell") return;
      const nb = vscode.workspace.notebookDocuments.find((n) => isOurs(n) && n.uri.fsPath === doc.uri.fsPath);
      const cell = nb?.getCells().find((c) => c.document === doc);
      const session = nb && lookup(nb)?.conn?.session;
      const id = cell && cellIdOf(cell);
      if (!session || !id) return;
      const s = ev.selections[0];
      session.focusChanged(id, {
        cell_id: id, line: s.active.line, column: s.active.character,
        selection: s.isEmpty ? null : [[s.start.line, s.start.character], [s.end.line, s.end.character]],
      });
    }),
    vscode.window.onDidChangeActiveTextEditor((editor) => {
      // Leaving a cell editor for something that is not a cell of the same notebook: blur.
      for (const e of entries.values()) {
        const session = e.conn?.session;
        if (!session) continue;
        const inThis = editor?.document.uri.scheme === "vscode-notebook-cell" && editor.document.uri.fsPath === e.notebook.uri.fsPath;
        if (!inThis && vscode.window.activeNotebookEditor?.notebook !== e.notebook) session.focusChanged(null);
      }
      refreshUi();
    }),
    vscode.window.onDidChangeNotebookEditorSelection((ev) => {
      const session = lookup(ev.notebookEditor.notebook)?.conn?.session;
      const range = ev.selections[0];
      if (!session || !range || range.isEmpty) return;
      const cell = ev.notebookEditor.notebook.cellAt(range.start);
      session.focusChanged(cellIdOf(cell) ?? null);
    }),
    vscode.window.onDidChangeActiveNotebookEditor(() => refreshUi()),
    vscode.window.onDidChangeVisibleTextEditors(() => refreshUi()),
    vscode.workspace.onDidChangeConfiguration((ev) => {
      if (ev.affectsConfiguration("darkpyonix.manager") || ev.affectsConfiguration("darkpyonix.home") || ev.affectsConfiguration("darkpyonix.cliPath")) {
        void reconnectAll();
      }
    }),
    { dispose: () => { for (const e of entries.values()) void e.conn?.dispose(); } },
  );

  for (const nb of vscode.workspace.notebookDocuments) void onOpen(nb);

  return { connectionFor: (nb) => lookup(nb)?.conn ?? null, cellIdOf };
}

export function deactivate(): void {
  // Disposables registered on the context release locks and leave presence.
}

function describe(err: unknown): string {
  if (err instanceof ApiError) return `${err.code}: ${err.message}`;
  return (err as Error)?.message ?? String(err);
}
