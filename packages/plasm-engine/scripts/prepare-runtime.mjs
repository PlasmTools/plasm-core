import { execFileSync } from 'node:child_process';
import { copyFileSync, chmodSync, mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const oss = path.resolve(root, '../..');
const targets = {
  'aarch64-apple-darwin': 'darwin-arm64',
  'x86_64-apple-darwin': 'darwin-x64',
  'x86_64-unknown-linux-gnu': 'linux-x64-gnu',
};
const [action, argument, output] = process.argv.slice(2);
if (action === '--package') {
  const artifacts = path.resolve(argument);
  for (const suffix of Object.values(targets)) {
    const destination = path.join(root, 'npm', suffix);
    if (!existsSync(path.join(destination, `plasm-engine.${suffix}.node`))) continue;
    copyFileSync(path.join(artifacts, `monty.${suffix}`), path.join(destination, 'monty'));
    chmodSync(path.join(destination, 'monty'), 0o755);
    copyFileSync(path.join(oss, 'licenses/monty-MIT.txt'), path.join(destination, 'monty-LICENSE.txt'));
    const manifest = path.join(destination, 'package.json');
    const json = JSON.parse(readFileSync(manifest, 'utf8'));
    json.files = [...new Set([...(json.files ?? []), 'monty', 'monty-LICENSE.txt'])];
    writeFileSync(manifest, `${JSON.stringify(json, null, 2)}\n`);
  }
} else {
  if (action !== undefined && action !== '--target') throw new Error('Expected --target TRIPLE [OUTPUT] or --package ARTIFACTS');
  const target = argument ?? execFileSync('rustc', ['-vV'], {encoding: 'utf8'}).match(/^host: (.+)$/m)?.[1];
  const suffix = targets[target];
  if (!suffix) throw new Error(`Unsupported Monty package target: ${target}`);
  const installRoot = path.join(oss, 'target/monty-node', target);
  execFileSync('bash', [path.join(oss, 'scripts/ci/build-monty-runtime.sh'), installRoot, target], {stdio: 'inherit'});
  const destination = output ? path.resolve(output) : root;
  mkdirSync(destination, {recursive: true});
  copyFileSync(path.join(installRoot, 'bin/monty'), path.join(destination, `monty.${suffix}`));
  chmodSync(path.join(destination, `monty.${suffix}`), 0o755);
  copyFileSync(path.join(oss, 'licenses/monty-MIT.txt'), path.join(destination, 'monty-LICENSE.txt'));
}
