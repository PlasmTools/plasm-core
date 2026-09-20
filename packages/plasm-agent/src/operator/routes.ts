import { readFile } from "node:fs/promises";
import path from "node:path";

import { CatalogManifestSchema, FilesystemCatalogLoader } from "../catalog/loader.js";
import type { LoadedCatalog } from "../catalog/loader.js";
import { isNativeEngineAvailable } from "../engine/napi-binding.js";
import { AgentRuntime } from "../runtime/agent-runtime.js";
import { createAgentStateStore } from "../state/define-state.js";
import {
  stubFreshness,
} from "../stubs/generator.js";

import type {
  OperatorCatalogsResponse,
  OperatorHealthResponse,
  OperatorOpsResponse,
  OperatorPlansResponse,
  OperatorSessionsResponse,
} from "./types.js";

export type NitroHandler = (event: {
  req: { method?: string; url?: string };
  res: { statusCode: number; end: (body: string) => void };
}) => void | Promise<void>;

export interface OperatorRouteContext {
  agentRoot: string;
  tenantScope?: string;
  /** Pre-bootstrapped runtime; when omitted, catalog routes bootstrap on first request. */
  runtime?: AgentRuntime;
}

export interface OperatorHandler {
  health(): Promise<OperatorHealthResponse>;
  listCatalogs(): Promise<OperatorCatalogsResponse>;
  listSessions(): Promise<OperatorSessionsResponse>;
  listPlans(): Promise<OperatorPlansResponse>;
  ops(): Promise<OperatorOpsResponse>;
  listTraces(): Promise<{ traces: unknown[] }>;
  listArchives(): Promise<{ plans: unknown[]; runs: unknown[]; paths: unknown }>;
  listRuns(): Promise<{ runs: unknown[] }>;
}

interface BootstrappedCatalog {
  catalog: LoadedCatalog;
  catalogCgsHash: string;
  authScheme?: string;
  entityCount: number;
  capabilityCount: number;
}

async function bootstrapCatalogs(ctx: OperatorRouteContext): Promise<BootstrappedCatalog[]> {
  let runtime = ctx.runtime;
  if (!runtime) {
    runtime = new AgentRuntime({ agentRoot: ctx.agentRoot });
    await runtime.bootstrap();
    ctx.runtime = runtime;
  }

  const loader = new FilesystemCatalogLoader();
  const discovered = await loader.discover(ctx.agentRoot);
  const loaded = runtime.listCatalogs();

  const byEntry = new Map<string, LoadedCatalog>();
  for (const catalog of [...discovered, ...loaded]) {
    byEntry.set(catalog.manifest.entryId, catalog);
  }

  const bootstrapped: BootstrappedCatalog[] = [];
  for (const catalog of byEntry.values()) {
    const manifest = CatalogManifestSchema.parse(JSON.parse(await readFile(catalog.manifestPath, "utf8")));
    const parsed = JSON.parse(await readFile(path.join(catalog.rootDir, manifest.cgs_json), "utf8")) as { auth?: { scheme?: string }; entities: Record<string, unknown>; capabilities: Record<string, unknown> };
    const catalogCgsHash = manifest.cgs_hash;
    bootstrapped.push({
      catalog: {
        ...catalog,
        manifest: { ...catalog.manifest, cgsHash: catalogCgsHash },
      },
      catalogCgsHash,
      authScheme: parsed.auth?.scheme,
      entityCount: Object.keys(parsed.entities).length,
      capabilityCount: Object.keys(parsed.capabilities).length,
    });
  }

  bootstrapped.sort((a, b) => a.catalog.manifest.entryId.localeCompare(b.catalog.manifest.entryId));
  return bootstrapped;
}

