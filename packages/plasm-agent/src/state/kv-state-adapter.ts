import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import { decodeSessionState, decodeListedSession, encodeSessionState, type AgentSessionState } from "../session-state.js";
import { type LogicalSessionRef } from "../runtime/session-contract.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";
import { sessionKvKey, sessionKvPrefix, symbolsKvKey } from "./fs-state-adapter.js";

type KvClient = {
  get<T>(key: string): Promise<T | null>;
  set(key: string, value: unknown): Promise<unknown>;
  keys(pattern: string): Promise<string[]>;
};

async function loadKv(): Promise<KvClient> {
  const mod = await import("@vercel/kv");
  return mod.kv as KvClient;
}

export class KvStateAdapter implements AgentStateStore {
  constructor(
    private readonly agentRoot: string,
    private readonly tenantScope: string,
  ) {
    void this.agentRoot;
  }

  backend(): StateBackend {
    return "kv";
  }

  async get(ref: LogicalSessionRef): Promise<AgentSessionState | null> {
    const kv = await loadKv();
    const raw = await kv.get<unknown>(sessionKvKey(this.tenantScope, ref));
    return raw === null ? null : decodeSessionState(raw, { tenantScope: this.tenantScope, logicalSessionRef: ref });
  }

  async put(state: AgentSessionState): Promise<void> {
    const raw: unknown = JSON.parse(encodeSessionState(state, { tenantScope: this.tenantScope, logicalSessionRef: state.logicalSessionRef }));
    const kv = await loadKv();
    await kv.set(sessionKvKey(this.tenantScope, state.logicalSessionRef), raw);
  }

  async listSessions(): Promise<AgentSessionState[]> {
    const kv = await loadKv();
    const keys = await kv.keys(`${sessionKvPrefix(this.tenantScope)}*`);
    const sessions: AgentSessionState[] = [];
    for (const key of keys) {
      const raw = await kv.get<unknown>(key);
      if (raw === null) continue;
      const state = decodeListedSession(raw, this.tenantScope);
      if (key !== sessionKvKey(this.tenantScope, state.logicalSessionRef)) throw new Error("Session key does not match its identity");
      sessions.push(state);
    }
    return sessions;
  }

  async getSymbolRegistry(tenantId: string): Promise<SymbolRegistrySnapshot | null> {
    const kv = await loadKv();
    return kv.get<SymbolRegistrySnapshot>(symbolsKvKey(tenantId));
  }

  async putSymbolRegistry(
    tenantId: string,
    snapshot: SymbolRegistrySnapshot,
  ): Promise<void> {
    const kv = await loadKv();
    await kv.set(symbolsKvKey(tenantId), snapshot);
  }
}
