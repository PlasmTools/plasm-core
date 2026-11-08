/** Offline native-to-host counterexamples. Discovery is deterministic fixture routing;
 * Python admission, planning, execution, transport, runtime and terminals are real.
 *
 * Generate the packed fixture by running the Rust witness with
 * PLASM_EFFECT_FIXTURE_DIR=<absolute-directory>. Build plasm-node and the vendored
 * Monty worker, then set NAPI_RS_NATIVE_LIBRARY_PATH, PLASM_MONTY_BINARY and
 * PLASM_EFFECT_FIXTURE_MANIFEST before running this script via run-plasm-cli.mjs.
 * Use the plasm-oss build output for the NAPI library, not the parent workspace's
 * potentially stale target directory. On macOS, copy its .dylib to a .node path.
 * Fixture generation: PLASM_EFFECT_FIXTURE_DIR=/tmp/plasm-obligation-fixture cargo test
 * -p plasm-agent-core --lib known_rejection_leaves_the_selected_suffix_unattempted
 * (also set PLASM_MONTY_BINARY to the current vendored worker for that command).
 * For the positive repair witness, generate a second fixture with
 * PLASM_EFFECT_FIXTURE_ENGINE_AUTH=1, point the manifest at it, and pass --repair.
 * Optional flags: --loop, --answer, --repeat-rejection, --duplicates,
 * --two-obligations, --unknown, --cancel, --host-auth, --auth-change and --auth-loss exercise distinct offline boundaries.
 * --observe prints all cases without failing; the default fails on any violation.
 */
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { MockLanguageModelV3 } from "ai/test";
import { loadPackedCatalog } from "../src/catalog/loader.js";
import { NapiPlasmEngine, type HostTransportFn, type PlasmEngine } from "../src/engine/napi-binding.js";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import { AgentExecutionFailure, type ExecutionFailure } from "../src/runtime/execution-failure.js";
import { routingPacketSchema } from "../src/engine/routing.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

const manifest = process.env.PLASM_EFFECT_FIXTURE_MANIFEST;
if (!manifest) throw new Error("Set PLASM_EFFECT_FIXTURE_MANIFEST to the offline fixture manifest");
const catalog = await loadPackedCatalog(manifest);
const allCases = [
  {name: "failure_then_complete", terminal: "complete_task", intervening: "none"},
  {name: "failure_then_answer", terminal: "submit_answer", intervening: "none"},
  {name: "failure_then_unrelated_read_then_complete", terminal: "complete_task", intervening: "read"},
  {name: "failure_then_unrelated_write_then_complete", terminal: "complete_task", intervening: "write"},
  {name: "write_while_original_is_in_flight", terminal: "complete_task", intervening: "inflight"},
  {name: "native_repair_original_selection", terminal: "complete_task", intervening: "repair"},
] as const;
const cases = allCases.filter(scenario => process.argv.includes("--repair") ? scenario.intervening === "repair" : scenario.intervening !== "repair");

