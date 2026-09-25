// The generated native loader is kept intact; the worker is a platform artifact.
const path = require('node:path');
const fs = require('node:fs');
const native = require('./index.js');

function runtimePath(explicit) {
  const configured = explicit ?? process.env.PLASM_MONTY_BINARY;
  if (configured !== undefined) {
    if (!path.isAbsolute(configured)) throw new Error('Monty binary must be an absolute path');
    return configured;
  }
  const platform = `${process.platform}-${process.arch}${process.platform === 'linux' ? '-gnu' : ''}`;
  const local = path.join(__dirname, `monty.${platform}`);
  if (fs.existsSync(local)) return local;
  let packageRoot;
  try {
    packageRoot = path.dirname(require.resolve(`@plasm_lang/engine-${platform}`));
  } catch {
    throw new Error(`Missing packaged Monty runtime for ${platform}; rebuild @plasm_lang/engine or supply an absolute Monty binary path`);
  }
  const bundled = path.join(packageRoot, 'monty');
  if (!fs.existsSync(bundled)) throw new Error(`Missing packaged Monty runtime: ${bundled}`);
  return bundled;
}

class PlasmEngine extends native.PlasmEngine {
  constructor(montyBinary) {
    super(runtimePath(montyBinary));
  }
}

module.exports = { ...native, PlasmEngine };
