import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { watch, type FSWatcher } from "node:fs";
import path from "node:path";

import type { AgentDefinition } from "../define-agent.js";
import { sendJson } from "./http.js";
import {
  createPlasmApp,
  handlePlasmOperatorRequest,
  handlePlasmRequest,
  type PlasmApp,
} from "../server/plasm-handler.js";

export interface DevServerOptions {
  agentRoot: string;
  definition: AgentDefinition;
  port?: number;
  host?: string;
  tenantScope?: string;
  maxSteps?: number;
  telemetry?: boolean;
}

export interface DevServerHandle {
  server: Server;
  url: string;
  close(): Promise<void>;
  reload(): Promise<void>;
}

const HOT_RELOAD_PATHS = [
  "catalogs",
  "skills",
  "channels",
  "schedules",
  "hooks",
  "subagents",
  "instructions.md",
  "instructions.ts",
] as const;

function watchHotReload(
  agentRoot: string,
  onChange: () => void,
): FSWatcher[] {
  const watchers: FSWatcher[] = [];
  let timer: ReturnType<typeof setTimeout> | undefined;

  const schedule = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      onChange();
    }, 150);
  };

  for (const relative of HOT_RELOAD_PATHS) {
    const target = path.join(agentRoot, relative);
    try {
      const recursive = !relative.endsWith(".md") && !relative.endsWith(".ts");
      const watcher = watch(target, { recursive }, schedule);
      watchers.push(watcher);
    } catch {
      // Missing slot at dev start — discovery will report warnings.
    }
  }

  return watchers;
}

async function routeDevRequest(
  req: IncomingMessage,
  res: ServerResponse,
  app: PlasmApp,
): Promise<void> {
  const url = new URL(req.url ?? "/", "http://localhost");
  if (url.pathname === "/operator" || url.pathname.startsWith("/operator/")) {
    await handlePlasmOperatorRequest(req, res, app);
    return;
  }
  await handlePlasmRequest(req, res, app);
}

export async function createDevServer(options: DevServerOptions): Promise<DevServerHandle> {
  const agentRoot = path.resolve(options.agentRoot);

  const app = await createPlasmApp({
    agentRoot,
    definition: options.definition,
    mode: "dev",
    tenantScope: options.tenantScope,
    maxSteps: options.maxSteps,
    telemetry: options.telemetry,
    sessions: true,
  });

  const reload = async (): Promise<void> => {
    await app.reload();
  };

  const watchers = watchHotReload(agentRoot, () => {
    void reload().catch((err) => {
      console.error("[plasm:dev] hot reload failed:", err);
    });
  });

  const host = options.host ?? "127.0.0.1";
  const port = options.port ?? 3000;

  const server = createServer((req, res) => {
    void routeDevRequest(req, res, app).catch((err) => {
      console.error("[plasm:dev] request error:", err);
      sendJson(res, 500, { error: "internal_error", message: String(err) });
    });
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => resolve());
  });

  const address = server.address();
  const resolvedPort =
    typeof address === "object" && address ? address.port : port;
  const url = `http://${host}:${resolvedPort}`;

  return {
    server,
    url,
    reload,
    close: () =>
      new Promise((resolve, reject) => {
        for (const watcher of watchers) watcher.close();
        server.close((err) => (err ? reject(err) : resolve()));
      }),
  };
}

export async function startDevServer(options: DevServerOptions): Promise<DevServerHandle> {
  const handle = await createDevServer(options);
  console.log(`[plasm:dev] listening on ${handle.url}`);
  console.log(`[plasm:dev] GET  ${handle.url}/plasm/v1/info`);
  console.log(`[plasm:dev] POST ${handle.url}/plasm/v1/session`);
  console.log(`[plasm:dev] GET  ${handle.url}/plasm/v1/session/:id/stream`);
  console.log(`[plasm:dev] GET  ${handle.url}/operator`);
  return handle;
}
