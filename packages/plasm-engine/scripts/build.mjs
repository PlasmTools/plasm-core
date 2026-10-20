import { execFileSync } from 'node:child_process';
import { copyFileSync, chmodSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const core = path.resolve(root, '../..');
const platform = process.platform === 'darwin' && process.arch === 'arm64'
  ? 'darwin-arm64'
  : process.platform === 'linux' && process.arch === 'x64' ? 'linux-x64-gnu' : null;
if (!platform) throw new Error(`Unsupported native build host: ${process.platform}-${process.arch}`);
const temporary = mkdtempSync(path.join(tmpdir(), 'plasm-engine-build-'));
try {
  const flags = process.argv.includes('--debug') ? [] : ['--config=release'];
  execFileSync('bash', ['build/bazel/export.sh', '//packages/plasm-engine:platform_package', temporary, ...flags], { cwd: core, stdio: 'inherit' });
  execFileSync('tar', ['-xzf', path.join(temporary, `plasm-engine-${platform}.tgz`), '-C', temporary]);
  const binding = `plasm-engine.${platform}.node`;
  copyFileSync(path.join(temporary, 'package', binding), path.join(root, binding));
  const monty = path.join(root, `monty.${platform}`);
  copyFileSync(path.join(temporary, 'package', 'monty'), monty);
  chmodSync(monty, 0o755);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
