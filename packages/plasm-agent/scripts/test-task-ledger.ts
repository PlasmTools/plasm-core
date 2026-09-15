#!/usr/bin/env node
/**
 * Persistent task ledger: host stores model-authored fields; host does not
 * invent obligations; facts vs interpretations stay distinct; control loop
 * unchanged when the experiment flag is off.
 */
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import type { ToolSet } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import {
  EVAL_TERMINAL_COMPLETION,
  TASK_LEDGER_LITURGY,
  TASK_LEDGER_PLAN_OVERLAY,
  TASK_LEDGER_PLAN_SLOT,
  WORKFLOW_COMPLETION_SLOT,
  buildDefaultSystemLiturgy,
} from "../src/prompts/index.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";
import {
  TASK_LEDGER_MAX_BYTES,
  TASK_LEDGER_STATE_HEADER,
  TASK_LEDGER_TOOL_NAME,
  TaskLedgerStore,
  emptyTaskLedger,
  isKnownEvidenceRef,
  parseTaskLedgerRecord,
  renderTaskLedgerState,
  taskLedgerRecordBytes,
} from "../src/tools/task-ledger.js";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";

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

const authored = {
  requested_outcomes: ["deliver the named item"],
  scope: "one recipient, one item",
  alternative_conditions: [
    {
      condition: "user named the fallback channel",
      action: "use the fallback channel",
      condition_met: "no" as const,
    },
  ],
  observed_facts: [
    { kind: "fact" as const, text: "preferred channel returned an error", evidence_ref: "pr" + "a".repeat(64) },
  ],
  interpretations: [
    { kind: "interpretation" as const, text: "preferred channel is unavailable" },
  ],
  unresolved_questions: ["whether the user named a fallback"],
  completed_effects: [],
  remaining_work: ["satisfy the original delivery condition"],
};

const product = buildDefaultSystemLiturgy();
assert.equal(product.includes(WORKFLOW_COMPLETION_SLOT), true);
assert.equal(product.includes(TASK_LEDGER_PLAN_SLOT), true);
assert.equal(product.includes(TASK_LEDGER_PLAN_OVERLAY), false);
assert.equal(product.includes(TASK_LEDGER_LITURGY), false);
assert.equal(product.includes(TASK_LEDGER_TOOL_NAME), false);
assert.equal(product.includes(TASK_LEDGER_STATE_HEADER), false);

const evalOnly = buildDefaultSystemLiturgy({ includeEvalTerminals: true });
assert.equal(evalOnly.includes(EVAL_TERMINAL_COMPLETION), true);
assert.equal(evalOnly.includes(TASK_LEDGER_PLAN_SLOT), true);
assert.equal(evalOnly.includes(TASK_LEDGER_LITURGY), false);

const ledgerLiturgy = buildDefaultSystemLiturgy({ includeTaskLedger: true });
assert.equal(ledgerLiturgy.includes(TASK_LEDGER_PLAN_OVERLAY), true);
assert.equal(ledgerLiturgy.includes(TASK_LEDGER_PLAN_SLOT), false);
assert.equal(ledgerLiturgy.includes(TASK_LEDGER_LITURGY), true);
assert.equal(ledgerLiturgy.includes(WORKFLOW_COMPLETION_SLOT), true);
assert.match(ledgerLiturgy, /model-authored state/);
assert.equal(ledgerLiturgy.includes(TASK_LEDGER_STATE_HEADER), false);
assert.match(ledgerLiturgy, /not a terminal/);
assert.equal(ledgerLiturgy.includes("halt-after"), false);
assert.equal(ledgerLiturgy.includes("AppWorld"), false);
assert.equal(ledgerLiturgy.includes("grader"), false);

const both = buildDefaultSystemLiturgy({ includeEvalTerminals: true, includeTaskLedger: true });
assert.equal(both.includes(EVAL_TERMINAL_COMPLETION), true);
assert.equal(both.includes(WORKFLOW_COMPLETION_SLOT), false);
assert.equal(both.includes(TASK_LEDGER_PLAN_OVERLAY), true);
assert.equal(both.includes(TASK_LEDGER_LITURGY), true);

