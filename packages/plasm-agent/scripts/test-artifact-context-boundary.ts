import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import { LocalArchiveStore } from "../src/archive/index.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { pinLocalArtifactImageSync, runArtefactTransform } from "../src/tools/artifact-process.js";
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
  let previousRead: string | undefined;
  for (const size of [0, 1, 32, 3240]) {
    snapshot = { rows: Array.from({ length: size }, (_, i) => ({
      id: i % 7, content: `ARTIFACT_ONLY_${i}: 漢字🦾\n${"x".repeat(256)}`,
    })) };
    const run = await runtime.plasmRun({ logicalSessionRef: session.logicalSessionRef, runRef: "pc0" });
    assert.ok(run.startsWith(message), "native response must be preserved");
    assert.ok(!run.includes("ARTIFACT_ONLY_"));
    const read = await runtime.readRunArtifact({ logicalSessionRef: session.logicalSessionRef, runId });
    assert.ok(!read.includes("ARTIFACT_ONLY_"));
    assert.ok(!read.includes("```json"));
    if (previousRun !== undefined) assert.equal(run, previousRun, "artifact size/content cannot affect run response");
    if (previousRead !== undefined) assert.equal(read, previousRead, "artifact size/content cannot affect read response");
    previousRun = run;
    previousRead = read;
    const file = read.match(/^File: (.+)$/m)?.[1];
    assert.ok(file);
    assert.deepEqual(JSON.parse(await readFile(path.join(root, "work", file), "utf8")), snapshot);
    assert.deepEqual((await archive.getRun(runId, session.logicalSessionRef))?.native_snapshot, snapshot);
    assert.equal(runtime.hasMaterializedArtefact(), true);
  }
  if (process.env.PLASM_TEST_ARTIFACT_EXECUTION === "1") {
    assert.ok(pinLocalArtifactImageSync());
    const locator = previousRead!.match(/^File: (.+)$/m)![1]!;
    const derived = await runArtefactTransform(path.join(root, "work"),
      "export default ([snapshot]: {rows: {id: number}[]}[]) => ({count: snapshot.rows.length, first: snapshot.rows[0].id, last: snapshot.rows.at(-1)?.id})", [locator]);
    assert.deepEqual(JSON.parse(derived), {count: 3240, first: 0, last: 5});
    assert.ok(!derived.includes("ARTIFACT_ONLY_"));
  }
  console.log("PASS: artifact payloads never enter tool context; full ordered Unicode data survives in files and archive");
} finally { await rm(root, { recursive: true, force: true }); }
