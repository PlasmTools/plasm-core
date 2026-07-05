import {
  createArchiveStore,
  resolveArchiveBackend,
  type RunSnapshot,
} from "@plasm_lang/vercel-agent";

/** One scan cycle's plasm_run rows — derived from durable operator archives. */
export interface LastRunMeta {
  at: string;
  status: "ok" | "error";
  message?: string;
  runIds: string[];
  logicalSessionRef?: string;
  okCount: number;
  errorCount: number;
  source: "archive";
}

const SCAN_BATCH_GAP_MS = 15 * 60 * 1000;

function radarRunsForIntent(runs: RunSnapshot[], intent: string): RunSnapshot[] {
  return runs
    .filter((run) => run.intent === intent)
    .sort((a, b) => b.archived_at.localeCompare(a.archived_at));
}

/** Latest plasm_run batch for one agent scan (same session, within one workflow window). */
function latestScanBatch(runs: RunSnapshot[]): RunSnapshot[] {
  if (runs.length === 0) return [];
  const [latest] = runs;
  const latestMs = Date.parse(latest.archived_at);
  const sessionRef = latest.logical_session_ref;
  const batch: RunSnapshot[] = [];

  for (const run of runs) {
    if (sessionRef && run.logical_session_ref !== sessionRef) continue;
    const runMs = Date.parse(run.archived_at);
    if (latestMs - runMs > SCAN_BATCH_GAP_MS) break;
    batch.push(run);
  }

  return batch.sort((a, b) => a.archived_at.localeCompare(b.archived_at));
}

export function deriveLastRunFromRuns(
  runs: RunSnapshot[],
  intent: string,
): LastRunMeta | null {
  const intentRuns = radarRunsForIntent(runs, intent);
  const batch = latestScanBatch(intentRuns);
  if (batch.length === 0) return null;

  const latest = batch[batch.length - 1];
  const okCount = batch.filter((run) => run.ok).length;
  const errorCount = batch.length - okCount;

  return {
    at: latest.archived_at,
    status: latest.ok ? "ok" : "error",
    message: latest.ok ? undefined : latest.message,
    runIds: batch.map((run) => run.run_id),
    logicalSessionRef: latest.logical_session_ref,
    okCount,
    errorCount,
    source: "archive",
  };
}

export async function loadLastRunFromArchives(
  agentRoot: string,
  intent: string,
): Promise<LastRunMeta | null> {
  const archive = createArchiveStore(agentRoot);
  const runs = await archive.listRuns(200);
  return deriveLastRunFromRuns(runs, intent);
}

export async function radarArchiveSummary(
  agentRoot: string,
  intent: string,
): Promise<{
  archiveBackend: ReturnType<typeof resolveArchiveBackend>;
  totalRuns: number;
  intentRuns: number;
  lastRun: LastRunMeta | null;
}> {
  const archive = createArchiveStore(agentRoot);
  const runs = await archive.listRuns(200);
  const intentRuns = radarRunsForIntent(runs, intent);

  return {
    archiveBackend: resolveArchiveBackend(),
    totalRuns: runs.length,
    intentRuns: intentRuns.length,
    lastRun: deriveLastRunFromRuns(runs, intent),
  };
}
