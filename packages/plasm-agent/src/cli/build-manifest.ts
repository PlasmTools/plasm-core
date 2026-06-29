import { readFile } from "node:fs/promises";
import path from "node:path";

export interface PlasmBuildManifest {
  compiledSlots?: Record<string, string>;
  compiledStubs?: Record<string, string>;
  projectRoot?: string;
}

/** Read `.plasm/discovery/manifest.json` written by `plasm-agent build`. */
export async function readBuildManifest(agentRoot: string): Promise<PlasmBuildManifest | null> {
  const manifestPath = path.join(agentRoot, ".plasm", "discovery", "manifest.json");
  try {
    const raw = await readFile(manifestPath, "utf8");
    return JSON.parse(raw) as PlasmBuildManifest;
  } catch {
    return null;
  }
}
