/** Eve-aligned Vercel Blob helpers — OIDC on hosted Vercel, no manual tokens required. */

export function hasLinkedBlobStore(): boolean {
  return Boolean(
    process.env.BLOB_READ_WRITE_TOKEN?.trim() ||
      process.env.BLOB_STORE_ID?.trim() ||
      process.env.PLASM_RUN_ARTIFACTS_URL?.trim(),
  );
}

export async function blobGetText(key: string): Promise<string | null> {
  const { head } = await import("@vercel/blob");
  const meta = await head(key).catch(() => null);
  if (!meta?.url) return null;
  const response = await fetch(meta.url);
  if (!response.ok) return null;
  return response.text();
}

export async function blobPutText(key: string, body: string): Promise<void> {
  const { put } = await import("@vercel/blob");
  await put(key, body, { access: "public", addRandomSuffix: false });
}

export async function blobGetJson<T>(key: string): Promise<T | null> {
  const text = await blobGetText(key);
  if (!text) return null;
  return JSON.parse(text) as T;
}

export async function blobPutJson(key: string, value: unknown): Promise<void> {
  await blobPutText(key, `${JSON.stringify(value, null, 2)}\n`);
}

export async function blobList(prefix: string): Promise<string[]> {
  const { list } = await import("@vercel/blob");
  const keys: string[] = [];
  let cursor: string | undefined;
  do {
    const page = await list({ prefix, cursor });
    for (const blob of page.blobs) {
      keys.push(blob.pathname);
    }
    cursor = page.hasMore ? page.cursor : undefined;
  } while (cursor);
  return keys;
}
