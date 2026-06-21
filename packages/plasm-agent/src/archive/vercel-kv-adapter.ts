import type { KvArchiveIndexAdapter } from "./types.js";

export class VercelKvArchiveIndexAdapter implements KvArchiveIndexAdapter {
  private async kv() {
    const mod = await import("@vercel/kv");
    return mod.kv;
  }

  async set(key: string, value: string): Promise<void> {
    const kv = await this.kv();
    await kv.set(key, value);
  }

  async get(key: string): Promise<string | null> {
    const kv = await this.kv();
    const value = await kv.get<string>(key);
    return value ?? null;
  }

  async list(prefix: string): Promise<string[]> {
    const kv = await this.kv();
    return kv.keys(`${prefix}*`);
  }
}
