#!/usr/bin/env node
/**
 * Isolated ledger-review seat: separate generate, host does not auto-block
 * complete_task on nonempty remaining_work, and does not judge domain facts.
 */
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { jsonSchema, tool, type ToolSet } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import {
  TASK_LEDGER_REVIEW_ACTOR_LITURGY,
  buildDefaultSystemLiturgy,
} from "../src/prompts/index.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";
import { TASK_LEDGER_TOOL_NAME, TaskLedgerStore } from "../src/tools/task-ledger.js";
import {
  TASK_LEDGER_REVIEW_ACTION_HEADER,
  TASK_LEDGER_REVIEW_INSTRUCTION_HEADER,
  TASK_LEDGER_REVIEW_LEDGER_HEADER,
  TASK_LEDGER_REVIEW_LITURGY,
  TASK_LEDGER_REVIEW_OBSERVATIONS_HEADER,
  composeTaskLedgerReviewPacket,
  extractRecentPlasmObservations,
  parseTaskLedgerReviewVerdict,
  reviewAllowsProposedAction,
} from "../src/tools/task-ledger-review.js";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { COMPLETE_TASK_TOOL_NAME } from "../src/tools/format.js";

const unused = async (): Promise<never> => {
  throw new Error("unexpected engine call");
};
const engine: PlasmEngine = {
  loadCatalog: unused,
  activateDiscovery: unused,
  routeIntent: unused,
  synthesizeTeaching: unused,
  dryRun: unused,
  runPlan: unused,
  run: unused,
  introspectCatalog: unused,
};

const venmoLedger = {
  requested_outcomes: ["pay both friends on Venmo"],
  scope: "Venmo-account holders",
  alternative_conditions: [],
  observed_facts: [
    {
      kind: "fact" as const,
      text: "not in 25-row directory",
      evidence_ref: "plasm_run",
    },
  ],
  interpretations: [
    {
      kind: "interpretation" as const,
      text: "user did not authorize a fallback for Venmo-account holders",
    },
  ],
  unresolved_questions: [],
  completed_effects: [
    {
      effect: "expense created and receipt attached",
      satisfaction: "interpretation" as const,
    },
  ],
  remaining_work: ["Venmo payment to A", "Venmo payment to B"],
};

const INSTRUCTION = "I owe them. Pay via Venmo.";

const allowVerdict = {
  decision: "allow" as const,
  negative_evidence: "insufficient" as const,
  role_satisfaction: "unsatisfied" as const,
  unresolved_obligations: "do_not_permit" as const,
  rationale: "Reviewer allows the proposed finish; host must not invert this.",
};

const continueVerdict = {
  decision: "continue" as const,
  negative_evidence: "insufficient" as const,
  role_satisfaction: "unsatisfied" as const,
  unresolved_obligations: "do_not_permit" as const,
  rationale: "Outstanding Venmo obligations are inconsistent with finishing.",
};

const usage = {
  inputTokens: { total: 10, noCache: 10, cacheRead: 0, cacheWrite: 0 },
  outputTokens: { total: 2, text: 2, reasoning: 0 },
};

function finish(controller: ReadableStreamDefaultController, reason: "stop" | "tool-calls") {
  controller.enqueue({
    type: "finish",
    finishReason: { unified: reason, raw: undefined },
    usage,
  });
  controller.close();
}

function mockReviewJson(verdict: object) {
  return new MockLanguageModelV3({
    doStream: async () => ({
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({ type: "text-start", id: "text" });
          controller.enqueue({
            type: "text-delta",
            id: "text",
            delta: JSON.stringify(verdict),
          });
          controller.enqueue({ type: "text-end", id: "text" });
          finish(controller, "stop");
        },
      }),
    }),
  });
}

function mockReviewMustNotRun() {
  return new MockLanguageModelV3({
    doStream: async () => {
      throw new Error("review generate must not run when the flag is off");
    },
  });
}

