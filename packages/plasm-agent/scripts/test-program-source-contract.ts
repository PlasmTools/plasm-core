import assert from "node:assert/strict";
import { asSchema } from "ai";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { loadPromptAsset } from "../src/prompts/index.js";

const received: string[] = [];
const tools = createPlasmTools({
  recordExecutionFailure: async () => {},
  plasmContext: async () => "context",
  plasm: async (input) => { received.push(input.program); return "planned"; },
  plasmRun: async () => "executed",
});
const source = 'class P(Program):\n    """Explain the workflow."""\n    def build(self):\n        # Valid Python commentary\n        return "</parameter>"';
const session = "l_AAAAAAAAQACAAAAAAAAAAQ";
for (const name of ["plasm", "plasm_run"] as const) {
  const definition = tools[name]!;
  const schema = asSchema(definition.inputSchema);
  const args = name === "plasm" ? { logical_session_ref: session, program: source } : { logical_session_ref: session, run_ref: "pc0" };
  assert.equal((await schema.validate!(args)).success, true);
  assert.equal((await schema.validate!({ ...args, reasoning: "explain" })).success, false);
  const json = await schema.jsonSchema;
  assert.equal(Object.hasOwn(json.properties ?? {}, "reasoning"), false);
  if (name === "plasm") {
    assert.equal(json.properties?.program?.description, loadPromptAsset("program_param.txt"));
    await definition.execute!(args, { toolCallId: "contract", messages: [], context: {} });
  }
}
assert.deepEqual(received, [source]);
console.log("program source contract passed: no reasoning field; literate source unchanged");