let exposed = 0;
for (const scenario of cases) {
  const root = await mkdtemp(path.join(tmpdir(), "plasm-effect-native-"));
  const native = new NapiPlasmEngine();
  let runtime: AgentRuntime | undefined;
  const source = [0, 1, 2, 3].map(i => ({expense_id: `source-${i}`, group_id: "source", description: `item-${i}`}));
  if (scenario.intervening === "repair" && process.argv.includes("--duplicates")) {
    for (let i = 0; i < source.length; i++) source[i] = {expense_id: "source-0", group_id: "source", description: "same-value"};
  }
  const rows: Array<{expense_id: string; group_id: string; description: string}> = [];
  let posts = 0;
  let reads = 0;
  let runConcurrent: (() => Promise<void>) | undefined;
  let hostAuth: string | null = "Bearer host-fixture-one";
  const transport: HostTransportFn = async request => {
    if (request.method === "GET") {
      reads++;
      return {status: 200, body: JSON.stringify(request.url.includes("/source/") ? source : rows)};
    }
    assert.equal(request.method, "POST");
    const body = JSON.parse(request.body ?? "null") as {group_id: string; description: string};
    if (process.argv.includes("--host-auth")) {
      assert.equal(Object.entries(request.headers ?? {}).find(([name]) => name.toLowerCase() === "authorization")?.[1], "Bearer host-fixture-one");
    }
    const attempt = posts++;
    if (attempt === 1 || (scenario.intervening === "repair" && process.argv.includes("--repeat-rejection") && attempt === 3)
      || (scenario.intervening === "repair" && process.argv.includes("--two-obligations") && attempt === 2)) {
      if (scenario.intervening === "inflight") {
        assert.ok(runConcurrent);
        await Promise.race([
          runConcurrent(),
          new Promise<never>((_, reject) => setTimeout(() => reject(new Error("concurrent write blocked by original run")), 5000)),
        ]);
      }
      return process.argv.includes("--unknown") ? {status: 503, body: "ambiguous fixture failure"} : {status: 422, body: "fixture 422"};
    }
    const row = {expense_id: `created-${attempt}`, group_id: body.group_id, description: body.description};
    rows.push(row);
    return {status: 200, body: JSON.stringify(row)};
  };
  if (process.argv.includes("--host-auth")) transport.resolveAuth = async () => hostAuth;
  try {
    await native.loadCatalog(catalog);
    const engine: PlasmEngine = {
      sealEffectWorkflows: () => native.sealEffectWorkflows([""]),
      resumeEffectRun: ref => native.resumeEffectRun(ref),
      close: () => native.close(), loadCatalog: c => native.loadCatalog(c),
      synthesizeTeaching: (intent, seeds) => native.synthesizeTeaching(intent, seeds),
      activateDiscovery: (deployment, bindings) => native.activateDiscovery(deployment, bindings),
      introspectCatalog: entry => native.introspectCatalog(entry),
      takeCompletedTeaching: () => Promise.resolve(""),
      // The fixture routes discovery deterministically. The native default engine
      // owns the actual execute session; the runtime's ref aliases that same engine.
      dryRun: program => native.dryRun(program),
      runPlan: ref => native.runPlan(ref),
      runPlanLive: (ref, callback) => native.runPlanLive(ref, callback),
      run: (plan, callback) => native.run(plan, callback),
      async routeIntent(provenance) {
        const intent = provenance.nodes.at(-1)!.intent;
        const exposure = await native.synthesizeTeaching(intent, [{api: "matrix", entity: "PwExpense"}]);
        return routingPacketSchema.parse({routing: {
          intent_provenance: provenance, intent, pin_id: randomUUID(),
          authorization: {catalogs: ["matrix"], capabilities: {}},
          retrieval: {generation: "fixture", candidates: [{id: "matrix:pwexpense_query",
            reference: {catalog: "matrix", capability: "pwexpense_query"}, document: {entity: "PwExpense"}}]},
          matching: {matches: [{capability_id: "matrix:pwexpense_query", choice: "relevant"}]},
          search: {policy: "additive-connected-frontier-current-hybrid-evidence-3-judgments-3-attempts-16000-byte-packet-v37",
            exposed: [], considered: [{catalog: "matrix", capability: "pwexpense_query"}], unexplored: [], deferred: [],
            requests: 1, input_bytes: 1, reported_input_tokens: 1, missing_usage: 0},
          recovery: null, closure: {business: [{catalog: "matrix", capability: "pwexpense_query"}],
            input_sources: [], prerequisites: [], acquisitions: [], edges: []},
        }, teaching: {prompt: exposure.prompt, declarations_changed: true, delta_refs: exposure.deltaRefs}});
      },
    };
    runtime = new AgentRuntime({agentRoot: root, engine, archive: null, hostTransport: transport});
    const opened = await runtime.plasmContext({intent: "Copy selected records"});
    const session = opened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(session, "workflow ref");
    const entity = "e1";
    const create = "m1";
    const original = `class Copy(Program):\n    def build(self):\n        source = ${entity}.query(group_id="source")\n        return source.flat_map(lambda row: ${entity}.${create}(group_id="dest", description=row.description))\n`;
    const write = `class Other(Program):\n    def build(self):\n        return ${entity}.${create}(group_id="other", description="unrelated")\n`;
    const read = `class Read(Program):\n    def build(self):\n        return ${entity}.query(group_id="other")\n`;
    async function plan(program: string): Promise<string> {
      const result = await runtime!.plasm({logicalSessionRef: session!, program});
      const ref = result.match(/\*\*Run:\*\* pass `run_ref`: `([^`]+)`/)?.[1];
      assert.ok(ref, `reviewed plan missing: ${result}`);
      return ref;
    }
    const originalRef = await plan(original);
    const unrelatedRef = scenario.intervening === "write" || scenario.intervening === "inflight" || scenario.intervening === "repair" ? await plan(write) : undefined;
    const recorded: ExecutionFailure[] = [];
    const live = runtime;
    if (scenario.intervening === "repair") {
      if (process.argv.includes("--loop")) {
        let step = 0;
        const model = new MockLanguageModelV3({doStream: async request => {
          const current = step++;
          if (current === 2) for (const row of source) row.description = "changed-after-selection";
          const continuation = JSON.stringify(request.prompt).match(/continuation_ref=([0-9a-f-]{36})/)?.[1];
          if (current === 2) assert.ok(continuation, "checkpoint must reach the model as a usable reference");
          const name = current < 2 ? "plasm_run" : current === 2 ? "plasm_resume" : "complete_task";
          const input = current < 2 ? {logical_session_ref: session, run_ref: current === 0 ? originalRef : unrelatedRef}
            : current === 2 ? {logical_session_ref: session, continuation_ref: continuation} : {};
          return {stream: new ReadableStream({start(controller) {
            controller.enqueue({type: "tool-call", toolCallId: `repair-${current}`, toolName: name, input: JSON.stringify(input)});
            controller.enqueue({type: "finish", finishReason: {unified: "tool-calls", raw: undefined},
              usage: {inputTokens: {total: 0, noCache: 0, cacheRead: 0, cacheWrite: 0}, outputTokens: {total: 0, text: 0, reasoning: 0}}});
            controller.close();
          }})};
        }});
        const result = await runEveToolLoop({model, system: "Native repair fixture",
          messages: [{role: "user", content: "Copy the selected records"}],
          tools: {...createPlasmTools(live), ...createHarnessTools({includeEvalTerminals: true,
            discoveryCompleted: () => live.hasOpenWorkflow(), sealEffectWorkflows: () => live.sealEffectWorkflows(),
            responseObligation: {resolve: async () => "no_answer"}})},
          maxSteps: 4, agentName: "native-repair-loop", telemetry: {isEnabled: false}});
        assert.equal(result.stopReason, "completed");
        assert.deepEqual(rows.map(row => row.description), ["item-0", "unrelated", "item-1", "item-2", "item-3"]);
        assert.equal(posts, 6);
        console.log(JSON.stringify({case: "native_repair_agent_loop", contract: "certified", posts, reads}));
        continue;
      }
      let continuation: string | undefined;
      try { await live.plasmRun({logicalSessionRef: session, runRef: originalRef}); }
      catch (error) {
        assert.ok(error instanceof AgentExecutionFailure);
        assert.ok(!error.summary.includes("Bearer fixture"), "checkpoint presentation must not expose authentication");
        if (process.argv.includes("--unknown")) assert.equal(error.failure.effects_unresolved, true);
        continuation = error.summary.match(/continuation_ref=([0-9a-f-]{36})/)?.[1];
      }
      if (process.argv.includes("--unknown")) {
        assert.equal(continuation, undefined, "unknown effects cannot grant a resume capability");
        assert.equal(posts, 2);
        await assert.rejects(live.sealEffectWorkflows(), /unfinished or uncertain effects/);
        console.log(JSON.stringify({case: "native_unknown_dispatch", contract: "unresolved", posts, reads}));
        continue;
      }
      assert.ok(continuation, "native checkpoint must expose its opaque continuation");
      assert.deepEqual(rows.map(row => row.description), [process.argv.includes("--duplicates") ? "same-value" : "item-0"]);
      await assert.rejects(live.plasmResume({logicalSessionRef: "wrong-workflow", continuationRef: continuation}), /unknown or belongs/);
      await assert.rejects(native.resumeEffectRun(continuation, "wrong-workflow"));
      assert.equal(posts, 2, "foreign workflow cannot consume or dispatch the checkpoint");
      if (process.argv.includes("--auth-change") || process.argv.includes("--auth-loss")) {
        hostAuth = process.argv.includes("--auth-loss") ? null : "Bearer host-fixture-two";
        await assert.rejects(live.plasmResume({logicalSessionRef: session, continuationRef: continuation}), error => {
          assert.ok(error instanceof AgentExecutionFailure);
          assert.equal(error.failure.code, hostAuth === null ? "continuation_auth_revalidation_failed" : "continuation_auth_scope_changed");
          assert.ok(!JSON.stringify(error.failure).includes("host-fixture"));
          return true;
        });
        assert.equal(posts, 2, "changed authority must not dispatch the rejected operation");
        await assert.rejects(live.sealEffectWorkflows(), /unfinished or uncertain effects/);
        console.log(JSON.stringify({case: "native_changed_host_auth", contract: "unresolved", posts, reads}));
        continue;
      }
      if (process.argv.includes("--cancel")) {
        await Promise.race([
          native.close(),
          new Promise<never>((_, reject) => setTimeout(() => reject(new Error("closing did not cancel the paused worker")), 5000)),
        ]);
        assert.equal(posts, 2, "closing a paused run cannot retry the rejected write");
        assert.deepEqual(rows.map(row => row.description), ["item-0"]);
        console.log(JSON.stringify({case: "native_paused_close", contract: "cancelled", posts, reads}));
        continue;
      }
      let secondContinuation: string | undefined;
      if (process.argv.includes("--two-obligations")) {
        try { await live.plasmRun({logicalSessionRef: session, runRef: unrelatedRef!}); }
        catch (error) {
          assert.ok(error instanceof AgentExecutionFailure);
          secondContinuation = error.summary.match(/continuation_ref=([0-9a-f-]{36})/)?.[1];
        }
        assert.ok(secondContinuation, "second reviewed mutation must retain its own checkpoint");
      } else {
        await live.plasmRun({logicalSessionRef: session, runRef: unrelatedRef!});
      }
      for (const row of source) row.description = "changed-after-selection";
      if (process.argv.includes("--repeat-rejection")) {
        let next: string | undefined;
        try { await live.plasmResume({logicalSessionRef: session, continuationRef: continuation}); }
        catch (error) {
          assert.ok(error instanceof AgentExecutionFailure);
          next = error.summary.match(/continuation_ref=([0-9a-f-]{36})/)?.[1];
        }
        assert.ok(next);
        assert.notEqual(next, continuation, "each checkpoint has a fresh one-use reference");
        await assert.rejects(live.plasmResume({logicalSessionRef: session, continuationRef: continuation}), /unknown or belongs/);
        continuation = next;
      }
      const resumeTool = createPlasmTools(live).plasm_resume?.execute;
      assert.ok(resumeTool);
      const resumed = await resumeTool({logical_session_ref: session, continuation_ref: continuation}, {toolCallId: "native-repair", messages: [], context: {}});
      assert.equal(typeof resumed, "string", "the public repair tool must deliver the resumed result");
      const originals = process.argv.includes("--duplicates") ? ["same-value", "same-value", "same-value", "same-value"] : ["item-0", "item-1", "item-2", "item-3"];
      assert.deepEqual(rows.map(row => row.description), process.argv.includes("--two-obligations")
        ? originals : [originals[0], "unrelated", ...originals.slice(1)]);
      assert.equal(posts, process.argv.includes("--repeat-rejection") ? 7 : 6);
      await assert.rejects(live.plasmResume({logicalSessionRef: session, continuationRef: continuation}), /unknown or belongs/);
      assert.equal(posts, process.argv.includes("--repeat-rejection") ? 7 : 6, "replaying a consumed continuation cannot dispatch again");
      if (process.argv.includes("--two-obligations")) {
        await assert.rejects(live.sealEffectWorkflows(), /unfinished or uncertain effects/);
        await live.plasmResume({logicalSessionRef: session, continuationRef: secondContinuation!});
        assert.equal(posts, 7);
        assert.equal(rows.at(-1)?.description, "unrelated");
      }
      const terminalName = process.argv.includes("--answer") ? "submit_answer" : "complete_task";
      const terminals = createHarnessTools({includeEvalTerminals: true,
        discoveryCompleted: () => live.hasOpenWorkflow(), sealEffectWorkflows: () => live.sealEffectWorkflows(),
        responseObligation: {resolve: async () => terminalName === "submit_answer" ? "value_required" : "no_answer"}});
      const terminalModel = new MockLanguageModelV3({doStream: async () => ({stream: new ReadableStream({start(controller) {
        controller.enqueue({type: "tool-call", toolCallId: "certified-terminal", toolName: terminalName,
          input: JSON.stringify(terminalName === "submit_answer" ? {answer: "42"} : {})});
        controller.enqueue({type: "finish", finishReason: {unified: "tool-calls", raw: undefined},
          usage: {inputTokens: {total: 0, noCache: 0, cacheRead: 0, cacheWrite: 0}, outputTokens: {total: 0, text: 0, reasoning: 0}}});
        controller.close();
      }})})});
      const terminalResult = await runEveToolLoop({model: terminalModel, system: "Certified repair fixture",
        messages: [{role: "user", content: "Complete the reviewed copy"}], tools: terminals,
        maxSteps: 1, agentName: "certified-native-repair", telemetry: {isEnabled: false}});
      assert.equal(terminalResult.stopReason, "completed", "native certificate permits the requested terminal");
      console.log(JSON.stringify({case: scenario.name, contract: "certified", posts, reads}));
      continue;
    }
    runConcurrent = async () => { await live.plasmRun({logicalSessionRef: session, runRef: unrelatedRef!}); };
    const tools = createPlasmTools({plasmContext: live.plasmContext.bind(live), plasm: live.plasm.bind(live),
      plasmRun: live.plasmRun.bind(live), plasmResume: live.plasmResume.bind(live), recordExecutionFailure: async (name, failure) => {
        recorded.push(failure);
        await live.recordExecutionFailure(name, failure);
      }});
    let step = 0;
    let readsBeforeIntervention = 0;
    const model = new MockLanguageModelV3({doStream: async () => {
      const current = step++;
      if (scenario.intervening === "read" && current === 1) readsBeforeIntervention = reads;
      const readPlan = scenario.intervening === "read" && current === 2 && reads === readsBeforeIntervention
        ? (await live.sessionManager.getByLogicalRef(session))?.planCommits.at(-1)?.ref : undefined;
      const name = current === 0 ? "plasm_run"
        : scenario.intervening === "read" && current === 1 ? "plasm"
        : readPlan ? "plasm_run"
        : scenario.intervening === "write" && current === 1 ? "plasm_run" : scenario.terminal;
      const input = name === "plasm_run" ? {logical_session_ref: session, run_ref: current === 0 ? originalRef : readPlan ?? unrelatedRef}
        : name === "plasm" ? {logical_session_ref: session, program: read}
        : name === "submit_answer" ? {answer: "42"} : {};
      return {stream: new ReadableStream({start(controller) {
        controller.enqueue({type: "tool-call", toolCallId: `call-${current}`, toolName: name, input: JSON.stringify(input)});
        controller.enqueue({type: "finish", finishReason: {unified: "tool-calls", raw: undefined},
          usage: {inputTokens: {total: 0, noCache: 0, cacheRead: 0, cacheWrite: 0}, outputTokens: {total: 0, text: 0, reasoning: 0}}});
        controller.close();
      }})};
    }});
    const result = await runEveToolLoop({model, system: "Abstract completion contract",
      messages: [{role: "user", content: "Copy the selected records"}],
      tools: {...tools, ...createHarnessTools({includeEvalTerminals: true, discoveryCompleted: () => live.hasOpenWorkflow(), sealEffectWorkflows: () => live.sealEffectWorkflows(),
        responseObligation: {resolve: async () => scenario.terminal === "submit_answer" ? "value_required" : "no_answer"}})},
      maxSteps: scenario.intervening === "none" ? 2 : scenario.intervening === "read" ? 4 : 3,
      agentName: "native-completion-contract", telemetry: {isEnabled: false}});
    assert.equal(recorded.length, 1, "the native failure must be recorded exactly once");
    assert.equal(recorded[0]!.effects_unresolved, false);
    assert.ok(recorded[0]!.dispatches.some(d => d.status === "rejected_without_effect"));
    assert.ok(recorded[0]!.effect_selections?.some(selection => selection.count === 4),
      "failure carries native accounting for the original selection");
    assert.equal(rows[0]?.description, "item-0");
    assert.ok(rows.every(row => !["item-1", "item-2", "item-3"].includes(row.description)));
    assert.equal(posts, scenario.intervening === "write" || scenario.intervening === "inflight" ? 3 : 2);
    if (scenario.intervening === "read") assert.ok(reads > readsBeforeIntervention, "unrelated read reaches transport after failure");
    if (scenario.intervening === "write" || scenario.intervening === "inflight") assert.equal(rows.at(-1)?.description, "unrelated");
    const violated = result.stopReason === "completed";
    exposed += Number(violated);
    console.log(JSON.stringify({case: scenario.name, stopReason: result.stopReason,
      contract: violated ? "VIOLATED" : "not_completed", posts, reads}));
  } finally {
    if (runtime) await runtime.close(); else await native.close();
    await rm(root, {recursive: true, force: true});
  }
}
if (!process.argv.includes("--observe")) assert.equal(exposed, 0, "unfinished native selections must not complete");
