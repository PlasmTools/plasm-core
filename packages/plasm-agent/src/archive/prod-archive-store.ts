import { LocalArchiveStore } from "./index.js";
import type {
  BlobArchiveAdapter,
  KvArchiveIndexAdapter,
  PlanArchiveSnapshot,
  RunSnapshot,
  TraceDetail,
  TraceRecord,
  TraceSummary,
} from "./types.js";

function blobKey(kind: "plan" | "run" | "trace", id: string): string {
  return `plasm/archives/${kind}/${id}.json`;
}

function indexKey(kind: "plan" | "run" | "trace", id: string): string {
  return `plasm:index:${kind}:${id}`;
}

/** Durable archive store: local FS cache + Blob bodies + KV/Postgres index. */
export class ProdArchiveStore {
  readonly local: LocalArchiveStore;

  constructor(
    agentRoot: string,
    private readonly blob: BlobArchiveAdapter,
    private readonly index: KvArchiveIndexAdapter,
  ) {
    this.local = LocalArchiveStore.fromAgentRoot(agentRoot);
  }

  get paths() {
    return this.local.paths;
  }

  async bootstrap(): Promise<void> {
    await this.local.bootstrap();
  }

  private async putJson(key: string, value: unknown): Promise<void> {
    await this.blob.put(key, JSON.stringify(value));
  }

  private async getJson<T>(key: string): Promise<T | null> {
    const bytes = await this.blob.get(key);
    if (!bytes) return null;
    return JSON.parse(Buffer.from(bytes).toString("utf8")) as T;
  }

  async recordToolEvent(
    tenantId: string,
    traceId: string,
    kind: string,
    name: string,
    attributes?: Record<string, string | number | boolean>,
  ): Promise<void> {
    await this.local.recordToolEvent(tenantId, traceId, kind, name, attributes);
  }

  async finalizeTrace(detail: TraceDetail): Promise<void> {
    await this.local.finalizeTrace(detail);
    const key = blobKey("trace", detail.summary.trace_id);
    await this.putJson(key, detail);
    await this.index.set(indexKey("trace", detail.summary.trace_id), key);
  }

  async writePlanArchive(snapshot: PlanArchiveSnapshot): Promise<void> {
    await this.local.writePlanArchive(snapshot);
    const key = blobKey("plan", snapshot.plan_commit_ref);
    await this.putJson(key, snapshot);
    await this.index.set(indexKey("plan", snapshot.plan_commit_ref), key);
  }

  async writeRunSnapshot(snapshot: RunSnapshot): Promise<void> {
    await this.local.writeRunSnapshot(snapshot);
    const key = blobKey("run", snapshot.run_id);
    await this.putJson(key, snapshot);
    await this.index.set(indexKey("run", snapshot.run_id), key);
  }

  async listTraces(tenantId: string, limit = 50): Promise<TraceSummary[]> {
    const keys = await this.index.list("plasm:index:trace:");
    const items: TraceSummary[] = [];
    for (const idx of keys) {
      const blobRef = await this.index.get(idx);
      if (!blobRef) continue;
      const detail = await this.getJson<TraceDetail>(blobRef);
      if (detail?.summary) items.push(detail.summary);
    }
    if (items.length) {
      items.sort((a, b) => b.started_at_ms - a.started_at_ms);
      return items.slice(0, limit);
    }
    return this.local.listTraces(tenantId, limit);
  }

  async getTrace(tenantId: string, traceId: string): Promise<TraceDetail | null> {
    const blobRef = await this.index.get(indexKey("trace", traceId));
    if (blobRef) {
      const detail = await this.getJson<TraceDetail>(blobRef);
      if (detail) return detail;
    }
    return this.local.getTrace(tenantId, traceId);
  }

  async listPlans(limit = 50): Promise<PlanArchiveSnapshot[]> {
    const keys = await this.index.list("plasm:index:plan:");
    const items: PlanArchiveSnapshot[] = [];
    for (const idx of keys) {
      const blobRef = await this.index.get(idx);
      if (!blobRef) continue;
      const plan = await this.getJson<PlanArchiveSnapshot>(blobRef);
      if (plan) items.push(plan);
    }
    if (items.length) {
      items.sort((a, b) => b.archived_at.localeCompare(a.archived_at));
      return items.slice(0, limit);
    }
    return this.local.listPlans(limit);
  }

  async listRuns(limit = 50): Promise<RunSnapshot[]> {
    const keys = await this.index.list("plasm:index:run:");
    const items: RunSnapshot[] = [];
    for (const idx of keys) {
      const blobRef = await this.index.get(idx);
      if (!blobRef) continue;
      const run = await this.getJson<RunSnapshot>(blobRef);
      if (run) items.push(run);
    }
    if (items.length) {
      items.sort((a, b) => b.archived_at.localeCompare(a.archived_at));
      return items.slice(0, limit);
    }
    return this.local.listRuns(limit);
  }

  async listArchives(limit?: number) {
    return this.local.listArchives(limit);
  }
}

export type { TraceRecord };
