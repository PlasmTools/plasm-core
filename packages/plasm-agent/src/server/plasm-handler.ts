import type { IncomingMessage, ServerResponse } from "node:http";
import path from "node:path";

import type { AgentDefinition } from "../define-agent.js";
import {
  createAgentFromDefinition,
  resolveAgentDefinition,
} from "../define-agent.js";
import { readBuildManifest } from "../cli/build-manifest.js";
import { createAuthoringContext, type AuthoringContext } from "../authoring/context.js";
import { tryHandleChannelRoute } from "../authoring/channel-dispatch.js";
import {
  loadAuthoredSlots,
  type LoadedProjectSlots,
} from "../authoring/slot-loader.js";
import {
  createSubagentRegistry,
  loadSubagents,
  summarizeSubagents,
  type SubagentRegistry,
} from "../authoring/subagent-loader.js";
import {
  tryHandleScheduleDevDispatch,
  startScheduleTimers,
  type ScheduleHandle,
} from "../authoring/schedule-manager.js";
import { walkAgentProject, type ProjectDiscovery } from "../discovery/project-walker.js";
import { collectProjectInfo } from "../project-info.js";
import type { PlasmAgent } from "../runtime/plasm-agent.js";
import { DevSessionStore } from "../dev/dev-session.js";
import { sendJson } from "../dev/http.js";
import { tryHandleSessionRoutes } from "../dev/session-routes.js";
import { nitroOperatorHandler } from "../operator/routes.js";
import { renderOperatorShell } from "../operator/ui-shell.js";

export type PlasmAppMode = "dev" | "prod";

export interface PlasmAppOptions {
  agentRoot: string;
  definition: AgentDefinition;
  mode?: PlasmAppMode;
  tenantScope?: string;
  maxSteps?: number;
  telemetry?: boolean;
  /** Session SSE routes; default true in dev, false in prod. */
  sessions?: boolean;
}

export interface PlasmApp {
  agentRoot: string;
  projectRoot: string;
  definition: AgentDefinition;
  mode: PlasmAppMode;
  sessionsEnabled: boolean;
  reload(): Promise<void>;
  getAgent(): Promise<PlasmAgent>;
  getDiscovery(): Promise<ProjectDiscovery>;
  getLoadedSlots(): Promise<LoadedProjectSlots | undefined>;
  getSubagents(): Promise<ReturnType<typeof summarizeSubagents>>;
  getAuthoringContext(): AuthoringContext;
  sessionStore: DevSessionStore | undefined;
  handleRequest(req: IncomingMessage, res: ServerResponse): Promise<void>;
  handleOperatorRequest(req: IncomingMessage, res: ServerResponse): Promise<void>;
}

export type VercelHandler = (
  req: IncomingMessage,
  res: ServerResponse,
) => void | Promise<void>;

/** Strip `/api` prefix from Vercel catch-all rewrites so routes match dev paths. */
export function normalizePlasmPathname(url: string | undefined): string {
  const pathname = new URL(url ?? "/", "http://localhost").pathname;
  if (pathname === "/api" || pathname === "/api/") return "/";
  if (pathname.startsWith("/api/")) return pathname.slice(4) || "/";
  return pathname;
}

/** Resolve request path on Vercel (rewrite-to-/api preserves public path in req.url). */
function vercelIncomingUrl(req: IncomingMessage): string {
  for (const key of [
    "x-vercel-original-url",
    "x-middleware-request-url",
    "x-forwarded-uri",
  ] as const) {
    const raw = req.headers[key];
    if (typeof raw === "string" && raw.length > 0) {
      return raw.startsWith("/") ? raw : new URL(raw, "http://localhost").pathname;
    }
  }
  return req.url ?? "/";
}

export function rewriteRequestPath(req: IncomingMessage): IncomingMessage {
  const original = vercelIncomingUrl(req);
  const pathname = normalizePlasmPathname(original);
  const query = original.includes("?") ? original.slice(original.indexOf("?")) : "";
  const proxy = Object.create(req) as IncomingMessage;
  Object.defineProperty(proxy, "url", {
    value: `${pathname}${query}`,
    writable: true,
    configurable: true,
  });
  return proxy;
}

