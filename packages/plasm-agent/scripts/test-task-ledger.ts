#!/usr/bin/env node
/**
 * Persistent task ledger: host stores model-authored fields; host does not
 * invent obligations; facts vs interpretations stay distinct; control loop
 * unchanged when the experiment flag is off.
 */
import assert from "node:assert/strict";
import { mkdtemp, rm, readFile, writeFile, mkdir, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { asSchema, tool, type ToolSet } from "ai";
import { z } from "zod";
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
  type TaskLedgerRecord,
  emptyTaskLedger,
  isKnownEvidenceRef,
  parseTaskLedgerRecord,
  renderTaskLedgerState,
  taskLedgerRecordBytes,
  taskLedgerRecordSchema,
  taskLedgerInputSchema,
  createTaskLedgerTool,
} from "../src/tools/task-ledger.js";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";

const unused = async (): Promise<never> => {
  throw new Error("unexpected engine call");
};
const engine: PlasmEngine = {
  close: async () => {},
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
  obligations: [{ id: 1, task: "satisfy the original delivery condition", status: "pending" as const }],
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

function patterns(value: unknown): string[] {
  if (value === null || typeof value !== "object") return [];
  return Object.entries(value).flatMap(([key, child]) =>
    key === "pattern" && typeof child === "string" ? [child] : patterns(child));
}
const advertisedPatterns = [...new Set(patterns(asSchema(ledgerTools.task_ledger!.inputSchema).jsonSchema))];
assert.equal(advertisedPatterns.length, 1, "every evidence field shares one advertised grammar");
const advertised = new RegExp(advertisedPatterns[0]!);
assert.doesNotMatch(advertised.source, /\(\?(?:[=!]|<[=!])/, "provider pattern must not contain lookaround");
for (const [ref, accepted] of [
  ["pr" + "a".repeat(64), true], ["pc0", true], ["pg12", true],
  ["plasm://execute/x/y/run/pr" + "b".repeat(64), true],
  ["plasm", true], ["plasm_run", true], ["plasm_context", true], ["plasm_failure", true],
  ["pr" + "a".repeat(63), false], ["pr" + "a".repeat(65), false],
  ["pr" + "G".repeat(64), false], ["run_ref pg1", false], ["run_id pr" + "a".repeat(64), false],
  [" pc1 ", false], ["pc1\n", false], ["p 1", false], ["pg-1", false], ["plasm://", false],
] as const) {
  assert.equal(isKnownEvidenceRef(ref), accepted, ref);
  assert.equal(advertised.exec(ref)?.[0] === ref, accepted, `full-match grammar: ${ref}`);
}
// Exact host admission must preserve the old absolute-end grammar, including
// every JS final-line-terminator case; the provider receives only portable syntax.
const previous = /^(?:pr[a-f0-9]{64}|p[cg]\d+|plasm:\/\/\S+|plasm|plasm_run|plasm_context|plasm_failure)(?![\s\S])/;
for (const valid of ["pr" + "f".repeat(64), "pc12", "pg0", "plasm", "plasm_run", "plasm_context", "plasm_failure", "plasm://abstract/record"]) {
  for (const suffix of ["", "\n", "\r", "\r\n", "\u2028", "\u2029", " ", "\t", "\nextra"]) {
    const candidate = valid + suffix;
    assert.equal(isKnownEvidenceRef(candidate), previous.test(candidate), JSON.stringify(candidate));
    assert.equal(parseTaskLedgerRecord({ ...authored, observed_facts: [{kind: "fact", text: "abstract evidence", evidence_ref: candidate}] }).ok,
      previous.test(candidate), "record admission retains exact validation");
  }
}

const parsed = parseTaskLedgerRecord(authored);
const trailingLine = taskLedgerRecordSchema.safeParse({ ...authored,
  observed_facts: [{kind: "fact", text: "abstract evidence", evidence_ref: "pc1\n"}] });
assert.equal(trailingLine.success, false);
if (!trailingLine.success) {
  assert.ok(trailingLine.error.issues.some(issue => issue.code === "invalid_string" && issue.validation === "regex"),
    "full-match rejection retains typed regex repair diagnostics");
}
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
  obligations: ["x", "y", "z"].map((x, i) => ({ id: i + 1, task: x.repeat(1500), status: "pending" })),
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
  compact = parseTaskLedgerRecord({ ...authored, obligations: [{ id: 1, task: remaining, status: "pending" }] });
  assert.equal(compact.ok, true, "repeated compact updates succeed");
  if (!compact.ok) throw new Error("expected compact revise");
  assert.ok(taskLedgerRecordBytes(compact.record) <= TASK_LEDGER_MAX_BYTES);
  store.replace(compact.record);
}
assert.equal(store.record.obligations[0]?.task, "one more compact revise");
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
  obligations: [],
});
assert.equal(sparse.ok, true);
if (!sparse.ok) throw new Error("expected sparse parse ok");
store.replace(sparse.record);
assert.deepEqual(
  store.record.obligations,
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

function mockLedgerThenStop(
  record: TaskLedgerRecord,
  intermediateCalls: { toolName: string; input: unknown }[] = [],
) {
  let count = 0;
  return new MockLanguageModelV3({
    doStream: async () => {
      const index = count++;
      const call = index === 0
        ? { toolName: TASK_LEDGER_TOOL_NAME, input: { action: "write", record } }
        : intermediateCalls[index - 1];
      const hasCall = call !== undefined;
      return {
        stream: new ReadableStream({
          start(controller) {
            if (hasCall) {
              controller.enqueue({
                type: "tool-call",
                toolCallId: `ledger-${index}`,
                toolName: call.toolName,
                input: JSON.stringify(call.input),
              });
            } else {
              controller.enqueue({ type: "text-start", id: "text" });
              controller.enqueue({ type: "text-delta", id: "text", delta: "continuing" });
              controller.enqueue({ type: "text-end", id: "text" });
            }
            controller.enqueue({
              type: "finish",
              finishReason: { unified: hasCall ? "tool-calls" : "stop", raw: undefined },
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
  control.runtime.hasOpenWorkflow = () => true;
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
  treatment.runtime.hasOpenWorkflow = () => true;
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
  const priorPrefix = freshnessModel.doStreamCalls[0]!.prompt.slice(0, -1);
  assert.deepEqual(freshnessModel.doStreamCalls[1]!.prompt.slice(0, priorPrefix.length), priorPrefix,
    "changing ledger snapshot leaves the entire preceding request prefix identical");
  assert.deepEqual(freshnessModel.doStreamCalls[0]!.tools, freshnessModel.doStreamCalls[1]!.tools,
    "ledger updates do not mutate provider tool schemas");
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
  agent.runtime.hasOpenWorkflow = () => true;
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

const obligationRoot = await mkdtemp(path.join(tmpdir(), "plasm-obligation-"));
try {
  const obligation = {
    ...authored,
    requested_outcomes: ["publish the requested revision"],
    scope: "Preserve unrelated resource R's original location and properties.",
    alternative_conditions: [],
    observed_facts: [{
      kind: "fact" as const,
      text: "Resource R originally occupied slot A with mode quiet.",
      evidence_ref: "pr" + "c".repeat(64),
    }],
    interpretations: [],
    unresolved_questions: [],
    obligations: [{ id: 1, task: "Restore R to slot A with mode quiet and verify the observed result.", status: "deferred" as const, reason: "Await reconciliation before restoration." }],
  };
  const model = mockLedgerThenStop(obligation, [
    { toolName: "temporary_operation", input: {} },
    { toolName: TASK_LEDGER_TOOL_NAME, input: {
      action: "write", record: { interpretations: [] },
    } },
    { toolName: TASK_LEDGER_TOOL_NAME, input: {
      action: "write",
      record: { ...obligation, obligations: ["x", "y", "z"].map((x, i) => ({ id: i + 2, task: x.repeat(1500), status: "pending" })) },
    } },
    { toolName: TASK_LEDGER_TOOL_NAME, input: { action: "read" } },
  ]);
  const agent = new PlasmAgent({
    agentRoot: obligationRoot, model, engine, hostTransport: null,
    archiveEnabled: false, telemetry: false, includeTaskLedger: true,
  });
  agent.runtime.hasOpenWorkflow = () => true;
  let executions = 0;
  const wrap = (tools: ToolSet): ToolSet => ({
    [TASK_LEDGER_TOOL_NAME]: tools[TASK_LEDGER_TOOL_NAME]!,
    temporary_operation: tool({
      inputSchema: z.object({}),
      execute: async (): Promise<string> => {
        executions += 1;
        throw new Error("abstract operation failed after a temporary change");
      },
    }),
  });
  const result = await agent.generate("ABSTRACT_WORKFLOW", { wrapTools: wrap });
  assert.equal(executions, 1, "a failure must not automatically replay an effect");
  assert.deepEqual(agent.taskLedger, obligation);
  const audit = JSON.stringify(result.messages);
  assert.match(audit, /abstract operation failed/);
  assert.match(audit, /aggregate budget/);
  for (const call of model.doStreamCalls.slice(1)) {
    // Inspect the fresh state echo, not an old ledger write buried in history.
    assert.equal(call.prompt.at(-1)?.role, "user");
    const echo = JSON.stringify(call.prompt.at(-1));
    assert.match(echo, /Model-authored task ledger/);
    assert.ok(echo.includes(obligation.scope));
    assert.ok(echo.includes(obligation.observed_facts[0]!.text));
    assert.ok(echo.includes(obligation.obligations[0]!.task));
    assert.ok(echo.includes("deferred"));
    assert.ok(echo.includes(obligation.obligations[0]!.reason));
  }
  await agent.generate("FOLLOW_UP", { wrapTools: wrap });
  assert.deepEqual(agent.taskLedger, obligation, "ending a turn does not discharge remaining work");
  assert.ok(JSON.stringify(model.doStreamCalls.at(-1)?.prompt.at(-1)).includes(obligation.obligations[0]!.task));
  const independent = new PlasmAgent({
    agentRoot: obligationRoot, model, engine, hostTransport: null,
    archiveEnabled: false, telemetry: false, includeTaskLedger: true,
  });
  assert.deepEqual(independent.taskLedger, obligation, "restart loads the accepted obligation and completed effects");
  independent.runtime.hasOpenWorkflow = () => true;
  await independent.generate("RESUME", { wrapTools: wrap });
  assert.equal(executions, 1, "loading a checkpoint must never replay effects");
  assert.ok(JSON.stringify(model.doStreamCalls.at(-1)?.prompt.at(-1)).includes(obligation.obligations[0]!.task));
  const checkpoint = path.join(obligationRoot, "task-ledger.json");
  assert.equal((await stat(checkpoint)).mode & 0o777, 0o600);
  const updateStore = new TaskLedgerStore(checkpoint);
  const updateTool = createTaskLedgerTool(updateStore)[TASK_LEDGER_TOOL_NAME]!;
  const writeUpdate = async (record: unknown) => {
    const input = taskLedgerInputSchema.parse({ action: "write", record });
    return updateTool.execute!(input, { toolCallId: "abstract-update", messages: [], context: {} });
  };
  const acknowledgement = await writeUpdate({ completed_effects: [{ effect: "Published revision", satisfaction: "interpretation" }] });
  assert.equal(acknowledgement, "Host stored your write. Omitted fields retained.");
  assert.deepEqual(updateStore.record.obligations, obligation.obligations);
  assert.deepEqual(updateStore.record.observed_facts, obligation.observed_facts);
  assert.deepEqual(new TaskLedgerStore(checkpoint).record, updateStore.record, "merged updates survive restart");
  const snapshot = renderTaskLedgerState(updateStore.record);
  assert.deepEqual(JSON.parse(snapshot.split("```json\n")[1]!.split("\n```")[0]!), updateStore.record,
    "compact snapshot preserves every field and evidence reference without truncation");
  const acceptedBytes = await readFile(checkpoint, "utf8");
  for (const invalid of [{ obligations: null }, { invented: [] }, { observed_facts: [{ kind: "fact", text: "X", evidence_ref: "bad" }] }]) {
    assert.equal(taskLedgerInputSchema.safeParse({ action: "write", record: invalid }).success, false);
    assert.equal(await readFile(checkpoint, "utf8"), acceptedBytes);
  }
  assert.match(String(await writeUpdate({ interpretations: [
    { kind: "interpretation", text: "x".repeat(1900) },
    { kind: "interpretation", text: "y".repeat(1900) },
  ] })), /aggregate budget/);
  assert.equal(await readFile(checkpoint, "utf8"), acceptedBytes, "merged budget rejection preserves checkpoint");
  await writeUpdate({});
  assert.equal(await readFile(checkpoint, "utf8"), acceptedBytes, "empty update preserves state");
  await writeUpdate({ obligations: [], scope: "" });
  assert.deepEqual(updateStore.record.obligations, obligation.obligations);
  assert.equal(updateStore.record.scope, "");
  assert.deepEqual(updateStore.record.observed_facts, obligation.observed_facts);
  // Each update concerns one commitment; omission is never resolution.
  for (const task of ["Restore a routing definition", "Verify a calibration", "Release a temporary reservation"]) {
    updateStore.replace(emptyTaskLedger());
    await writeUpdate({ obligations: [
      { id: 1, task, status: "pending" },
      { id: 2, task: "Inspect another resource", status: "pending" },
    ] });
    for (const status of ["pending", "deferred"] as const) {
      const refined = status === "pending"
        ? { id: 1, task: `${task}; verify the original configuration.`, status }
        : { id: 1, task: `${task}; verify the original configuration.`, status, reason: "Await the observed prerequisite" };
      assert.equal(await writeUpdate({ obligations: [refined], unresolved_questions: ["Which observation confirms restoration?"] }),
        "Host stored your write. Omitted fields retained.");
      assert.deepEqual(updateStore.record.obligations, [refined, { id: 2, task: "Inspect another resource", status: "pending" }],
        "wording refinement updates the same ID without dropping other pending work");
      assert.deepEqual(updateStore.record.unresolved_questions, ["Which observation confirms restoration?"],
        "refinement does not reject accompanying fields");
      assert.deepEqual(new TaskLedgerStore(checkpoint).record, updateStore.record, "refinements survive restart");
      assert.ok(renderTaskLedgerState(updateStore.record).includes(refined.task), "fresh snapshot exposes refined wording");
      const retained = updateStore.record;
      await writeUpdate({ obligations: [] });
      assert.deepEqual(updateStore.record, retained, "empty update cannot discharge refined work");
      const before = await readFile(checkpoint, "utf8");
      await assert.rejects(writeUpdate({ obligations: [{ ...refined, task: "x".repeat(5000) }] }), /at most 2000/);
      assert.equal(await readFile(checkpoint, "utf8"), before, "invalid refinement cannot damage accepted state");
    }
    await writeUpdate({ obligations: [{ id: 1, task, status: "deferred", reason: "Await the observed prerequisite" }] });
    await writeUpdate({ obligations: [{ id: 2, task: "Inspect another resource", status: "resolved", evidence_ref: "plasm_run" }] });
    assert.equal(updateStore.record.obligations[0]?.status, "deferred");
    assert.equal(updateStore.record.obligations[1]?.status, "resolved");
    assert.deepEqual(new TaskLedgerStore(checkpoint).record, updateStore.record);
    assert.match(renderTaskLedgerState(updateStore.record), /"status":"deferred"/);
    for (const entry of [
      { id: 1, task, status: "deferred" },
      { id: 1, task, status: "resolved" },
      { id: 1, task, status: "resolved", evidence_ref: "invented reference" },
      { id: 1, task, status: "cancelled" },
    ]) assert.equal(taskLedgerInputSchema.safeParse({ action: "write", record: { obligations: [entry] } }).success, false);
    assert.equal(taskLedgerInputSchema.safeParse({ action: "write", record: { obligations: [
      { id: 1, task, status: "pending" }, { id: 1, task, status: "resolved", evidence_ref: "plasm_run" },
    ] } }).success, false);
    await writeUpdate({ obligations: [{ id: 1, task, status: "resolved", evidence_ref: "plasm_run" }] });
    assert.equal(updateStore.record.obligations[0]?.status, "resolved");
    assert.deepEqual(updateStore.record.completed_effects, [], "resolving an obligation does not fabricate an effect");
    await writeUpdate({ obligations: [{ id: 1, task, status: "pending" }] });
    assert.equal(updateStore.record.obligations[0]?.status, "pending", "a mistaken resolution can be explicitly reopened");
    await writeUpdate({ obligations: [{ id: 1, task, status: "cancelled", reason: "The user withdrew this outcome" }] });
    assert.equal(updateStore.record.obligations[0]?.status, "cancelled", "withdrawal is not completion");
  }
  updateStore.replace(obligation); // restore fixture for existing persistence-failure matrix
  const saved = await readFile(checkpoint, "utf8");
  const restarted = spawnSync(process.execPath, ["--import", "tsx", "--input-type=module", "--eval",
    `import { TaskLedgerStore } from ${JSON.stringify(new URL("../src/tools/task-ledger.ts", import.meta.url).href)};
     process.stdout.write(JSON.stringify(new TaskLedgerStore(process.argv[1]).record));`, checkpoint], { encoding: "utf8" });
  assert.equal(restarted.status, 0, restarted.stderr);
  assert.deepEqual(JSON.parse(restarted.stdout), obligation, "separate process recovers accepted state");
  const validStore = new TaskLedgerStore(checkpoint);
  assert.throws(() => validStore.replace({ ...obligation, obligations: [{ id: 1, task: "x".repeat(5000), status: "pending" }] }), /Task ledger write failed/);
  assert.equal(await readFile(checkpoint, "utf8"), saved, "invalid replacement preserves the last good file byte-for-byte");
  // A failed atomic rename must not replace the accepted in-memory record.
  const durable = new TaskLedgerStore(checkpoint);
  await rm(checkpoint);
  await mkdir(checkpoint);
  assert.throws(() => durable.clear(), /Task ledger write failed/);
  assert.deepEqual(durable.record, obligation);
  await rm(checkpoint, { recursive: true });
  await writeFile(checkpoint, saved);
  assert.deepEqual(new TaskLedgerStore(checkpoint).record, obligation);
  durable.clear();
  assert.deepEqual(new TaskLedgerStore(checkpoint).record, emptyTaskLedger(), "reset is durable");
  const oldCheckpoint = JSON.stringify({ ...JSON.parse(saved), version: 1 });
  await writeFile(checkpoint, oldCheckpoint);
  assert.throws(() => new TaskLedgerStore(checkpoint), /Task ledger load failed/, "old formats must not silently erase obligations");
  assert.equal(await readFile(checkpoint, "utf8"), oldCheckpoint, "rejected checkpoints remain intact");
  await writeFile(checkpoint, "{broken");
  assert.throws(() => new TaskLedgerStore(checkpoint), /Task ledger load failed/, "corruption must not silently erase obligations");
} finally {
  await rm(obligationRoot, { recursive: true, force: true });
}

console.log("PASS: ledger survives process restart, rejected writes and turns; no effects replayed; reset durable; corruption explicit");
