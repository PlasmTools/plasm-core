import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import type { AgentSessionState } from "../session-state.js";
import {
  blobGetJson,
  blobList,
  blobPutJson,
} from "../storage/vercel-blob.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";
import { intentKey } from "./fs-state-adapter.js";

function sessionBlobKey(tenantScope: string, intent: string): string {
  return `plasm/state/${tenantScope}/sessions/${intentKey(intent)}.json`;
}

function symbolsBlobKey(tenantScope: string): string {
  return `plasm/state/${tenantScope}/symbols.json`;
}

function sessionPrefix(tenantScope: string): string {
  return `plasm/state/${tenantScope}/sessions/`;
}

export class BlobStateAdapter implements AgentStateStore {
  constructor(
    private readonly agentRoot: string,
    private readonly tenantScope: string,
  ) {
    void this.agentRoot;
  }

  backend(): StateBackend {
    return "blob";
  }

  async get(intent: string): Promise<AgentSessionState | null> {
    return blobGetJson<AgentSessionState>(sessionBlobKey(this.tenantScope, intent));
  }

  async put(state: AgentSessionState): Promise<void> {
    await blobPutJson(sessionBlobKey(this.tenantScope, state.intent), state);
  }

  async listIntents(): Promise<string[]> {
    const prefix = sessionPrefix(this.tenantScope);
    const paths = await blobList(prefix);
    const intents: string[] = [];
    for (const pathname of paths) {
      if (!pathname.endsWith(".json")) continue;
      const state = await blobGetJson<AgentSessionState>(pathname);
      if (state?.intent) intents.push(state.intent);
    }
    return intents;
  }

  async getSymbolRegistry(tenantId: string): Promise<SymbolRegistrySnapshot | null> {
    return blobGetJson<SymbolRegistrySnapshot>(symbolsBlobKey(tenantId));
  }

  async putSymbolRegistry(
    tenantId: string,
    snapshot: SymbolRegistrySnapshot,
  ): Promise<void> {
    await blobPutJson(symbolsBlobKey(tenantId), snapshot);
  }
}
