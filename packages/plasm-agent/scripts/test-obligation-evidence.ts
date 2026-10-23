import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { checkObligationEvidence, sameObligationPredicate, type ObligationVerification } from "../src/tools/obligation-evidence.js";
import { TaskLedgerStore, createTaskLedgerTool } from "../src/tools/task-ledger.js";
import type { PlanArchiveSnapshot, RunSnapshot } from "../src/archive/types.js";
import { LocalArchiveStore } from "../src/archive/index.js";

const verification: ObligationVerification = {
  logical_session_ref: "workflow", plan_commit_ref: "pc1", binding: "verify", field: "satisfied",
};
const runId = `pr${"a".repeat(64)}`;
const comp = {
  steps: {
    observed: { kind: "invoke", effect_class: "read" },
    verify: { kind: "map", effect_class: "artifact_read", compute: { source: "observed" } },
  },
  bind: { holes: { verify: [{ step: "observed", alias: "source" }] }, deps: { verify: ["observed"] } },
};
const plan: PlanArchiveSnapshot = { plan_commit_ref: "pc1", logical_session_ref: "workflow",
  program: "abstract verifier", comp_json: comp, archived_at: "2026-01-01T00:00:00Z" };
function run(value: unknown = true): RunSnapshot {
  return { run_id: runId, plan_commit_ref: "pc1", logical_session_ref: "workflow", ok: true,
    message: "complete", archived_at: "2026-01-01T00:01:00Z",
    native_snapshot: { row_materialization: "Computed", entities: [{ completeness: "complete", fields: { satisfied: value } }] },
    _meta: { plasm: { steps: [{ run_id: runId, node_id: "verify", artifact_complete: true }] } } };
}
assert.deepEqual(checkObligationEvidence(verification, plan, run()), { ok: true });
for (const value of [false, 0, 1, "true", [], {}]) {
  assert.deepEqual(checkObligationEvidence(verification, plan, run(value)), { ok: false, code: "not_true" });
}
assert.deepEqual(checkObligationEvidence(verification, plan, run(null)), { ok: false, code: "null_field" });
assert.deepEqual(checkObligationEvidence({ ...verification, field: "absent" }, plan, run()), { ok: false, code: "missing_field" });
for (const changed of [
  { ...run(), ok: false },
  { ...run(), _meta: undefined },
  { ...run(), native_snapshot: { row_materialization: "Catalog", entities: [] } },
  { ...run(), native_snapshot: { row_materialization: "Computed", entities: [{ completeness: "partial", fields: { satisfied: true } }] } },
]) assert.deepEqual(checkObligationEvidence(verification, plan, changed), { ok: false, code: "incomplete" });
assert.deepEqual(checkObligationEvidence(verification, plan, { ...run(), logical_session_ref: "other" }), { ok: false, code: "wrong_plan" });
assert.deepEqual(checkObligationEvidence(verification, plan, { ...run(), plan_commit_ref: "pc2" }), { ok: false, code: "wrong_plan" });
for (const altered of [
  { ...comp, bind: { holes: {}, deps: { verify: ["observed"] } } }, // order alone is not data
  { ...comp, steps: { ...comp.steps, observed: { kind: "pure", effect_class: "artifact_read", recorded_output: { run_id: runId } } } },
  { ...comp, steps: { ...comp.steps, observed: { kind: "invoke", effect_class: "write" } } },
  { ...comp, bind: { holes: { verify: [{ step: "missing" }] } } },
  { ...comp, bind: { holes: { verify: [{ step: "verify" }] } } },
]) assert.deepEqual(checkObligationEvidence(verification, { ...plan, comp_json: altered }, run()), { ok: false, code: "not_observed_compute" });

