import assert from "node:assert/strict";
import { NoSuchToolError, tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { z } from "zod";
import { checkSessionExtension } from "./test-session-extension.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { ToolCallRecovery } from "../src/telemetry/tool-call-recovery.js";

const malformed = "class Read(Program):\n    def build(self):\n        return e1.query()\n</parameter\n<tool_call>";
const valid = "# </parameter> is legal in a comment\nclass Read(Program):\n    def build(self):\n        return e1.query()\n";
const failure = { cause: "program", recovery: "repair_program", code: "program_syntax", diagnostic: "syntax error", node: null,
  occurrence_path: [], catalog_digest: null, effects: [], dispatches: [], effects_unresolved: false };

// SDK unknown-tool errors must retain ordinary selection authority, not pin a
// repair to an absent tool and thereby expose an empty tool set.
{
  const recovery = new ToolCallRecovery();
  recovery.observe(0, [{ toolCallId: "unknown", toolName: "plams", input: {}, invalid: true,
    error: new NoSuchToolError({ toolName: "plams", availableTools: ["plasm"] }) }], []);
  assert.equal(recovery.activeToolNames, undefined);
  assert.deepEqual(recovery.events, []);
  let requests = 0;
  let effects = 0;
  const model = new MockLanguageModelV3({ doStream: async request => {
    const index = requests++;
    if (index > 0) assert(request.tools?.some(t => t.type === "function" && t.name === "effect"), "valid tool remains available after unknown selection");
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: "tool-call", toolName: index === 0 ? "unknown" : index === 1 ? "effect" : "complete_task", toolCallId: `selection-${index}`, input: "{}" });
      controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined }, usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 2, text: 2, reasoning: 0 } } });
      controller.close();
    } }) };
  } });
  const result = await runEveToolLoop({ model, system: "Abstract selection fixture", messages: [{ role: "user", content: "Use the available tool" }],
    tools: { ...createEvalTerminalTools(), effect: tool({ inputSchema: z.object({}), execute: async () => { effects++; return "done"; } }) },
    maxSteps: 6, agentName: "selection-fixture", telemetry: { isEnabled: false } });
  assert.equal(result.stopReason, "completed");
  assert.equal(effects, 1);
  assert.equal(requests, 3);
  assert.deepEqual(result.toolCallRecovery, []);
}

// Multiple rejected programs using one tool require distinct replacement calls.
for (const recoverSecond of [true, false]) {
  const recovery = new ToolCallRecovery();
  recovery.observe(0, ["first", "second"].map(toolCallId => ({ toolCallId, toolName: "plasm", input: { program: toolCallId }, invalid: true })), []);
  recovery.observe(1, [{ toolCallId: "replacement-first", toolName: "plasm", input: {} }],
    [{ toolCallId: "replacement-first", toolName: "plasm", output: "planned" }]);
  assert.equal(recovery.events.at(-1)?.state, "retrying");
  assert.deepEqual(recovery.events.at(-1)?.calls.map(call => call.toolCallId), ["second"]);
  recovery.observe(2, recoverSecond ? [{ toolCallId: "replacement-second", toolName: "plasm", input: {} }] : [],
    recoverSecond ? [{ toolCallId: "replacement-second", toolName: "plasm", output: "planned" }] : []);
  assert.equal(recovery.events.at(-1)?.state, recoverSecond ? "recovered" : "exhausted");
}
{
  const recovery = new ToolCallRecovery();
  recovery.observe(0, ["first", "second"].map(toolCallId => ({ toolCallId, toolName: "plasm", input: {}, invalid: true })), []);
  recovery.observe(1, [{ toolCallId: "still-first", toolName: "plasm", input: {}, invalid: true }], []);
  assert.deepEqual(recovery.events.at(-1)?.calls.map(call => call.toolCallId), ["still-first", "second"], "malformed replacement cannot erase its sibling obligation");
  recovery.observe(2, ["fixed-first", "fixed-second"].map(toolCallId => ({ toolCallId, toolName: "plasm", input: {} })),
    ["fixed-first", "fixed-second"].map(toolCallId => ({ toolCallId, toolName: "plasm", output: "planned" })));
  assert.equal(recovery.events.at(-1)?.state, "recovered");
}

