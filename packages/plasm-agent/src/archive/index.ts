import { LocalRunArchive } from "./local-run-archive.js";
import { LocalTraceArchive } from "./local-trace-archive.js";
import { resolveArchivePaths } from "./paths.js";
import type {
  ArchivePaths,
  PlanArchiveSnapshot,
  RunSnapshot,
  TraceDetail,
  TraceRecord,
  TraceSummary,
} from "./types.js";

export class LocalArchiveStore {
  readonly paths: ArchivePaths;
  readonly traces: LocalTraceArchive;
  readonly runs: LocalRunArchive;

  constructor(agentRoot: string, paths?: ArchivePaths) {
    this.paths = paths ?? resolveArchivePaths(agentRoot);
    this.traces = new LocalTraceArchive(this.paths.traceRoot);
    this.runs = new LocalRunArchive(this.paths.runRoot);
  }

  static fromAgentRoot(agentRoot: string, tenantId?: string): LocalArchiveStore {
    void tenantId;
    return new LocalArchiveStore(agentRoot);
  }

  async bootstrap(): Promise<void> {
    await Promise.all([this.traces.ensureRoot(), this.runs.ensureRoot()]);
  }

  async recordToolEvent(
    tenantId: string,
    traceId: string,
    kind: string,
    name: string,
    attributes?: Record<string, string | number | boolean>,
  ): Promise<void> {
    const record: TraceRecord = {
      at_ms: Date.now(),
      kind,
      name,
      attributes,
    };
    await this.traces.appendRecord(tenantId, traceId, record);
  }

  async finalizeTrace(detail: TraceDetail): Promise<void> {
    await this.traces.persistTrace(detail);
  }

  async writePlanArchive(snapshot: PlanArchiveSnapshot): Promise<void> {
    await this.runs.writePlanArchive(snapshot);
  }

  async writeRunSnapshot(snapshot: RunSnapshot): Promise<void> {
    await this.runs.writeRunSnapshot(snapshot);
  }

  async listTraces(tenantId: string, limit?: number): Promise<TraceSummary[]> {
    return this.traces.listTraces(tenantId, limit);
  }

  async getTrace(tenantId: string, traceId: string): Promise<TraceDetail | null> {
    return this.traces.getTrace(tenantId, traceId);
  }

  async listPlans(limit?: number) {
    return this.runs.listPlans(limit);
  }

  async listRuns(limit?: number) {
    return this.runs.listRuns(limit);
  }

  async listArchives(limit?: number) {
    const [plans, runs] = await Promise.all([this.listPlans(limit), this.listRuns(limit)]);
    return { plans, runs, paths: this.paths };
  }
}

export { resolveArchivePaths } from "./paths.js";
export { computeRunId } from "./run-id.js";
export type { RunIdBundle } from "./run-id.js";
export {
  UnimplementedBlobArchiveAdapter,
  UnimplementedKvArchiveIndexAdapter,
} from "./adapters.js";
export type {
  ArchivePaths,
  BlobArchiveAdapter,
  KvArchiveIndexAdapter,
  PlanArchiveSnapshot,
  RunSnapshot,
  TraceDetail,
  TraceRecord,
  TraceSummary,
} from "./types.js";
