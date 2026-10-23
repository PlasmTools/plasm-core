import { z } from "zod";
import { isDeepStrictEqual } from "node:util";
import type { PlanArchiveSnapshot, RunSnapshot } from "../archive/types.js";

/** A model-authored predicate, pinned to an existing reviewed computation. */
export const obligationVerificationSchema = z.object({
  logical_session_ref: z.string().min(1).max(200),
  plan_commit_ref: z.string().regex(/^pc\d+$/),
  binding: z.string().min(1).max(200),
  field: z.string().min(1).max(200),
}).strict();
export type ObligationVerification = z.infer<typeof obligationVerificationSchema>;
export type EvidenceCheck = { ok: true; current?: () => boolean } | { ok: false; code:
  "unavailable" | "stale_observation" | "wrong_plan" | "not_observed_compute" | "incomplete" | "missing_field" | "null_field" | "not_true" };
export interface ObligationEvidence {
  check(verification: ObligationVerification, runId?: string): Promise<EvidenceCheck>;
  samePredicate?(previous: ObligationVerification, next: ObligationVerification): Promise<boolean>;
}
/** Fresh authorization may change; the reviewed computation and its output may not. */
export function sameObligationPredicate(previous: ObligationVerification, next: ObligationVerification,
  before: PlanArchiveSnapshot | null, after: PlanArchiveSnapshot | null): boolean {
  return previous.logical_session_ref === next.logical_session_ref && previous.binding === next.binding &&
    previous.field === next.field && before !== null && after !== null &&
    checkObligationEvidence(previous, before).ok && checkObligationEvidence(next, after).ok &&
    before.program === after.program && before.catalog_cgs_hash === after.catalog_cgs_hash &&
    isDeepStrictEqual(before.comp_json, after.comp_json);
}
export function evidenceCheckMessage(code: Exclude<EvidenceCheck, { ok: true }>["code"]): string {
  const messages = {
    unavailable: "review or run artifact unavailable; retain the obligation until evidence is available",
    stale_observation: "execution occurred after or overlapped this observation; obtain fresh verification before resolving",
    wrong_plan: "artifact belongs to another review or workflow; cite the bound verification result",
    not_observed_compute: "bind a compute output in a read-only plan with an observed read input",
    incomplete: "verification result is incomplete or has no matching producer receipt",
    missing_field: "verification field is absent; inspect the computation's output",
    null_field: "verification field is null, not true; retain the unresolved obligation",
    not_true: "verification did not return boolean true; repair the state before resolving",
  };
  return messages[code];
}

const stepSchema = z.object({
  kind: z.string(), effect_class: z.string(), compute: z.object({ source: z.string() }).optional(),
});
const planSchema = z.object({
  steps: z.record(stepSchema),
  bind: z.object({ holes: z.record(z.array(z.object({ step: z.string() }))) }),
});
const receiptSchema = z.object({
  run_id: z.string(), node_id: z.string(), artifact_complete: z.boolean(),
});
const snapshotSchema = z.object({
  row_materialization: z.literal("Computed"),
  entities: z.array(z.object({ completeness: z.literal("complete"), fields: z.record(z.unknown()) })).length(1),
});

/** Read-only archive validation. Never runs a plan or follows a provider URL. */
export function checkObligationEvidence(
  verification: ObligationVerification,
  plan: PlanArchiveSnapshot | null,
  run?: RunSnapshot | null,
): EvidenceCheck {
  if (!plan) return { ok: false, code: "unavailable" };
  if (plan.logical_session_ref !== verification.logical_session_ref ||
      plan.plan_commit_ref !== verification.plan_commit_ref) return { ok: false, code: "wrong_plan" };
  const parsed = planSchema.safeParse(plan.comp_json);
  if (!parsed.success) return { ok: false, code: "not_observed_compute" };
  const { steps, bind } = parsed.data;
  const selected = steps[verification.binding];
  // Verification is a separate read-only plan. Unknown/nested effects fail closed.
  if (selected?.kind !== "map" || !selected.compute ||
      !(bind.holes[verification.binding] ?? []).some(input => input.step === selected.compute?.source) ||
      Object.values(steps).some(step =>
    !["read", "artifact_read"].includes(step.effect_class) || !["invoke", "map", "derive", "pure"].includes(step.kind))) {
    return { ok: false, code: "not_observed_compute" };
  }
  const visiting = new Set<string>();
  const visited = new Set<string>();
  let observed = false;
  const visit = (name: string): boolean => {
    if (visiting.has(name) || !steps[name]) return false;
    if (visited.has(name)) return true;
    visiting.add(name);
    const step = steps[name]!;
    if (step.kind === "invoke" && step.effect_class === "read") observed = true;
    // Input holes carry values; ordering-only deps do not establish observation.
    if (!(bind.holes[name] ?? []).every(input => visit(input.step))) return false;
    visiting.delete(name); visited.add(name); return true;
  };
  if (!visit(verification.binding) || !observed) return { ok: false, code: "not_observed_compute" };
  if (run === undefined) return { ok: true };
  if (!run) return { ok: false, code: "unavailable" };
  if (run.logical_session_ref !== verification.logical_session_ref || run.plan_commit_ref !== verification.plan_commit_ref) {
    return { ok: false, code: "wrong_plan" };
  }
  const receipts = z.array(receiptSchema).safeParse(run._meta?.plasm?.steps);
  const receipt = receipts.success ? receipts.data.find(item => item.run_id === run.run_id && item.node_id === verification.binding) : undefined;
  const snapshot = snapshotSchema.safeParse(run.native_snapshot);
  if (!run.ok || !receipt?.artifact_complete || !snapshot.success) return { ok: false, code: "incomplete" };
  const fields = snapshot.data.entities[0]!.fields;
  if (!Object.hasOwn(fields, verification.field)) return { ok: false, code: "missing_field" };
  if (fields[verification.field] === null) return { ok: false, code: "null_field" };
  return fields[verification.field] === true
    ? { ok: true } : { ok: false, code: "not_true" };
}
