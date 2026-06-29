import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { LoadedProjectSlots } from "../authoring/slot-loader.js";
import { listChannelRoutes } from "../authoring/channel-dispatch.js";
import { exportScheduleTaskManifest } from "../authoring/schedule-manager.js";
import type { AgentDefinition } from "../define-agent.js";
import type { ProjectDiscovery } from "../discovery/project-walker.js";
import { frameworkPackageVersion } from "../package-version.js";
import { plasmAgentSummaryPath } from "./paths.js";

export const VERCEL_PLASM_AGENT_SUMMARY_KIND = "vercel-plasm-agent-summary";
export const VERCEL_PLASM_AGENT_SUMMARY_VERSION = 3;

export interface PlasmAgentSummaryPayload {
  kind: typeof VERCEL_PLASM_AGENT_SUMMARY_KIND;
  schemaVersion: typeof VERCEL_PLASM_AGENT_SUMMARY_VERSION;
  generatorVersion: string;
  agent: {
    name: string;
    description: string | null;
    modelId: string;
  };
  instructions: {
    logicalPath: string;
    sourceKind: string;
    markdown: string | null;
  } | null;
  schedules: Array<{ name: string; cron: string; logicalPath: string }>;
  tools: Array<{ name: string; description: string; logicalPath: string }>;
  skills: Array<{ name: string; description: string; logicalPath: string; sourceKind: string }>;
  connections: [];
  channels: Array<{
    name: string;
    method: string;
    urlPath: string;
    type: string;
    logicalPath: string;
  }>;
  sandbox: null;
  subagents: Array<{ name: string; description: string; logicalPath: string }>;
  diagnostics: { errors: number; warnings: number };
}

function normalizeChannelKind(method: string): string {
  if (method === "WEBSOCKET") return "websocket";
  return "http";
}

export function buildPlasmAgentSummary(options: {
  projectRoot: string;
  agentRoot: string;
  packageName?: string;
  definition: AgentDefinition;
  discovery: ProjectDiscovery;
  loadedSlots: import("../authoring/slot-loader.js").LoadedProjectSlots;
  generatorVersion?: string;
}): PlasmAgentSummaryPayload {
  const { projectRoot, agentRoot, packageName, definition, discovery, loadedSlots } = options;
  const rel = (p: string) => path.relative(projectRoot, p);

  const instructions =
    discovery.instructions === undefined
      ? null
      : {
          logicalPath: rel(discovery.instructions.path),
          sourceKind: discovery.instructions.kind,
          markdown: null,
        };

  const channels = loadedSlots.channels.flatMap((channel) =>
    channel.definition.routes.map((route) => ({
      name: path.basename(channel.sourcePath, path.extname(channel.sourcePath)),
      method: route.method,
      urlPath: route.path,
      type: normalizeChannelKind(route.method),
      logicalPath: rel(channel.sourcePath),
    })),
  );

  const scheduleManifest = exportScheduleTaskManifest(loadedSlots.schedules);

  const diagnostics = [...loadedSlots.diagnostics, ...discovery.diagnostics];

  return {
    kind: VERCEL_PLASM_AGENT_SUMMARY_KIND,
    schemaVersion: VERCEL_PLASM_AGENT_SUMMARY_VERSION,
    generatorVersion: options.generatorVersion ?? frameworkPackageVersion(),
    agent: {
      name: packageName ?? path.basename(path.dirname(agentRoot)),
      description: null,
      modelId: definition.model,
    },
    instructions,
    schedules: scheduleManifest.tasks.map((task) => ({
      name: task.name,
      cron: task.cron,
      logicalPath: rel(
        loadedSlots.schedules.find((s) => s.definition.name === task.name)?.sourcePath ??
          path.join(agentRoot, "schedules", `${task.name}.ts`),
      ),
    })),
    tools: [],
    skills: loadedSlots.skills.map((skill) => ({
      name: skill.definition.name,
      description: skill.definition.description ?? "",
      logicalPath: rel(skill.sourcePath),
      sourceKind: skill.sourcePath.endsWith(".md") ? "markdown" : "typescript",
    })),
    connections: [],
    channels,
    sandbox: null,
    subagents: discovery.subagents.map((sub) => ({
      name: sub.name,
      description: "",
      logicalPath: rel(sub.path),
    })),
    diagnostics: {
      errors: diagnostics.filter((d) => d.level === "error").length,
      warnings: diagnostics.filter((d) => d.level === "warning").length,
    },
  };
}

export async function emitPlasmAgentSummary(options: {
  projectRoot: string;
  summary: PlasmAgentSummaryPayload;
}): Promise<string> {
  const outPath = plasmAgentSummaryPath(options.projectRoot);
  await mkdir(path.dirname(outPath), { recursive: true });
  await writeFile(outPath, `${JSON.stringify(options.summary, null, 2)}\n`, "utf8");
  return outPath;
}
