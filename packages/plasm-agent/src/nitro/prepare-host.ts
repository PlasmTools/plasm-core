import path from "node:path";
import { pathToFileURL } from "node:url";

import type { CompiledSlotMap } from "../cli/compile-authored-slots.js";
import { loadAuthoredSlots } from "../authoring/slot-loader.js";
import type { AgentDefinition } from "../define-agent.js";
import { walkAgentProject, type ProjectDiscovery } from "../discovery/project-walker.js";
import { exportScheduleTaskManifest } from "../authoring/schedule-manager.js";
import { projectUsesWorkflowDirectives } from "./workflow-directives.js";

export interface PreparedPlasmHost {
  projectRoot: string;
  agentRoot: string;
  definition: AgentDefinition;
  discovery: ProjectDiscovery;
  compiledSlots: CompiledSlotMap;
  scheduleCrons: Array<{ path: string; schedule: string }>;
  scheduledTasks: Record<string, string | string[]>;
  externalDeps: string[];
  workflowEnabled: boolean;
}

async function loadAgentDefinition(agentRoot: string): Promise<AgentDefinition> {
  const agentPath = path.join(agentRoot, "agent.ts");
  const mod = (await import(`${pathToFileURL(agentPath).href}?v=${Date.now()}`)) as {
    default: AgentDefinition;
  };
  if (!mod.default?.model) {
    throw new Error(`agent/agent.ts must default-export defineAgent({ model: ... })`);
  }
  return mod.default;
}

export async function preparePlasmHost(options: {
  projectRoot: string;
  agentRoot: string;
  compiledSlots: CompiledSlotMap;
  discovery: ProjectDiscovery;
}): Promise<PreparedPlasmHost> {
  const { projectRoot, agentRoot, compiledSlots, discovery } = options;
  const definition = await loadAgentDefinition(agentRoot);

  const loadedSlots = await loadAuthoredSlots({
    discovery,
    agentRoot,
    projectRoot,
    compiledSlots,
  });

  const taskManifest = exportScheduleTaskManifest(loadedSlots.schedules);

  const externalDeps = definition.build?.externalDependencies ?? [];
  const workflowConfigured = definition.experimental?.workflow !== undefined;
  const workflowEnabled =
    workflowConfigured && (await projectUsesWorkflowDirectives(projectRoot));

  return {
    projectRoot,
    agentRoot,
    definition,
    discovery,
    compiledSlots,
    scheduleCrons: [],
    scheduledTasks: taskManifest.scheduledTasks,
    externalDeps,
    workflowEnabled,
  };
}
