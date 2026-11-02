// Real installed engine and packaged worker; abstract fixture and local transport only.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { PlasmEngine } = require('../runtime.cjs');
const manifest = process.env.PLASM_NATIVE_MATRIX_MANIFEST;
assert.ok(manifest, 'PLASM_NATIVE_MATRIX_MANIFEST must name the exported execute_tiny matrix');
assert.equal(process.env.PLASM_MONTY_BINARY, undefined, 'Exercise packaged worker resolution');
const engine = new PlasmEngine();
try {
  await engine.configureWorkflowLearning(JSON.stringify({
    mode: 'admission',
    scope: { tenant: 'offline-runtime-proof', project: 'matrix' },
    databaseUrl: 'postgresql://unused@127.0.0.1:1/unused',
  }));
  await engine.loadCatalog(manifest);
  // Admission-mode context creates the retained host before execution begins.
  const teaching = await engine.exposeSeeds('Count abstract products', [{ api: 'matrix', entity: 'Product' }]);
  const queries = [...teaching.prompt.matchAll(/\b(e\d+)\.query\(\)/g)];
  assert.equal(queries.length, 1, 'The fixture must teach exactly one product query');
  const symbol = queries[0][1];
  const plan = await engine.dryRun(`class CountProducts(Program):
    @compute
    def count(self, rows) -> int:
        return len(rows)
    def build(self):
        products = ${symbol}.query()
        return self.count(products)
`);
  assert.ok(plan.planCommitRef, plan.summary);
  let requests = 0;
  const result = await engine.runPlanLive(plan.planCommitRef, async request => {
    requests++;
    assert.equal(request.method, 'GET');
    assert.equal(new URL(request.url).pathname, '/products');
    return { status: 200, body: JSON.stringify([{ id: 'one', name: 'One' }, { id: 'two', name: 'Two' }]) };
  });
  assert.equal(result.ok, true, result.message);
  assert.equal(requests, 1);
  assert.match(result.message, /\b2\b/);
  console.log('PASS: retained context executes compute through the packaged runtime; no providers or live effects');
} finally {
  await engine.close();
}
