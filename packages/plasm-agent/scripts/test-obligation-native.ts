import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { z } from "zod";
import { checkObligationEvidence } from "../src/tools/obligation-evidence.js";
import { runOutputArtifactSchema } from "../src/tools/recorded-outputs.js";

// Produced by plasm-node::obligation_verifier_native_artifact_contract using
// execute_tiny CGS, authored Python @compute, and an offline transport.
const fixture = z.object({ comp: z.unknown(), artifacts: z.array(runOutputArtifactSchema),
  meta: z.object({ plasm: z.object({ steps: z.array(z.unknown()) }) }),
}).parse(JSON.parse(readFileSync(new URL("./fixtures/native-obligation-evidence.json", import.meta.url), "utf8")));
const output = fixture.artifacts.find(item => item.producer_binding === "verified");
assert.ok(output);
const verification = { logical_session_ref: "abstract-workflow", plan_commit_ref: "pc0", binding: "verified", field: "value" };
const plan = { plan_commit_ref: "pc0", logical_session_ref: verification.logical_session_ref,
  program: "native fixture", comp_json: fixture.comp, archived_at: "2026-10-23T00:00:00Z" };
assert.deepEqual(checkObligationEvidence(verification, plan, {
  run_id: output.run_id, plan_commit_ref: "pc0", logical_session_ref: verification.logical_session_ref,
  ok: true, message: "native fixture", native_snapshot: output.snapshot,
  _meta: fixture.meta, archived_at: "2026-10-23T00:00:01Z",
}), { ok: true });
console.log("PASS: native authored @compute plan, input dependencies, producer receipt and boolean artifact satisfy ledger admission.");
