import { readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";

const WORKFLOW_DIRS_NEEDLE =
  "dirs: ['.'], // Different apps that use nitro have different directories";
const WORKFLOW_DIRS_REPLACEMENT =
  "dirs: nitro.options.workflow?.dirs ?? ['workflows'], // plasm: scoped workflow scan";

async function patchBuildersFile(buildersPath: string): Promise<void> {
  const source = await readFile(buildersPath, "utf8");
  if (!source.includes(WORKFLOW_DIRS_NEEDLE)) return;
  await writeFile(
    buildersPath,
    source.replaceAll(WORKFLOW_DIRS_NEEDLE, WORKFLOW_DIRS_REPLACEMENT),
    "utf8",
  );
}

/** @workflow/nitro hardcodes dirs: ['.'] — scope to nitro.options.workflow.dirs for Eve apps. */
export async function ensureWorkflowBuilderDirs(projectRoot: string): Promise<void> {
  const candidates = new Set<string>();
  candidates.add(path.join(projectRoot, "node_modules/@workflow/nitro/dist/builders.js"));

  try {
    const require = createRequire(import.meta.url);
    const nitroEntry = require.resolve("@workflow/nitro");
    candidates.add(path.join(path.dirname(nitroEntry), "builders.js"));
  } catch {
    // framework package may not resolve @workflow/nitro in all contexts
  }

  await Promise.all(
    [...candidates].map(async (buildersPath) => {
      try {
        await patchBuildersFile(buildersPath);
      } catch {
        // optional copy
      }
    }),
  );
}
