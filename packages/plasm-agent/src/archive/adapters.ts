import type { BlobArchiveAdapter, KvArchiveIndexAdapter } from "./types.js";

/** Stub Blob adapter for production wiring (Vercel Blob / object_store URL). */
export class UnimplementedBlobArchiveAdapter implements BlobArchiveAdapter {
  async put(): Promise<void> {
    throw new Error("BlobArchiveAdapter not configured — set PLASM_RUN_ARTIFACTS_URL in prod");
  }

  async get(): Promise<Uint8Array | null> {
    throw new Error("BlobArchiveAdapter not configured — set PLASM_RUN_ARTIFACTS_URL in prod");
  }
}

/** Stub KV index adapter for production archive listings. */
export class UnimplementedKvArchiveIndexAdapter implements KvArchiveIndexAdapter {
  async set(): Promise<void> {
    throw new Error("KvArchiveIndexAdapter not configured");
  }

  async get(): Promise<string | null> {
    throw new Error("KvArchiveIndexAdapter not configured");
  }

  async list(): Promise<string[]> {
    throw new Error("KvArchiveIndexAdapter not configured");
  }
}
