import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  createAgentFromDefinition,
  defineAgent,
} from "../../../src/define-agent.js";

const agentRoot = path.dirname(fileURLToPath(import.meta.url));

const agentDefinition = defineAgent({
  model: process.env.PLASM_AGENT_MODEL ?? "anthropic/claude-sonnet-4.6",
  modelOptions: { temperature: 0.1 },
  experimental: {
    workflow: { world: { type: "local" } },
  },
});

export default agentDefinition;

export async function createTinySubagent() {
  return createAgentFromDefinition(agentDefinition, {
    agentRoot,
    tenantScope: process.env.PLASM_TENANT_SCOPE ?? "local",
    maxSteps: 10,
    telemetry: false,
  });
}
