import path from "node:path";
import { pathToFileURL } from "node:url";

import type { ResolvedAgentProject } from "./project-root.js";
import { startDevServer, type DevServerHandle } from "../dev/server.js";

export interface StartDevServerForProjectOptions {
  project: ResolvedAgentProject;
  port?: number;
  host?: string;
  tenantScope?: string;
  maxSteps?: number;
  telemetry?: boolean;
  /** `auto` attaches TUI when stdin is a TTY. Pass `false` for headless server only. */
  tui?: boolean | "auto";
}

function shouldAttachTui(tui?: boolean | "auto"): boolean {
  if (tui === false) return false;
  if (tui === true) return true;
  return Boolean(process.stdin.isTTY && !process.env.PLASM_DEV_NO_TUI);
}

export async function startDevServerForProject(
  options: StartDevServerForProjectOptions,
): Promise<DevServerHandle> {
  const agentModule = path.join(options.project.agentRoot, "agent.ts");
  const mod = await import(pathToFileURL(agentModule).href);
  const definition = mod.default;
  if (!definition) {
    throw new Error(`No default export in ${agentModule}`);
  }

  const handle = await startDevServer({
    agentRoot: options.project.agentRoot,
    definition,
    port: options.port ?? Number(process.env.PORT ?? 3000),
    host: options.host ?? process.env.PLASM_DEV_HOST ?? "127.0.0.1",
    tenantScope: options.tenantScope ?? process.env.PLASM_TENANT_SCOPE ?? "local",
    maxSteps: options.maxSteps ?? 20,
    telemetry: options.telemetry ?? process.env.PLASM_AGENT_TELEMETRY !== "0",
  });

  if (shouldAttachTui(options.tui)) {
    const { runDevTui } = await import("../dev/client/repl.js");
    await runDevTui({ baseUrl: handle.url });
    await handle.close();
    process.exit(0);
  }

  return handle;
}

export function installDevServerShutdown(handle: DevServerHandle): void {
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.on(signal, () => {
      void handle.close().finally(() => process.exit(0));
    });
  }
}