const productTools = createHarnessTools({});
assert.equal(TASK_LEDGER_TOOL_NAME in productTools, false);
assert.equal("complete_task" in productTools, false);

assert.throws(
  () => createHarnessTools({ includeTaskLedger: true }),
  /includeTaskLedger requires taskLedgerStore/,
);

const store = new TaskLedgerStore();
assert.deepEqual(store.record, emptyTaskLedger());
const ledgerTools = createHarnessTools({ includeTaskLedger: true, taskLedgerStore: store });
assert.equal(TASK_LEDGER_TOOL_NAME in ledgerTools, true);

const parsed = parseTaskLedgerRecord(authored);
assert.equal(parsed.ok, true);
if (!parsed.ok) throw new Error("expected parse ok");
const persisted = store.replace(parsed.record);
assert.deepEqual(persisted, authored);
assert.deepEqual(store.record, authored, "host persists model-authored fields verbatim");

assert.equal(isKnownEvidenceRef("pr" + "a".repeat(64)), true);
assert.equal(isKnownEvidenceRef("pc0"), true);
assert.equal(isKnownEvidenceRef("plasm_run"), true);
assert.equal(isKnownEvidenceRef("plasm://execute/x/y/run/pr" + "b".repeat(64)), true);
assert.equal(isKnownEvidenceRef(""), false);
assert.equal(isKnownEvidenceRef("because I think so"), false);

const missingCite = parseTaskLedgerRecord({
  ...authored,
  observed_facts: [{ kind: "fact", text: "unsupported as fact" }],
});
assert.equal(missingCite.ok, false, "observed_facts without evidence_ref are rejected");

const bogusCite = parseTaskLedgerRecord({
  ...authored,
  observed_facts: [{ kind: "fact", text: "unsupported as fact", evidence_ref: "because I think so" }],
});
assert.equal(bogusCite.ok, false, "unknown evidence_ref shape is rejected");

const cited = parseTaskLedgerRecord(authored);
assert.equal(cited.ok, true);
if (!cited.ok) throw new Error("expected cited parse ok");
assert.equal(cited.record.observed_facts[0]?.evidence_ref, authored.observed_facts[0]?.evidence_ref);
assert.equal(
  "proven" in (cited.record.observed_facts[0] as object),
  false,
  "host does not treat evidence_ref as proof",
);
assert.match(renderTaskLedgerState(cited.record), /does not treat citations as proof/);

const oversized = parseTaskLedgerRecord({
  ...emptyTaskLedger(),
  remaining_work: ["x".repeat(1500), "y".repeat(1500), "z".repeat(1500)],
});
assert.equal(oversized.ok, false, "aggregate budget rejects oversized replace");
if (oversized.ok) throw new Error("expected oversized reject");
assert.match(oversized.error, /aggregate budget/);
assert.ok(1500 * 3 > TASK_LEDGER_MAX_BYTES / 2);

let compact = parseTaskLedgerRecord(authored);
assert.equal(compact.ok, true);
if (!compact.ok) throw new Error("expected compact parse ok");
store.replace(compact.record);
for (const remaining of ["still the original condition", "one more compact revise"]) {
  compact = parseTaskLedgerRecord({ ...authored, remaining_work: [remaining] });
  assert.equal(compact.ok, true, "repeated compact updates succeed");
  if (!compact.ok) throw new Error("expected compact revise");
  assert.ok(taskLedgerRecordBytes(compact.record) <= TASK_LEDGER_MAX_BYTES);
  store.replace(compact.record);
}
assert.deepEqual(store.record.remaining_work, ["one more compact revise"]);
store.replace(authored);

const mixed = parseTaskLedgerRecord({
  ...authored,
  observed_facts: [{ kind: "interpretation", text: "smuggled" }],
});
assert.equal(mixed.ok, false, "facts vs interpretations stay distinct");
assert.deepEqual(store.record, authored, "rejected write must not mutate the store");

