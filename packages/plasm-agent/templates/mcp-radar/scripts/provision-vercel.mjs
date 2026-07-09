#!/usr/bin/env node
/**
 * Eve-aligned Vercel bootstrap for mcp-radar:
 * - Sync secrets from monorepo `.env` → Vercel project env
 */
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDir, "..");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, options);
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
  return result;
}

function parseEnvFile(filePath) {
  const vars = new Map();
  if (!existsSync(filePath)) return vars;
  const text = readFileSync(filePath, "utf8");
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const withoutExport = trimmed.startsWith("export ")
      ? trimmed.slice("export ".length).trim()
      : trimmed;
    const eq = withoutExport.indexOf("=");
    if (eq <= 0) continue;
    const key = withoutExport.slice(0, eq).trim();
    let value = withoutExport.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    vars.set(key, value);
  }
  return vars;
}

function resolveMonorepoEnv() {
  const files = [];
  let dir = projectRoot;
  for (let depth = 0; depth < 8; depth++) {
    const candidate = path.join(dir, ".env");
    if (existsSync(candidate)) files.push(candidate);
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  const merged = new Map();
  for (const file of files.reverse()) {
    for (const [key, value] of parseEnvFile(file)) {
      if (value.trim()) merged.set(key, value);
    }
  }
  return merged;
}

function envAlreadySet(name) {
  const result = run("vercel", ["env", "ls"], { encoding: "utf8", stdio: ["inherit", "pipe", "inherit"] });
  return (result.stdout ?? "").includes(name);
}

function syncEnvVar(name, value) {
  if (!value?.trim()) {
    console.warn(`[provision] skip ${name}: not found in monorepo .env`);
    return;
  }
  if (envAlreadySet(name)) {
    console.log(`[provision] ${name} already set on Vercel — skipping`);
    return;
  }
  for (const target of ["production", "preview"]) {
    console.log(`[provision] adding ${name} → ${target}…`);
    run("vercel", ["env", "add", name, target], {
      input: value,
      stdio: ["pipe", "inherit", "inherit"],
    });
  }
}

const env = resolveMonorepoEnv();

console.log("Syncing env from monorepo .env → Vercel project…");
syncEnvVar("TAVILY_API_TOKEN", env.get("TAVILY_API_TOKEN"));
syncEnvVar("PLASM_TENANT_SCOPE", env.get("PLASM_TENANT_SCOPE") ?? "mcp-radar");
// Proof: no host env. Agent creates the doc via share_link_create; slug + share URL come from that response.

console.log("Done. Redeploy production so env bindings take effect.");
