import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { PlasmEngine } = require(process.argv[2] ?? '..');

assert.equal(process.env.PLASM_MONTY_BINARY, undefined, 'Run this smoke without the deployment override');
const manifest = process.env.PLASM_MATRIX_MANIFEST;
assert.ok(manifest, 'PLASM_MATRIX_MANIFEST must identify the packed abstract language matrix');
const engine = new PlasmEngine();
await engine.loadCatalog(manifest);
const catalog = JSON.parse(await engine.introspectCatalog('plasm_language_matrix'));
await engine.exposeSeeds('Verify packaged Python execution', catalog.entities.map(entity => ({api: catalog.entry_id, entity: entity.name})));
const get = catalog.capabilities.find(capability => capability.name === 'langitem_get');
assert.ok(get);
const symbol = get.python.entity_symbol;
const plan = await engine.dryRun(`class PackagedRuntime(Program):
    @compute
    def format(self, row: Value[${symbol}]) -> str:
        return f"{row.title}:{row.score}"
    def build(self):
        item = ${symbol}.get("i1")
        return self.format(item)
`);
let calls = 0;
const result = await engine.runPlanLive(plan.planCommitRef, async request => {
  calls++;
  assert.equal(request.method, 'GET');
  return {status: 200, body: JSON.stringify({id: 'i1', title: 'packaged-worker', score: -7, owner: 'bot'})};
});
assert.equal(result.ok, true, result.message);
assert.equal(calls, 1);
assert.match(result.rowsJson, /packaged-worker:-7/);
console.log('PASS: packaged worker executes reviewed HTTP read and typed Python compute without PLASM_MONTY_BINARY');
