import { readFile } from "node:fs/promises";
import path from "node:path";

import { FilesystemCatalogLoader } from "../catalog/loader.js";
import type { LoadedCatalog } from "../catalog/loader.js";
import { isNativeEngineAvailable } from "../engine/napi-binding.js";
import { AgentRuntime } from "../runtime/agent-runtime.js";
import { createAgentStateStore } from "../state/define-state.js";
import {
  parseDomainYaml,
  stubFreshness,
} from "../stubs/generator.js";
import { resolveCatalogLiveHash } from "../stubs/catalog-hash.js";

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

async function resolveLiveHash(catalog: LoadedCatalog, runtime?: AgentRuntime): Promise<string> {
  if (catalog.manifest.cgsHash) return catalog.manifest.cgsHash;
  if (runtime) {
    const loaded = runtime.listCatalogs().find((c) => c.manifest.entryId === catalog.manifest.entryId);
    if (loaded?.manifest.cgsHash) return loaded.manifest.cgsHash;
  }
  return resolveCatalogLiveHash(catalog.rootDir);
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
    const domainYaml = await readFile(path.join(catalog.rootDir, "domain.yaml"), "utf8");
    const parsed = parseDomainYaml(domainYaml, path.basename(catalog.rootDir));
    const catalogCgsHash = await resolveLiveHash(catalog, runtime);
    bootstrapped.push({
      catalog: {
        ...catalog,
        manifest: { ...catalog.manifest, cgsHash: catalogCgsHash },
      },
      catalogCgsHash,
      authScheme: parsed.authScheme,
      entityCount: parsed.entities.length,
      capabilityCount: parsed.capabilities.length,
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
      const intents = await store.listIntents();
      const sessions = await Promise.all(
        intents.map(async (intent) => {
          const state = await store.get(intent);
          if (!state) return null;
          return {
            intent: state.intent,
            logicalSessionRef: state.logicalSessionRef,
            logicalSessionId: state.logicalSessionId,
            waveCount: state.waves.length,
            seedCount: state.seeds.length,
            planCommitCount: state.planCommits.length,
            updatedAt: state.updatedAt,
          };
        }),
      );
      return {
        sessions: sessions.filter((s): s is NonNullable<typeof s> => s !== null),
      };
    },

    async listPlans() {
      const store = createAgentStateStore({
        agentRoot: ctx.agentRoot,
        tenantScope: ctx.tenantScope ?? "local",
      });
      const intents = await store.listIntents();
      const plans = [];
      for (const intent of intents) {
        const state = await store.get(intent);
        if (!state) continue;
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
      const intents = await store.listIntents();
      let planCommitCount = 0;
      for (const intent of intents) {
        const state = await store.get(intent);
        planCommitCount += state?.planCommits.length ?? 0;
      }
      const native = isNativeEngineAvailable();
      return {
        nativeEngineAvailable: native,
        engineMode: native ? "napi" : "stub",
        agentRoot: ctx.agentRoot,
        catalogCount: bootstrapped.length,
        sessionCount: intents.length,
        planCommitCount,
      };
    },

    async listTraces() {
      const { LocalArchiveStore } = await import("../archive/index.js");
      const archive = LocalArchiveStore.fromAgentRoot(ctx.agentRoot);
      const tenantScope = ctx.tenantScope ?? "local";
      const traces = await archive.listTraces(tenantScope);
      return { traces };
    },

    async listArchives() {
      const { LocalArchiveStore } = await import("../archive/index.js");
      const archive = LocalArchiveStore.fromAgentRoot(ctx.agentRoot);
      const { plans, runs, paths } = await archive.listArchives();
      return { plans, runs, paths };
    },

    async listRuns() {
      const { LocalArchiveStore } = await import("../archive/index.js");
      const archive = LocalArchiveStore.fromAgentRoot(ctx.agentRoot);
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
