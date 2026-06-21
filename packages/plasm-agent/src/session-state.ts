import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { SymbolRegistrySnapshot } from "./symbol-registry.js";

export interface ExecuteSessionRef {
  promptHash: string;
  sessionId: string;
}

export interface TeachingWave {
  entryId: string;
  entities: string[];
  tsv: string;
  at: string;
}

export interface AgentSessionState {
  intent: string;
  logicalSessionRef: string;
  logicalSessionId: string;
  tenantScope: string;
  seeds: Array<{ api: string; entity: string }>;
  teachingTsv: string;
  waves: TeachingWave[];
  symbolRegistry?: SymbolRegistrySnapshot;
  planCommits: Array<{ ref: string; program: string; at: string }>;
  updatedAt: string;
}

export interface SessionStore {
  get(intent: string): Promise<AgentSessionState | null>;
  put(state: AgentSessionState): Promise<void>;
  listIntents(): Promise<string[]>;
}

function intentKey(intent: string): string {
  return Buffer.from(intent, "utf8").toString("base64url");
}

export class LocalSessionStore implements SessionStore {
  constructor(private readonly rootDir: string) {}

  private sessionDir(): string {
    return path.join(this.rootDir, ".plasm", "sessions");
  }

  private sessionPath(intent: string): string {
    return path.join(this.sessionDir(), `${intentKey(intent)}.json`);
  }

  private teachingPath(intent: string): string {
    return path.join(this.sessionDir(), `${intentKey(intent)}.teaching.tsv`);
  }

  async get(intent: string): Promise<AgentSessionState | null> {
    try {
      const raw = await readFile(this.sessionPath(intent), "utf8");
      return JSON.parse(raw) as AgentSessionState;
    } catch {
      return null;
    }
  }

  async put(state: AgentSessionState): Promise<void> {
    const dir = this.sessionDir();
    await mkdir(dir, { recursive: true });
    await writeFile(this.sessionPath(state.intent), JSON.stringify(state, null, 2), "utf8");
    await writeFile(this.teachingPath(state.intent), state.teachingTsv, "utf8");
  }

  async listIntents(): Promise<string[]> {
    try {
      const dir = this.sessionDir();
      const files = await readdir(dir);
      const intents: string[] = [];
      for (const file of files) {
        if (!file.endsWith(".json")) continue;
        const raw = await readFile(path.join(dir, file), "utf8");
        const state = JSON.parse(raw) as AgentSessionState;
        intents.push(state.intent);
      }
      return intents;
    } catch {
      return [];
    }
  }
}

export class SessionManager {
  constructor(
    readonly store: SessionStore,
    private readonly tenantScope = "local",
  ) {}

  tenant(): string {
    return this.tenantScope;
  }

  async get(intent: string): Promise<AgentSessionState | null> {
    return this.store.get(intent);
  }

  async getByLogicalRef(ref: string): Promise<AgentSessionState | null> {
    const intents = await this.store.listIntents();
    for (const intent of intents) {
      const session = await this.store.get(intent);
      if (session?.logicalSessionRef === ref) return session;
    }
    return null;
  }

  async getOrCreate(intent: string, logicalSessionRef: string, logicalSessionId: string) {
    const existing = await this.store.get(intent);
    if (existing) return existing;
    const fresh: AgentSessionState = {
      intent,
      logicalSessionRef,
      logicalSessionId,
      tenantScope: this.tenantScope,
      seeds: [],
      teachingTsv: "",
      waves: [],
      planCommits: [],
      updatedAt: new Date().toISOString(),
    };
    await this.store.put(fresh);
    return fresh;
  }

  async update(state: AgentSessionState): Promise<void> {
    state.updatedAt = new Date().toISOString();
    await this.store.put(state);
  }
}
