import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import { readBuildManifest } from "./build-manifest.js";
import { compileAuthoredSlots } from "./compile-authored-slots.js";
import type { ResolvedAgentProject } from "./project-root.js";
import { isNativeEngineAvailable } from "../engine/napi-binding.js";
import { walkAgentProject } from "../discovery/project-walker.js";
import { generateAllStubs } from "../stubs/generator.js";
import { buildPlasmApplication } from "../nitro/build-application.js";
import { readPackageName } from "./project-root.js";

export interface PlasmBuildResult {
  stubsDir: string;
  discoveryDir: string;
  manifestPath: string;
  stubs: Array<{ entryId: string; catalogCgsHash: string; outPath: string }>;
  outputDir?: string;
  agentSummaryPath?: string;
  vercelOutput?: boolean;
}

export async function runPlasmBuild(project: ResolvedAgentProject): Promise<PlasmBuildResult> {
  const stubs = await generateAllStubs(project.agentRoot);
  const discovery = await walkAgentProject(project.agentRoot);
  const { compiledSlots } = await compileAuthoredSlots(
    project.projectRoot,
    project.agentRoot,
    discovery,
  );

  const discoveryDir = path.join(project.agentRoot, ".plasm", "discovery");
  await mkdir(discoveryDir, { recursive: true });
  const manifestPath = path.join(discoveryDir, "manifest.json");
  const native = isNativeEngineAvailable();
  const manifest = {
    builtAt: new Date().toISOString(),
    projectRoot: project.projectRoot,
    agentRoot: project.agentRoot,
    compiledSlots,
    stubs: stubs.map((s) => ({
      entryId: s.entryId,
      catalogCgsHash: s.catalogCgsHash,
      outPath: path.relative(project.projectRoot, s.outPath),
    })),
    discovery: {
      agentRoot: discovery.agentRoot,
      catalogs: discovery.catalogs,
      skills: discovery.skills,
      channels: discovery.channels,
      schedules: discovery.schedules,
      hooks: discovery.hooks,
      subagents: discovery.subagents,
      diagnostics: discovery.diagnostics,
    },
    engine: {
      native,
      mode: native ? "napi" : "stub",
    },
  };
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");

  const packageName = await readPackageName(project.projectRoot);
  const appBuild = await buildPlasmApplication({
    projectRoot: project.projectRoot,
    agentRoot: project.agentRoot,
    discovery,
    compiledSlots,
    packageName: packageName ?? undefined,
  });

  return {
    stubsDir: path.join(project.agentRoot, ".plasm", "stubs"),
    discoveryDir,
    manifestPath,
    stubs,
    outputDir: appBuild.outputDir,
    agentSummaryPath: appBuild.agentSummaryPath,
    vercelOutput: appBuild.vercelOutput,
  };
}

export { readBuildManifest } from "./build-manifest.js";
export type { PlasmBuildManifest } from "./build-manifest.js";
