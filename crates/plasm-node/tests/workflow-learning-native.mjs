// Offline native protocol proof: real NAPI compilation/execution and immutable evidence.
// Core worker integration invokes separate processes around durable promotion.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
const require = createRequire(import.meta.url);
const { PlasmEngine } = require(process.env.PLASM_NATIVE_BINDING);
let raw = '';
for await (const chunk of process.stdin) raw += chunk;
const input = JSON.parse(raw);
const command = process.argv[2];
const engine = new PlasmEngine(process.env.PLASM_MONTY_BINARY);
const scope = input.scope;
const sources = [
  'class InspectProducts(Program):\n    def build(self):\n        first = e1.query().take(1)\n        second = e1.query().take(2)\n        return first, second\n',
  'class InspectNames(Program):\n    def build(self):\n        return e1.get("p1").select("name")\n',
];
const config = mode => ({mode, scope, databaseUrl: process.env.PLASM_TEST_POSTGRES_URL});
const transport = async request => {
  assert.equal(request.method, 'GET');
  const product = {id:'p1',name:'Widget',category_id:'c1'};
  const path = new URL(request.url).pathname;
  return {status: 200, body: JSON.stringify(path === '/products' ? [product, {...product,id:'p2',name:'Second'}] : product)};
};
try {
  await engine.configureWorkflowLearning(JSON.stringify(config(command === 'collect' ? 'collect' : 'examples')));
  await engine.loadCatalog(process.env.PLASM_NATIVE_LEARNING_FIXTURE_MANIFEST);
  if (command === 'collect') {
    await engine.exposeSeeds('Inspect fixture products', [{api:'matrix',entity:'Product'}]);
    for (const source of sources) {
      const plan = await engine.dryRun(source);
      assert.ok(plan.failureJson == null, plan.summary);
      const result = await engine.runPlanLive(plan.planCommitRef, transport);
      assert.equal(result.ok, true, result.message);
    }
    await engine.drain();
    await assert.rejects(() => engine.dryRun(sources[0]));
    const index = JSON.parse(await engine.completedWorkflowRuns());
    const evidence = await Promise.all(index.runs.map(async run => JSON.parse(await engine.readWorkflowEvidence(run.run_id))));
    assert.ok(evidence.length >= 3, 'two authored programs must retain all output receipts');
    assert.equal(new Set(evidence.map(item => item.logical_session)).size, 1);
    const groups = sources.map(source => evidence.filter(item => item.source === source.trim()));
    assert.ok(groups[0].length >= 2, 'first whole program has multiple result artifacts');
    assert.ok(groups[1].length >= 1);
    assert.equal(groups.flat().length, evidence.length, 'every receipt retains its complete authored source');
    for (const item of evidence) {
      assert.equal(item.scope.tenant, `tenant:${scope.tenant}`);
      assert.equal(item.observation.state, 'available', JSON.stringify(item.observation));
      assert.equal(item.observation.value.disclosure, "NoKnownSensitiveOperands");
      assert.equal(item.catalogue_requirements.state, 'available');
      assert.equal(item.symbol_context.state, 'captured', JSON.stringify(item.symbol_context));
      assert.deepEqual(item.symbol_context.scope, item.scope);
      assert.equal(item.symbol_context.logical_session, item.logical_session);
      assert.equal(item.symbol_context.execute_session, item.execute_session);
      assert.ok(item.symbol_context.compilation.ledger_bytes.length > 5);
      assert.deepEqual(item.symbol_context.compilation.ledger_bytes.slice(0, 4), [80, 76, 83, 76]);
    }
    await assert.rejects(() => engine.reviewWorkflow(JSON.stringify({subtasks:[{intent:'Unobserved',observed_outcome:'No proof',run_ids:['missing']}]})));
    const review = JSON.parse(await engine.reviewWorkflow(JSON.stringify({subtasks:groups.map((items, i) => ({
      intent: i === 0 ? 'Inspect fixture products at two bounded sizes' : 'Inspect fixture product names',
      observed_outcome: i === 0 ? 'Observed one product and two products in separate outputs of the complete program' : 'Observed the selected product name in the complete program output',
      run_ids: items.map(item => item.run_id),
    }))})));
    process.stdout.write(JSON.stringify({...review, logical_session_id:evidence[0].logical_session, sources:sources.map(source => source.trim()), symbol_contexts:evidence.map(item => ({run_id:item.run_id,ledger_sha256:createHash("sha256").update(Buffer.from(item.symbol_context.compilation.ledger_bytes)).digest("hex")})), groups:groups.map(items => items.map(item => item.run_id)), run_ids:index.runs.map(run => run.run_id)}));
  } else if (command === 'fetch') {
    await engine.exposeSeeds('Inspect fixture categories', [{api:'matrix',entity:'Category'}]);
    const example = JSON.parse(await engine.fetchWorkflowExample(input.example_ref));
    assert.match(example.source, /e2\./);
    assert.ok(example.teaching_delta.length > 0);
    assert.deepEqual(Object.keys(example.catalog_fingerprints), ['matrix']);
    if (input.program_kind === 'products') {
      assert.match(example.source, /first = e2\.query\(\)\.take\(1\)/);
      assert.match(example.source, /second = e2\.query\(\)\.take\(2\)/);
      assert.match(example.source, /return \(?(?:first, second)\)?/);
      assert.equal(example.outputs.length, 2);
    } else {
      assert.equal(input.program_kind, 'names');
      assert.match(example.source, /e2\.get\([^)]*\)\.select\(["']name["']\)/);
      assert.match(example.source, /["']p1["']/, "authored identity literal survives teaching example");
      assert.equal(Object.hasOwn(example, "inputs"), false, "teaching source does not manufacture template parameters");
      assert.equal(example.outputs.length, 1);
    }
    const plan = await engine.dryRun(example.source);
    assert.ok(plan.failureJson == null, plan.summary);
    process.stdout.write(JSON.stringify({example_ref:example.example_id,source:example.source,outputs:example.outputs.length,catalogs:Object.keys(example.catalog_fingerprints),compiled:plan.failureJson == null}));
  } else throw new Error(`unsupported fixture command ${command}`);
} finally {
  await engine.close();
}