const root = await mkdtemp(path.join(tmpdir(), "obligation-evidence-"));
try {
  const archive = new LocalArchiveStore(root);
  await archive.bootstrap();
  await archive.writePlanArchive(plan);
  let lookups = 0;
  const evidence = { check: async (v: ObligationVerification, id?: string) => {
    lookups++;
    return checkObligationEvidence(v, await archive.getPlan(v.plan_commit_ref, v.logical_session_ref),
      id === undefined ? undefined : await archive.getRun(id, v.logical_session_ref));
  }, samePredicate: async (a: ObligationVerification, b: ObligationVerification) => sameObligationPredicate(a, b,
    await archive.getPlan(a.plan_commit_ref, a.logical_session_ref), await archive.getPlan(b.plan_commit_ref, b.logical_session_ref)) };
  for (const task of ["Restore route targets", "Restore reservation quantities", "Restore access membership"]) {
    const checkpoint = path.join(root, `${lookups}.json`);
    const store = new TaskLedgerStore(checkpoint);
    const tool = createTaskLedgerTool(store, evidence).task_ledger!;
    const write = async (record: unknown) => tool.execute!({ action: "write", record }, { toolCallId: "test", messages: [], context: {} });
    await write({ obligations: [{ id: 1, task, status: "pending", verification }] });
    assert.equal(store.record.obligations[0]?.status, "pending");
    const before = store.record;
    await archive.writeRunSnapshot(run(false));
    assert.match(String(await write({ obligations: [{ id: 1, task, status: "resolved", evidence_ref: runId }] })), /did not return boolean true/);
    assert.deepEqual(store.record, before);
    assert.deepEqual(new TaskLedgerStore(checkpoint).record, before);
    assert.match(String(await write({ obligations: [{ id: 1, task: "Intermediate isolation", status: "pending",
      verification: { ...verification, binding: "isolate" } }] })), /cannot change/);
    await write({ obligations: [{ id: 1, task: `${task}; refined wording`, status: "deferred", reason: "Await observation" }] });
    assert.equal(store.record.obligations[0]?.original_task, task);
    assert.deepEqual(store.record.obligations[0]?.verification, verification);
    await write({ obligations: [{ id: 1, task: `${task} only if the primary operation fails`, status: "pending" }] });
    assert.equal(store.record.obligations[0]?.original_task, task, "conditional wording cannot overwrite original work");
    assert.match(String(await write({ obligations: [{ id: 1, task, status: "resolved", evidence_ref: runId }] })), /did not return boolean true/);
    assert.equal(store.record.obligations[0]?.status, "pending", "refinement does not weaken resolution evidence");
    const fresh = { ...verification, plan_commit_ref: "pc2" };
    await archive.writePlanArchive({ ...plan, plan_commit_ref: "pc2" });
    assert.equal(sameObligationPredicate(verification, fresh, plan, { ...plan, plan_commit_ref: "pc2", program: "changed predicate" }), false);
    await archive.writeRunSnapshot({ ...run(true), plan_commit_ref: "pc2" });
    assert.match(String(await write({ obligations: [{ id: 1, task, status: "resolved", verification: fresh, evidence_ref: runId }] })), /Host stored/);
    assert.deepEqual(store.record.obligations[0]?.verification, fresh);
    assert.equal(store.record.obligations[0]?.status, "resolved");
    assert.deepEqual(new TaskLedgerStore(checkpoint).record, store.record);
    assert.deepEqual(store.record.completed_effects, [], "resolution creates no effect receipts");
  }
  assert.ok(lookups > 0);
  const oldCheckpoint = path.join(root, "old.json");
  await writeFile(oldCheckpoint, JSON.stringify({ version: 2, record: new TaskLedgerStore().record }));
  assert.throws(() => new TaskLedgerStore(oldCheckpoint), /Task ledger load failed/,
    "old resolutions are never silently promoted to checked evidence");
  const concurrent = new TaskLedgerStore();
  let release!: () => void;
  const delayed = new Promise<void>(resolve => { release = resolve; });
  const concurrentTool = createTaskLedgerTool(concurrent, { check: async () => { await delayed; return { ok: true }; } }).task_ledger!;
  const pending = concurrentTool.execute!({ action: "write", record: { obligations: [{ id: 1, task: "Verify", status: "pending", verification }] } },
    { toolCallId: "first", messages: [], context: {} });
  await concurrentTool.execute!({ action: "write", record: { scope: "New accepted scope" } },
    { toolCallId: "second", messages: [], context: {} });
  release();
  assert.match(String(await pending), /ledger changed/);
  assert.equal(concurrent.record.scope, "New accepted scope");
  assert.deepEqual(concurrent.record.obligations, []);
  const interleaved = new TaskLedgerStore();
  interleaved.replace({ ...interleaved.record, obligations: [1, 2].map(id => ({ id, task: "Verify", status: "pending", verification })) });
  let revision = 0;
  let reads = 0;
  const guarded = createTaskLedgerTool(interleaved, { check: async () => {
    const observedRevision = revision;
    if (++reads === 2) revision++; // An execution starts during the second lookup.
    return { ok: true, current: () => observedRevision === revision };
  } }).task_ledger!;
  const unchanged = interleaved.record;
  const rejected = await guarded.execute!({ action: "write", record: { obligations: [1, 2].map(id => ({ id, task: "Verify", status: "resolved", evidence_ref: runId })) } },
    { toolCallId: "interleaved", messages: [], context: {} });
  assert.match(String(rejected), /execution occurred/);
  assert.deepEqual(interleaved.record, unchanged, "all evidence witnesses are checked atomically at acceptance");
} finally { await rm(root, { recursive: true, force: true }); }
console.log("PASS: observed-compute evidence matrix, immutable binding, atomic rejection, durable resolution; archive reads only, no dispatch.");
