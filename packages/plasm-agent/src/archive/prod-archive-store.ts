import { LocalArchiveStore } from "./index.js";
import type {
  BlobArchiveAdapter,
  PlanArchiveSnapshot,
  RunSnapshot,
  TraceDetail,
  TraceRecord,
  TraceSummary,
} from "./types.js";

function blobKey(kind: "plan" | "run" | "trace", id: string): string {
  return `plasm/archives/${kind}/${id}.json`;
}

function archivePrefix(kind: "plan" | "run" | "trace"): string {
  return `plasm/archives/${kind}/`;
}

/** Durable archive store: local FS cache + Blob bodies (Eve-aligned, no KV index). */
export class ProdArchiveStore {
  readonly local: LocalArchiveStore;

  constructor(
    agentRoot: string,
    private readonly blob: BlobArchiveAdapter,
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

  private async listJson<T>(kind: "plan" | "run" | "trace"): Promise<T[]> {
    const paths = await this.blob.list(archivePrefix(kind));
    const items: T[] = [];
    for (const key of paths) {
      if (!key.endsWith(".json")) continue;
      const item = await this.getJson<T>(key);
      if (item) items.push(item);
    }
    return items;
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
    await this.putJson(blobKey("trace", detail.summary.trace_id), detail);
  }

  async writePlanArchive(snapshot: PlanArchiveSnapshot): Promise<void> {
    await this.local.writePlanArchive(snapshot);
    await this.putJson(blobKey("plan", snapshot.plan_commit_ref), snapshot);
  }

  async writeRunSnapshot(snapshot: RunSnapshot): Promise<void> {
    await this.local.writeRunSnapshot(snapshot);
    await this.putJson(blobKey("run", snapshot.run_id), snapshot);
  }

  async listTraces(tenantId: string, limit = 50): Promise<TraceSummary[]> {
    const items = await this.listJson<TraceDetail>("trace");
    if (items.length) {
      const summaries = items
        .map((detail) => detail.summary)
        .filter((summary) => summary.tenant_id === tenantId);
      summaries.sort((a, b) => b.started_at_ms - a.started_at_ms);
      return summaries.slice(0, limit);
    }
    return this.local.listTraces(tenantId, limit);
  }

  async getTrace(tenantId: string, traceId: string): Promise<TraceDetail | null> {
    void tenantId;
    const detail = await this.getJson<TraceDetail>(blobKey("trace", traceId));
    if (detail) return detail;
    return this.local.getTrace(tenantId, traceId);
  }

  async listPlans(limit = 50): Promise<PlanArchiveSnapshot[]> {
    const items = await this.listJson<PlanArchiveSnapshot>("plan");
    if (items.length) {
      items.sort((a, b) => b.archived_at.localeCompare(a.archived_at));
      return items.slice(0, limit);
    }
    return this.local.listPlans(limit);
  }

  async listRuns(limit = 50): Promise<RunSnapshot[]> {
    const items = await this.listJson<RunSnapshot>("run");
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
