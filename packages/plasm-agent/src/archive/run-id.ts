import { createHash } from "node:crypto";

export interface RunIdBundle {
  catalogCgsHash: string;
  planCommitRef: string;
  program: string;
  entryId?: string;
  requestFingerprints?: string[];
}

/** Deterministic `pr` + 64 hex run id aligned with OSS wire form. */
export function computeRunId(bundle: RunIdBundle): string {
  const canonical = JSON.stringify({
    v: 1,
    catalog_cgs_hash: bundle.catalogCgsHash,
    plan_commit_ref: bundle.planCommitRef,
    program: bundle.program,
    entry_id: bundle.entryId ?? null,
    request_fingerprints: [...(bundle.requestFingerprints ?? [])].sort(),
  });
  const digest = createHash("sha256").update(canonical).digest("hex");
  return `pr${digest}`;
}
