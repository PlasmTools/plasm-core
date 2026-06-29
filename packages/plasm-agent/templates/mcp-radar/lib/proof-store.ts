import { appendFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

function isVercelHosted(): boolean {
  return (
    process.env.VERCEL === "1" ||
    Boolean(process.env.VERCEL_DEPLOYMENT_ID?.trim()) ||
    Boolean(process.env.VERCEL_ENV?.trim())
  );
}

function hasLinkedBlobStore(): boolean {
  return Boolean(
    process.env.BLOB_READ_WRITE_TOKEN?.trim() || process.env.BLOB_STORE_ID?.trim(),
  );
}

export interface SeenState {
  itemIds: string[];
  lastRunAt?: string;
  lastRunStatus?: "ok" | "skipped" | "error";
  lastNewCount?: number;
}

export interface LastRunMeta {
  at: string;
  status: "ok" | "skipped" | "error";
  newItems: number;
  message?: string;
}

export type ProofStoreBackend = "fs" | "vercel";

export interface ProofStore {
  backend(): ProofStoreBackend;
  readProofMarkdown(): Promise<string>;
  appendProofRun(section: { runAt: string; body: string }): Promise<void>;
  loadSeenState(): Promise<SeenState>;
  saveSeenState(state: SeenState): Promise<void>;
  addSeenIds(ids: string[]): Promise<SeenState>;
  loadLastRun(): Promise<LastRunMeta | null>;
  saveLastRun(meta: LastRunMeta): Promise<void>;
}

const PROOF_BLOB_KEY = "research/mcp-innovations-proof.md";
const SEEN_BLOB_KEY = "research/seen-hn-items.json";
const LAST_RUN_BLOB_KEY = "research/last-run.json";
const PROOF_HEADER = "# MCP Innovations Proof Log\n\n";

function researchDir(agentRoot: string): string {
  return path.join(agentRoot, ".plasm", "research");
}

function proofPath(agentRoot: string): string {
  return path.join(agentRoot, "research", "mcp-innovations-proof.md");
}

function seenPath(agentRoot: string): string {
  return path.join(researchDir(agentRoot), "seen-hn-items.json");
}

function lastRunPath(agentRoot: string): string {
  return path.join(researchDir(agentRoot), "last-run.json");
}

export function resolveProofStoreBackend(): ProofStoreBackend {
  const explicit = process.env.PLASM_PROOF_STORE_BACKEND?.trim().toLowerCase();
  if (explicit === "fs" || explicit === "vercel") return explicit;
  if (isVercelHosted() || hasLinkedBlobStore()) return "vercel";
  return "fs";
}

async function blobGetText(key: string): Promise<string | null> {
  const { head } = await import("@vercel/blob");
  const meta = await head(key).catch(() => null);
  if (!meta?.url) return null;
  const response = await fetch(meta.url);
  if (!response.ok) return null;
  return response.text();
}

async function blobPutText(key: string, body: string): Promise<void> {
  const { put } = await import("@vercel/blob");
  await put(key, body, { access: "public", addRandomSuffix: false });
}

async function blobGetJson<T>(key: string): Promise<T | null> {
  const text = await blobGetText(key);
  if (!text) return null;
  return JSON.parse(text) as T;
}

async function blobPutJson(key: string, value: unknown): Promise<void> {
  await blobPutText(key, `${JSON.stringify(value, null, 2)}\n`);
}

function parseSeenState(parsed: SeenState | null): SeenState {
  if (!parsed) return { itemIds: [] };
  return {
    itemIds: Array.isArray(parsed.itemIds) ? parsed.itemIds.map(String) : [],
    lastRunAt: parsed.lastRunAt,
    lastRunStatus: parsed.lastRunStatus,
    lastNewCount: parsed.lastNewCount,
  };
}

class FsProofStore implements ProofStore {
  constructor(private readonly agentRoot: string) {}

  backend(): ProofStoreBackend {
    return "fs";
  }

  async readProofMarkdown(): Promise<string> {
    try {
      return await readFile(proofPath(this.agentRoot), "utf8");
    } catch {
      return PROOF_HEADER;
    }
  }

  async appendProofRun(section: { runAt: string; body: string }): Promise<void> {
    const date = section.runAt.slice(0, 10);
    const header = `\n\n## ${date} run (${section.runAt})\n\n`;
    const block = section.body.trim();
    if (!block) return;
    await appendFile(proofPath(this.agentRoot), `${header}${block}\n`, "utf8");
  }

  async loadSeenState(): Promise<SeenState> {
    try {
      const raw = await readFile(seenPath(this.agentRoot), "utf8");
      return parseSeenState(JSON.parse(raw) as SeenState);
    } catch {
      return { itemIds: [] };
    }
  }

  async saveSeenState(state: SeenState): Promise<void> {
    await mkdir(researchDir(this.agentRoot), { recursive: true });
    await writeFile(seenPath(this.agentRoot), `${JSON.stringify(state, null, 2)}\n`, "utf8");
  }

  async addSeenIds(ids: string[]): Promise<SeenState> {
    const state = await this.loadSeenState();
    const set = new Set(state.itemIds);
    for (const id of ids) set.add(String(id));
    const next: SeenState = { ...state, itemIds: [...set].sort() };
    await this.saveSeenState(next);
    return next;
  }

  async loadLastRun(): Promise<LastRunMeta | null> {
    try {
      const raw = await readFile(lastRunPath(this.agentRoot), "utf8");
      return JSON.parse(raw) as LastRunMeta;
    } catch {
      return null;
    }
  }

  async saveLastRun(meta: LastRunMeta): Promise<void> {
    await mkdir(researchDir(this.agentRoot), { recursive: true });
    await writeFile(lastRunPath(this.agentRoot), `${JSON.stringify(meta, null, 2)}\n`, "utf8");
  }
}

class VercelProofStore implements ProofStore {
  backend(): ProofStoreBackend {
    return "vercel";
  }

  async readProofMarkdown(): Promise<string> {
    const text = await blobGetText(PROOF_BLOB_KEY);
    return text ?? PROOF_HEADER;
  }

  async appendProofRun(section: { runAt: string; body: string }): Promise<void> {
    const date = section.runAt.slice(0, 10);
    const header = `\n\n## ${date} run (${section.runAt})\n\n`;
    const block = section.body.trim();
    if (!block) return;
    const existing = await this.readProofMarkdown();
    await blobPutText(PROOF_BLOB_KEY, `${existing}${header}${block}\n`);
  }

  async loadSeenState(): Promise<SeenState> {
    return parseSeenState(await blobGetJson<SeenState>(SEEN_BLOB_KEY));
  }

  async saveSeenState(state: SeenState): Promise<void> {
    await blobPutJson(SEEN_BLOB_KEY, state);
  }

  async addSeenIds(ids: string[]): Promise<SeenState> {
    const state = await this.loadSeenState();
    const set = new Set(state.itemIds);
    for (const id of ids) set.add(String(id));
    const next: SeenState = { ...state, itemIds: [...set].sort() };
    await this.saveSeenState(next);
    return next;
  }

  async loadLastRun(): Promise<LastRunMeta | null> {
    return blobGetJson<LastRunMeta>(LAST_RUN_BLOB_KEY);
  }

  async saveLastRun(meta: LastRunMeta): Promise<void> {
    await blobPutJson(LAST_RUN_BLOB_KEY, meta);
  }
}

const storeCache = new Map<string, ProofStore>();

export function resolveProofStore(agentRoot: string): ProofStore {
  const key = `${resolveProofStoreBackend()}:${agentRoot}`;
  const cached = storeCache.get(key);
  if (cached) return cached;
  const store =
    resolveProofStoreBackend() === "vercel"
      ? new VercelProofStore()
      : new FsProofStore(agentRoot);
  storeCache.set(key, store);
  return store;
}

export async function readProofMarkdown(agentRoot: string): Promise<string> {
  return resolveProofStore(agentRoot).readProofMarkdown();
}

export async function appendProofRun(
  agentRoot: string,
  section: { runAt: string; body: string },
): Promise<void> {
  return resolveProofStore(agentRoot).appendProofRun(section);
}

export async function loadSeenState(agentRoot: string): Promise<SeenState> {
  return resolveProofStore(agentRoot).loadSeenState();
}

export async function saveSeenState(agentRoot: string, state: SeenState): Promise<void> {
  return resolveProofStore(agentRoot).saveSeenState(state);
}

export async function addSeenIds(agentRoot: string, ids: string[]): Promise<SeenState> {
  return resolveProofStore(agentRoot).addSeenIds(ids);
}

export async function saveLastRun(agentRoot: string, meta: LastRunMeta): Promise<void> {
  return resolveProofStore(agentRoot).saveLastRun(meta);
}

export async function loadLastRun(agentRoot: string): Promise<LastRunMeta | null> {
  return resolveProofStore(agentRoot).loadLastRun();
}

export function tavilyConfigured(): boolean {
  return Boolean(process.env.TAVILY_API_TOKEN?.trim());
}

export function gatewayConfigured(): boolean {
  if (
    process.env.AI_GATEWAY_API_KEY?.trim() ||
    process.env.AI_API_GATEWAY_KEY?.trim() ||
    process.env.AI_GATEWAY_KEY?.trim()
  ) {
    return true;
  }
  return isVercelHosted();
}
