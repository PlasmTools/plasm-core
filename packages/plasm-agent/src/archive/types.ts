export interface TraceSummary {
  trace_id: string;
  tenant_id: string;
  logical_session_id?: string;
  logical_session_ref?: string;
  intent?: string;
  status: "completed" | "live";
  started_at_ms: number;
  ended_at_ms?: number;
  project_slug: string;
  totals?: {
    tool_calls?: number;
    model_calls?: number;
  };
}

export interface TraceDetail {
  summary: TraceSummary;
  records: TraceRecord[];
}

export interface TraceRecord {
  at_ms: number;
  kind: string;
  name: string;
  attributes?: Record<string, string | number | boolean>;
}

export interface PlanArchiveSnapshot {
  plan_commit_ref: string;
  program: string;
  catalog_cgs_hash: string;
  entry_id?: string;
  logical_session_ref?: string;
  intent?: string;
  comp_json?: unknown;
  archived_at: string;
}

export interface RunSnapshot {
  run_id: string;
  plan_commit_ref: string;
  catalog_cgs_hash: string;
  entry_id?: string;
  logical_session_ref?: string;
  intent?: string;
  ok: boolean;
  message: string;
  results?: unknown[];
  _meta?: {
    plasm?: {
      steps?: unknown[];
      request_fingerprints?: string[];
    };
  };
  archived_at: string;
}

export interface ArchivePaths {
  traceRoot: string;
  runRoot: string;
}

/** Production Blob adapter (Vercel Blob / S3-compatible). */
export interface BlobArchiveAdapter {
  put(key: string, body: string | Uint8Array): Promise<void>;
  get(key: string): Promise<Uint8Array | null>;
  list(prefix: string): Promise<string[]>;
}

/** Production KV / Postgres index adapter for archive listings. */
export interface KvArchiveIndexAdapter {
  set(key: string, value: string): Promise<void>;
  get(key: string): Promise<string | null>;
  list(prefix: string): Promise<string[]>;
}
