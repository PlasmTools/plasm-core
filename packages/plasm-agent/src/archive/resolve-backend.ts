import type { AgentWorkflowWorldDefinition } from "../define-agent.js";
import { LocalArchiveStore } from "./index.js";
import { PostgresArchiveIndexAdapter } from "./postgres-kv-adapter.js";
import { ProdArchiveStore } from "./prod-archive-store.js";
import { VercelBlobArchiveAdapter } from "./vercel-blob-adapter.js";
import { VercelKvArchiveIndexAdapter } from "./vercel-kv-adapter.js";
import type { BlobArchiveAdapter, KvArchiveIndexAdapter } from "./types.js";

export type ArchiveBackend = "local" | "vercel" | "postgres";

export function resolveArchiveBackend(
  world?: AgentWorkflowWorldDefinition,
): ArchiveBackend {
  const explicit = process.env.PLASM_ARCHIVE_BACKEND?.trim().toLowerCase();
  if (explicit === "local" || explicit === "vercel" || explicit === "postgres") {
    return explicit;
  }
  if (
    process.env.BLOB_READ_WRITE_TOKEN?.trim() ||
    process.env.PLASM_RUN_ARTIFACTS_URL?.trim()
  ) {
    if (process.env.KV_REST_API_URL?.trim() || process.env.PLASM_KV_REST_API_URL?.trim()) {
      return "vercel";
    }
  }
  if (world?.type === "postgres") return "postgres";
  return "local";
}

function resolveBlobAdapter(): BlobArchiveAdapter | undefined {
  if (process.env.BLOB_READ_WRITE_TOKEN?.trim()) {
    return new VercelBlobArchiveAdapter();
  }
  return undefined;
}

function resolveIndexAdapter(
  backend: ArchiveBackend,
): KvArchiveIndexAdapter | undefined {
  if (backend === "vercel") {
    return new VercelKvArchiveIndexAdapter();
  }
  if (backend === "postgres") {
    return new PostgresArchiveIndexAdapter();
  }
  return undefined;
}

export function createArchiveStore(
  agentRoot: string,
  options?: { backend?: ArchiveBackend; world?: AgentWorkflowWorldDefinition },
): LocalArchiveStore | ProdArchiveStore {
  const backend = options?.backend ?? resolveArchiveBackend(options?.world);
  const blob = resolveBlobAdapter();
  const index = resolveIndexAdapter(backend);
  if (backend !== "local" && blob && index) {
    return new ProdArchiveStore(agentRoot, blob, index);
  }
  return LocalArchiveStore.fromAgentRoot(agentRoot);
}
