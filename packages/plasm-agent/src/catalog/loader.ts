import { access, readFile, readdir, stat } from "node:fs/promises";
import path from "node:path";

import { z } from "zod";

export const CatalogManifestSchema = z.object({
  entryId: z.string(),
  label: z.string().optional(),
  cgsHash: z.string().optional(),
});

export type CatalogManifest = z.infer<typeof CatalogManifestSchema>;

export interface LoadedCatalog {
  rootDir: string;
  manifest: CatalogManifest;
}

export interface CatalogLoader {
  discover(agentRoot: string): Promise<LoadedCatalog[]>;
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function readEntryId(catalogDir: string): Promise<string> {
  const domainPath = path.join(catalogDir, "domain.yaml");
  const raw = await readFile(domainPath, "utf8");
  const match = raw.match(/^entry_id:\s*["']?([^"'\n]+)["']?/m);
  if (match?.[1]) return match[1].trim();
  return path.basename(catalogDir);
}

export class FilesystemCatalogLoader implements CatalogLoader {
  async discover(agentRoot: string): Promise<LoadedCatalog[]> {
    const catalogsDir = path.join(agentRoot, "catalogs");
    if (!(await pathExists(catalogsDir))) {
      return [];
    }
    const entries = await readdir(catalogsDir, { withFileTypes: true });
    const loaded: LoadedCatalog[] = [];
    for (const entry of entries) {
      if (!entry.isDirectory() && !entry.isSymbolicLink()) continue;
      const rootDir = path.join(catalogsDir, entry.name);
      try {
        const info = await stat(rootDir);
        if (!info.isDirectory()) continue;
      } catch {
        continue;
      }
      const domainYaml = path.join(rootDir, "domain.yaml");
      const mappingsYaml = path.join(rootDir, "mappings.yaml");
      if (!(await pathExists(domainYaml)) || !(await pathExists(mappingsYaml))) {
        continue;
      }
      const entryId = await readEntryId(rootDir);
      loaded.push({
        rootDir,
        manifest: { entryId, label: entry.name },
      });
    }
    loaded.sort((a, b) => a.manifest.entryId.localeCompare(b.manifest.entryId));
    return loaded;
  }
}
