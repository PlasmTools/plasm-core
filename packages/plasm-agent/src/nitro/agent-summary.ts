import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { LoadedProjectSlots } from "../authoring/slot-loader.js";
import { listChannelRoutes } from "../authoring/channel-dispatch.js";
import { exportScheduleTaskManifest } from "../authoring/schedule-manager.js";
import type { AgentDefinition } from "../define-agent.js";
import type { ProjectDiscovery } from "../discovery/project-walker.js";
import { frameworkPackageVersion } from "../package-version.js";
import { plasmAgentSummaryPath, eveAgentSummaryPath } from "./paths.js";

export const VERCEL_PLASM_AGENT_SUMMARY_KIND = "vercel-plasm-agent-summary";
export const VERCEL_PLASM_AGENT_SUMMARY_VERSION = 3;
export const VERCEL_EVE_AGENT_SUMMARY_KIND = "vercel-eve-agent-summary";

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

function mapSkillSourceKind(sourceKind: string): "markdown" | "module" | "skill-package" {
  if (sourceKind === "markdown") return "markdown";
  if (sourceKind === "skill-package") return "skill-package";
  return "module";
}

/** Eve-compatible summary for Vercel Agent Runs build ingestion (`.eve/agent-summary.json`). */
export function toEveAgentSummary(
  summary: PlasmAgentSummaryPayload,
  instructionsMarkdown?: string | null,
): Record<string, unknown> {
  return {
    kind: VERCEL_EVE_AGENT_SUMMARY_KIND,
    schemaVersion: VERCEL_PLASM_AGENT_SUMMARY_VERSION,
    generatorVersion: summary.generatorVersion,
    agent: {
      name: summary.agent.name,
      ...(summary.agent.description ? { description: summary.agent.description } : {}),
      modelId: summary.agent.modelId,
    },
    instructions: summary.instructions
      ? {
          logicalPath: summary.instructions.logicalPath,
          sourceKind: summary.instructions.sourceKind === "markdown" ? "markdown" : "module",
          markdown: instructionsMarkdown ?? summary.instructions.markdown ?? "",
        }
      : null,
    schedules: summary.schedules,
    tools: summary.tools,
    skills: summary.skills.map((skill) => ({
      name: skill.name,
      description: skill.description,
      logicalPath: skill.logicalPath,
      sourceKind: mapSkillSourceKind(skill.sourceKind),
    })),
    connections: summary.connections,
    channels: summary.channels,
    sandbox: summary.sandbox,
    subagents: summary.subagents,
    diagnostics: summary.diagnostics,
  };
}

export async function emitEveCompatibleAgentSummary(options: {
  projectRoot: string;
  summary: PlasmAgentSummaryPayload;
  instructionsMarkdown?: string | null;
}): Promise<string> {
  const outPath = eveAgentSummaryPath(options.projectRoot);
  await mkdir(path.dirname(outPath), { recursive: true });
  await writeFile(
    outPath,
    `${JSON.stringify(toEveAgentSummary(options.summary, options.instructionsMarkdown), null, 2)}\n`,
    "utf8",
  );
  return outPath;
}
