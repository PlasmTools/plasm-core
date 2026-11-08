import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { buildDefaultSystemLiturgy } from "../src/prompts/index.js";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import { LocalArchiveStore } from "../src/archive/index.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { sessionCases } from "./test-session-contract.js";

const root = await mkdtemp(path.join(tmpdir(), "plasm-artifact-context-"));
const unused = async (): Promise<never> => { throw new Error("unexpected engine call"); };
const runId = `pr${"a".repeat(64)}`;
const session = { ...[...sessionCases(1)][0]!, tenantScope: "test" };
const message = "Native Plasm response: snapshot available.";
let snapshot: unknown;
let executions = 0;
let reusable = false;
const engine: PlasmEngine = { sealEffectWorkflows: async () => {}, resumeEffectRun: async () => { throw new Error("unexpected repair"); }, takeCompletedTeaching: async () => "",
  close: async () => {},
  loadCatalog: unused, activateDiscovery: unused, routeIntent: unused,
  synthesizeTeaching: unused, dryRun: unused, run: unused, introspectCatalog: unused,
  runPlan: async () => {
    executions++;
    return { ok: true, message, rowsJson: JSON.stringify(snapshot),
      artifactsJson: JSON.stringify([{ run_id: runId, producer_binding: "observed", reusable, snapshot }]) };
  },
};
try {
  const archive = new LocalArchiveStore(path.join(root, "archive"));
  const runtime = new AgentRuntime({ agentRoot: root, tenantScope: session.tenantScope, engine, archive, hostTransport: null,
    artefactWorkspaceRoot: path.join(root, "work") });
  // Install a typed session fixture without exercising unrelated discovery.
  Reflect.set(runtime, "workflowSession", session);
  let previousRun: string | undefined;
  for (const size of [0, 1, 32, 3240]) {
    snapshot = { entities: Array.from({ length: size }, (_, i) => ({
      id: i % 7, content: `ARTIFACT_ONLY_${i}: 漢字🦾\n${"x".repeat(256)}`,
    })) };
    const run = await runtime.plasmRun({ logicalSessionRef: session.logicalSessionRef, runRef: "pc0" });
    assert.ok(run.startsWith(message), "native response must be preserved");
    assert.ok(!run.includes("ARTIFACT_ONLY_"));
    if (previousRun !== undefined) assert.equal(run, previousRun, "snapshot size cannot affect tool response");
    previousRun = run;
    const file = `artefacts/${session.logicalSessionRef}/${runId}.json`;
    assert.deepEqual(JSON.parse(await readFile(path.join(root, "work", file), "utf8")), snapshot);
    assert.deepEqual((await archive.getRun(runId, session.logicalSessionRef))?.native_snapshot, snapshot);
  }
  snapshot = {entry_id: "matrix", entity: "Resource", row_materialization: "Catalog",
    entities: [{completeness: "complete", fields: {token: "ARTIFACT_ONLY_secret", optional: null}},
      {completeness: "complete", fields: {token: "ARTIFACT_ONLY_other"}}]};
  reusable = true;
  const labelled = await runtime.plasmRun({logicalSessionRef: session.logicalSessionRef, runRef: "pc1"});
  assert.match(labelled, /observed: matrix::Resource; 2 rows;/);
  assert.ok(labelled.includes(`self.recorded("${runId}")`), "callable intermediate output reaches the model-facing response");
  assert.match(labelled, /optional \(some rows\), token/);
  assert.ok(!labelled.includes("ARTIFACT_ONLY_") && !labelled.includes("**run_id:**"));
  assert.deepEqual((await archive.getRun(runId, session.logicalSessionRef))?.native_snapshot, snapshot);
  assert.equal(executions, 5, "labelling outputs never replays or fetches a producer");
  assert.deepEqual(Object.keys(createPlasmTools(runtime)).sort(), ["plasm", "plasm_context", "plasm_failure", "plasm_run"]);
  for (const prompt of [buildDefaultSystemLiturgy(), buildDefaultSystemLiturgy({includeEvalTerminals: true})]) {
    assert.match(prompt, /@compute/);
    assert.doesNotMatch(prompt, /plasm_read_run_artifact|plasm_artefact_transform|resources\/read/);
  }
  console.log("PASS: snapshots stay outside agent context; ordered Unicode evidence survives in host archives");
} finally { await rm(root, { recursive: true, force: true }); }
