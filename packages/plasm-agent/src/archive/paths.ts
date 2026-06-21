import path from "node:path";

import type { ArchivePaths } from "./types.js";

/** Resolve local trace/run archive roots from env or agent defaults. */
export function resolveArchivePaths(agentRoot: string): ArchivePaths {
  const archivesDir = path.join(agentRoot, ".plasm", "archives");
  const traceRoot =
    process.env.PLASM_TRACE_ARCHIVE_DIR?.trim() ||
    path.join(archivesDir, "traces");
  const runRoot =
    process.env.PLASM_RUN_ARTIFACTS_DIR?.trim() ||
    path.join(archivesDir, "runs");
  return { traceRoot, runRoot };
}
