#!/usr/bin/env node
/**
 * Eve-aligned Blob bootstrap for mcp-radar on Vercel.
 * Links a public Blob store to the project (OIDC at runtime — no manual tokens).
 */
import { spawnSync } from "node:child_process";

function run(command, args) {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

console.log("Linking Vercel Blob store (OIDC, public)…");
run("vercel", ["blob", "create-store", "mcp-radar-proof", "--access", "public", "--yes"]);

console.log("Done. Redeploy so production picks up linked Blob storage.");
