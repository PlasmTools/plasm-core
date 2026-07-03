export interface RunAuditRecord {
  runIds: string[];
  planCommitRefs: string[];
  logicalSessionRef?: string;
  intent?: string;
}

let pending: RunAuditRecord = {
  runIds: [],
  planCommitRefs: [],
};

export function resetRunAudit(): void {
  pending = { runIds: [], planCommitRefs: [] };
}

export function recordRunAudit(detail: Record<string, unknown>): void {
  const runId = typeof detail.runId === "string" ? detail.runId : undefined;
  const planCommitRef =
    typeof detail.planCommitRef === "string"
      ? detail.planCommitRef
      : typeof detail.runRef === "string"
        ? detail.runRef
        : undefined;
  const logicalSessionRef =
    typeof detail.logicalSessionRef === "string" ? detail.logicalSessionRef : undefined;
  const intent = typeof detail.intent === "string" ? detail.intent : undefined;

  if (runId && !pending.runIds.includes(runId)) pending.runIds.push(runId);
  if (planCommitRef && !pending.planCommitRefs.includes(planCommitRef)) {
    pending.planCommitRefs.push(planCommitRef);
  }
  if (logicalSessionRef) pending.logicalSessionRef = logicalSessionRef;
  if (intent) pending.intent = intent;
}

export function drainRunAudit(): RunAuditRecord {
  const snapshot = { ...pending, runIds: [...pending.runIds], planCommitRefs: [...pending.planCommitRefs] };
  resetRunAudit();
  return snapshot;
}
