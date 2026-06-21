import path from "node:path";

import type { LoadedProjectSlots } from "./authoring/slot-loader.js";
import { loadAuthoredSlots } from "./authoring/slot-loader.js";
import type { SubagentRegistry } from "./authoring/subagent-loader.js";
import {
  createSubagentRegistry,
  loadSubagents,
} from "./authoring/subagent-loader.js";
import { walkAgentProject } from "./discovery/project-walker.js";
import { PlasmAgent, type PlasmAgentConfig } from "./runtime/plasm-agent.js";
import type { AgentRuntimeConfig } from "./runtime/agent-runtime.js";
import { bootstrapWorkflowWorld } from "./workflow/world-bootstrap.js";

/** Context-window compaction (eve-shaped; harness wiring is phase 7+). */
export interface AgentCompactionConfig {
  thresholdPercent?: number;
  model?: string;
}

/** Provider-specific generation knobs passed through to AI SDK calls. */
export interface AgentModelOptions {
  temperature?: number;
  maxOutputTokens?: number;
  topP?: number;
  topK?: number;
  [key: string]: unknown;
}

/** Workflow SDK world adapter (local dev vs durable Postgres vs Vercel-managed). */
export interface AgentWorkflowWorldDefinition {
  type?: "local" | "postgres" | "vercel" | "kv";
  [key: string]: unknown;
}

export interface AgentWorkflowDefinition {
  world?: AgentWorkflowWorldDefinition;
}

export interface AgentBuildConfig {
  /** Packages the bundler must not inline (NAPI prebuilds, native addons). */
  externalDependencies?: string[];
}

export interface AgentExperimentalConfig {
  workflow?: AgentWorkflowDefinition;
  /** `true` (default) = skill index + read_skill tool; `"inline"` = inject full bodies. */
  skills?: boolean | "inline";
}

/** Authored agent runtime config (`agent/agent.ts`). Identity comes from the filesystem. */
export interface AgentDefinition {
  model: string;
  compaction?: AgentCompactionConfig;
  modelOptions?: AgentModelOptions;
  experimental?: AgentExperimentalConfig;
  build?: AgentBuildConfig;
}

export interface CreateAgentFromDefinitionOptions {
  agentRoot: string;
  tenantScope?: string;
  maxSteps?: number;
  telemetry?: boolean;
  hostTransport?: AgentRuntimeConfig["hostTransport"];
  loadedSlots?: LoadedProjectSlots;
  subagentRegistry?: SubagentRegistry;
  getAuthoringContext?: PlasmAgentConfig["getAuthoringContext"];
}

export interface ResolvedAgentDefinition extends AgentDefinition {
  agentRoot: string;
}

/** Eve-compatible helper: default-export the frozen definition from `agent/agent.ts`. */
export function defineAgent(definition: AgentDefinition): Readonly<AgentDefinition> {
  return Object.freeze({ ...definition });
}

export function resolveAgentDefinition(
  definition: AgentDefinition,
  agentRoot: string,
): ResolvedAgentDefinition {
  return Object.freeze({
    ...definition,
    agentRoot: path.resolve(agentRoot),
  });
}

export function createPlasmAgentConfig(
  definition: AgentDefinition,
  options: CreateAgentFromDefinitionOptions,
): PlasmAgentConfig {
  return {
    model: definition.model,
    agentRoot: path.resolve(options.agentRoot),
    tenantScope: options.tenantScope,
    maxSteps: options.maxSteps,
    telemetry: options.telemetry,
    hostTransport: options.hostTransport,
    compaction: definition.compaction,
    modelOptions: definition.modelOptions,
    build: definition.build,
    experimental: definition.experimental,
    stateWorld: definition.experimental?.workflow?.world,
    loadedSkills: options.loadedSlots?.skills.map((s) => s.definition),
    hookRunner: options.loadedSlots?.hookRunner,
    subagentRegistry: options.subagentRegistry,
    getAuthoringContext: options.getAuthoringContext,
  };
}

export async function createAgentFromDefinition(
  definition: AgentDefinition,
  options: CreateAgentFromDefinitionOptions,
): Promise<PlasmAgent> {
  await bootstrapWorkflowWorld(definition.experimental?.workflow);
  const agent = new PlasmAgent(createPlasmAgentConfig(definition, options));
  await agent.bootstrap();
  return agent;
}

/** Walk agent/, load slots + subagents, then bootstrap the root agent. */
export async function createAgentFromProject(
  definition: AgentDefinition,
  options: CreateAgentFromDefinitionOptions & { importCacheBust?: number },
): Promise<PlasmAgent> {
  const agentRoot = path.resolve(options.agentRoot);
  const discovery = await walkAgentProject(agentRoot);
  const loadedSlots = await loadAuthoredSlots({
    discovery,
    importCacheBust: options.importCacheBust,
  });
  const { subagents, diagnostics } = await loadSubagents({
    discovery,
    parentSlots: loadedSlots,
    tenantScope: options.tenantScope,
    maxSteps: options.maxSteps,
    telemetry: options.telemetry,
    importCacheBust: options.importCacheBust,
  });
  loadedSlots.diagnostics.push(...diagnostics);

  return createAgentFromDefinition(definition, {
    ...options,
    agentRoot,
    loadedSlots,
    subagentRegistry: createSubagentRegistry(subagents),
  });
}
