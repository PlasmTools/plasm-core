import path from "node:path";

import { listChannelRoutes } from "./authoring/channel-dispatch.js";
import { loadAuthoredSlots, summarizeLoadedSlots } from "./authoring/slot-loader.js";
import { loadSubagents, summarizeSubagents } from "./authoring/subagent-loader.js";
import type { AgentDefinition } from "./define-agent.js";
import type { ProjectDiscovery } from "./discovery/project-walker.js";
import { walkAgentProject } from "./discovery/project-walker.js";
import { isNativeEngineAvailable } from "./engine/napi-binding.js";
import { exportScheduleCronManifest } from "./authoring/schedule-manager.js";
import { isGatewayConfigured } from "./gateway-model.js";
import type { LoadedProjectSlots } from "./authoring/slot-loader.js";
import { frameworkPackageVersion } from "./package-version.js";
import { resolveCatalogLiveHash } from "./stubs/catalog-hash.js";
import { stubFreshness } from "./stubs/generator.js";

export const PLASM_LANGUAGE_TOOLS = [
  "discover_capabilities",
  "plasm_context",
  "plasm",
  "plasm_run",
] as const;

export const FRAMEWORK_NAME = "@plasm_lang/vercel-agent";
export const FRAMEWORK_VERSION = frameworkPackageVersion();

export interface CatalogInfoEntry {
  name: string;
  entryId?: string;
  stubPath: string;
  stubFresh?: Awaited<ReturnType<typeof stubFreshness>>;
}

export interface ProjectInfoRoutes {
  health: string;
  info: string;
  session: string;
  sessionContinue: string;
  sessionStream: string;
  channels: Array<{ method: string; path: string }>;
  scheduleCrons: string[];
  operator: string;
}

export interface ProjectInfoPayload {
  status: "ok" | "degraded";
  framework: string;
  version: string;
  projectRoot: string;
  agentRoot: string;
  packageName?: string;
  engine: {
    native: boolean;
    mode: "napi" | "stub";
  };
  gateway: {
    configured: boolean;
  };
  discovery: ProjectDiscovery;
  loadedSlots: ReturnType<typeof summarizeLoadedSlots> & {
    subagents: ReturnType<typeof summarizeSubagents>;
  };
  catalogs: CatalogInfoEntry[];
  diagnostics: Array<{
    level: string;
    slot: string;
    path: string;
    message: string;
  }>;
  languageTools: readonly string[];
  /** Present on dev-server `/plasm/v1/info` only. */
  dev?: {
    model: AgentDefinition["model"];
    compaction: AgentDefinition["compaction"] | null;
    modelOptions: AgentDefinition["modelOptions"] | null;
    build: AgentDefinition["build"] | null;
    experimental: AgentDefinition["experimental"] | null;
    scheduleCrons: ReturnType<typeof exportScheduleCronManifest> | null;
    sessions: { active: number };
    routes: ProjectInfoRoutes;
  };
}

export interface CollectProjectInfoOptions {
  projectRoot: string;
  agentRoot: string;
  packageName?: string;
  cached?: {
    discovery: ProjectDiscovery;
    loadedSlots: LoadedProjectSlots;
    subagents: ReturnType<typeof summarizeSubagents>;
  };
  dev?: {
    definition: AgentDefinition;
    sessionCount: number;
  };
}

async function catalogEntries(
  project: { projectRoot: string; agentRoot: string },
  discovery: ProjectDiscovery,
): Promise<CatalogInfoEntry[]> {
  const catalogs: CatalogInfoEntry[] = [];
  for (const catalog of discovery.catalogs) {
    const entryId = catalog.entryId ?? catalog.name;
    const stubPath = path.join(project.agentRoot, ".plasm", "stubs", `${entryId}.ts`);
    let stubFresh: Awaited<ReturnType<typeof stubFreshness>> | undefined;
    try {
      const liveHash = await resolveCatalogLiveHash(catalog.path);
      stubFresh = await stubFreshness(liveHash, stubPath);
    } catch {
      stubFresh = undefined;
    }
    catalogs.push({
      name: catalog.name,
      entryId: catalog.entryId,
      stubPath: path.relative(project.projectRoot, stubPath),
      stubFresh,
    });
  }
  return catalogs;
}