const sparse = parseTaskLedgerRecord({
  requested_outcomes: ["only what the model wrote"],
  scope: "",
  alternative_conditions: [],
  observed_facts: [],
  interpretations: [],
  unresolved_questions: [],
  completed_effects: [],
  remaining_work: [],
});
assert.equal(sparse.ok, true);
if (!sparse.ok) throw new Error("expected sparse parse ok");
store.replace(sparse.record);
assert.deepEqual(
  store.record.remaining_work,
  [],
  "host does not invent remaining-work obligations",
);
assert.deepEqual(store.record.requested_outcomes, ["only what the model wrote"]);
assert.equal(store.record.alternative_conditions.length, 0);

const unauthorizedAlt = parseTaskLedgerRecord({
  ...emptyTaskLedger(),
  requested_outcomes: ["use the preferred channel"],
  alternative_conditions: [
    {
      condition: "user named the fallback",
      action: "used the fallback after preferred failed",
      condition_met: "no",
    },
  ],
});
assert.equal(unauthorizedAlt.ok, true);
if (!unauthorizedAlt.ok) throw new Error("expected alt parse ok");
const afterAlt = store.replace(unauthorizedAlt.record);
assert.equal(
  afterAlt.alternative_conditions[0]?.condition_met,
  "no",
  "host does not flip condition_met after a failed preferred action",
);

function mockProseStop() {
  return new MockLanguageModelV3({
    doStream: async () => ({
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({ type: "text-start", id: "text" });
          controller.enqueue({ type: "text-delta", id: "text", delta: "ordinary prose" });
          controller.enqueue({ type: "text-end", id: "text" });
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "stop", raw: undefined },
            usage: {
              inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
              outputTokens: { total: 2, text: 2, reasoning: 0 },
            },
          });
          controller.close();
        },
      }),
    }),
  });
}

function mockLedgerThenStop(record: typeof authored) {
  let count = 0;
  return new MockLanguageModelV3({
    doStream: async () => {
      const index = count++;
      const write = index === 0;
      return {
        stream: new ReadableStream({
          start(controller) {
            if (write) {
              controller.enqueue({
                type: "tool-call",
                toolCallId: "ledger-0",
                toolName: TASK_LEDGER_TOOL_NAME,
                input: JSON.stringify({ action: "write", record }),
              });
            } else {
              controller.enqueue({ type: "text-start", id: "text" });
              controller.enqueue({ type: "text-delta", id: "text", delta: "continuing" });
              controller.enqueue({ type: "text-end", id: "text" });
            }
            controller.enqueue({
              type: "finish",
              finishReason: { unified: write ? "tool-calls" : "stop", raw: undefined },
              usage: {
                inputTokens: { total: 10, noCache: 10, cacheRead: 0, cacheWrite: 0 },
                outputTokens: { total: 2, text: 2, reasoning: 0 },
              },
            });
            controller.close();
          },
        }),
      };
    },
  });
}

