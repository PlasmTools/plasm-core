import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import type { AgentSessionState } from "../session-state.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";
import { sessionKvKey, symbolsKvKey } from "./fs-state-adapter.js";

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

  async get(intent: string): Promise<AgentSessionState | null> {
    const kv = await loadKv();
    return kv.get<AgentSessionState>(sessionKvKey(this.tenantScope, intent));
  }

  async put(state: AgentSessionState): Promise<void> {
    const kv = await loadKv();
    await kv.set(sessionKvKey(this.tenantScope, state.intent), state);
  }

  async listIntents(): Promise<string[]> {
    const kv = await loadKv();
    const keys = await kv.keys(`plasm:${this.tenantScope}:session:*`);
    const intents: string[] = [];
    for (const key of keys) {
      const state = await kv.get<AgentSessionState>(key);
      if (state?.intent) intents.push(state.intent);
    }
    return intents;
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