export async function handlePlasmRequest(
  req: IncomingMessage,
  res: ServerResponse,
  app: PlasmApp,
): Promise<void> {
  const method = req.method ?? "GET";
  const url = new URL(req.url ?? "/", "http://localhost");

  if (method === "GET" && url.pathname === "/plasm/v1/health") {
    sendJson(res, 200, { status: "ok" });
    return;
  }

  if (method === "GET" && url.pathname === "/plasm/v1/info") {
    const discovery = await app.getDiscovery();
    const loadedSlots = await app.getLoadedSlots();
    const subagents = await app.getSubagents();
    if (!loadedSlots) {
      sendJson(res, 503, { error: "slots_not_ready" });
      return;
    }
    const info = await collectProjectInfo({
      projectRoot: app.projectRoot,
      agentRoot: app.agentRoot,
      cached: { discovery, loadedSlots, subagents },
      dev:
        app.mode === "dev"
          ? {
              definition: app.definition,
              sessionCount: app.sessionStore?.list().length ?? 0,
            }
          : undefined,
    });
    sendJson(res, 200, info);
    return;
  }

  const loadedSlots = await app.getLoadedSlots();
  if (loadedSlots?.schedules.length) {
    const handledSchedule = tryHandleScheduleDevDispatch(
      req,
      res,
      loadedSlots.schedules,
      app.getAuthoringContext(),
    );
    if (handledSchedule) return;
  }

  if (loadedSlots?.channels.length) {
    const handled = tryHandleChannelRoute(
      req,
      res,
      loadedSlots.channels,
      app.getAuthoringContext(),
    );
    if (handled) return;
  }

  if (
    app.sessionsEnabled &&
    app.sessionStore &&
    (await tryHandleSessionRoutes(req, res, url, {
      sessionStore: app.sessionStore,
      getAgent: app.getAgent,
    }))
  ) {
    return;
  }

  sendJson(res, 404, { error: "not_found", path: url.pathname });
}

export async function handlePlasmOperatorRequest(
  req: IncomingMessage,
  res: ServerResponse,
  app: PlasmApp,
): Promise<void> {
  const url = new URL(req.url ?? "/", "http://localhost");
  if (req.method === "GET" && (url.pathname === "/operator" || url.pathname === "/operator/")) {
    res.statusCode = 200;
    res.setHeader("content-type", "text/html; charset=utf-8");
    res.end(renderOperatorShell("/operator"));
    return;
  }

  const operatorHandler = nitroOperatorHandler({
    agentRoot: app.agentRoot,
    runtime: undefined,
  });

  const opReq = {
    req: { method: req.method, url: url.pathname + url.search },
    res: {
      statusCode: 200,
      end: (body: string) => {
        res.statusCode = opReq.res.statusCode;
        res.setHeader("content-type", "application/json; charset=utf-8");
        res.end(body);
      },
    },
  };
  await operatorHandler(opReq);
}