export function createOperatorRoutes(ctx: OperatorRouteContext): OperatorHandler {
  return {
    async health() {
      return { status: "ok" };
    },

    async listCatalogs() {
      const bootstrapped = await bootstrapCatalogs(ctx);
      const stubsDir = path.join(ctx.agentRoot, ".plasm", "stubs");
      const catalogs = await Promise.all(
        bootstrapped.map(async (item) => {
          const stubPath = path.join(stubsDir, `${item.catalog.manifest.entryId}.ts`);
          const stub = await stubFreshness(item.catalogCgsHash, stubPath);
          return {
            entryId: item.catalog.manifest.entryId,
            label: item.catalog.manifest.label ?? item.catalog.manifest.entryId,
            rootDir: item.catalog.rootDir,
            catalogCgsHash: item.catalogCgsHash,
            authScheme: item.authScheme,
            entityCount: item.entityCount,
            capabilityCount: item.capabilityCount,
            stub,
          };
        }),
      );
      return { catalogs, generatedAt: new Date().toISOString() };
    },

    async listSessions() {
      const store = createAgentStateStore({
        agentRoot: ctx.agentRoot,
        tenantScope: ctx.tenantScope ?? "local",
      });
      const states = await store.listSessions();
      return { sessions: states.map((state) => ({
        intent: state.intent,
        logicalSessionRef: state.logicalSessionRef,
        logicalSessionId: state.logicalSessionId,
        waveCount: state.waves.length,
        seedCount: state.seeds.length,
        planCommitCount: state.planCommits.length,
        updatedAt: state.updatedAt,
      })) };
    },

    async listPlans() {
      const store = createAgentStateStore({
        agentRoot: ctx.agentRoot,
        tenantScope: ctx.tenantScope ?? "local",
      });
      const states = await store.listSessions();
      const plans = [];
      for (const state of states) {
        for (const commit of state.planCommits) {
          plans.push({
            intent: state.intent,
            logicalSessionRef: state.logicalSessionRef,
            ref: commit.ref,
            program: commit.program,
            at: commit.at,
          });
        }
      }
      plans.sort((a, b) => b.at.localeCompare(a.at));
      return { plans };
    },

    async ops() {
      const bootstrapped = await bootstrapCatalogs(ctx);
      const store = createAgentStateStore({
        agentRoot: ctx.agentRoot,
        tenantScope: ctx.tenantScope ?? "local",
      });
      const states = await store.listSessions();
      let planCommitCount = 0;
      for (const state of states) {
        planCommitCount += state.planCommits.length;
      }
      const native = isNativeEngineAvailable();
      return {
        nativeEngineAvailable: native,
        engineMode: native ? "napi" : "stub",
        agentRoot: ctx.agentRoot,
        catalogCount: bootstrapped.length,
        sessionCount: states.length,
        planCommitCount,
      };
    },

    async listTraces() {
      const { createArchiveStore } = await import("../archive/resolve-backend.js");
      const archive = createArchiveStore(ctx.agentRoot);
      const tenantScope = ctx.tenantScope ?? "local";
      const traces = await archive.listTraces(tenantScope);
      return { traces };
    },

    async listArchives() {
      const { createArchiveStore } = await import("../archive/resolve-backend.js");
      const archive = createArchiveStore(ctx.agentRoot);
      const { plans, runs, paths } = await archive.listArchives();
      return { plans, runs, paths };
    },

    async listRuns() {
      const { createArchiveStore } = await import("../archive/resolve-backend.js");
      const archive = createArchiveStore(ctx.agentRoot);
      const runs = await archive.listRuns();
      return { runs };
    },
  };
}

function routePath(url: string): string {
  const pathOnly = url.split("?")[0] ?? url;
  if (pathOnly.startsWith("/operator/")) {
    return pathOnly.slice("/operator".length);
  }
  return pathOnly;
}

function matchesRoute(pathOnly: string, route: string): boolean {
  return pathOnly === route || pathOnly.startsWith(`${route}?`);
}

export function nitroOperatorHandler(ctx: OperatorRouteContext): NitroHandler {
  const routes = createOperatorRoutes(ctx);
  return async (event) => {
    const pathOnly = routePath(event.req.url ?? "/");

    if (matchesRoute(pathOnly, "/health") || matchesRoute(pathOnly, "/ops/health")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.health()));
      return;
    }
    if (matchesRoute(pathOnly, "/catalogs")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.listCatalogs()));
      return;
    }
    if (matchesRoute(pathOnly, "/sessions")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.listSessions()));
      return;
    }
    if (matchesRoute(pathOnly, "/plans")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.listPlans()));
      return;
    }
    if (matchesRoute(pathOnly, "/ops")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.ops()));
      return;
    }
    if (matchesRoute(pathOnly, "/traces")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.listTraces()));
      return;
    }
    if (matchesRoute(pathOnly, "/archives")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.listArchives()));
      return;
    }
    if (matchesRoute(pathOnly, "/runs")) {
      event.res.statusCode = 200;
      event.res.end(JSON.stringify(await routes.listRuns()));
      return;
    }

    event.res.statusCode = 404;
    event.res.end(JSON.stringify({ error: "not_found", path: pathOnly }));
  };
}
