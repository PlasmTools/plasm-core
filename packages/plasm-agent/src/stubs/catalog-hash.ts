import { readFile } from "node:fs/promises";
import path from "node:path";

import { computeCatalogCgsHash } from "./generator.js";

/** Live CGS digest from authored `domain.yaml` + `mappings.yaml` on disk. */
export async function resolveCatalogLiveHash(catalogDir: string): Promise<string> {
  const domainYaml = await readFile(path.join(catalogDir, "domain.yaml"), "utf8");
  const mappingsYaml = await readFile(path.join(catalogDir, "mappings.yaml"), "utf8");
  return computeCatalogCgsHash(domainYaml, mappingsYaml);
}