function mockLedgerThenComplete(record: typeof venmoLedger) {
  let count = 0;
  return new MockLanguageModelV3({
    doStream: async () => {
      const index = count++;
      return {
        stream: new ReadableStream({
          start(controller) {
            if (index === 0) {
              controller.enqueue({
                type: "tool-call",
                toolCallId: "ledger-0",
                toolName: TASK_LEDGER_TOOL_NAME,
                input: JSON.stringify({ action: "write", record }),
              });
              finish(controller, "tool-calls");
              return;
            }
            if (index === 1) {
              controller.enqueue({
                type: "tool-call",
                toolCallId: "complete-1",
                toolName: COMPLETE_TASK_TOOL_NAME,
                input: "{}",
              });
              finish(controller, "tool-calls");
              return;
            }
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({ type: "text-delta", id: "text", delta: "after review" });
            controller.enqueue({ type: "text-end", id: "text" });
            finish(controller, "stop");
          },
        }),
      };
    },
  });
}

function mockCompleteOnly() {
  let count = 0;
  return new MockLanguageModelV3({
    doStream: async () => {
      const index = count++;
      return {
        stream: new ReadableStream({
          start(controller) {
            if (index === 0) {
              controller.enqueue({
                type: "tool-call",
                toolCallId: "complete-0",
                toolName: COMPLETE_TASK_TOOL_NAME,
                input: "{}",
              });
              finish(controller, "tool-calls");
              return;
            }
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({ type: "text-delta", id: "text", delta: "prose after complete" });
            controller.enqueue({ type: "text-end", id: "text" });
            finish(controller, "stop");
          },
        }),
      };
    },
  });
}

function mockPlasmRunThenStop() {
  let count = 0;
  return new MockLanguageModelV3({
    doStream: async () => {
      const index = count++;
      return {
        stream: new ReadableStream({
          start(controller) {
            if (index === 0) {
              controller.enqueue({
                type: "tool-call",
                toolCallId: "run-0",
                toolName: "plasm_run",
                input: JSON.stringify({ logical_session_ref: "ls_test", run_ref: "pc0" }),
              });
              finish(controller, "tool-calls");
              return;
            }
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({ type: "text-delta", id: "text", delta: "after run review" });
            controller.enqueue({ type: "text-end", id: "text" });
            finish(controller, "stop");
          },
        }),
      };
    },
  });
}

const parsedFence = parseTaskLedgerReviewVerdict(`\`\`\`json\n${JSON.stringify(allowVerdict)}\n\`\`\``);
assert.equal(parsedFence.ok, true);
if (!parsedFence.ok) throw new Error("expected fenced verdict");
assert.equal(parsedFence.verdict.decision, "allow");

assert.equal(reviewAllowsProposedAction("allow", COMPLETE_TASK_TOOL_NAME), true);
assert.equal(reviewAllowsProposedAction("continue", COMPLETE_TASK_TOOL_NAME), false);
assert.equal(reviewAllowsProposedAction("unresolved", COMPLETE_TASK_TOOL_NAME), false);
assert.equal(reviewAllowsProposedAction("complete_without_value", COMPLETE_TASK_TOOL_NAME), true);
assert.equal(reviewAllowsProposedAction("complete_without_value", "submit_answer"), false);
assert.equal(reviewAllowsProposedAction("submit", "submit_answer"), true);
assert.equal(reviewAllowsProposedAction("submit", COMPLETE_TASK_TOOL_NAME), false);

const packet = composeTaskLedgerReviewPacket({
  instruction: INSTRUCTION,
  ledger: venmoLedger,
  proposedAction: { toolName: COMPLETE_TASK_TOOL_NAME, input: {} },
  observations: "(no plasm plan or run observations in recent steps)",
});
assert.match(packet, new RegExp(TASK_LEDGER_REVIEW_INSTRUCTION_HEADER));
assert.match(packet, new RegExp(TASK_LEDGER_REVIEW_LEDGER_HEADER));
assert.match(packet, new RegExp(TASK_LEDGER_REVIEW_ACTION_HEADER));
assert.match(packet, new RegExp(TASK_LEDGER_REVIEW_OBSERVATIONS_HEADER));
assert.match(packet, /I owe them/);
assert.match(packet, /not in 25-row directory/);
assert.match(packet, /expense created and receipt attached/);
assert.match(packet, /did not authorize a fallback/);
assert.match(packet, /Venmo payment to A/);
assert.equal(packet.includes("they owe me"), false);
assert.equal(packet.includes("grader"), false);
assert.equal(packet.includes("EvalTerminal"), false);
assert.equal(packet.includes("AppWorld"), false);

