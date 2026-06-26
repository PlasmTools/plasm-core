#!/usr/bin/env node
/**
 * Smoke: plasm-agent build via CLI + manifest + stub paths (Vercel buildCommand parity).
 */
import { access, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { runPlasmBuild } from "../src/cli/build.js";
import { resolveAgentProject } from "../src/cli/project-root.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

async function main(): Promise<void> {
  const projectRoot =
    process.argv[2] ?? path.join(packageRoot, "../../../examples/mcp-radar-agent");
  process.chdir(projectRoot);

  const project = await resolveAgentProject();
  if (!project) {
    throw new Error(`No agent project in ${projectRoot}`);
  }

  const result = await runPlasmBuild(project);
  if (result.stubs.length < 1) {
    throw new Error(`expected stubs, got ${result.stubs.length}`);
  }

  const manifestRaw = await readFile(result.manifestPath, "utf8");
  const manifest = JSON.parse(manifestRaw) as {
    stubs?: Array<{ entryId: string }>;
    discovery?: { catalogs?: unknown[] };
  };
  if (!manifest.stubs?.length || !manifest.discovery?.catalogs?.length) {
    throw new Error(`invalid manifest: ${manifestRaw.slice(0, 200)}`);
  }

  for (const stub of result.stubs) {
    await access(stub.outPath);
  }

  const vercelJson = JSON.parse(
    await readFile(path.join(projectRoot, "vercel.json"), "utf8"),
  ) as { buildCommand?: string };
  if (vercelJson.buildCommand !== "plasm-agent build") {
    throw new Error(`vercel.json buildCommand must be plasm-agent build`);
  }

  console.log(`OK: plasm build (${result.stubs.length} stub(s), manifest + vercel.json)`);
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
