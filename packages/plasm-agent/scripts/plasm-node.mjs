#!/usr/bin/env node
import { register } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));

register(pathToFileURL(path.join(scriptsDir, "resolve-ts-extension.mjs")));

const entry = process.argv[2];
if (!entry) {
  console.error("usage: plasm-node.mjs <entry.ts> [args...]");
  process.exit(1);
}

// Forward CLI args to the entry script (argv[0]=node, argv[1]=entry path).
process.argv = [process.argv[0], path.resolve(process.cwd(), entry), ...process.argv.slice(3)];

await import(pathToFileURL(path.resolve(process.cwd(), entry)));
