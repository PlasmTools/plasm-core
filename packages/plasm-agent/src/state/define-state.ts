import type { AgentWorkflowWorldDefinition } from "../define-agent.js";
import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import type { AgentSessionState, SessionStore } from "../session-state.js";
import { FsStateAdapter } from "./fs-state-adapter.js";
import { KvStateAdapter } from "./kv-state-adapter.js";
import { PostgresStateAdapter } from "./postgres-state-adapter.js";

export type StateBackend = "fs" | "kv" | "postgres";

export interface AgentStateStore extends SessionStore {
  backend(): StateBackend;
  getSymbolRegistry(tenantId: string): Promise<SymbolRegistrySnapshot | null>;
  putSymbolRegistry(tenantId: string, snapshot: SymbolRegistrySnapshot): Promise<void>;
}

export function resolveStateBackend(
  world?: AgentWorkflowWorldDefinition,
): StateBackend {
  const explicit = process.env.PLASM_STATE_BACKEND?.trim().toLowerCase();
  if (explicit === "fs" || explicit === "kv" || explicit === "postgres") {
    return explicit;
  }
  if (process.env.KV_REST_API_URL?.trim() || process.env.PLASM_KV_REST_API_URL?.trim()) {
    return "kv";
  }
  if (
    process.env.WORKFLOW_POSTGRES_URL?.trim() ||
    process.env.DATABASE_URL?.trim() ||
    process.env.PLASM_STATE_POSTGRES_URL?.trim()
  ) {
    if (world?.type === "postgres") return "postgres";
    if (!world?.type && process.env.PLASM_STATE_BACKEND?.trim() === "postgres") {
      return "postgres";
    }
  }
  if (world?.type === "kv") return "kv";
  if (world?.type === "postgres") return "postgres";
  return "fs";
}

export function createAgentStateStore(options: {
  agentRoot: string;
  tenantScope?: string;
  backend?: StateBackend;
  world?: AgentWorkflowWorldDefinition;
}): AgentStateStore {
  const backend = options.backend ?? resolveStateBackend(options.world);
  const tenantScope = options.tenantScope ?? "local";
  switch (backend) {
    case "kv":
      return new KvStateAdapter(options.agentRoot, tenantScope);
    case "postgres":
      return new PostgresStateAdapter(tenantScope);
    default:
      return new FsStateAdapter(options.agentRoot, tenantScope);
  }
}
