import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import { decodeSessionState, decodeListedSession, encodeSessionState, type AgentSessionState } from "../session-state.js";
import { sessionStorageKey, sessionTenantKey, type LogicalSessionRef } from "../runtime/session-contract.js";
import {
  blobGetJson,
  blobList,
  blobPutJson,
} from "../storage/vercel-blob.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";

function sessionBlobKey(tenantScope: string, logicalSessionRef: LogicalSessionRef): string {
  return `${sessionPrefix(tenantScope)}${sessionStorageKey({ tenantScope, logicalSessionRef })}.json`;
}

function symbolsBlobKey(tenantScope: string): string {
  return `plasm/state/${tenantScope}/symbols.json`;
}

function sessionPrefix(tenantScope: string): string {
  return `plasm/state/${sessionTenantKey(tenantScope)}/sessions-v2/`;
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

  async get(ref: LogicalSessionRef): Promise<AgentSessionState | null> {
    const raw = await blobGetJson<unknown>(sessionBlobKey(this.tenantScope, ref));
    return raw === null ? null : decodeSessionState(raw, { tenantScope: this.tenantScope, logicalSessionRef: ref });
  }

  async put(state: AgentSessionState): Promise<void> {
    const raw: unknown = JSON.parse(encodeSessionState(state, { tenantScope: this.tenantScope, logicalSessionRef: state.logicalSessionRef }));
    await blobPutJson(sessionBlobKey(this.tenantScope, state.logicalSessionRef), raw);
  }

  async listSessions(): Promise<AgentSessionState[]> {
    const prefix = sessionPrefix(this.tenantScope);
    const paths = await blobList(prefix);
    const sessions: AgentSessionState[] = [];
    for (const pathname of paths) {
      if (!pathname.endsWith(".json")) continue;
      const raw = await blobGetJson<unknown>(pathname);
      if (raw === null) continue;
      const state = decodeListedSession(raw, this.tenantScope);
      if (pathname !== sessionBlobKey(this.tenantScope, state.logicalSessionRef)) throw new Error("Session key does not match its identity");
      sessions.push(state);
    }
    return sessions;
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
