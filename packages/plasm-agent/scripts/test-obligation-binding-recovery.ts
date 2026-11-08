import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { TaskLedgerStore, createTaskLedgerTool } from "../src/tools/task-ledger.js";
import type { ObligationEvidence } from "../src/tools/obligation-evidence.js";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { SessionManager } from "../src/session-state.js";
import { sessionCases } from "./test-session-contract.js";

const root = await mkdtemp(path.join(tmpdir(), "binding-recovery-"));
const verification = { logical_session_ref: "abstract", plan_commit_ref: "pc1", binding: "verify", field: "value" };
const runId = `pr${"a".repeat(64)}`;
try {
  for (const status of ["pending", "deferred"] as const) {
    for (const code of ["unavailable", "wrong_plan", "not_observed_compute"] as const) {
      const checkpoint = path.join(root, `${status}-${code}.json`);
      const store = new TaskLedgerStore(checkpoint);
      let available = false;
      let result = false;
      let lookups = 0;
      const evidence: ObligationEvidence = {
        check: async (_v, run) => {
          lookups++;
          if (!available) return { ok: false, code };
          return run && !result ? { ok: false, code: "not_true" } : { ok: true };
        },
        samePredicate: async () => false,
      };
      const tool = createTaskLedgerTool(store, evidence).task_ledger!;
      const write = (record: unknown) => tool.execute!({ action: "write", record }, { toolCallId: "test", messages: [], context: {} });
      const work = status === "pending"
        ? { id: 1, task: "Restore original resource state", status }
        : { id: 1, task: "Restore original resource state", status, reason: "Await observation" };
      await write({ obligations: [work] });
      assert.equal(lookups, 0, "pending work never needs archive access");
      const ack = String(await write({ scope: "Preserve unrelated state", obligations: [{ ...work, verification }] }));
      assert.match(ack, /saved open; verification binding rejected/);
      assert.match(ack, /No binding stored/);
      assert.equal(JSON.stringify(store.record.obligations), JSON.stringify([work]));
      assert.equal(store.record.scope, "Preserve unrelated state");
      assert.deepEqual(new TaskLedgerStore(checkpoint).record, store.record);
      const before = store.record;
      assert.match(String(await write({ obligations: [{ id: 1, task: work.task, status: "resolved", evidence_ref: runId }] })), /write rejected/);
      assert.deepEqual(store.record, before);
      available = true;
      await write({ obligations: [{ ...work, verification }] });
      assert.deepEqual(store.record.obligations[0]?.verification, verification);
      const rebound = String(await write({ obligations: [{ ...work, task: "Refined restoration wording", verification: { ...verification, binding: "different" } }] }));
      assert.match(rebound, /Previous binding retained/);
      assert.equal(store.record.obligations[0]?.task, "Refined restoration wording");
      assert.deepEqual(store.record.obligations[0]?.verification, verification);
      const bound = store.record;
      assert.match(String(await write({ obligations: [{ id: 1, task: work.task, status: "resolved", evidence_ref: runId }] })), /did not return boolean true/);
      assert.deepEqual(store.record, bound);
      result = true;
      await write({ obligations: [{ id: 1, task: work.task, status: "resolved", evidence_ref: runId }] });
      assert.equal(store.record.obligations[0]?.status, "resolved");
      assert.deepEqual(new TaskLedgerStore(checkpoint).record, store.record);
    }
  }
  const store = new TaskLedgerStore();
  const tool = createTaskLedgerTool(store).task_ledger!;
  const ack = await tool.execute!({ action: "write", record: { obligations: [
    { id: 1, task: "Retain original configuration", status: "pending", verification },
  ] } }, { toolCallId: "no-archive", messages: [], context: {} });
  assert.match(String(ack), /No binding stored/);
  assert.equal(store.record.obligations[0]?.status, "pending");
  const before = store.record;
  const rejected = await tool.execute!({ action: "write", record: { obligations: [
    { id: 2, task: "Another open resource", status: "pending", verification },
    { id: 1, task: "Retain original configuration", status: "resolved", evidence_ref: runId },
  ] } }, { toolCallId: "mixed", messages: [], context: {} });
  assert.match(String(rejected), /write rejected/);
  assert.deepEqual(store.record, before, "invalid resolution rejects the whole update, including new open work");
  const unused = async (): Promise<never> => { throw new Error("no engine execution permitted"); };
  const engine: PlasmEngine = { sealEffectWorkflows: async () => {}, resumeEffectRun: async () => { throw new Error("unexpected repair"); }, takeCompletedTeaching: async () => "", close: async () => {}, loadCatalog: unused, activateDiscovery: unused,
    routeIntent: unused, synthesizeTeaching: unused, dryRun: unused, run: unused, runPlan: unused, introspectCatalog: unused };
  const runtime = new AgentRuntime({ agentRoot: root, tenantScope: "test", engine, hostTransport: null });
  const manager: unknown = Reflect.get(runtime, "sessionManager");
  assert.ok(manager instanceof SessionManager);
  const session = { ...[...sessionCases(1)][0]!, tenantScope: "test", engineInstanceId: "expired-engine" };
  const runtimeStore = new TaskLedgerStore();
  const runtimeTool = createTaskLedgerTool(runtimeStore, {
    check: (v, id) => runtime.checkObligation(v, id),
    samePredicate: (a, b) => runtime.sameObligationPredicate(a, b),
  }).task_ledger!;
  for (const state of ["missing", "expired"] as const) {
    if (state === "expired") await manager.update(session);
    const reply = await runtimeTool.execute!({ action: "write", record: { obligations: [
      { id: 1, task: "Restore abstract resource", status: "pending", verification: { ...verification, logical_session_ref: session.logicalSessionRef } },
    ] } }, { toolCallId: state, messages: [], context: {} });
    assert.match(String(reply), /saved open; verification binding rejected/);
    assert.equal(runtimeStore.record.obligations[0]?.verification, undefined);
  }
  manager.getByLogicalRef = async () => { throw new Error("storage unavailable"); };
  await assert.rejects(runtime.checkObligation({ ...verification, logical_session_ref: session.logicalSessionRef }), /storage unavailable/);
} finally { await rm(root, { recursive: true, force: true }); }
console.log("PASS: open work independent of evidence; rejected bindings retain work and prior predicates; strict resolution and restart recovery.");
