#!/usr/bin/env node
/**
 * Run TypeScript via tsx (works on Node 20–26 and Vercel build).
 * With no .ts first arg, runs plasm-cli.ts.
 */
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const args = process.argv.slice(2);

const scriptArg = args[0];
const isScriptEntry = scriptArg?.endsWith(".ts");
const entry = isScriptEntry
  ? path.resolve(packageRoot, scriptArg)
  : path.join(packageRoot, "scripts", "plasm-cli.ts");
const entryArgs = isScriptEntry ? args.slice(1) : args;

function runNode(nodeArgs) {
  const result = spawnSync(process.execPath, [...nodeArgs, entry, ...entryArgs], {
    stdio: "inherit",
    env: process.env,
  });
  process.exit(result.status ?? 1);
}

try {
  const tsxCli = require.resolve("tsx/cli");
  runNode([tsxCli]);
} catch {
  const plasmNode = path.join(packageRoot, "scripts", "plasm-node.mjs");
  const result = spawnSync(
    process.execPath,
    ["--experimental-strip-types", "--experimental-transform-types", plasmNode, entry, ...entryArgs],
    { stdio: "inherit", env: process.env },
  );
  process.exit(result.status ?? 1);
}
