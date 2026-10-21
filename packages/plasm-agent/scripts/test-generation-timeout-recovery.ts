#!/usr/bin/env node
/**
 * A provider stream that never finishes must be aborted at the agent-loop
 * boundary. Its partial output is not executable; the next bounded model turn
 * receives a recovery diagnostic and can finish the same workflow.
 */
import assert from "node:assert/strict";
import { mock } from "node:test";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { z } from "zod";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";

const usage = {
  inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
  outputTokens: { total: 2, text: 2, reasoning: 0 },
};

let calls = 0;
let abortObserved = false;
const model = new MockLanguageModelV3({
  doStream: async (options) => {
    const index = calls++;
    if (index === 0) {
      assert.ok(options.abortSignal, "loop supplies a cancellable generation");
      return {
        stream: new ReadableStream({
          start(controller) {
            controller.enqueue({
              type: "tool-input-start",
              id: "partial",
              toolName: "complete_task",
            });
            controller.enqueue({
              type: "tool-input-delta",
              id: "partial",
              delta: "{",
            });
            options.abortSignal?.addEventListener(
              "abort",
              () => {
                abortObserved = true;
                controller.error(options.abortSignal?.reason);
              },
              { once: true },
            );
          },
        }),
      };
    }

    const prompt = JSON.stringify(options.prompt);
    assert.match(prompt, /provider generation exceeded its wall-clock deadline/);
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: "complete",
            toolName: "complete_task",
            input: "{}",
          });
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "tool-calls", raw: "tool_calls" },
            usage,
          });
          controller.close();
        },
      }),
    };
  },
});

const result = await runEveToolLoop({
  model,
  system: "system",
  tools: createEvalTerminalTools(),
  messages: [{ role: "user", content: "finish the task" }],
  maxSteps: 3,
  generationTimeoutMs: 20,
  maxConsecutiveProviderFailures: 2,
  agentName: "generation-timeout-recovery",
  telemetry: { isEnabled: false },
});

assert.equal(abortObserved, true, "the unfinished provider stream was aborted");
assert.equal(calls, 2, "one timed-out generation receives one recovery turn");
assert.equal(result.stopReason, "completed");
assert.equal(result.generationTimeoutCount, 1);
assert.deepEqual(result.stepFinishReasons, ["error", "tool-calls"]);

let exhaustedCalls = 0;
const exhaustedModel = new MockLanguageModelV3({
  doStream: async (options) => {
    exhaustedCalls += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          options.abortSignal?.addEventListener(
            "abort",
            () => controller.error(options.abortSignal?.reason),
            { once: true },
          );
        },
      }),
    };
  },
});
const exhausted = await runEveToolLoop({
  model: exhaustedModel,
  system: "system",
  tools: createEvalTerminalTools(),
  messages: [{ role: "user", content: "finish the task" }],
  maxSteps: 5,
  generationTimeoutMs: 20,
  maxConsecutiveProviderFailures: 2,
  agentName: "generation-timeout-exhaustion",
  telemetry: { isEnabled: false },
});
assert.equal(exhaustedCalls, 2, "failure cap stops before the outer watchdog");
assert.equal(exhausted.stopReason, "provider_exhausted");
assert.equal(exhausted.generationTimeoutCount, 2);
assert.deepEqual(exhausted.stepFinishReasons, ["error", "error"]);

let mutations = 0;
let postMutationCalls = 0;
mock.timers.enable({ apis: ["setTimeout"] });
const postMutationModel = new MockLanguageModelV3({
  doStream: async (options) => {
    postMutationCalls += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: "mutate-once",
            toolName: "mutate",
            input: "{}",
          });
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "tool-calls", raw: "tool_calls" },
            usage,
          });
          options.abortSignal?.addEventListener(
            "abort",
            () => controller.error(options.abortSignal?.reason),
            { once: true },
          );
          controller.close();
        },
      }),
    };
  },
});
const postMutation = await runEveToolLoop({
  model: postMutationModel,
  system: "system",
  tools: {
    mutate: tool({
      inputSchema: z.object({}),
      execute: async () => {
        mutations += 1;
        // Advance beyond the generation deadline only after tool admission.
        mock.timers.tick(300_001);
        return "mutated";
      },
    }),
  },
  messages: [{ role: "user", content: "mutate once" }],
  maxSteps: 1,
  generationTimeoutMs: 20,
  maxConsecutiveProviderFailures: 2,
  agentName: "generation-timeout-after-mutation",
  telemetry: { isEnabled: false },
});
mock.timers.reset();
assert.equal(mutations, 1, "the slow tool executes exactly once");
assert.equal(postMutationCalls, 1, "slow tools do not trigger provider recovery");
assert.equal(postMutation.stopReason, "budget_exhausted");
assert.equal(postMutation.generationTimeoutCount, 0);
assert.equal(postMutation.usage.totalTokens, 10, "completed generation usage survives slow execution");
assert.equal(postMutation.steps[0]!.toolResults.length, 1);
assert.match(JSON.stringify(postMutation.messages), /mutate-once/);
assert.match(JSON.stringify(postMutation.messages), /mutated/);

