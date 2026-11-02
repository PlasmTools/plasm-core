#!/usr/bin/env node
/** Offline public CLI proof using an archive exported from an actual fixture run. */
import assert from 'node:assert/strict';
import { readFile, writeFile, mkdtemp, stat } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
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
const archiveBytes = await readFile(archivePath);
const archive = JSON.parse(archiveBytes);
const policy = join(directory, 'policy.json');
await writeFile(policy, JSON.stringify({expected_digest: archive.digest,
  source_scope: archive.manifest.source_scope, excluded_tasks: [], excluded_groups: []}), {flag:'wx', mode:0o600});
const cli = fileURLToPath(new URL('./workflow-archive.mjs', import.meta.url));
const deployment = `serving-proof-${randomUUID()}`;
async function invoke(action) {
  const output = join(directory, `${action}.json`);
  const result = spawnSync(process.execPath, [cli, action, '--config', config,
    '--archive', archivePath, '--catalogs', catalogs, '--bindings', bindings,
    '--deployment', deployment, '--output', output, '--audit', join(directory, `${action}.audit.jsonl`),
    ...(action === 'import' ? ['--policy', policy] : [])],
  {env:process.env, encoding:'utf8', timeout:180000, maxBuffer:1024*1024});
  await writeFile(join(directory, `${action}.process.json`), JSON.stringify({status:result.status,
    stdout:result.stdout,stderr:result.stderr,error:result.error?.message}), {flag:'wx',mode:0o600});
  assert.equal(result.status, 0, `CLI ${action} failed; private diagnostics ${directory}`);
  assert.equal((await stat(output)).mode & 0o777, 0o600);
  return JSON.parse(await readFile(output));
}
const imported = await invoke('import');
assert.ok(imported.items.some(item => item.disposition.status === 'eligible_candidate'), 'Fixture must import an actual eligible example');
const checked = await invoke('verify');
assert.equal(checked.kind, 'workflow_serving_compatibility_v1');
assert.equal(checked.archive_sha256, createHash('sha256').update(archiveBytes).digest('hex'));
assert.equal(checked.report.truncated, false);
assert.ok(checked.report.examples.length > 0);
assert.ok(checked.report.examples.every(example => example.outcome.status === 'checked'));
assert.deepEqual(await readFile(archivePath), archiveBytes);
console.log(JSON.stringify({status:'passed',examples:checked.report.examples.length,proof:join(directory,'verify.json')}));
