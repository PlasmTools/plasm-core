import assert from "node:assert/strict";
import { MockLanguageModelV3 } from "ai/test";
import { asSchema } from "ai";
import { checkSessionExtension } from "./test-session-extension.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { terminalExecutionFailure } from "../src/runtime/execution-failure.js";
import { newEphemeralLogicalSession } from "../src/runtime/logical-session.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

await checkSessionExtension(async () => {}, async (runtime, ref, engine) => {
  let admissions = 0;
  engine.dryRun = async () => {
    admissions++;
    return { writeCount: 0, planCommitRef: "pc1", summary: "corrected reference admitted", fusedCleanRead: false };
  };
  const tools = createPlasmTools(runtime);
  for (const name of ["plasm_context", "plasm", "plasm_run"] as const) {
    const schema = asSchema(tools[name]!.inputSchema);
    assert.ok(schema.validate);
    const args = name === "plasm_context" ? { intent: "Read abstract records", session_mode: "extend" }
      : name === "plasm" ? { program: "fixture" } : { run_ref: "pc1" };
    assert.equal((await schema.validate({ ...args, logical_session_ref: ref })).success, true);
    assert.equal((await schema.validate({ ...args, logical_session_ref: ref.slice(0, -1) })).success, false);
    assert.ok(JSON.stringify(await schema.jsonSchema).includes('"pattern"'), "model sees the canonical reference shape");
  }
  const options = { toolCallId: "fixture", messages: [], context: {} };
  for (const invalid of ["for", ref.slice(0, -1), newEphemeralLogicalSession().logicalSessionRef]) {
    for (const name of ["plasm_context", "plasm", "plasm_run"] as const) {
      const args = name === "plasm_context"
        ? { intent: "Read abstract records", session_mode: "extend", logical_session_ref: invalid }
        : name === "plasm" ? { logical_session_ref: invalid, program: "fixture" }
        : { logical_session_ref: invalid, run_ref: "pc1" };
      const observed = await tools[name]!.execute!(args, options);
      assert.equal(typeof observed, "object");
      const value = observed as { failure: { cause: string; recovery: string; effects_unresolved: boolean; diagnostic: string } };
      assert.equal(value.failure.cause, "program");
      assert.equal(value.failure.recovery, "repair_program");
      assert.equal(value.failure.effects_unresolved, false);
      assert.ok(value.failure.diagnostic.includes(ref), "correction supplies the open workflow ref verbatim");
      assert.equal(terminalExecutionFailure([{ role: "tool", content: [{ toolName: name, output: { type: "json", value: observed } }] }]), undefined);
    }
  }
  assert.equal(admissions, 0, "bad handles never reach the engine");
  for (const run_ref of ["????", "", "pr" + "0".repeat(64)]) {
    const observed = await tools.plasm_run!.execute!({ logical_session_ref: ref, run_ref }, options) as { failure: { code: string; recovery: string; effects_unresolved: boolean } };
    assert.equal(observed.failure.code, "invalid_run_reference");
    assert.equal(observed.failure.recovery, "repair_program");
    assert.equal(observed.failure.effects_unresolved, false);
    assert.equal(terminalExecutionFailure([{ role: "tool", content: [{ toolName: "plasm_run", output: { type: "json", value: observed } }] }]), undefined);
  }
  let requests = 0;
  const model = new MockLanguageModelV3({ doStream: async () => {
    requests++;
    const input = { logical_session_ref: requests === 1 ? ref.slice(0, -1) : ref, program: "fixture" };
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: "tool-call", toolCallId: `call-${requests}`, toolName: "plasm", input: JSON.stringify(input) });
      controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined }, usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 2, text: 2, reasoning: 0 } } });
      controller.close();
    } }) };
  } });
  const result = await runEveToolLoop({ model, system: "Abstract recovery fixture", messages: [{ role: "user", content: "Read abstract records" }], tools, maxSteps: 2, agentName: "session-input-recovery", telemetry: { isEnabled: false } });
  assert.equal(requests, 2, "invalid input buys a bounded correction generation");
  assert.equal(admissions, 1, "only the corrected call reaches the engine");
  assert.notEqual(result.stopReason, "execution_failed");
  let runAdmissions = 0;
  engine.runPlan = async () => {
    runAdmissions++;
    return { ok: false, message: "corrected reference validated" };
  };
  let runRequests = 0;
  const runModel = new MockLanguageModelV3({ doStream: async () => {
    runRequests++;
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: "tool-call", toolCallId: `run-${runRequests}`, toolName: "plasm_run", input: JSON.stringify({ logical_session_ref: ref, run_ref: runRequests === 1 ? "????" : "pc1" }) });
      controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined }, usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 2, text: 2, reasoning: 0 } } });
      controller.close();
    } }) };
  } });
  const repairedRun = await runEveToolLoop({ model: runModel, system: "Abstract run admission fixture", messages: [{ role: "user", content: "Run the reviewed plan" }], tools, maxSteps: 2, agentName: "run-input-recovery", telemetry: { isEnabled: false } });
  assert.equal(runRequests, 2);
  assert.equal(runAdmissions, 1, "only the corrected run reference reaches native admission");
  assert.notEqual(repairedRun.stopReason, "execution_failed");
  let policyRequests = 0;
  let staleAdmissions = 0;
  let freshAdmissions = 0;
  engine.runPlan = async (runRef) => {
    if (runRef === "pc1") {
      staleAdmissions++;
      return { ok: false, message: "review again", failureJson: JSON.stringify({
        cause:"program", recovery:"repair_program", code:"plan_commit_stale_policy",
        diagnostic:"The policy changed; review the program again before execution.",
        node:null, occurrence_path:[], catalog_digest:null, effects:[], dispatches:[], effects_unresolved:false,
      }) };
    }
    assert.equal(runRef,"pc2"); freshAdmissions++;
    return {ok:false,message:"fresh reviewed reference admitted"};
  };
  engine.dryRun = async () => ({writeCount:0,planCommitRef:"pc2",summary:"fresh policy review",fusedCleanRead:false});
  const policyModel = new MockLanguageModelV3({ doStream: async () => {
    policyRequests++;
    const review = policyRequests === 2;
    const input = review ? {logical_session_ref:ref,program:"fixture"} : {logical_session_ref:ref,run_ref:policyRequests===1?"pc1":"pc2"};
    return {stream:new ReadableStream({start(controller) {
      controller.enqueue({type:"tool-call",toolCallId:`policy-${policyRequests}`,toolName:review?"plasm":"plasm_run",input:JSON.stringify(input)});
      controller.enqueue({type:"finish",finishReason:{unified:"tool-calls",raw:undefined},usage:{inputTokens:{total:8,noCache:8,cacheRead:0,cacheWrite:0},outputTokens:{total:2,text:2,reasoning:0}}});
      controller.close();
    }})};
  }});
  const policyRun = await runEveToolLoop({model:policyModel,system:"Abstract policy recovery fixture",messages:[{role:"user",content:"Execute after current policy review"}],tools,maxSteps:3,agentName:"policy-input-recovery",telemetry:{isEnabled:false}});
  assert.equal(policyRequests,3); assert.equal(staleAdmissions,1); assert.equal(freshAdmissions,1);
  assert.notEqual(policyRun.stopReason,"execution_failed");
});
console.log("PASS: malformed and unknown session refs are effect-free corrections; the model loop admits a corrected call");
