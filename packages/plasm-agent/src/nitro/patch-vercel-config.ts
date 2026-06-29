import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

import { frameworkPackageVersion } from "../package-version.js";
import type { PreparedPlasmHost } from "./prepare-host.js";
import { vercelOutputDir } from "./paths.js";

interface VercelOutputConfig {
  version?: number;
  framework?: { name?: string; version?: string };
  crons?: Array<{ path: string; schedule: string }>;
  [key: string]: unknown;
}

/** Merge Plasm crons + framework metadata into Nitro-emitted config.json. */
export async function patchVercelOutputConfig(
  projectRoot: string,
  host: PreparedPlasmHost,
): Promise<void> {
  const configPath = path.join(vercelOutputDir(projectRoot), "config.json");
  let config: VercelOutputConfig;
  try {
    config = JSON.parse(await readFile(configPath, "utf8")) as VercelOutputConfig;
  } catch {
    return;
  }

  config.version = 3;
  config.framework = {
    version: frameworkPackageVersion(),
  };

  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, "utf8");
}
