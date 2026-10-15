#!/usr/bin/env node
/** Eval terminals require an opened workflow; initialization is host-owned. */
import assert from "node:assert/strict";
import { asSchema, tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { z } from "zod";

import {
  createEvalTerminalTools,
  EVAL_TERMINAL_REQUIRES_DISCOVERY,
} from "../src/tools/harness-tools.js";
import {
  COMPLETE_TASK_TOOL_NAME,
  SUBMIT_ANSWER_TOOL_NAME,
  evalTerminalGrade,
} from "../src/tools/format.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { toolInput } from "../src/tools/tool-input.js";
const INITIAL_DISCOVERY_TOOL_NAME = "plasm_context";

// --- Unit: refusal throws (tool-error path), success after discovery ---

let discovered = false;
const tools = createEvalTerminalTools({
  discoveryCompleted: () => discovered,
});

const complete = tools[COMPLETE_TASK_TOOL_NAME]?.execute;
const submit = tools[SUBMIT_ANSWER_TOOL_NAME]?.execute;
assert.ok(complete);
assert.ok(submit);
const executionOptions = { toolCallId: "unit", messages: [], context: {} };

await assert.rejects(
  async () => complete({}, executionOptions),
  (err: unknown) =>
    err instanceof Error && err.message === EVAL_TERMINAL_REQUIRES_DISCOVERY,
  "complete_task refusal must throw (tool-error), not return success text",
);

await assert.rejects(
  async () => submit({ answer: "42" }, executionOptions),
  (err: unknown) =>
    err instanceof Error && err.message === EVAL_TERMINAL_REQUIRES_DISCOVERY,
  "submit_answer refusal must throw (tool-error), not return success text",
);

discovered = true;
assert.match(String(await complete({}, executionOptions)), /claim recorded.*not been independently verified/);
assert.match(String(await submit({ answer: "42" }, executionOptions)), /submission recorded.*not been independently verified/);

const ungated = createEvalTerminalTools();
const ungatedComplete = ungated[COMPLETE_TASK_TOOL_NAME]?.execute;
assert.ok(ungatedComplete);
assert.match(String(await ungatedComplete({}, executionOptions)), /claim recorded/);

const actionTerminals = createEvalTerminalTools({responseObligation: {kind: "no_answer", instruction: "Send the total to my colleague."}});
const conflictingSubmit = actionTerminals[SUBMIT_ANSWER_TOOL_NAME]?.execute;
assert.ok(conflictingSubmit);
await assert.rejects(async () => conflictingSubmit({answer:"42"}, executionOptions), /Response-obligation conflict/);
await assert.rejects(async () => conflictingSubmit({answer:"42"}, executionOptions),
  /Repair the unfinished requirement before choosing a terminal/);
await assert.rejects(async () => conflictingSubmit({answer:"42", response_obligation_quote:"invented wording"}, executionOptions), /Response-obligation conflict/);
// Even an exact quotation cannot authorize a different terminal destination.
const advisory = createEvalTerminalTools({responseObligation: {kind: "no_answer", instruction: "Tell me the total."}});
const overrideSubmit = advisory[SUBMIT_ANSWER_TOOL_NAME]?.execute;
assert.ok(overrideSubmit);
await assert.rejects(async () => overrideSubmit({answer:"42",response_obligation_quote:"Tell me the total."}, executionOptions), /Response-obligation conflict/);
const submitSchema = asSchema(actionTerminals[SUBMIT_ANSWER_TOOL_NAME]!.inputSchema);
assert.ok(submitSchema.validate);
assert.equal((await submitSchema.validate({ answer: "42", response_obligation_quote: "Send the total to my colleague." })).success, false);
const uncertain = createEvalTerminalTools({ responseObligation: { kind: "uncertain", instruction: "Inspect records." } });
assert.match(String(await uncertain[SUBMIT_ANSWER_TOOL_NAME]!.execute!({answer:"42"}, executionOptions)), /submission recorded/);
assert.match(String(await uncertain[COMPLETE_TASK_TOOL_NAME]!.execute!({}, executionOptions)), /claim recorded/);
const valueTerminals = createEvalTerminalTools({responseObligation:{kind:"value_required", instruction:"How many items?"}});
const conflictingComplete = valueTerminals[COMPLETE_TASK_TOOL_NAME]?.execute;
assert.ok(conflictingComplete);
await assert.rejects(async () => conflictingComplete({}, executionOptions), /Response-obligation conflict/);

// --- Loop P1-1: discovery false + complete_task → NOT completed / NOT null grade ---

let blockedCalls = 0;
let attemptedTool: string = COMPLETE_TASK_TOOL_NAME;
let attemptedInput = "{}";
const blockedModel = new MockLanguageModelV3({
  doStream: async () => {
    blockedCalls += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: `blocked-${blockedCalls}`,
            toolName: attemptedTool,
            input: attemptedInput,
          });
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "tool-calls", raw: undefined },
            usage: {
              inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
              outputTokens: { total: 2, text: 2, reasoning: 0 },
            },
          });
          controller.close();
        },
      }),
    };
  },
});

const blockedTerminals = createEvalTerminalTools({
  discoveryCompleted: () => false,
});

const blocked = await runEveToolLoop({
  model: blockedModel,
  system: "system",
  tools: {
    ...blockedTerminals,
    [INITIAL_DISCOVERY_TOOL_NAME]: tool({
      inputSchema: toolInput(z.object({ intent: z.string() })),
      execute: async () => "should not be required for this case",
    }),
  },
  messages: [{ role: "user", content: "done without discovery" }],
  maxSteps: 3,
  agentName: "test-discovery-gate-refusal",
  telemetry: { isEnabled: false },
  // Do not enforce first-step discovery here — isolate terminal refusal grading.
});

assert.notEqual(
  blocked.stopReason,
  "completed",
  "discovery-gate refusal must not yield stopReason completed",
);
assert.deepEqual(
  evalTerminalGrade({ messages: blocked.messages, stopReason: blocked.stopReason }),
  { kind: "unterminated" },
  "discovery-gate refusal must not grade as null/complete_task success",
);
assert.ok(blockedCalls >= 1, "model must have attempted complete_task");

attemptedTool = SUBMIT_ANSWER_TOOL_NAME;
attemptedInput = JSON.stringify({ answer: "42", response_obligation_quote: "Send the total to my colleague." });
const quoted = await runEveToolLoop({
  model: blockedModel, system: "Abstract terminal contract fixture",
  tools: createEvalTerminalTools({ discoveryCompleted: () => true,
    responseObligation: { kind: "no_answer", instruction: "Send the total to my colleague." } }),
  messages: [{ role: "user", content: "Send the total to my colleague." }],
  maxSteps: 3, agentName: "quoted-terminal-conflict", telemetry: { isEnabled: false },
});
assert.notEqual(quoted.stopReason, "completed");
assert.deepEqual(evalTerminalGrade({ messages: quoted.messages, stopReason: quoted.stopReason }), { kind: "unterminated" });


console.log("PASS: terminal refusal remains an error until a workflow exists");
