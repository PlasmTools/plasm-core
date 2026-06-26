import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  createAgentFromProject,
  createProductionHostTransport,
  defineAgent,
  loadAgentEnv,
} from "@plasm_lang/vercel-agent";

loadAgentEnv();

const agentRoot = path.dirname(fileURLToPath(import.meta.url));

const agentDefinition = defineAgent({
  model: process.env.PLASM_AGENT_MODEL ?? "anthropic/claude-sonnet-4.6",
  compaction: { thresholdPercent: 0.75 },
  modelOptions: { temperature: 0.2 },
  experimental: {
    workflow: { world: { type: "vercel" } },
    skills: true,
  },
  build: {
    externalDependencies: ["@plasm_lang/engine"],
  },
});

export default agentDefinition;

export async function createPlasmAgent() {
  return createAgentFromProject(agentDefinition, {
    agentRoot,
    tenantScope: process.env.PLASM_TENANT_SCOPE ?? "mcp-radar",
    maxSteps: 24,
    telemetry: process.env.PLASM_AGENT_TELEMETRY !== "0",
    hostTransport: createProductionHostTransport(),
  });
}