export async function createPlasmApp(options: PlasmAppOptions): Promise<PlasmApp> {
  const agentRoot = path.resolve(options.agentRoot);
  const projectRoot = path.dirname(agentRoot);
  const mode = options.mode ?? "prod";
  const sessionsEnabled = options.sessions ?? mode === "dev";
  const definition = resolveAgentDefinition(options.definition, agentRoot);
  const sessionStore = sessionsEnabled ? new DevSessionStore() : undefined;

  let agent: PlasmAgent | undefined;
  let discovery: ProjectDiscovery | undefined;
  let loadedSlots: LoadedProjectSlots | undefined;
  let subagentRegistry: SubagentRegistry | undefined;
  let subagentSummary: ReturnType<typeof summarizeSubagents> = [];
  let scheduleHandle: ScheduleHandle | undefined;
  let importCacheBust = Date.now();

  const getAuthoringContext = () =>
    createAuthoringContext({
      agentRoot,
      getAgent: async () => agent ?? bootstrap(),
      importCacheBust,
    });

  const bootstrap = async (): Promise<PlasmAgent> => {
    agent = await createAgentFromDefinition(definition, {
      agentRoot,
      tenantScope: options.tenantScope,
      maxSteps: options.maxSteps,
      telemetry: options.telemetry,
      loadedSlots,
      subagentRegistry,
      getAuthoringContext,
    });
    return agent;
  };

  const refreshDiscovery = async (): Promise<ProjectDiscovery> => {
    discovery = await walkAgentProject(agentRoot);
    return discovery;
  };

  const refreshSlots = async (): Promise<LoadedProjectSlots> => {
    const currentDiscovery = discovery ?? (await refreshDiscovery());
    const buildManifest = await readBuildManifest(agentRoot);
    loadedSlots = await loadAuthoredSlots({
      discovery: currentDiscovery,
      importCacheBust,
      agentRoot,
      projectRoot,
      compiledSlots: buildManifest?.compiledSlots,
    });
    return loadedSlots;
  };

  const refreshSubagents = async (): Promise<SubagentRegistry> => {
    const currentDiscovery = discovery ?? (await refreshDiscovery());
    const loaded = await loadSubagents({
      discovery: currentDiscovery,
      parentSlots: loadedSlots,
      tenantScope: options.tenantScope,
      maxSteps: options.maxSteps,
      telemetry: options.telemetry,
      importCacheBust,
    });
    if (loadedSlots) {
      loadedSlots.diagnostics.push(...loaded.diagnostics);
    }
    subagentRegistry = createSubagentRegistry(loaded.subagents);
    subagentSummary = summarizeSubagents(loaded.subagents, agentRoot);
    return subagentRegistry;
  };

  const reload = async (): Promise<void> => {
    importCacheBust = Date.now();
    scheduleHandle?.stop();
    await refreshDiscovery();
    await refreshSlots();
    await refreshSubagents();
    await bootstrap();
    if (mode === "dev") {
      scheduleHandle = startScheduleTimers(
        loadedSlots?.schedules ?? [],
        getAuthoringContext(),
        definition.experimental?.workflow,
      );
    }
    if (loadedSlots?.hookRunner) {
      await loadedSlots.hookRunner.emit("agent:start", getAuthoringContext(), {
        reason: mode === "dev" ? "hot_reload" : "bootstrap",
      });
    }
  };

  await reload();

  const getAgent = async () => agent ?? bootstrap();
  const getDiscovery = async () => discovery ?? refreshDiscovery();
  const getLoadedSlots = async () => loadedSlots ?? refreshSlots();
  const getSubagents = async () => {
    if (!subagentRegistry) await refreshSubagents();
    return subagentSummary;
  };

  const app: PlasmApp = {
    agentRoot,
    projectRoot,
    definition,
    mode,
    sessionsEnabled,
    reload,
    getAgent,
    getDiscovery,
    getLoadedSlots,
    getSubagents,
    getAuthoringContext,
    sessionStore,
    handleRequest: (req, res) => handlePlasmRequest(req, res, app),
    handleOperatorRequest: (req, res) => handlePlasmOperatorRequest(req, res, app),
  };

  return app;
}

export function vercelPlasmHandler(app: PlasmApp): VercelHandler {
  return async (req, res) => {
    const proxied = rewriteRequestPath(req);
    const url = new URL(proxied.url ?? "/", "http://localhost");
    try {
      if (url.pathname === "/operator" || url.pathname.startsWith("/operator/")) {
        await app.handleOperatorRequest(proxied, res);
        return;
      }
      await app.handleRequest(proxied, res);
    } catch (err) {
      console.error("[plasm:vercel] request error:", err);
      if (!res.writableEnded) {
        sendJson(res, 500, { error: "internal_error", message: String(err) });
      }
    }
  };
}
