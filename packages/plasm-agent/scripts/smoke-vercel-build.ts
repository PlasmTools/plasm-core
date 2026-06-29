#!/usr/bin/env node
/**
 * Smoke: plasm-agent build → Nitro output + agent summary (eve-style Vercel build).
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

  const vercel = process.argv.includes("--vercel");
  if (vercel) {
    process.env.VERCEL = "1";
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
    const mjsPath = stub.outPath.replace(/\.ts$/, ".mjs");
    await access(mjsPath);
  }

  const manifestCompiledStubs = (manifest as { compiledStubs?: Record<string, string> }).compiledStubs;
  if (!manifestCompiledStubs || Object.keys(manifestCompiledStubs).length < result.stubs.length) {
    throw new Error("manifest missing compiledStubs entries");
  }

  if (!result.outputDir) {
    throw new Error("build must emit nitro outputDir");
  }
  if (vercel) {
    await access(path.join(result.outputDir, "config.json"));
    await access(path.join(result.outputDir, "functions", "__server.func", "agent"));
  } else {
    await access(path.join(result.outputDir, "server", "index.mjs"));
  }

  if (!result.agentSummaryPath) {
    throw new Error("build must emit agent summary");
  }
  await access(result.agentSummaryPath);
  const summary = JSON.parse(await readFile(result.agentSummaryPath, "utf8")) as {
    kind?: string;
    schemaVersion?: number;
  };
  if (summary.kind !== "vercel-plasm-agent-summary" || summary.schemaVersion !== 3) {
    throw new Error(`unexpected agent summary: ${JSON.stringify(summary)}`);
  }

  const vercelJson = JSON.parse(
    await readFile(path.join(projectRoot, "vercel.json"), "utf8"),
  ) as { buildCommand?: string };
  if (vercelJson.buildCommand !== "plasm-agent build") {
    throw new Error(`vercel.json buildCommand must be plasm-agent build`);
  }

  const mode = vercel ? "vercel .vercel/output" : "local .plasm/nitro-output";
  console.log(`OK: plasm build (${result.stubs.length} stub(s), ${mode}, agent summary)`);
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
