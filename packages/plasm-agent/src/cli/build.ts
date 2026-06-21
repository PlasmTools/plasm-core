import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import { isNativeEngineAvailable } from "../engine/napi-binding.js";
import { walkAgentProject } from "../discovery/project-walker.js";
import { generateAllStubs } from "../stubs/generator.js";
import type { ResolvedAgentProject } from "./project-root.js";

export interface PlasmBuildResult {
  stubsDir: string;
  discoveryDir: string;
  manifestPath: string;
  stubs: Array<{ entryId: string; catalogCgsHash: string; outPath: string }>;
}

export async function runPlasmBuild(project: ResolvedAgentProject): Promise<PlasmBuildResult> {
  const stubs = await generateAllStubs(project.agentRoot);
  const discovery = await walkAgentProject(project.agentRoot);
  const discoveryDir = path.join(project.agentRoot, ".plasm", "discovery");
  await mkdir(discoveryDir, { recursive: true });
  const manifestPath = path.join(discoveryDir, "manifest.json");
  const native = isNativeEngineAvailable();
  const manifest = {
    builtAt: new Date().toISOString(),
    projectRoot: project.projectRoot,
    agentRoot: project.agentRoot,
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
  return {
    stubsDir: path.join(project.agentRoot, ".plasm", "stubs"),
    discoveryDir,
    manifestPath,
    stubs,
  };
}
