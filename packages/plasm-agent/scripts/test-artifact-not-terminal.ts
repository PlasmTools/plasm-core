#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { jsonSchema, tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { runIdFromArtifactRef } from "../src/tools/artifact-contract.js";

const runId = `pr${"ab".repeat(32)}`;
const uri = `plasm://execute/ph/s/run/${runId}`;
assert.equal(runIdFromArtifactRef(runId), runId);
assert.equal(runIdFromArtifactRef(uri), runId);
assert.equal(runIdFromArtifactRef("not-a-run"), null);
const snapshotOnly = `result_delivery: snapshot_only\ncontent: (in artifact)\nresource_link: ${uri}`;

// Actual loop behavior: advertised snapshots never force reads, tools or another generation.
for (const ending of ["complete_task", "submit_answer", "stop"] as const) {
  let calls = 0;
  let reads = 0;
  const choices: unknown[] = [];
  const model = new MockLanguageModelV3({
    doStream: async (options) => {
      choices.push(options.toolChoice);
      const index = calls++;
      const name = index === 0 ? "plasm_run"
        : ending;
      return { stream: new ReadableStream({ start(controller) {
        if (name === "stop") {
          controller.enqueue({ type: "text-start", id: "text" });
          controller.enqueue({ type: "text-delta", id: "text", delta: "Finished." });
          controller.enqueue({ type: "text-end", id: "text" });
        } else {
          controller.enqueue({ type: "tool-call", toolCallId: `call-${index}`, toolName: name,
            input: name === "plasm_run" ? '{"run_ref":"pc0"}'
              : name === "submit_answer" ? '{"answer":"42"}'
              : "{}" });
        }
        controller.enqueue({ type: "finish",
          finishReason: { unified: name === "stop" ? "stop" : "tool-calls", raw: undefined },
          usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
            outputTokens: { total: 2, text: 2, reasoning: 0 } } });
        controller.close();
      } }) };
    },
  });
  const schema = jsonSchema<Record<string, unknown>>({ type: "object", properties: {} });
  const result = await runEveToolLoop({ model, system: "system",
    tools: {
      plasm_run: tool({ inputSchema: schema, execute: async () => snapshotOnly }),
      complete_task: tool({ inputSchema: schema, execute: async () => "Task marked complete." }),
      submit_answer: tool({ inputSchema: schema, execute: async () => "Answer submitted." }),
    },
    messages: [{ role: "user", content: "Finish the task." }], maxSteps: 5,
    agentName: "test-artifact-optional", telemetry: { isEnabled: false },
  });
  assert.equal(calls, 2, ending);
  assert.equal(reads, 0, ending);
  assert.notEqual(result.stopReason, "budget_exhausted", ending);
  if (ending !== "stop") assert.equal(result.stopReason, "completed", ending);
  for (const choice of choices.slice(1)) assert.notDeepEqual(choice, { type: "required" });
}

const nativeCard = await readFile(new URL("../../../crates/plasm-core/src/prompt_render/assets/plasm_tool.txt", import.meta.url), "utf8");
const agentCard = await readFile(new URL("../src/prompts/assets/plasm_tool.txt", import.meta.url), "utf8");
assert.equal(agentCard, nativeCard, "one shared teaching convention");
assert.ok(!nativeCard.includes("snapshot.entities"), "artifact JSON shape belongs to the harness");
assert.ok(!nativeCard.includes("export default"), "TypeScript processing is not Plasm language teaching");
console.log("PASS: optional snapshot reads preserve agent-controlled completion");