assert.match(
  extractRecentPlasmObservations([
    { role: "user", content: "GRADER_SECRET EvalTerminalGrade AppWorld recipe" },
    {
      role: "tool",
      content: [
        {
          type: "tool-result",
          toolCallId: "obs-0",
          toolName: "plasm",
          output: { type: "text", value: "plan: 1n 0w" },
        },
      ],
    },
  ]),
  /plan: 1n 0w/,
);
assert.equal(
  extractRecentPlasmObservations([
    { role: "user", content: "GRADER_SECRET EvalTerminalGrade AppWorld recipe" },
  ]).includes("GRADER_SECRET"),
  false,
  "review observations must not dump the transcript",
);

const product = buildDefaultSystemLiturgy();
assert.equal(product.includes(TASK_LEDGER_REVIEW_ACTOR_LITURGY), false);
assert.equal(product.includes(TASK_LEDGER_REVIEW_LITURGY), false);
assert.equal(product.includes("grader"), false);

const reviewLiturgy = buildDefaultSystemLiturgy({ includeTaskLedgerReview: true });
assert.equal(reviewLiturgy.includes(TASK_LEDGER_REVIEW_ACTOR_LITURGY), true);
assert.equal(reviewLiturgy.includes("grader"), false);
assert.equal(reviewLiturgy.includes("AppWorld"), false);
assert.equal(reviewLiturgy.includes("EvalTerminal"), false);
assert.match(TASK_LEDGER_REVIEW_LITURGY, /not automatic failure/);
assert.equal(TASK_LEDGER_REVIEW_LITURGY.includes("grader"), false);
assert.equal(TASK_LEDGER_REVIEW_LITURGY.includes("AppWorld"), false);

const store = new TaskLedgerStore();
const toolsOff = createHarnessTools({ includeEvalTerminals: true, taskLedgerStore: store });
assert.equal("complete_task" in toolsOff, true);

function keepLedgerAndComplete(tools: ToolSet): ToolSet {
  assert.equal(TASK_LEDGER_TOOL_NAME in tools, true);
  assert.equal(COMPLETE_TASK_TOOL_NAME in tools, true);
  return {
    [TASK_LEDGER_TOOL_NAME]: tools[TASK_LEDGER_TOOL_NAME]!,
    [COMPLETE_TASK_TOOL_NAME]: tools[COMPLETE_TASK_TOOL_NAME]!,
  };
}