const root = await mkdtemp(path.join(tmpdir(), "plasm-ledger-"));
try {
  const controlModel = mockProseStop();
  const control = new PlasmAgent({
    agentRoot: root,
    model: controlModel,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
  });
  assert.equal(control.taskLedger, null, "flag off: host exposes no ledger");
  const controlLiturgy = await control.loadInstructions();
  assert.equal(controlLiturgy.includes(TASK_LEDGER_LITURGY), false);
  assert.equal(controlLiturgy.includes(TASK_LEDGER_STATE_HEADER), false);
  assert.equal(controlLiturgy.includes(TASK_LEDGER_PLAN_SLOT), true);
  await control.generate("CONTROL_TASK", {
    wrapTools: (tools) => {
      assert.equal(TASK_LEDGER_TOOL_NAME in tools, false, "flag off: no ledger tool");
      return {};
    },
  });
  assert.equal(control.taskLedger, null);
  assert.equal(
    JSON.stringify(controlModel.doStreamCalls[0]?.prompt).includes("Model-authored task ledger"),
    false,
    "flag off: no per-step ledger state message",
  );

  const freshnessModel = mockLedgerThenStop(authored);
  const treatment = new PlasmAgent({
    agentRoot: root,
    model: freshnessModel,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
    includeTaskLedger: true,
  });
  assert.deepEqual(treatment.taskLedger, emptyTaskLedger());
  const keepLedger = (tools: ToolSet): ToolSet => {
    assert.equal(TASK_LEDGER_TOOL_NAME in tools, true);
    return { [TASK_LEDGER_TOOL_NAME]: tools[TASK_LEDGER_TOOL_NAME]! };
  };
  const first = await treatment.generate("ORIGINAL_TASK", { wrapTools: keepLedger });
  assert.equal(first.toolInvocations.includes(TASK_LEDGER_TOOL_NAME), true);
  assert.equal(
    first.toolInvocations.filter((name) => name === TASK_LEDGER_TOOL_NAME).length,
    1,
    "ledger write is one in-loop tool call — not a nested model generate",
  );
  assert.deepEqual(treatment.taskLedger, authored);
  assert.match(first.messages.map((m) => JSON.stringify(m)).join("\n"), /Host stored your write/);

  const step0 = JSON.stringify(freshnessModel.doStreamCalls[0]?.prompt);
  const step1 = JSON.stringify(freshnessModel.doStreamCalls[1]?.prompt);
  assert.match(step0, /Model-authored task ledger/);
  assert.equal(
    step0.includes("deliver the named item"),
    false,
    "step 0 sees the empty store, not a pre-loop invented record",
  );
  assert.match(step1, /Model-authored task ledger/);
  assert.match(step1, /deliver the named item/);
  assert.match(step1, /not host law/);
  assert.match(step1, /does not treat citations as proof/);

  const followLiturgy = await treatment.loadInstructions();
  assert.equal(
    followLiturgy.includes("deliver the named item"),
    false,
    "system liturgy must not embed the record as host law",
  );
  assert.equal(followLiturgy.includes(TASK_LEDGER_STATE_HEADER), false);
  assert.match(followLiturgy, /model-authored state/);
  assert.ok(first.usage.totalTokens !== undefined && first.usage.totalTokens > 0);

  await treatment.generate("NEW_TASK", { resetConversation: true, wrapTools: keepLedger });
  assert.deepEqual(
    treatment.taskLedger,
    emptyTaskLedger(),
    "resetConversation clears the host store — host does not reconstruct obligations",
  );
} finally {
  await rm(root, { recursive: true, force: true });
}

const persistRoot = await mkdtemp(path.join(tmpdir(), "plasm-ledger-persist-"));
try {
  const model = mockLedgerThenStop(authored);
  const agent = new PlasmAgent({
    agentRoot: persistRoot,
    model,
    engine,
    hostTransport: null,
    archiveEnabled: false,
    telemetry: false,
    includeTaskLedger: true,
  });
  const wrap = (tools: ToolSet): ToolSet => {
    assert.equal(TASK_LEDGER_TOOL_NAME in tools, true);
    return { [TASK_LEDGER_TOOL_NAME]: tools[TASK_LEDGER_TOOL_NAME]! };
  };
  await agent.generate("ORIGINAL_TASK", { wrapTools: wrap });
  await agent.generate("FOLLOW_UP", { wrapTools: wrap });
  const followCall = model.doStreamCalls.at(-1);
  const prompt = JSON.stringify(followCall?.prompt);
  assert.match(prompt, /Model-authored task ledger/);
  assert.match(prompt, /deliver the named item/, "next turn sees the persisted write as model-authored state");
  assert.match(prompt, /ORIGINAL_TASK/);
  assert.match(prompt, /FOLLOW_UP/);
} finally {
  await rm(persistRoot, { recursive: true, force: true });
}

console.log("PASS: task ledger persists model fields; host invents nothing; flag-off loop unchanged");
