import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import {
  LocalSessionStore,
  type AgentSessionState,
} from "../session-state.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";

export function intentKey(intent: string): string {
  return Buffer.from(intent, "utf8").toString("base64url");
}

export class FsStateAdapter implements AgentStateStore {
  private readonly sessions: LocalSessionStore;

  constructor(
    private readonly agentRoot: string,
    private readonly tenantScope: string,
  ) {
    this.sessions = new LocalSessionStore(agentRoot);
  }

  backend(): StateBackend {
    return "fs";
  }

  async get(intent: string): Promise<AgentSessionState | null> {
    return this.sessions.get(intent);
  }

  async put(state: AgentSessionState): Promise<void> {
    await this.sessions.put(state);
  }

  async listIntents(): Promise<string[]> {
    return this.sessions.listIntents();
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

export function sessionKvKey(tenantId: string, intent: string): string {
  return `plasm:${tenantId}:session:${intentKey(intent)}`;
}

export function symbolsKvKey(tenantId: string): string {
  return `plasm:${tenantId}:symbols`;
}
