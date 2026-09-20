import { randomUUID } from "node:crypto";
import { mkdir, readFile, readdir, writeFile, rename, rm } from "node:fs/promises";
import path from "node:path";

import { z } from "zod";
import { prerequisiteClosureSchema, type PrerequisiteClosure } from "./engine/routing.js";
import {
  logicalSessionRefSchema, sessionIdentitySchema, sessionStorageKey, sessionTenantKey,
  unicodeTextSchema, workflowIntentSchema, intentProvenanceSchema,
  type IntentProvenance, type LogicalSessionRef, type SessionIdentity, type WorkflowIntent,
} from "./runtime/session-contract.js";
import { parseLogicalSessionWireRef } from "./runtime/logical-session.js";
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
  readonly schemaVersion: 2;
  intentProvenance: IntentProvenance;
  readonly intent: WorkflowIntent;
  readonly logicalSessionRef: LogicalSessionRef;
  readonly logicalSessionId: string;
  readonly tenantScope: string;
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

/** Strict, versioned boundary shared by every persistence adapter. */
const sessionSchema = z.object({
  schemaVersion: z.literal(2),
  intentProvenance: intentProvenanceSchema,
  intent: workflowIntentSchema,
  logicalSessionRef: logicalSessionRefSchema,
  logicalSessionId: z.string().uuid(),
  tenantScope: unicodeTextSchema,
  seeds: z.array(z.object({ api: z.string(), entity: z.string() }).strict()),
  engineInstanceId: z.string().optional(),
  registryGeneration: z.string().optional(),
  routingClosures: z.array(prerequisiteClosureSchema).optional(),
  teachingTsv: z.string(),
  waves: z.array(z.object({
    entryId: z.string(), entities: z.array(z.string()), tsv: z.string(), at: z.string(),
  }).strict()),
  symbolRegistry: z.object({
    bindings: z.array(z.object({
      symbol: z.string(), kind: z.enum(["entity", "method", "param", "relation"]),
      entryId: z.string(), wire: z.string(), entity: z.string().optional(), tombstoned: z.boolean().optional(),
    }).strict()),
    nextEntity: z.number().int().nonnegative(),
    nextMethod: z.number().int().nonnegative(),
    nextParam: z.number().int().nonnegative(),
    nextRelation: z.number().int().nonnegative(),
  }).strict().optional(),
  planCommits: z.array(z.object({
    ref: z.string(), program: z.string(), at: z.string(), writeCount: z.number().int().nonnegative().optional(),
  }).strict()),
  updatedAt: z.string(),
}).strict();

function validateJsonText(value: unknown): void {
  if (typeof value === "string") unicodeTextSchema.parse(value);
  else if (Array.isArray(value)) value.forEach(validateJsonText);
  else if (value && typeof value === "object") {
    for (const [key, child] of Object.entries(value)) {
      unicodeTextSchema.parse(key);
      validateJsonText(child);
    }
  }
}

export function decodeSessionState(value: unknown, expected: SessionIdentity): AgentSessionState {
  const identity = sessionIdentitySchema.parse(expected);
  validateJsonText(value);
  const state = sessionSchema.parse(value);
  if (state.tenantScope !== identity.tenantScope || state.logicalSessionRef !== identity.logicalSessionRef) {
    throw new Error("Persisted session identity does not match its storage address");
  }
  if (parseLogicalSessionWireRef(state.logicalSessionRef).toString("hex") !== state.logicalSessionId.replaceAll("-", "")) {
    throw new Error("Persisted session UUID does not match its wire ref");
  }
  if (state.intent !== state.intentProvenance.nodes[0]!.intent) throw new Error("Session intent does not match provenance root");
  return state;
}

export function encodeSessionState(state: AgentSessionState, identity: SessionIdentity): string {
  return JSON.stringify(decodeSessionState(state, identity));
}

export function decodeListedSession(value: unknown, tenantScope: string): AgentSessionState {
  const parsed = sessionSchema.parse(value);
  return decodeSessionState(value, { tenantScope, logicalSessionRef: parsed.logicalSessionRef });
}

