import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import { LocalArchiveStore } from "../src/archive/index.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { sessionCases } from "./test-session-contract.js";
import { SessionManager } from "../src/session-state.js";
import type { RunSnapshot } from "../src/archive/types.js";

const root = await mkdtemp(path.join(tmpdir(), "obligation-freshness-"));
const session = { ...[...sessionCases(1)][0]!, tenantScope: "test" };
const id = `pr${"c".repeat(64)}`;
const verification = { logical_session_ref: session.logicalSessionRef, plan_commit_ref: "pc0", binding: "verify", field: "value" };
const unused = async (): Promise<never> => { throw new Error("unexpected engine call"); };
let fail = false;
let calls = 0;
let pause: Promise<void> | undefined;
const consumed = new Set<string>();
const engine: PlasmEngine = { sealEffectWorkflows: async () => {}, takeCompletedTeaching: async () => "",
  close: async () => {}, loadCatalog: unused, activateDiscovery: unused, routeIntent: unused,
  synthesizeTeaching: unused, dryRun: unused, run: unused, introspectCatalog: unused,
  runPlan: async input => {
    assert.ok(!consumed.has(input), "execution authorization is single-use");
    consumed.add(input);
    calls++;
    await pause;
    if (fail) throw new Error("partial provider failure");
    return { ok: true, message: "observed", metaJson: JSON.stringify({ plasm: { steps: [{ run_id: id, node_id: "verify", artifact_complete: true }] } }),
      artifactsJson: JSON.stringify([{ run_id: id, producer_binding: "verify", reusable: false, snapshot: {
        row_materialization: "Computed", entities: [{ completeness: "complete", fields: { value: true } }],
      } }]) };
  },
};
try {
  const archive = new LocalArchiveStore(root);
  await archive.bootstrap();
  await archive.writePlanArchive({ plan_commit_ref: "pc0", program: "abstract", logical_session_ref: session.logicalSessionRef,
    archived_at: new Date().toISOString(), comp_json: {
      steps: { observed: { kind: "invoke", effect_class: "read" }, verify: { kind: "map", effect_class: "artifact_read", compute: { source: "observed" } } },
      bind: { holes: { verify: [{ step: "observed", alias: "source" }] } },
    } });
  const runtime = new AgentRuntime({ agentRoot: root, tenantScope: session.tenantScope, engine, archive, hostTransport: null, artefactWorkspaceRoot: root });
  Reflect.set(runtime, "workflowSession", session);
  const originalPlan = await archive.getPlan("pc0", session.logicalSessionRef);
  assert.ok(originalPlan);
  let reviews = 0;
  const execute = async () => {
    const runRef = `pc${++reviews}`;
    await archive.writePlanArchive({ ...originalPlan, plan_commit_ref: runRef });
    verification.plan_commit_ref = runRef;
    return runtime.plasmRun({ logicalSessionRef: session.logicalSessionRef, runRef });
  };
  await execute();
  assert.equal((await runtime.checkObligation(verification, id)).ok, true);
  fail = true;
  await assert.rejects(execute(), /partial provider failure/);
  assert.deepEqual(await runtime.checkObligation(verification, id), { ok: false, code: "stale_observation" });
  fail = false;
  await execute();
  assert.equal((await runtime.checkObligation(verification, id)).ok, true);
  let release!: () => void;
  pause = new Promise<void>(resolve => { release = resolve; });
  const first = execute();
  const second = execute();
  // Both begin paths persist before dispatch; wait for the mock engine admissions.
  while (calls < 5) await new Promise(resolve => setTimeout(resolve, 1));
  assert.deepEqual(await runtime.checkObligation(verification, id), { ok: false, code: "stale_observation" });
  release();
  await Promise.all([first, second]);
  assert.deepEqual(await runtime.checkObligation(verification, id), { ok: false, code: "stale_observation" });
  pause = undefined;
  await execute();
  assert.equal((await runtime.checkObligation(verification, id)).ok, true);
  assert.equal(calls, 6, "evidence checks never dispatch effects or repeat the verifier");
  const manager: unknown = Reflect.get(runtime, "sessionManager");
  assert.ok(manager instanceof SessionManager);
  const originalUpdate = manager.update.bind(manager);
  let persisted = 0;
  let unblock!: () => void;
  let entered!: () => void;
  const blocked = new Promise<void>(resolve => { unblock = resolve; });
  const enteredWrite = new Promise<void>(resolve => { entered = resolve; });
  manager.update = async state => {
    if (++persisted === 2) { entered(); await blocked; }
    await originalUpdate(state);
  };
  const snapshots: RunSnapshot[] = [];
  const originalArchiveWrite = archive.writeRunSnapshot.bind(archive);
  archive.writeRunSnapshot = async snapshot => { snapshots.push(snapshot); await originalArchiveWrite(snapshot); };
  const older = execute();
  await enteredWrite;
  const newer = execute();
  // Admission changes the shared revision before its queued persistence completes.
  await new Promise(resolve => setTimeout(resolve, 0));
  unblock();
  await Promise.all([older, newer]);
  assert.equal(snapshots.length, 2);
  assert.notEqual(snapshots[0]!.observation_revision, session.observationRevision,
    "an older execution cannot adopt a revision changed during persistence");
} finally { await rm(root, { recursive: true, force: true }); }
console.log("PASS: execution attempts, partial failures and overlaps invalidate old observations; fresh verification recovers without replay.");
