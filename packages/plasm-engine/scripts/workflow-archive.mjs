#!/usr/bin/env node
/** Host administration only: private observations and portable teaching libraries. */
import { open } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import { createRequire } from 'node:module';
import { resolve, dirname } from 'node:path';
import { createHash } from 'node:crypto';
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

const require = createRequire(import.meta.url);
const { PlasmEngine } = require('../runtime.cjs');
const { values, positionals } = parseArgs({ allowPositionals: true, options: {
  limit: { type: 'string' }, config: { type: 'string' }, output: { type: 'string' }, audit: { type: 'string' },
  request: { type: 'string' }, assessments: { type: 'string' }, digest: { type: 'string' },
  archive: { type: 'string' }, policy: { type: 'string' },
  catalogs: { type: 'string' }, bindings: { type: 'string' }, deployment: { type: 'string' },
} });
const action = positionals[0];
if (positionals.length !== 1 || !['export', 'import', 'verify'].includes(action)
    || !values.config || !values.output || !values.audit) {
  throw new Error('Usage: plasm-workflow-archive export|import|verify --config FILE --output NEW_FILE --audit NEW_FILE [--request FILE --assessments FILE] [--archive FILE --policy FILE --catalogs FILE --bindings FILE --deployment ID]');
}
if (resolve(values.output) === resolve(values.audit)) throw new Error('Archive and audit destinations must differ');

async function readBounded(file, limit = 1024 * 1024) {
  const handle = await open(file, 'r');
  try {
    const size = (await handle.stat()).size;
    if (size > limit) throw new Error('Archive input exceeds its byte limit');
    // Read at most limit + 1 bytes even if the file grows after stat.
    const chunks = [];
    let total = 0;
    while (total <= limit) {
      const buffer = Buffer.alloc(Math.min(65536, limit + 1 - total));
      const { bytesRead } = await handle.read(buffer);
      if (bytesRead === 0) break;
      total += bytesRead;
      if (total > limit) throw new Error('Archive input exceeds its byte limit');
      chunks.push(buffer.subarray(0, bytesRead));
    }
    return Buffer.concat(chunks).toString('utf8');
  } finally { await handle.close(); }
}

const config = JSON.parse(await readBounded(values.config));
if (!config || Object.keys(config).sort().join(',') !== 'database_env,scope'
    || typeof config.database_env !== 'string' || !/^[A-Z][A-Z0-9_]*$/.test(config.database_env)) {
  throw new Error('Configuration requires only scope and a database_env variable name');
}
const databaseUrl = process.env[config.database_env];
if (!databaseUrl) throw new Error('Configured workflow database environment is absent');
const engine = new PlasmEngine();
// Reserve both destinations before any durable mutation. Never overwrite evidence.
const audit = await open(values.audit, 'wx', 0o600);
let output;
async function record(value) {
  await audit.writeFile(JSON.stringify(value) + '\n');
  await audit.sync();
}
try {
  output = await open(values.output, 'wx', 0o600);
  await engine.configureWorkflowLearning(JSON.stringify({mode: 'collect', scope: config.scope, databaseUrl}));
  await record({event: 'archive_administration', action, scope: config.scope});
  if (action === 'export') {
    if (Boolean(values.request) === Boolean(values.digest)) throw new Error('Export requires exactly one of --request or --digest');
    if (values.digest && values.assessments) throw new Error('Retained export cannot attach assessments');
    if (values.assessments) {
      const assessments = JSON.parse(await readBounded(values.assessments));
      if (!Array.isArray(assessments) || assessments.length > 10000) throw new Error('Invalid assessment batch');
      for (const assessment of assessments) {
        await engine.recordWorkflowAssessment(JSON.stringify(assessment));
      }
      await record({event: 'assessments_retained', count: assessments.length});
    }
    const archive = values.digest
      ? await engine.exportRetainedWorkflowArchive(values.digest)
      : await engine.exportWorkflowArchive(await readBounded(values.request));
    await output.writeFile(archive);
    await output.sync();
    const document = JSON.parse(archive);
    await record({event: 'archive_exported', digest: document.digest, items: document.items.length});
  } else {
    if (!values.archive || (action === 'import' && !values.policy) || !values.catalogs || !values.bindings || !values.deployment) {
      throw new Error('Import requires --archive, --policy, --catalogs, --bindings and --deployment');
    }
    const catalogBytes = await readBounded(values.catalogs);
    const bindingBytes = await readBounded(values.bindings);
    const catalogs = JSON.parse(catalogBytes);
    if (!Array.isArray(catalogs) || catalogs.length === 0 || catalogs.length > 10000
        || catalogs.some(value => typeof value !== 'string' || value.length === 0)) {
      throw new Error('Catalogues must be an array of authoritative packed manifest paths');
    }
    const fingerprints = [];
    for (const catalog of catalogs) {
      const manifestBytes = await readBounded(catalog);
      const manifest = JSON.parse(manifestBytes);
      if (typeof manifest.cgs_json !== 'string' || typeof manifest.recipes_json !== 'string') {
        throw new Error('Packed catalogue requires CGS and recipes artifacts');
      }
      fingerprints.push({
        manifest_sha256: sha256(manifestBytes),
        cgs_sha256: sha256(await readBounded(resolve(dirname(catalog), manifest.cgs_json), 512 * 1024 * 1024)),
        recipes_sha256: sha256(await readBounded(resolve(dirname(catalog), manifest.recipes_json), 512 * 1024 * 1024)),
      });
      await engine.loadCatalog(catalog);
    }
    await engine.activateDiscovery(values.deployment, bindingBytes);
    const archiveBytes = await readBounded(values.archive, 512 * 1024 * 1024);
    if (action === 'verify') {
      const limit = values.limit === undefined ? 32 : Number(values.limit);
      if (!Number.isInteger(limit) || limit < 1 || limit > 32) throw new Error('Verification limit must be 1..32');
      const archive = JSON.parse(archiveBytes);
      const retained = await engine.exportRetainedWorkflowArchive(archive.digest);
      if (retained !== archiveBytes) throw new Error('Archive bytes differ from the retained target archive');
      const checked = JSON.parse(await engine.verifyWorkflowLibrary(limit));
      const proof = {
        schema_version: 1, kind: 'workflow_serving_compatibility_v1',
        archive_digest: archive.digest, archive_sha256: sha256(archiveBytes),
        catalogs_sha256: sha256(catalogBytes), bindings_sha256: sha256(bindingBytes),
        deployment: values.deployment, catalogs: fingerprints, report: checked,
      };
      await output.writeFile(JSON.stringify(proof) + '\n');
      await output.sync();
      await record({event: 'library_compatibility_checked', count: checked.examples.length, truncated: checked.truncated});
      if (checked.truncated || checked.examples.length === 0 || checked.examples.some(example => example.outcome.status !== 'checked')) {
        throw new Error('Library compatibility gate failed; bounded report retained');
      }
    } else {
      const report = await engine.importWorkflowArchive(archiveBytes, await readBounded(values.policy));
      await output.writeFile(report + '\n');
      await output.sync();
      const parsed = JSON.parse(report);
      await record({event: 'archive_imported', digest: parsed.archive_digest, items: parsed.items.length});
    }
  }
} catch (error) {
  const message = String(error instanceof Error ? error.message : 'archive administration failed')
    .replaceAll(databaseUrl, '[workflow database]')
    .replace(/postgres(?:ql)?:\/\/[^\s]+/g, '[database credential]');
  await record({event: 'archive_administration_failed', message});
  process.exitCode = 1;
} finally {
  await engine.close();
  await output?.close();
  await audit.close();
}