export async function collectProjectInfo(
  options: CollectProjectInfoOptions,
): Promise<ProjectInfoPayload> {
  const { projectRoot, agentRoot, packageName, cached, dev } = options;

  let discovery: ProjectDiscovery;
  let loadedSlots: LoadedProjectSlots;
  let subagentSummary: ReturnType<typeof summarizeSubagents>;
  let subDiagnostics: Array<{ level: string; slot: string; path: string; message: string }>;

  if (cached) {
    discovery = cached.discovery;
    loadedSlots = cached.loadedSlots;
    subagentSummary = cached.subagents;
    subDiagnostics = [];
  } else {
    discovery = await walkAgentProject(agentRoot);
    loadedSlots = await loadAuthoredSlots({ discovery });
    const loaded = await loadSubagents({ discovery, parentSlots: loadedSlots });
    subagentSummary = summarizeSubagents(loaded.subagents, agentRoot);
    subDiagnostics = loaded.diagnostics;
  }

  const diagnostics = [
    ...loadedSlots.diagnostics,
    ...subDiagnostics,
    ...discovery.diagnostics,
  ];
  const catalogs = await catalogEntries({ projectRoot, agentRoot }, discovery);
  const hasErrors = diagnostics.some((d) => d.level === "error");
  const native = isNativeEngineAvailable();

  const payload: ProjectInfoPayload = {
    status: hasErrors ? "degraded" : "ok",
    framework: FRAMEWORK_NAME,
    version: FRAMEWORK_VERSION,
    projectRoot,
    agentRoot,
    packageName,
    engine: {
      native,
      mode: native ? "napi" : "stub",
    },
    gateway: { configured: isGatewayConfigured() },
    discovery,
    loadedSlots: {
      ...summarizeLoadedSlots(loadedSlots, agentRoot),
      subagents: subagentSummary,
    },
    catalogs,
    diagnostics,
    languageTools: PLASM_LANGUAGE_TOOLS,
  };

  if (dev) {
    const scheduleCrons = exportScheduleCronManifest(loadedSlots.schedules);
    payload.dev = {
      model: dev.definition.model,
      compaction: dev.definition.compaction ?? null,
      modelOptions: dev.definition.modelOptions ?? null,
      build: dev.definition.build ?? null,
      experimental: dev.definition.experimental ?? null,
      scheduleCrons,
      sessions: { active: dev.sessionCount },
      routes: {
        health: "GET /plasm/v1/health",
        info: "GET /plasm/v1/info",
        session: "POST /plasm/v1/session",
        sessionContinue: "POST /plasm/v1/session/:sessionId",
        sessionStream: "GET /plasm/v1/session/:id/stream",
        channels: listChannelRoutes(loadedSlots.channels),
        scheduleCrons: scheduleCrons.crons.map((c) => `GET ${c.path}`),
        operator: "GET /operator",
      },
    };
  }

  return payload;
}

export function formatPlasmInfoHuman(info: ProjectInfoPayload): string {
  const lines: string[] = [];
  lines.push(`${info.framework}  status=${info.status}`);
  lines.push(`project: ${info.projectRoot}`);
  lines.push(`agent:   ${info.agentRoot}`);
  lines.push(`engine:  ${info.engine.mode}${info.engine.native ? " (native)" : " (fallback)"}`);
  lines.push(
    `gateway: ${info.gateway.configured ? "configured" : "missing — set AI_GATEWAY_API_KEY or run plasm-agent link"}`,
  );
  lines.push("");
  lines.push(`catalogs (${info.discovery.catalogs.length})`);
  for (const c of info.catalogs) {
    const fresh = c.stubFresh?.fresh ? "fresh" : c.stubFresh ? "stale" : "missing";
    lines.push(`  - ${c.name} entry_id=${c.entryId ?? "?"} stub=${fresh}`);
  }
  lines.push(`channels: ${info.discovery.channels.length}  schedules: ${info.discovery.schedules.length}`);
  lines.push(`skills: ${info.discovery.skills.length}  hooks: ${info.discovery.hooks.length}`);
  lines.push(`subagents: ${info.loadedSlots.subagents.length}`);
  if (info.diagnostics.length) {
    lines.push("");
    lines.push("diagnostics:");
    for (const d of info.diagnostics) {
      lines.push(`  [${d.level}] ${d.slot}: ${d.message} (${d.path})`);
    }
  }
  return lines.join("\n");
}
