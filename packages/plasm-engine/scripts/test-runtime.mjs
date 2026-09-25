import assert from 'node:assert/strict';
import { mkdtempSync, copyFileSync, writeFileSync, mkdirSync, rmSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = realpathSync(mkdtempSync(path.join(tmpdir(), 'plasm-runtime-package-')));
try {
  copyFileSync(path.join(root, 'runtime.cjs'), path.join(fixture, 'runtime.cjs'));
  writeFileSync(path.join(fixture, 'index.js'), 'exports.PlasmEngine = class { constructor(binary) { this.binary = binary; } };');
  const require = createRequire(path.join(fixture, 'test.cjs'));
  const { PlasmEngine } = require('./runtime.cjs');
  assert.throws(() => new PlasmEngine('relative/monty'), /absolute/);
  assert.equal(new PlasmEngine('/explicit/monty').binary, '/explicit/monty');
  const platform = `${process.platform}-${process.arch}${process.platform === 'linux' ? '-gnu' : ''}`;
  const packaged = path.join(fixture, 'node_modules/@plasm_lang', `engine-${platform}`);
  mkdirSync(packaged, {recursive: true});
  writeFileSync(path.join(packaged, 'package.json'), JSON.stringify({main: 'native.node'}));
  writeFileSync(path.join(packaged, 'native.node'), '');
  assert.throws(() => new PlasmEngine(), /Missing packaged Monty runtime/);
  writeFileSync(path.join(packaged, 'monty'), '');
  assert.equal(new PlasmEngine().binary, path.join(packaged, 'monty'));
  const local = path.join(fixture, `monty.${platform}`);
  writeFileSync(local, '');
  assert.equal(new PlasmEngine().binary, local);
  console.log('PASS: explicit, local and platform-package runtime resolution; missing sidecar fails closed');
} finally {
  rmSync(fixture, {recursive: true, force: true});
}
