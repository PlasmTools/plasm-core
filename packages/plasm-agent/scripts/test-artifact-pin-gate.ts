#!/usr/bin/env node
import assert from "node:assert/strict";
import { jsonSchema, tool } from "ai";
import {
  artefactTransformAdvertised,
  gateArtefactTransform,
} from "../src/tools/format.js";
import {
  ARTIFACT_IMAGE_PIN_RE,
  pinnedArtifactImage,
} from "../src/tools/artifact-process.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";

const valid =
  "docker.io/library/node@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
assert.equal(ARTIFACT_IMAGE_PIN_RE.test(valid), true);
assert.equal(ARTIFACT_IMAGE_PIN_RE.test("node:20"), false);
assert.equal(ARTIFACT_IMAGE_PIN_RE.test("node@sha256:deadbeef"), false);

const previous = process.env.PLASM_ARTIFACT_IMAGE;
delete process.env.PLASM_ARTIFACT_IMAGE;
assert.equal(pinnedArtifactImage(), null);
assert.equal(artefactTransformAdvertised(true), false);
assert.equal(artefactTransformAdvertised(false), false);

const tools = {
  plasm: tool({
    inputSchema: jsonSchema({ type: "object", properties: {} }),
    execute: async () => "ok",
  }),
  plasm_artefact_transform: tool({
    inputSchema: jsonSchema({ type: "object", properties: {} }),
    execute: async () => "should stay hidden without pin",
  }),
};

const hidden = gateArtefactTransform(tools, true);
assert.equal("plasm_artefact_transform" in hidden, false);
assert.equal("plasm" in hidden, true);

const unregistered = createHarnessTools({
  artefactWorkspaceRoot: "/tmp/plasm-artifact-pin-gate",
  includeArtefactTransform: true,
});
assert.equal("plasm_artefact_transform" in unregistered, false);
assert.equal("complete_task" in unregistered, false, "non-eval harness must not invent complete_task");
assert.equal("submit_answer" in unregistered, false, "non-eval harness must not invent submit_answer");

const evalTerminals = createHarnessTools({ includeEvalTerminals: true });
assert.equal("complete_task" in evalTerminals, true);
assert.equal("submit_answer" in evalTerminals, true);
assert.equal("plasm_artefact_transform" in evalTerminals, false);

process.env.PLASM_ARTIFACT_IMAGE = valid;
assert.equal(pinnedArtifactImage(), valid);
assert.equal(artefactTransformAdvertised(false), false);
assert.equal(artefactTransformAdvertised(true), true);
const shown = gateArtefactTransform(tools, true);
assert.equal("plasm_artefact_transform" in shown, true);
const stillHiddenUntilReady = gateArtefactTransform(tools, false);
assert.equal("plasm_artefact_transform" in stillHiddenUntilReady, false);

if (previous === undefined) delete process.env.PLASM_ARTIFACT_IMAGE;
else process.env.PLASM_ARTIFACT_IMAGE = previous;

console.log("test-artifact-pin-gate: ok");
