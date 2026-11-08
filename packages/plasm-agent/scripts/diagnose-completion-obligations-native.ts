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
import type { ExecutionFailure } from "../src/runtime/execution-failure.js";
import { routingPacketSchema } from "../src/engine/routing.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

const manifest = process.env.PLASM_EFFECT_FIXTURE_MANIFEST;
if (!manifest) throw new Error("Set PLASM_EFFECT_FIXTURE_MANIFEST to the offline fixture manifest");
const catalog = await loadPackedCatalog(manifest);
const cases = [
  {name: "failure_then_complete", terminal: "complete_task", intervening: "none"},
  {name: "failure_then_answer", terminal: "submit_answer", intervening: "none"},
  {name: "failure_then_unrelated_read_then_complete", terminal: "complete_task", intervening: "read"},
  {name: "failure_then_unrelated_write_then_complete", terminal: "complete_task", intervening: "write"},
] as const;

let exposed = 0;
for (const scenario of cases) {
  const root = await mkdtemp(path.join(tmpdir(), "plasm-effect-native-"));
  const native = new NapiPlasmEngine();
  let runtime: AgentRuntime | undefined;
  const source = [0, 1, 2, 3].map(i => ({expense_id: `source-${i}`, group_id: "source", description: `item-${i}`}));
  const rows: Array<{expense_id: string; group_id: string; description: string}> = [];
  let posts = 0;
  let reads = 0;
  const transport: HostTransportFn = async request => {
    if (request.method === "GET") {
      reads++;
      return {status: 200, body: JSON.stringify(request.url.includes("/source/") ? source : rows)};
    }
    assert.equal(request.method, "POST");
    const body = JSON.parse(request.body ?? "null") as {group_id: string; description: string};
    const attempt = posts++;
    if (attempt === 1) return {status: 422, body: "fixture 422"};
    const row = {expense_id: `created-${attempt}`, group_id: body.group_id, description: body.description};
    rows.push(row);
    return {status: 200, body: JSON.stringify(row)};
  };
  try {
    await native.loadCatalog(catalog);
    const engine: PlasmEngine = {
      sealEffectWorkflows: () => native.sealEffectWorkflows([""]),
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
    const unrelatedRef = scenario.intervening === "write" ? await plan(write) : undefined;
    const recorded: ExecutionFailure[] = [];
    const live = runtime;
    const tools = createPlasmTools({plasmContext: live.plasmContext.bind(live), plasm: live.plasm.bind(live),
      plasmRun: live.plasmRun.bind(live), recordExecutionFailure: async (name, failure) => {
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
    assert.equal(posts, scenario.intervening === "write" ? 3 : 2);
    if (scenario.intervening === "read") assert.ok(reads > readsBeforeIntervention, "unrelated read reaches transport after failure");
    if (scenario.intervening === "write") assert.equal(rows.at(-1)?.description, "unrelated");
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
