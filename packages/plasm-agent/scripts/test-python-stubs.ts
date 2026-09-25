import assert from "node:assert/strict";
import ts from "typescript";
import { spawnSync } from "node:child_process";
import { plasmLiteral, buildDottedArgs, executeRows } from "../src/stubs/catalog-client.js";
import { renderProgramStatements } from "../src/stubs/plasm-value-emitter.js";
import { assignCapabilityBindings } from "../src/stubs/stub-symbols.js";
import { renderCapabilityInputType } from "../src/stubs/input-type-to-ts.js";
import type { CatalogIntrospectionJson, CapabilityIntrospectionJson } from "../src/stubs/catalog-introspection.js";

const cap: CapabilityIntrospectionJson = {
  name: "record_get", kind: "get", entity: "Record", invoke_wire_name: "record-get",
  python: { entity_symbol: "e1", method: "get", receiver: false, identity: ["id"], unavailable: null,
    parameters: [{name: "access_token", input_type: {type: "value", field_type: "string"}, required: true}] },
  inputs: {}, provides: ["id"], output_schema: null,
};
const catalog: CatalogIntrospectionJson = { entry_id: "fixture", catalog_cgs_hash: "fixture", entities: [{name: "Record", id_field: "id", fields: [{name: "id", value_ref: "integer", required: true}]}], values: {integer: {field_type: "integer"}}, capabilities: [cap] };
function emit(input: unknown): string {
  const binding = assignCapabilityBindings(catalog).get(cap.name)!;
  const source = renderProgramStatements(binding, cap, catalog.values, binding.invokeShape, "id", "input");
  const js = ts.transpile(`${source}\nreturn program;`, {target: ts.ScriptTarget.ES2022});
  return new Function("input", "plasmLiteral", "buildDottedArgs", js)(input, plasmLiteral, buildDottedArgs);
}
const get = emit({id: 3, access_token: "a\nb"});
assert.equal(get, 'class Invoke(Program):\n    def build(self):\n        return e1.get(3, access_token="a\\nb")\n');
cap.kind = "update";
cap.python.method = "m7";
cap.python.receiver = true;
cap.python.parameters = [{name: "payload", required: true, input_type: {type: "array", element_type: {type: "object", fields: [{name: "enabled", required: true, input_type: {type: "value", field_type: "boolean"}}]}}}];
const update = emit({id: 3, payload: [{enabled: true}]});
assert.match(update, /e1\.get\(3\)\.m7\(payload=\[\{"enabled": True\}\]\)/);
assert.match(renderCapabilityInputType(cap, catalog, "ScopedUpdate")!, /"enabled": boolean/);
assert.equal(plasmLiteral({nested: [false, null, 3, "line\nnext"]}), '{"nested": [False, None, 3, "line\\nnext"]}');
for (const invalid of [undefined, NaN, Infinity, new Date()]) assert.throws(() => plasmLiteral(invalid));
assert.equal(buildDottedArgs([{key: "optional", value: undefined, optional: true}, {key: "nullable", value: null}]), "nullable=None");
cap.python.identity = ["namespace", "id"];
assert.match(emit({namespace: "a", id: 3, payload: []}), /get\(namespace="a", id=3\)/);
cap.python.identity = ["id"];
cap.python.parameters = [];
cap.inputs.payload = {input_type: {type: "union", variants: [
  {name: "words", constructor_symbol: "retired_ctor", wire: {field: "mode", value: "text"},
    fields: [{name: "text", required: true, input_type: {type: "value", field_type: "string"}}]},
  {name: "counted", wire: {field: "mode", value: "count"},
    fields: [{name: "count", required: true, input_type: {type: "value", field_type: "integer"}}]},
]}};
const unionType = renderCapabilityInputType(cap, catalog, "MethodUnion")!;
assert.match(unionType, /readonly "mode": "text"/);
assert.match(unionType, /readonly "mode": "count"/);
assert.ok(!unionType.includes("retired_ctor"));
assert.match(emit({id: 3, mode: "text", text: "hello"}), /mode="text", text="hello"/);
assert.match(emit({id: 3, mode: "count", count: 2}), /mode="count", count=2/);
assert.match(emit({id: 3, mode: "text", text: "hello", count: 2}), /text="hello", count=2/);
cap.python.unavailable = "unsupported payload";
assert.throws(() => emit({}), /unsupported payload/);
for (const source of [get, update]) {
  const parsed = spawnSync("python3", ["-c", "import ast,sys; ast.parse(sys.stdin.read())"], {input: source, encoding: "utf8"});
  assert.equal(parsed.status, 0, parsed.stderr);
}
console.log("PASS: generated Python calls, Get arguments, mutation receivers, compound keys, nested literals and explicit rejection");

const { createProgramBuilder } = await import("../src/stubs/program-builder.js");
let envelope: unknown = {rows: [{id: 3}]};
const engine: import("../src/engine/napi-binding.js").PlasmEngine = {
  async loadCatalog() {},
  async synthesizeTeaching() { return {prompt: "", deltaRefs: []}; },
  async dryRun(source, ref) { assert.equal(source, get); assert.equal(ref, "session"); return {planCommitRef: "pc1", summary: "read"}; },
  async runPlanLive(_ref, _transport, session) { assert.equal(session, "session"); return {ok: true, message: "", rowsJson: JSON.stringify(envelope)}; },
  async runPlan() { throw new Error("validation is not execution"); },
  async activateDiscovery() { throw new Error("unused"); },
  async routeIntent() { throw new Error("unused"); },
  async run() { throw new Error("unused"); },
  async introspectCatalog() { throw new Error("unused"); },
};
const builder = createProgramBuilder({entryId: "fixture", cgsHash: "fixture", logicalSessionRef: "session", engine});
const transport = async () => ({status: 200, body: "{}"});
assert.deepEqual((await executeRows(builder, get, {transport})).rows, [{id: 3}]);
envelope = [{rows: [{id: 4}]}, {rows: [{receipt: true}]}];
assert.deepEqual((await executeRows(builder, get, {transport})).rows, [{id: 4}]);
envelope = {unexpected: true};
await assert.rejects(executeRows(builder, get, {transport}), /row-result envelope/);
console.log("PASS: generated client preserves session and explicit rows apart from operation receipts");
