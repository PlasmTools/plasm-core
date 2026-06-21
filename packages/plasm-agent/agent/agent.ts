import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  createAgentFromProject,
  defineAgent,
} from "../src/define-agent.js";
import {
  createFixtureMockTransport,
  fixtureMockTransportEnabled,
} from "../src/engine/fixture-mock-transport.js";
import { createProductionHostTransport } from "../src/engine/create-host-transport.js";
import { loadAgentEnv } from "../src/load-env.js";

loadAgentEnv();

const agentRoot = path.dirname(fileURLToPath(import.meta.url));

const agentDefinition = defineAgent({
  model: process.env.PLASM_AGENT_MODEL ?? "anthropic/claude-sonnet-4.6",
  compaction: {
    thresholdPercent: 0.75,
  },
  modelOptions: {
    temperature: 0.2,
  },
  experimental: {
    workflow: {
      world: {
        type: "local",
      },
    },
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
    tenantScope: process.env.PLASM_TENANT_SCOPE ?? "local",
    maxSteps: 20,
    telemetry: process.env.PLASM_AGENT_TELEMETRY !== "0",
    hostTransport: fixtureMockTransportEnabled()
      ? createFixtureMockTransport()
      : createProductionHostTransport(),
  });
}