const root = await mkdtemp(path.join(tmpdir(), "plasm-ledger-review-"));
try {
  const controlReview = mockReviewMustNotRun();
  const controlActor = mockCompleteOnly();
  const control = new PlasmAgent({
    agentRoot: root,
    model: controlActor,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
    includeEvalTerminals: true,
    includeTaskLedger: true,
    includeTaskLedgerReview: false,
    taskLedgerReviewModel: controlReview,
  });
  const controlLiturgy = await control.loadInstructions();
  assert.equal(controlLiturgy.includes(TASK_LEDGER_REVIEW_ACTOR_LITURGY), false);
  const controlResult = await control.generate(INSTRUCTION, {
    wrapTools: keepLedgerAndComplete,
    maxSteps: 4,
  });
  assert.equal(controlResult.reviewGenerateCount, 0, "flag off: no review generate");
  assert.equal(control.taskLedgerReviews.length, 0);
  assert.equal(controlResult.stopReason, "completed");
  assert.equal(controlResult.toolInvocations.includes(COMPLETE_TASK_TOOL_NAME), true);

  const allowReview = mockReviewJson(allowVerdict);
  const allowActor = mockLedgerThenComplete(venmoLedger);
  const allowAgent = new PlasmAgent({
    agentRoot: root,
    model: allowActor,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
    includeEvalTerminals: true,
    includeTaskLedger: true,
    includeTaskLedgerReview: true,
    taskLedgerReviewModel: allowReview,
  });
  const allowResult = await allowAgent.generate(INSTRUCTION, {
    wrapTools: keepLedgerAndComplete,
    maxSteps: 6,
  });
  assert.equal(allowResult.reviewGenerateCount, 1, "one review generate per gated finish");
  assert.equal(allowResult.stopReason, "completed", "host obeys allow; remaining_work is not auto-fail");
  assert.equal(allowAgent.taskLedgerReviews[0]?.allowed, true);
  assert.deepEqual(allowAgent.taskLedger?.remaining_work, venmoLedger.remaining_work);
  const allowPrompt = JSON.stringify(allowReview.doStreamCalls[0]);
  assert.match(allowPrompt, /I owe them/);
  assert.match(allowPrompt, /not in 25-row directory/);
  assert.match(allowPrompt, /expense created and receipt attached/);
  assert.match(allowPrompt, /did not authorize a fallback/);
  assert.match(allowPrompt, /Venmo payment to A/);
  assert.match(allowPrompt, /complete_task/);
  assert.equal(allowPrompt.includes("they owe me"), false, "host does not flip payer/debtor");
  assert.equal(allowPrompt.includes("grader"), false);
  assert.equal(allowPrompt.includes("EvalTerminal"), false);
  assert.equal(allowPrompt.includes("AppWorld"), false);
  assert.equal(allowPrompt.includes("recipe"), false);
  assert.equal(
    allowPrompt.includes("therefore they lack cards"),
    false,
    "host does not judge negative evidence true or false",
  );
  assert.equal((allowReview.doStreamCalls[0]?.tools ?? []).length, 0, "review context has no actor tools");
  assert.match(allowPrompt, new RegExp(TASK_LEDGER_REVIEW_INSTRUCTION_HEADER));
  assert.ok(
    allowResult.usage.totalTokens !== undefined && allowResult.usage.totalTokens > 0,
    "review usage is counted into the turn",
  );

  const continueReview = mockReviewJson(continueVerdict);
  const continueActor = mockLedgerThenComplete(venmoLedger);
  const continueAgent = new PlasmAgent({
    agentRoot: root,
    model: continueActor,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
    includeEvalTerminals: true,
    includeTaskLedger: true,
    includeTaskLedgerReview: true,
    taskLedgerReviewModel: continueReview,
  });
  const continueResult = await continueAgent.generate(INSTRUCTION, {
    wrapTools: keepLedgerAndComplete,
    maxSteps: 6,
  });
  assert.equal(continueResult.reviewGenerateCount, 1);
  assert.equal(continueResult.stopReason, "unterminated", "continue is not a graded complete");
  assert.equal(continueAgent.taskLedgerReviews[0]?.allowed, false);
  assert.match(
    continueResult.messages.map((m) => JSON.stringify(m)).join("\n"),
    /ledger review: continue/,
  );
  assert.deepEqual(
    continueAgent.taskLedger?.remaining_work,
    venmoLedger.remaining_work,
    "host still does not auto-fail from remaining_work; reviewer chose continue",
  );

  let runExecuted = 0;
  const runReview = mockReviewJson(continueVerdict);
  const runActor = mockPlasmRunThenStop();
  const runAgent = new PlasmAgent({
    agentRoot: root,
    model: runActor,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
    includeTaskLedgerReview: true,
    taskLedgerReviewModel: runReview,
  });
  const runResult = await runAgent.generate(INSTRUCTION, {
    wrapTools: () => ({
      plasm_run: tool({
        description: "run",
        inputSchema: jsonSchema({ type: "object", properties: {} }),
        execute: async () => {
          runExecuted += 1;
          return "RAN";
        },
      }),
    }),
    maxSteps: 4,
  });
  assert.equal(runResult.reviewGenerateCount, 1, "plasm_run is a gated review seat");
  assert.equal(runExecuted, 0, "continue must not execute the live run");
  const runPrompt = JSON.stringify(runReview.doStreamCalls[0]);
  assert.match(runPrompt, /plasm_run/);
  assert.match(runPrompt, /I owe them/);
  assert.equal(runPrompt.includes("grader"), false);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log(
  "PASS: ledger review is isolated, opt-in, and host-obedient; remaining_work is not auto-fail",
);
