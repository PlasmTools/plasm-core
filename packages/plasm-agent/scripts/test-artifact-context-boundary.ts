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
const session = [...sessionCases(1)][0]!;
const message = "Native Plasm response: snapshot available.";
let snapshot: unknown;
const engine: PlasmEngine = {
  loadCatalog: unused, activateDiscovery: unused, routeIntent: unused,
  synthesizeTeaching: unused, dryRun: unused, run: unused, introspectCatalog: unused,
  runPlan: async () => ({ ok: true, message, rowsJson: JSON.stringify(snapshot),
    artifactsJson: JSON.stringify([{ run_id: runId, snapshot }]) }),
};
try {
  const archive = new LocalArchiveStore(path.join(root, "archive"));
  const runtime = new AgentRuntime({ agentRoot: root, engine, archive, hostTransport: null,
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
  assert.deepEqual(Object.keys(createPlasmTools(runtime)).sort(), ["plasm", "plasm_context", "plasm_run"]);
  for (const prompt of [buildDefaultSystemLiturgy(), buildDefaultSystemLiturgy({includeEvalTerminals: true})]) {
    assert.match(prompt, /@compute/);
    assert.doesNotMatch(prompt, /plasm_read_run_artifact|plasm_artefact_transform|resources\/read/);
  }
  console.log("PASS: snapshots stay outside agent context; ordered Unicode evidence survives in host archives");
} finally { await rm(root, { recursive: true, force: true }); }
