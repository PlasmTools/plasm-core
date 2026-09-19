#!/usr/bin/env node
/**
 * A provider stream that never finishes must be aborted at the agent-loop
 * boundary. Its partial output is not executable; the next bounded model turn
 * receives a recovery diagnostic and can finish the same workflow.
 */
import assert from "node:assert/strict";
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
        return "mutated";
      },
    }),
  },
  messages: [{ role: "user", content: "mutate once" }],
  maxSteps: 3,
  generationTimeoutMs: 20,
  maxConsecutiveProviderFailures: 2,
  agentName: "generation-timeout-after-mutation",
  telemetry: { isEnabled: false },
});
assert.equal(mutations, 1, "the tool executed once before the stream hung");
assert.equal(postMutationCalls, 1, "the loop must not replay a timed-out side effect");
assert.equal(postMutation.stopReason, "provider_exhausted");

console.log("ok: generation-timeout-recovery");