for (const repaired of [true, false]) {
  await checkSessionExtension(async () => {}, async (runtime, ref, engine) => {
    const submitted: string[] = [];
    let effects = 0;
    engine.dryRun = async program => {
      submitted.push(program);
      return program === malformed
        ? { writeCount: 0, planCommitRef: "", summary: "Python syntax at line 4, column 1:\n</parameter\n^\nNo program prefix was executed.", failureJson: JSON.stringify(failure) }
        : { writeCount: 0, planCommitRef: "pc1", summary: "clean read", fusedCleanRead: false };
    };
    engine.runPlan = async () => ({ ok: true, message: "rows observed" });
    let requests = 0;
    const model = new MockLanguageModelV3({ doStream: async request => {
      const index = requests++;
      if (index > 0) {
        const calls = request.prompt.filter(m => m.role === "assistant").flatMap(m => m.content).filter(p => p.type === "tool-call");
        assert(!calls.some(p => p.type === "tool-call" && p.toolCallId === "bad-0"));
        assert(calls.some(p => p.type === "tool-call" && p.toolCallId === "effect"), "successful sibling retained");
        const text = JSON.stringify(request.prompt);
        assert(text.includes("effect already completed"), "effects must not be erased for repair");
        if (index === 1 || !repaired) {
          assert(text.includes("rejectedInput"));
          assert(text.includes("line 4, column 1"));
        } else assert(!text.includes("rejectedInput"), "repaired call leaves temporary repair context");
      }
      return { stream: new ReadableStream({ start(controller) {
        const emit = (toolName: string, toolCallId: string, input: unknown) => controller.enqueue({ type: "tool-call", toolName, toolCallId, input: JSON.stringify(input) });
        if (index === 0) emit("effect", "effect", {});
        if (index === 1) emit("effect", "forbidden-repair-effect", {});
        if (repaired && index === 2) emit("complete_task", "done", {});
        else emit("plasm", `bad-${index}`, { logical_session_ref: ref, program: repaired && index > 0 ? valid : malformed });
        controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined }, usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 2, text: 2, reasoning: 0 } } });
        controller.close();
      } }) };
    } });
    const result = await runEveToolLoop({ model, system: "Abstract repair fixture", messages: [{ role: "user", content: "Read abstract records" }],
      tools: { ...createPlasmTools(runtime), ...createEvalTerminalTools(), effect: tool({ inputSchema: z.object({}), execute: async () => { effects++; return "effect already completed"; } }) },
      maxSteps: 12, agentName: "repair-fixture", telemetry: { isEnabled: false } });
    assert.equal(requests, 3, `initial plus repair/terminal; stop=${result.stopReason}, failure=${JSON.stringify(result.executionFailure)}`);
    assert.equal(result.stopReason, repaired ? "completed" : "repair_exhausted");
    assert.equal(result.usage.inputTokens, 24);
    assert.equal(result.usage.outputTokens, 6);
    assert.equal(effects, 1);
    assert.equal(submitted[0], malformed, "source is never truncated or rewritten");
    assert.equal(result.toolCallRecovery.at(-1)?.state, repaired ? "recovered" : "exhausted");
    assert(result.messages.some(m => m.role === "assistant" && Array.isArray(m.content)
      && m.content.some(p => p.type === "tool-call" && p.toolCallId === "bad-0")), "audit retains original call");
  });
}
// Prose and service failures cannot grant malformed-source repair authority.
for (const output of ["needs_fix · parse", { status: "execution_failed", summary: "program_syntax", failure: { ...failure, cause: "upstream", recovery: "reconcile_effects" } }]) {
  const recovery = new ToolCallRecovery();
  recovery.observe(0, [{ toolCallId: "x", toolName: "plasm", input: {} }], [{ toolCallId: "x", toolName: "plasm", output }]);
  assert.equal(recovery.activeToolNames, undefined);
}
console.log("PASS: typed admission repair, bounded exhaustion, immutable source/audit, separate model context, preserved effects and exact usage");
