#!/usr/bin/env node
/** Offline public CLI proof using an archive exported from an actual fixture run. */
import assert from 'node:assert/strict';
import { readFile, writeFile, mkdtemp, stat } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
function required(name) {
  assert.ok(process.env[name], `Explicit fixture ${name} required`);
  return process.env[name];
}
for (const name of ['NAPI_RS_NATIVE_LIBRARY_PATH', 'PLASM_MONTY_BINARY', 'PLASM_DISCOVERY_DATABASE_URL']) required(name);
const config = required('PLASM_ARCHIVE_TARGET_CONFIG');
const archivePath = required('PLASM_SERVING_ARCHIVE');
const catalogs = required('PLASM_ARCHIVE_CATALOGS');
const bindings = required('PLASM_ARCHIVE_BINDINGS');
const directory = await mkdtemp(join(tmpdir(), 'plasm-serving-cli-'));
const archiveBytes = await readFile(join(archivePath, 'manifest.json'));
const archive = JSON.parse(archiveBytes);
const policy = join(directory, 'policy.json');
await writeFile(policy, JSON.stringify({expected_digest: archive.digest,
  source_scope: archive.source_scope}), {flag:'wx', mode:0o600});
const cli = fileURLToPath(new URL('./workflow-archive.mjs', import.meta.url));
const deployment = `serving-proof-${randomUUID()}`;
async function invoke(action, expectedStatus = 0) {
  const output = join(directory, `${action}.json`);
  const result = spawnSync(process.execPath, [cli, action, '--config', config,
    '--archive', archivePath, '--catalogs', catalogs, '--bindings', bindings,
    '--deployment', deployment, '--output', output, '--audit', join(directory, `${action}.audit.jsonl`),
    ...(action === 'import' ? ['--policy', policy] : [])],
  {env:process.env, encoding:'utf8', timeout:180000, maxBuffer:1024*1024});
  await writeFile(join(directory, `${action}.process.json`), JSON.stringify({status:result.status,
    stdout:result.stdout,stderr:result.stderr,error:result.error?.message}), {flag:'wx',mode:0o600});
  assert.equal(result.status, expectedStatus, `Unexpected CLI ${action} outcome; private diagnostics ${directory}`);
  assert.equal((await stat(output)).mode & 0o777, 0o600);
  return JSON.parse(await readFile(output));
}
const imported = await invoke('import');
assert.ok(imported.items.some(item => item.disposition.status === 'awaiting_compatibility' && item.disposition.reason?.kind === 'supporting_evidence_unavailable'), 'Import must hold unapproved source examples');
const checked = await invoke('verify', 1);
assert.equal(checked.kind, 'workflow_serving_compatibility_v1');
assert.equal(checked.manifest_sha256, createHash('sha256').update(archiveBytes).digest('hex'));
assert.equal(checked.report.truncated, false);
assert.equal(checked.report.examples.length, 0, 'Unapproved imported examples must not be served');
assert.deepEqual(await readFile(join(archivePath, 'manifest.json')), archiveBytes);
// This script is the trusted embedding host: configureTraining installs a
// project-scoped grant; archive contents and model input do not supply authority.
const configData = JSON.parse(await readFile(config, 'utf8'));
const databaseUrl = required(configData.database_env);
const {PlasmEngine} = createRequire(import.meta.url)('../runtime.cjs');
const engine = new PlasmEngine();
try {
  await engine.configureWorkflowLearning(JSON.stringify({mode:'collect', scope:configData.scope, databaseUrl}));
  const subject = `fixture-attestor-${randomUUID()}`;
  const credential = randomUUID();
  await engine.configureTraining(JSON.stringify({subject, credential, idempotency_key:randomUUID(),
    grant:{grantee:subject, credential, actions:['attest_imported_review'], expires_at:'2099-01-01T00:00:00Z',
      publication_policy:{policy:'human_reviewed'}, target:{scope:'project'}}}));
  for (const catalog of JSON.parse(await readFile(catalogs, 'utf8'))) await engine.loadCatalog(catalog);
  await engine.activateDiscovery(`trusted-${deployment}`, await readFile(bindings, 'utf8'));
  const seen = new Set();
  for (const item of Object.values(archive.records)) {
    if (!['example_support','guidance_support'].includes(item.kind)) continue;
    const record = JSON.parse(await readFile(join(archivePath, 'objects', item.digest.slice(0,2), item.digest), 'utf8'));
    const key = JSON.stringify(record.evidence);
    if (seen.has(key)) continue;
    seen.add(key);
    await engine.authorizeImportedWorkflowReview(JSON.stringify({archive_digest:archive.digest,
      evidence:record.evidence, decision_id:randomUUID(), expires_at:'2099-01-01T00:00:00Z'}));
  }
  assert.ok(seen.size > 0, 'Fixture must contain reviewed example support');
  const policyJson = await readFile(policy, 'utf8');
  const adopted = JSON.parse(await engine.importWorkflowArchive(archivePath, policyJson));
  assert.ok(adopted.items.some(item => item.disposition.status === 'eligible_candidate'), 'Attested fixture must be adopted');
  const repeated = JSON.parse(await engine.importWorkflowArchive(archivePath, policyJson));
  const adoptedIds = new Set(adopted.items.filter(item => item.disposition.status === 'eligible_candidate').map(item => item.identity));
  assert.ok(repeated.items.filter(item => adoptedIds.has(item.identity)).every(item => item.disposition.status === 'duplicate'), 'Adopted items must be idempotent');
  const serving = JSON.parse(await engine.verifyWorkflowLibrary(32));
  assert.ok(serving.examples.some(example => example.outcome.status === 'checked'), 'Adopted example must compile');
  // Export adopted provenance and require a fresh target to attest it again.
  const onwardPath = join(directory, 'onward-archive');
  const onward = JSON.parse(await engine.exportWorkflowArchive('private_collection', onwardPath));
  const secondConfig = JSON.parse(await readFile(required('PLASM_ARCHIVE_SECOND_TARGET_CONFIG'), 'utf8'));
  assert.notDeepEqual(secondConfig.scope, configData.scope);
  const next = new PlasmEngine();
  try {
    await next.configureWorkflowLearning(JSON.stringify({mode:'collect', scope:secondConfig.scope,
      databaseUrl:required(secondConfig.database_env)}));
    const secondSubject = `fixture-attestor-${randomUUID()}`;
    const secondCredential = randomUUID();
    await next.configureTraining(JSON.stringify({subject:secondSubject, credential:secondCredential, idempotency_key:randomUUID(),
      grant:{grantee:secondSubject, credential:secondCredential, actions:['attest_imported_review'], expires_at:'2099-01-01T00:00:00Z',
        publication_policy:{policy:'human_reviewed'}, target:{scope:'project'}}}));
    for (const catalog of JSON.parse(await readFile(catalogs, 'utf8'))) await next.loadCatalog(catalog);
    await next.activateDiscovery(`onward-${deployment}`, await readFile(bindings, 'utf8'));
    const onwardSeen = new Set();
    for (const item of Object.values(onward.records)) {
      if (!['example_support','guidance_support'].includes(item.kind)) continue;
      const record = JSON.parse(await readFile(join(onwardPath, 'objects', item.digest.slice(0,2), item.digest), 'utf8'));
      const key = JSON.stringify(record.evidence);
      if (onwardSeen.has(key)) continue;
      onwardSeen.add(key);
      await next.authorizeImportedWorkflowReview(JSON.stringify({archive_digest:onward.digest,
        evidence:record.evidence, decision_id:randomUUID(), expires_at:'2099-01-01T00:00:00Z'}));
    }
    assert.ok(onwardSeen.size > 0, 'Re-export must retain source support');
    const onwardPolicy = JSON.stringify({expected_digest:onward.digest, source_scope:onward.source_scope});
    const secondImport = JSON.parse(await next.importWorkflowArchive(onwardPath, onwardPolicy));
    assert.ok(secondImport.items.some(item => item.disposition.status === 'eligible_candidate'), 'Second target must adopt an attested example');
    const secondRepeat = JSON.parse(await next.importWorkflowArchive(onwardPath, onwardPolicy));
    const secondIds = new Set(secondImport.items.filter(item => item.disposition.status === 'eligible_candidate').map(item => item.identity));
    assert.ok(secondRepeat.items.filter(item => secondIds.has(item.identity)).every(item => item.disposition.status === 'duplicate'));
    const secondServing = JSON.parse(await next.verifyWorkflowLibrary(32));
    assert.ok(secondServing.examples.some(example => example.outcome.status === 'checked'));
    const finalArchive = join(directory, 'final-archive');
    await next.exportWorkflowArchive('private_collection', finalArchive);
  } finally { await next.close(); }
  console.log(JSON.stringify({status:'passed',attestations:seen.size,checked_examples:serving.examples.length,
    proof:join(directory,'verify.json')}));
} finally { await engine.close(); }
