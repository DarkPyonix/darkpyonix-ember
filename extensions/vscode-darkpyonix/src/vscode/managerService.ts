// Settings, per-machine identity and the manager endpoint (FR-C1) shared by all notebooks.
import { randomBytes } from "node:crypto";
import { realpathSync } from "node:fs";
import { hostname } from "node:os";
import * as vscode from "vscode";
import { ManagerClient, type ClientIdentity } from "../manager/client";
import { darkpyonixHome, mapPath, resolveManager, type ManagerEndpoint } from "../manager/discovery";

const CLIENT_ID_KEY = "darkpyonix.clientId";
export const TOKEN_SECRET = "darkpyonix.manager.token";

export class ManagerService {
  private endpoint: Promise<ManagerEndpoint> | null = null;

  constructor(private readonly ctx: vscode.ExtensionContext, private readonly log: vscode.LogOutputChannel) {}

  private cfg(): vscode.WorkspaceConfiguration {
    return vscode.workspace.getConfiguration("darkpyonix");
  }

  /** Stable per-machine client id (FR-S4), stored in global state. */
  identity(): ClientIdentity {
    let id = this.ctx.globalState.get<string>(CLIENT_ID_KEY);
    if (!id || !/^[A-Za-z0-9_-]{8,64}$/.test(id)) {
      id = `vsc-${randomBytes(8).toString("hex")}`;
      void this.ctx.globalState.update(CLIENT_ID_KEY, id);
    }
    const nickname = (this.cfg().get<string>("nickname") || hostname() || "vscode").slice(0, 64);
    return { clientId: id, nickname };
  }

  isRemote(): boolean {
    return !!this.cfg().get<string>("manager.url");
  }

  /** The file's path as the manager sees it. */
  managerPath(uri: vscode.Uri): string {
    if (this.isRemote()) return mapPath(uri.fsPath, this.cfg().get<Record<string, string>>("manager.pathMap") ?? {});
    try {
      return realpathSync.native(uri.fsPath);
    } catch {
      return uri.fsPath;
    }
  }

  reset(): void {
    this.endpoint = null;
  }

  async client(): Promise<ManagerClient> {
    if (!this.endpoint) {
      this.endpoint = this.resolve();
      this.endpoint.catch(() => { this.endpoint = null; });
    }
    const ep = await this.endpoint;
    return new ManagerClient(ep.url, ep.token, this.identity());
  }

  private async resolve(): Promise<ManagerEndpoint> {
    const cfg = this.cfg();
    const configuredHome = cfg.get<string>("home") || "";
    const remoteUrl = cfg.get<string>("manager.url") || undefined;
    const remoteToken = remoteUrl ? (await this.ctx.secrets.get(TOKEN_SECRET)) || cfg.get<string>("manager.token") || undefined : undefined;
    const ep = await resolveManager({
      remoteUrl,
      remoteToken,
      home: darkpyonixHome(configuredHome),
      passHome: !!configuredHome,
      cliPath: cfg.get<string>("cliPath") || "darkpyonix",
      managerArgs: cfg.get<string[]>("managerArgs") ?? ["manager", "--ephemeral"],
      log: (l) => this.log.info(l),
    });
    this.log.info(`manager: ${ep.source} ${ep.url}${ep.pid ? ` (pid ${ep.pid})` : ""}`);
    return ep;
  }
}