// Slow parallel tools (including a tool error) retain the SDK's exact audit
// projection, with no re-execution and no host-invented completion.
for (const fails of [false, true]) {
  mock.timers.enable({ apis: ["setTimeout"] });
  const executed: string[] = [];
  const checkpoints: string[] = [];
  const parallel = new MockLanguageModelV3({
    doStream: async () => ({
      stream: new ReadableStream({
        start(controller) {
          for (const id of ["first", "second"]) controller.enqueue({
            type: "tool-call", toolCallId: id, toolName: "record", input: JSON.stringify({ id }),
          });
          controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: "tool_calls" }, usage });
          controller.close();
        },
      }),
    }),
  });
  const recorded = await runEveToolLoop({
    model: parallel, system: "system", messages: [{ role: "user", content: "record two operations" }],
    tools: { record: tool({ inputSchema: z.object({ id: z.string() }), execute: async ({ id }) => {
      executed.push(id);
      mock.timers.tick(300_001);
      if (fails && id === "second") throw new Error("abstract operation rejected");
      return { id, receipt: "completed" };
    } }) },
    maxSteps: 1, generationTimeoutMs: 20, agentName: "parallel-slow-tools", telemetry: { isEnabled: false },
    onStepFinish: async (step) => { checkpoints.push(JSON.stringify(step.messages)); },
  });
  mock.timers.reset();
  assert.deepEqual(executed, ["first", "second"]);
  assert.equal(recorded.generationTimeoutCount, 0);
  assert.equal(checkpoints.length, 1);
  assert.equal(checkpoints[0], JSON.stringify(recorded.messages), "persisted step and returned audit agree");
  assert.equal(recorded.steps[0]!.toolCalls.length, 2);
  assert.equal(recorded.usage.totalTokens, 10);
  assert.match(checkpoints[0]!, /completed/);
  if (fails) assert.match(checkpoints[0]!, /abstract operation rejected/);
}

// A real timeout in a later generation must preserve an earlier completed
// call in both checkpoint history and the final result, without replay.
let laterCalls = 0;
let earlierExecutions = 0;
const checkpointHistory: string[] = [];
const laterTimeout = new MockLanguageModelV3({
  doStream: async (options) => ({ stream: new ReadableStream({ start(controller) {
    if (laterCalls++ === 0) {
      controller.enqueue({ type: "tool-call", toolCallId: "retained-call", toolName: "record", input: "{}" });
      controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: "tool_calls" }, usage });
      controller.close();
    } else {
      assert.match(JSON.stringify(options.prompt), /retained-receipt/);
      options.abortSignal?.addEventListener("abort", () => controller.error(options.abortSignal?.reason), { once: true });
    }
  } }) }),
});
const retained = await runEveToolLoop({
  model: laterTimeout, system: "system", messages: [{ role: "user", content: "record" }],
  tools: { record: tool({ inputSchema: z.object({}), execute: async () => { earlierExecutions++; return "retained-receipt"; } }) },
  maxSteps: 3, generationTimeoutMs: 20, maxConsecutiveProviderFailures: 1,
  agentName: "timeout-retains-history", telemetry: { isEnabled: false },
  onStepFinish: async (step) => { checkpointHistory.push(JSON.stringify(step.messages)); },
});
assert.equal(earlierExecutions, 1);
assert.equal(laterCalls, 2);
assert.equal(retained.stopReason, "provider_exhausted");
assert.equal(retained.generationTimeoutCount, 1);
assert.equal(checkpointHistory.length, 2);
for (const checkpoint of checkpointHistory) assert.match(checkpoint, /retained-receipt/);
assert.match(JSON.stringify(retained.messages), /retained-call/);

// A finish marker without EOF must remain bounded, before any effects are
// dispatched. The SDK normally starts tools at that marker, not at EOF.
let tailExecutions = 0;
let tailAborted = false;
const stalledTail = new MockLanguageModelV3({
  doStream: async (options) => ({ stream: new ReadableStream({ start(controller) {
    controller.enqueue({ type: "tool-call", toolCallId: "unadmitted", toolName: "record", input: "{}" });
    controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: "tool_calls" }, usage });
    options.abortSignal?.addEventListener("abort", () => {
      tailAborted = true;
      controller.error(options.abortSignal?.reason);
    }, { once: true });
  } }) }),
});
const stalled = await runEveToolLoop({
  model: stalledTail, system: "system", messages: [{ role: "user", content: "record" }],
  tools: { record: tool({ inputSchema: z.object({}), execute: async () => { tailExecutions++; return "must not execute"; } }) },
  maxSteps: 1, generationTimeoutMs: 20, maxConsecutiveProviderFailures: 1,
  agentName: "stalled-provider-tail", telemetry: { isEnabled: false },
});
assert.equal(tailAborted, true);
assert.equal(tailExecutions, 0);
assert.equal(stalled.stopReason, "provider_exhausted");
assert.equal(stalled.generationTimeoutCount, 1);

console.log("ok: generation-timeout-recovery");
