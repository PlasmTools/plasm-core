import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, readdir, writeFile, rename, rm } from "node:fs/promises";
import path from "node:path";

import type { PrerequisiteClosure } from "./engine/routing.js";
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
  engineInstanceId?: string;
  registryGeneration?: string;
  routingClosures?: PrerequisiteClosure[];
  teachingTsv: string;
  waves: TeachingWave[];
  symbolRegistry?: SymbolRegistrySnapshot;
  planCommits: Array<{ ref: string; program: string; at: string; writeCount?: number }>;
  updatedAt: string;
}

export interface SessionStore {
  get(intent: string): Promise<AgentSessionState | null>;
  put(state: AgentSessionState): Promise<void>;
  listIntents(): Promise<string[]>;
}

/** Filesystem / KV key — SHA-256 hex. Never encode the raw intent (ENAMETOOLONG). */
export function intentKey(intent: string): string {
  return createHash("sha256").update(intent, "utf8").digest("hex");
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
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    }
  }

  async put(state: AgentSessionState): Promise<void> {
    const dir = this.sessionDir();
    await mkdir(dir, { recursive: true });
    const target = this.sessionPath(state.intent);
    const temporary = `${target}.${randomUUID()}.tmp`;
    try {
      await writeFile(temporary, JSON.stringify(state, null, 2), {mode: 0o600});
      await rename(temporary, target);
    } finally { await rm(temporary, {force: true}); }
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
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
      throw error;
    }
  }
}

export class SessionManager {
  /** Hot index by wire ref — getByLogicalRef must not depend on store list scans. */
  private readonly byLogicalRef = new Map<string, AgentSessionState>();

  constructor(
    readonly store: SessionStore,
    private readonly tenantScope = "local",
  ) {}

  tenant(): string {
    return this.tenantScope;
  }

  async get(intent: string): Promise<AgentSessionState | null> {
    const session = await this.store.get(intent);
    if (session) this.index(session);
    return session;
  }

  async getByLogicalRef(ref: string): Promise<AgentSessionState | null> {
    const key = ref.trim();
    if (!key) return null;
    const hot = this.byLogicalRef.get(key);
    if (hot) return hot;
    const intents = await this.store.listIntents();
    for (const intent of intents) {
      const session = await this.store.get(intent);
      if (session?.logicalSessionRef === key) {
        this.index(session);
        return session;
      }
    }
    return null;
  }

  async getOrCreate(intent: string, logicalSessionRef: string, logicalSessionId: string) {
    const existing = await this.store.get(intent);
    if (existing) {
      this.index(existing);
      return existing;
    }
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
    this.index(fresh);
    return fresh;
  }

  async update(state: AgentSessionState): Promise<void> {
    state.updatedAt = new Date().toISOString();
    await this.store.put(state);
    this.index(state);
  }

  private index(state: AgentSessionState): void {
    this.byLogicalRef.set(state.logicalSessionRef.trim(), state);
  }
}
