import path from "node:path";
import { pathToFileURL } from "node:url";

import {
  createAgentFromDefinition,
  defineAgent,
  type AgentDefinition,
} from "../define-agent.js";
import type { ProjectDiscovery, DiscoveryDiagnostic } from "../discovery/project-walker.js";
import type { PlasmAgent } from "../runtime/plasm-agent.js";
import type { LoadedProjectSlots } from "./slot-loader.js";

export interface LoadedSubagent {
  name: string;
  agentRoot: string;
  sourcePath: string;
  definition: AgentDefinition;
  agent: PlasmAgent;
}

export interface SubagentRegistry {
  list(): LoadedSubagent[];
  get(name: string): LoadedSubagent | undefined;
  delegate(name: string, message: string): Promise<{ text: string; steps: number }>;
}

export interface LoadSubagentsOptions {
  discovery: ProjectDiscovery;
  parentSlots?: LoadedProjectSlots;
  tenantScope?: string;
  maxSteps?: number;
  telemetry?: boolean;
  importCacheBust?: number;
}

function isAgentDefinition(value: unknown): value is AgentDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AgentDefinition).model === "string"
  );
}

export async function loadSubagents(
  options: LoadSubagentsOptions,
): Promise<{ subagents: LoadedSubagent[]; diagnostics: DiscoveryDiagnostic[] }> {
  const cacheBust = options.importCacheBust ?? Date.now();
  const diagnostics: DiscoveryDiagnostic[] = [];
  const subagents: LoadedSubagent[] = [];

  for (const entry of options.discovery.subagents) {
    try {
      const url = `${pathToFileURL(entry.agentPath).href}?t=${cacheBust}`;
      const mod = await import(url);
      const exported = mod.default ?? mod;
      if (!isAgentDefinition(exported)) {
        diagnostics.push({
          level: "error",
          slot: "subagents",
          path: entry.agentPath,
          message: "agent.ts default export must be defineAgent() result",
        });
        continue;
      }

      const agent = await createAgentFromDefinition(exported, {
        agentRoot: entry.path,
        tenantScope: options.tenantScope,
        maxSteps: options.maxSteps,
        telemetry: options.telemetry,
        loadedSlots: options.parentSlots,
      });

      subagents.push({
        name: entry.name,
        agentRoot: entry.path,
        sourcePath: entry.agentPath,
        definition: exported,
        agent,
      });
    } catch (err) {
      diagnostics.push({
        level: "error",
        slot: "subagents",
        path: entry.agentPath,
        message: `failed to load subagent: ${String(err)}`,
      });
    }
  }

  subagents.sort((a, b) => a.name.localeCompare(b.name));
  return { subagents, diagnostics };
}

export function createSubagentRegistry(subagents: LoadedSubagent[]): SubagentRegistry {
  const byName = new Map(subagents.map((s) => [s.name, s]));
  return {
    list: () => [...subagents],
    get: (name: string) => byName.get(name),
    async delegate(name: string, message: string) {
      const sub = byName.get(name);
      if (!sub) {
        throw new Error(`unknown subagent: ${name}`);
      }
      const result = await sub.agent.generate(message);
      return { text: result.text, steps: result.steps.length };
    },
  };
}

export function summarizeSubagents(
  subagents: LoadedSubagent[],
  agentRoot: string,
): Array<{ name: string; path: string; model: string }> {
  const rel = (p: string) => path.relative(agentRoot, p);
  return subagents.map((s) => ({
    name: s.name,
    path: rel(s.agentRoot),
    model: s.definition.model,
  }));
}
