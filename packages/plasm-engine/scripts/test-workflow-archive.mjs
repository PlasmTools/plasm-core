#!/usr/bin/env node
/** Explicit, offline integration proof against a real native binding and PostgreSQL. */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp, readFile, writeFile, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`Explicit integration fixture ${name} is required`);
  return value;
}
for (const name of ['NAPI_RS_NATIVE_LIBRARY_PATH', 'PLASM_MONTY_BINARY', 'PLASM_DISCOVERY_DATABASE_URL']) required(name);
const sourcePath = required('PLASM_ARCHIVE_SOURCE_CONFIG');
const targetPath = required('PLASM_ARCHIVE_TARGET_CONFIG');
const catalogs = required('PLASM_ARCHIVE_CATALOGS');
const bindings = required('PLASM_ARCHIVE_BINDINGS');
const source = JSON.parse(await readFile(sourcePath, 'utf8'));
const target = JSON.parse(await readFile(targetPath, 'utf8'));
assert.notDeepEqual(source.scope, target.scope, 'Source and target must be distinct scopes');
for (const config of [source, target]) {
  assert.match(config.database_env, /^[A-Z][A-Z0-9_]*$/);
  required(config.database_env);
}
const directory = await mkdtemp(join(tmpdir(), 'plasm-archive-cli-'));
const cli = fileURLToPath(new URL('./workflow-archive.mjs', import.meta.url));
const attempt = `offline-${randomUUID()}`;
async function json(name, value) {
  const path = join(directory, name);
  await writeFile(path, JSON.stringify(value), { flag: 'wx', mode: 0o600 });
  return path;
}
async function invoke(action, config, name, args) {
  const output = join(directory, action === 'export' ? name : `${name}.json`);
  const audit = join(directory, `${name}.audit.jsonl`);
  const result = spawnSync(process.execPath, [cli, action, '--config', config,
    '--output', output, '--audit', audit, ...args], {
    env: process.env, encoding: 'utf8', timeout: 180_000, maxBuffer: 1024 * 1024,
  });
  // Preserve diagnostics privately; never echo URLs, archive source, or native errors.
  await writeFile(join(directory, `${name}.process.json`), JSON.stringify({
    status: result.status, signal: result.signal, stdout: result.stdout,
    stderr: result.stderr, error: result.error?.message,
  }), { flag: 'wx', mode: 0o600 });
  assert.equal(result.status, 0, `CLI ${name} failed; private diagnostics: ${directory}`);
  assert.equal((await stat(output)).mode & 0o777, action === 'export' ? 0o700 : 0o600);
  assert.equal((await stat(audit)).mode & 0o777, 0o600);
  return { output, value: JSON.parse(await readFile(action === 'export' ? join(output, 'manifest.json') : output, 'utf8')) };
}
const request = await json('request.json', {kind: 'private_collection'});
const exported = await invoke('export', sourcePath, 'export', ['--request', request]);
assert.equal(exported.value.version, 2);
assert.ok(Object.keys(exported.value.records).length > 0, 'Source fixture must contain workflow evidence');
assert.ok(Object.values(exported.value.records).some(item => item.kind === 'observation'));
assert.ok(Object.values(exported.value.records).some(item => item.kind === 'review'));
const originalBytes = await readFile(join(exported.output, 'manifest.json'));
for (const item of Object.values(exported.value.records)) {
  const bytes = await readFile(join(exported.output, 'objects', item.digest.slice(0, 2), item.digest));
  assert.equal(bytes.length, item.size);
  assert.equal(createHash('sha256').update(bytes).digest('hex'), item.digest);
}
const policy = await json('policy.json', {
  expected_digest: exported.value.digest, source_scope: exported.value.source_scope,
});
const importArgs = ['--archive', exported.output, '--policy', policy,
  '--catalogs', catalogs, '--bindings', bindings, '--deployment', `archive-proof-${attempt}`];
const imported = await invoke('import', targetPath, 'import', importArgs);
assert.ok(imported.value.items.length > 0);
const repeated = await invoke('import', targetPath, 'reimport', importArgs);
assert.ok(repeated.value.items.every(item => item.disposition.status === 'duplicate'));
assert.deepEqual(await readFile(join(exported.output, 'manifest.json')), originalBytes, 'Import must not mutate source archive');
// These prepared databases/schemas are exclusive fixtures, so whole-table counts also
// detect accidental fabrication under an unexpected scope.
const query = `SELECT json_build_object('receipts',(SELECT count(*) FROM workflow_learning_receipts),'reviews',(SELECT count(*) FROM workflow_learning_reviews),'archives',(SELECT count(*) FROM workflow_learning_archives))`;
// libpq does not expand a connection URI supplied through PGDATABASE. These
// isolated test fixtures support plain PostgreSQL URIs with optional search_path.
// Pass credentials through the child environment, never command-line arguments.
const connection = new URL(required(target.database_env));
assert.ok(['postgres:', 'postgresql:'].includes(connection.protocol));
assert.ok([...connection.searchParams.keys()].every(key => key === 'options'));
const pgEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('PG')));
Object.assign(pgEnv, {
  PGHOST: connection.hostname, PGPORT: connection.port || '5432',
  PGUSER: decodeURIComponent(connection.username), PGPASSWORD: decodeURIComponent(connection.password),
  PGDATABASE: decodeURIComponent(connection.pathname.slice(1)),
  PGOPTIONS: connection.searchParams.get('options') ?? '', PGPASSFILE: '/dev/null',
});
const count = spawnSync(process.env.PLASM_ARCHIVE_PSQL ?? 'psql', ['-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1', '-c', query], {
  env: pgEnv, encoding: 'utf8', timeout: 30_000,
});
await writeFile(join(directory, 'database-inspection.process.json'), JSON.stringify({
  status: count.status, stdout: count.stdout, stderr: count.stderr, error: count.error?.message,
}), { flag: 'wx', mode: 0o600 });
assert.equal(count.status, 0, 'Fixture database inspection failed');
assert.deepEqual(JSON.parse(count.stdout), { receipts: 0, reviews: 0, archives: 1 });
console.log(JSON.stringify({ result: 'passed', directory, archive_digest: exported.value.digest,
  assertions: ['content-addressed-evidence', 'real-cli-import', 'idempotent-reimport', 'immutable-source', 'no-local-execution-or-review'] }));
