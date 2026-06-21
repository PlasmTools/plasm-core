import type { BlobArchiveAdapter } from "./types.js";

export class VercelBlobArchiveAdapter implements BlobArchiveAdapter {
  async put(key: string, body: string | Uint8Array): Promise<void> {
    const { put } = await import("@vercel/blob");
    const payload = typeof body === "string" ? body : Buffer.from(body);
    await put(key, payload, {
      access: "public",
      addRandomSuffix: false,
    });
  }

  async get(key: string): Promise<Uint8Array | null> {
    const { head } = await import("@vercel/blob");
    const meta = await head(key).catch(() => null);
    if (!meta?.url) return null;
    const response = await fetch(meta.url);
    if (!response.ok) return null;
    return new Uint8Array(await response.arrayBuffer());
  }
}