export interface SessionStore {
  get(ref: LogicalSessionRef): Promise<AgentSessionState | null>;
  put(state: AgentSessionState): Promise<void>;
  listSessions(): Promise<AgentSessionState[]>;
}

export class LocalSessionStore implements SessionStore {
  constructor(private readonly rootDir: string, private readonly tenantScope = "local") { }

  private identity(ref: LogicalSessionRef): SessionIdentity {
    return { tenantScope: this.tenantScope, logicalSessionRef: ref };
  }

  private sessionDir(): string {
    return path.join(this.rootDir, ".plasm", "sessions-v2", sessionTenantKey(this.tenantScope));
  }

  private sessionPath(ref: LogicalSessionRef): string {
    return path.join(this.sessionDir(), `${sessionStorageKey(this.identity(ref))}.json`);
  }

  async get(ref: LogicalSessionRef): Promise<AgentSessionState | null> {
    try {
      const raw: unknown = JSON.parse(await readFile(this.sessionPath(ref), "utf8"));
      return decodeSessionState(raw, this.identity(ref));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    }
  }

  async put(state: AgentSessionState): Promise<void> {
    const encoded = encodeSessionState(state, this.identity(state.logicalSessionRef));
    await mkdir(this.sessionDir(), { recursive: true });
    const target = this.sessionPath(state.logicalSessionRef);
    const temporary = `${target}.${randomUUID()}.tmp`;
    try {
      await writeFile(temporary, encoded, { mode: 0o600 });
      await rename(temporary, target);
    } finally { await rm(temporary, { force: true }); }
  }

  async listSessions(): Promise<AgentSessionState[]> {
    let files: string[];
    try { files = await readdir(this.sessionDir()); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
      throw error;
    }
    const states: AgentSessionState[] = [];
    for (const file of files.filter((name) => name.endsWith(".json"))) {
      const raw: unknown = JSON.parse(await readFile(path.join(this.sessionDir(), file), "utf8"));
      const identity = sessionSchema.parse(raw);
      const state = decodeSessionState(raw, this.identity(identity.logicalSessionRef));
      if (file !== path.basename(this.sessionPath(state.logicalSessionRef))) throw new Error("Session filename does not match its identity");
      states.push(state);
    }
    return states;
  }
}

export class SessionManager {
  private readonly byLogicalRef = new Map<LogicalSessionRef, AgentSessionState>();

  constructor(readonly store: SessionStore, private readonly tenantScope = "local") { }

  tenant(): string { return this.tenantScope; }

  async getByLogicalRef(ref: string): Promise<AgentSessionState | null> {
    const key = logicalSessionRefSchema.parse(ref.trim());
    const hot = this.byLogicalRef.get(key);
    if (hot) return hot;
    const session = await this.store.get(key);
    if (session) this.byLogicalRef.set(key, session);
    return session;
  }

  async getOrCreate(input: { intentProvenance: IntentProvenance; intent: WorkflowIntent; logicalSessionRef: LogicalSessionRef; logicalSessionId: string }): Promise<AgentSessionState> {
    const existing = await this.getByLogicalRef(input.logicalSessionRef);
    if (existing) {
      if (existing.intent !== input.intent || existing.logicalSessionId !== input.logicalSessionId) throw new Error("Session identity is already bound to a different workflow");
      return existing;
    }
    const fresh: AgentSessionState = {
      schemaVersion: 2, ...input, tenantScope: this.tenantScope,
      seeds: [], teachingTsv: "", waves: [], planCommits: [], updatedAt: new Date().toISOString(),
    };
    await this.store.put(fresh);
    this.byLogicalRef.set(fresh.logicalSessionRef, fresh);
    return fresh;
  }

  async update(state: AgentSessionState): Promise<void> {
    state.updatedAt = new Date().toISOString();
    await this.store.put(state);
    this.byLogicalRef.set(state.logicalSessionRef, state);
  }
}
