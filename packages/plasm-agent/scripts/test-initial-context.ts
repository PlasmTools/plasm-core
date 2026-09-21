import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { MockLanguageModelV3 } from "ai/test";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";

const unused = async (): Promise<never> => { throw new Error("unexpected engine call"); };
const engine: PlasmEngine = {
  loadCatalog: unused, activateDiscovery: unused, routeIntent: unused,
  synthesizeTeaching: unused, dryRun: unused, runPlan: unused, run: unused,
  introspectCatalog: unused,
};
const root = await mkdtemp(path.join(tmpdir(), "plasm-initial-context-"));
try {
  for (const outcome of ["ready: canonical teaching", "insufficient: refine acquisition slots"]) {
    let opened = false;
    let initialized = 0;
    const task = "  Keep the original scope, punctuation, and spacing.  ";
    const context = `${outcome}; logical_session_ref: fixture-session`;
    const model = new MockLanguageModelV3({ doStream: async (request) => {
      assert.equal(opened, true, "context must exist before the first generation");
      assert.notEqual(request.toolChoice?.type, "tool", "initial discovery is never forced on the provider");
      assert.ok(JSON.stringify(request.prompt).includes(context));
      return { stream: new ReadableStream({ start(controller) {
        controller.enqueue({type: "text-start", id: "text"});
        controller.enqueue({type: "text-delta", id: "text", delta: "clarification"});
        controller.enqueue({type: "text-end", id: "text"});
        controller.enqueue({type: "finish", finishReason: {unified: "stop", raw: undefined}, usage: {
          inputTokens: {total: 10, noCache: 10, cacheRead: 0, cacheWrite: 0},
          outputTokens: {total: 2, text: 2, reasoning: 0},
        }});
        controller.close();
      }}) };
    }});
    const agent = new PlasmAgent({agentRoot: root, model, engine, hostTransport: null, archiveEnabled: false, telemetry: false, includeEvalTerminals: true});
    agent.runtime.hasOpenWorkflow = () => opened;
    agent.runtime.plasmContext = async (input) => {
      initialized++;
      assert.deepEqual(input, {intent: task, effectSlots: [task], sessionMode: "new", logicalSessionRef: undefined});
      opened = true;
      return context;
    };
    let observed = 0;
    const first = await agent.generate(task, {wrapTools: (tools) => {
      const contextTool = tools.plasm_context!;
      const execute = contextTool.execute!;
      return {...tools, plasm_context: {...contextTool, execute: async (input, options) => {
        observed++;
        assert.equal(options.toolCallId, "host-initial-context");
        return execute(input, options);
      }}};
    }});
    assert.equal(observed, 1, "host initialization preserves the normal observer path");
    assert.equal(initialized, 1);
    assert.equal(model.doStreamCalls.length, 1);
    assert.equal(first.toolCount, 0, "host bootstrap is not a model-authored tool call");
    assert.equal(first.stopReason, "unterminated");
    await agent.generate("Follow up in the same workflow");
    assert.equal(initialized, 1, "continuation does not create another root");
    assert.equal((JSON.stringify(model.doStreamCalls[1]!.prompt).match(/Host initialized plasm_context/g) ?? []).length, 1);
  }
  const model = new MockLanguageModelV3({doStream: unused});
  const failing = new PlasmAgent({agentRoot: root, model, engine, hostTransport: null, archiveEnabled: false, telemetry: false});
  failing.runtime.plasmContext = async () => { throw new Error("bootstrap unavailable"); };
  await assert.rejects(failing.generate("Task"), /bootstrap unavailable/);
  assert.equal(model.doStreamCalls.length, 0, "bootstrap failure must not fall through to model discovery");
  console.log("PASS: deterministic initial context, exact intent, ready/insufficient continuity, no forced call, failure before generation");
} finally { await rm(root, {recursive: true, force: true}); }
