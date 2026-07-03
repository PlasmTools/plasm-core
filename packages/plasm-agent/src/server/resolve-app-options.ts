import path from "node:path";

import { createProductionHostTransport } from "../engine/create-host-transport.js";
import { loadProjectAgentEnv } from "../load-env.js";
import type { AgentRuntimeConfig } from "../runtime/agent-runtime.js";

import type { PlasmAppOptions } from "./plasm-handler.js";

export interface ResolvedPlasmAppOptions {
  tenantScope: string;
  maxSteps: number;
  telemetry: boolean;
  hostTransport: NonNullable<AgentRuntimeConfig["hostTransport"]>;
}

/** Load project `.env` files and merge runtime options for prod Nitro bootstrap. */
export function resolvePlasmAppOptions(
  agentRoot: string,
  partial: PlasmAppOptions,
): ResolvedPlasmAppOptions {
  const projectRoot = path.dirname(path.resolve(agentRoot));
  loadProjectAgentEnv(projectRoot);

  const maxStepsRaw = partial.maxSteps ?? Number(process.env.PLASM_AGENT_MAX_STEPS ?? 24);
  const maxSteps = Number.isFinite(maxStepsRaw) && maxStepsRaw > 0 ? maxStepsRaw : 24;

  return {
    tenantScope: partial.tenantScope ?? process.env.PLASM_TENANT_SCOPE?.trim() ?? "local",
    maxSteps,
    telemetry: partial.telemetry ?? process.env.PLASM_AGENT_TELEMETRY !== "0",
    hostTransport: createProductionHostTransport(),
  };
}
