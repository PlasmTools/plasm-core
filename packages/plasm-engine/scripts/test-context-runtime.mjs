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
  const requests = [];
  const products = [{ id: 'one', name: 'One', category_id: null }, { id: 'two', name: 'Two', category_id: null }];
  const result = await engine.runPlanLive(plan.planCommitRef, async request => {
    assert.equal(request.method, 'GET');
    const path = new URL(request.url).pathname;
    requests.push(path);
    if (path === '/products') return { status: 200, body: JSON.stringify(products) };
    // The matrix permits hydration of the list rows through their Get capability.
    const product = products.find(row => path === `/products/${row.id}`);
    assert.ok(product, `Unexpected fixture request: ${path}`);
    return { status: 200, body: JSON.stringify(product) };
  });
  assert.equal(result.ok, true, result.message);
  assert.ok(requests.includes('/products'), 'Execute the fixture query before compute');
  assert.match(result.message, /\b2\b/);
  console.log('PASS: retained context executes compute through the packaged runtime; no providers or live effects');
} finally {
  await engine.close();
}
