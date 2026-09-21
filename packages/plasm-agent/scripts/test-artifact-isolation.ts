import assert from 'node:assert/strict';
import { mkdtemp, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pinLocalArtifactImageSync, runArtefactTransform } from '../src/tools/artifact-process.js';
assert.ok(pinLocalArtifactImageSync(), 'A locally available pinned Node image is required');
const workspace = await mkdtemp(path.join(tmpdir(), 'artifact-law-'));
try {
  await writeFile(path.join(workspace, 'rows.json'), JSON.stringify({ rows: [{ amount: 3 }, { amount: 4 }] }));
  const run = (code: string) => runArtefactTransform(workspace, code, ['rows.json']);
  assert.equal(await run('export default ([x]: {rows: {amount: number}[]}[]) => x.rows.reduce((n, r) => n + r.amount, 0)'), '7');
  process.env.PLASM_AUDIT_HOST_SENTINEL = 'secret';
  assert.equal(await run('export default () => typeof process.env.PLASM_AUDIT_HOST_SENTINEL'), '"undefined"');
  await symlink('/etc/passwd', path.join(workspace, 'link'));
  await assert.rejects(runArtefactTransform(workspace, 'export default x => x', ['link']), /[Ss]ymlink/);
  await assert.rejects(runArtefactTransform(workspace, 'export default x => x', ['../outside']), /path|relative/i);
  await assert.rejects(run('export default async () => fetch("http://192.0.2.1", {signal: AbortSignal.timeout(1000)})'), /failed/);
  await assert.rejects(run('import fs from "node:fs/promises"; export default () => fs.writeFile("/outside", "x")'), /read-only/);
  await assert.rejects(run('export default () => "x".repeat(9000)'), /8192/);
  await assert.rejects(run('export default async () => new Promise(r => setTimeout(r, 60000))'), /resource limit/);
  console.log('Artifact function, isolation, output bounds, and deadline laws passed');
} finally {
  delete process.env.PLASM_AUDIT_HOST_SENTINEL;
  await rm(workspace, {recursive: true, force: true});
}
