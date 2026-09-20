import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import {
  LocalSessionStore,
  type AgentSessionState,
} from "../session-state.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";
import { sessionStorageKey, sessionTenantKey, type LogicalSessionRef } from "../runtime/session-contract.js";

export class FsStateAdapter implements AgentStateStore {
  private readonly sessions: LocalSessionStore;

  constructor(
    private readonly agentRoot: string,
    private readonly tenantScope: string,
  ) {
    this.sessions = new LocalSessionStore(agentRoot, tenantScope);
  }

  backend(): StateBackend {
    return "fs";
  }

  async get(ref: LogicalSessionRef): Promise<AgentSessionState | null> {
    return this.sessions.get(ref);
  }

  async put(state: AgentSessionState): Promise<void> {
    await this.sessions.put(state);
  }

  async listSessions(): Promise<AgentSessionState[]> {
    return this.sessions.listSessions();
  }

  private symbolsPath(): string {
    return path.join(this.agentRoot, ".plasm", "symbols.json");
  }

  async getSymbolRegistry(tenantId: string): Promise<SymbolRegistrySnapshot | null> {
    void tenantId;
    try {
      const raw = await readFile(this.symbolsPath(), "utf8");
      return JSON.parse(raw) as SymbolRegistrySnapshot;
    } catch {
      return null;
    }
  }

  async putSymbolRegistry(
    tenantId: string,
    snapshot: SymbolRegistrySnapshot,
  ): Promise<void> {
    void tenantId;
    const file = this.symbolsPath();
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, JSON.stringify(snapshot, null, 2), "utf8");
  }
}

export function sessionKvPrefix(tenantScope: string): string {
  return `plasm:${sessionTenantKey(tenantScope)}:session-v2:`;
}

export function sessionKvKey(tenantScope: string, logicalSessionRef: LogicalSessionRef): string {
  return `${sessionKvPrefix(tenantScope)}${sessionStorageKey({ tenantScope, logicalSessionRef })}`;
}

export function symbolsKvKey(tenantId: string): string {
  return `plasm:${tenantId}:symbols`;
}
