import type { AgentWorkflowWorldDefinition } from "../define-agent.js";
import { isVercelHosted } from "../gateway-model.js";
import { hasLinkedBlobStore } from "../storage/vercel-blob.js";
import { LocalArchiveStore } from "./index.js";
import { ProdArchiveStore } from "./prod-archive-store.js";
import { VercelBlobArchiveAdapter } from "./vercel-blob-adapter.js";
import type { BlobArchiveAdapter } from "./types.js";

export type ArchiveBackend = "local" | "vercel" | "postgres";

export function resolveArchiveBackend(
  world?: AgentWorkflowWorldDefinition,
): ArchiveBackend {
  const explicit = process.env.PLASM_ARCHIVE_BACKEND?.trim().toLowerCase();
  if (explicit === "local" || explicit === "vercel" || explicit === "postgres") {
    return explicit;
  }
  if (isVercelHosted() || hasLinkedBlobStore()) {
    return "vercel";
  }
  if (world?.type === "postgres") return "postgres";
  return "local";
}

function resolveBlobAdapter(): BlobArchiveAdapter | undefined {
  if (hasLinkedBlobStore() || isVercelHosted()) {
    return new VercelBlobArchiveAdapter();
  }
  return undefined;
}

export function createArchiveStore(
  agentRoot: string,
  options?: { backend?: ArchiveBackend; world?: AgentWorkflowWorldDefinition },
): LocalArchiveStore | ProdArchiveStore {
  const backend = options?.backend ?? resolveArchiveBackend(options?.world);
  const blob = resolveBlobAdapter();
  if (backend === "vercel" && blob) {
    return new ProdArchiveStore(agentRoot, blob);
  }
  return LocalArchiveStore.fromAgentRoot(agentRoot);
}
