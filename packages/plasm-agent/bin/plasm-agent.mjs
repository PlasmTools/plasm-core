#!/usr/bin/env node
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const runner = path.join(packageRoot, "scripts", "plasm-node.mjs");
const cli = path.join(packageRoot, "scripts", "plasm-cli.ts");

const child = spawn(
  process.execPath,
  ["--experimental-strip-types", "--experimental-transform-types", runner, cli, ...process.argv.slice(2)],
  { stdio: "inherit", env: process.env },
);

child.on("exit", (code) => {
  process.exit(code ?? 0);
});
