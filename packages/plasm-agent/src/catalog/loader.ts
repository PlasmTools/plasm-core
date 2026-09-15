import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { z } from "zod";

const digest = z.string().regex(/^[a-f0-9]{64}$/);
const basename = z.string().min(1).refine((name) => path.basename(name) === name && name !== "." && name !== "..");
export const CatalogManifestSchema = z.object({
  format_version: z.literal(3),
  entry_id: z.string().min(1),
  version: z.number().int().positive(),
  cgs_hash: digest,
  label: z.string().optional(),
  tags: z.array(z.string()).default([]),
  cgs_json: basename,
  recipes_json: basename,
  recipes_hash: digest,
  discovery_json: basename,
  discovery_hash: digest,
  embedding_profile: z.object({
    model: z.literal("openai/text-embedding-3-small"),
    dimensions: z.literal(1536),
    encoding_format: z.literal("float"),
  }).strict(),
}).strict();

export interface CatalogManifest {
  entryId: string;
  label?: string;
  cgsHash: string;
}

export interface LoadedCatalog {
  rootDir: string;
  manifestPath: string;
  manifest: CatalogManifest;
}

export interface CatalogLoader {
  discover(agentRoot: string): Promise<LoadedCatalog[]>;
}

/** Both artifact digests are checked here; Rust validates their full typed content on load. */
export async function loadPackedCatalog(manifestPath: string): Promise<LoadedCatalog> {
  if (!manifestPath.endsWith(".manifest.json")) {
    throw new Error("Catalog loading requires a format-3 packed manifest; repack raw YAML first");
  }
  const absolute = path.resolve(manifestPath);
  const rootDir = path.dirname(absolute);
  const manifest = CatalogManifestSchema.parse(JSON.parse(await readFile(absolute, "utf8")));
  for (const [name, expected] of [
    [manifest.cgs_json, manifest.cgs_hash],
    [manifest.recipes_json, manifest.recipes_hash],
    [manifest.discovery_json, manifest.discovery_hash],
  ]) {
    const bytes = await readFile(path.join(rootDir, name!));
    if (createHash("sha256").update(bytes).digest("hex") !== expected) {
      throw new Error(`Catalog artifact digest mismatch: ${name}`);
    }
  }
  return { rootDir, manifestPath: absolute, manifest: { entryId: manifest.entry_id, label: manifest.label, cgsHash: manifest.cgs_hash } };
}

export class FilesystemCatalogLoader implements CatalogLoader {
  async discover(agentRoot: string): Promise<LoadedCatalog[]> {
    const catalogsDir = path.join(agentRoot, "catalogs");
    const set = z.object({ format_version: z.literal(3), manifests: z.array(basename).min(1) }).strict()
      .parse(JSON.parse(await readFile(path.join(catalogsDir, "catalog-set.json"), "utf8")));
    const loaded = await Promise.all(set.manifests.map((name) => loadPackedCatalog(path.join(catalogsDir, name))));
    const ids = new Set<string>();
    for (const catalog of loaded) {
      if (ids.has(catalog.manifest.entryId)) throw new Error(`Duplicate catalog revision: ${catalog.manifest.entryId}`);
      ids.add(catalog.manifest.entryId);
    }
    return loaded.sort((a, b) => a.manifest.entryId.localeCompare(b.manifest.entryId));
  }
}
