import path from "node:path";

import type { CompiledSlotMap } from "../cli/compile-authored-slots.js";
import { loadAuthoredSlots } from "../authoring/slot-loader.js";
import type { ProjectDiscovery } from "../discovery/project-walker.js";
import { buildPlasmAgentSummary, emitPlasmAgentSummary } from "./agent-summary.js";
import { buildNitroOutput } from "./build-nitro-output.js";
import { createPlasmNitro } from "./create-plasm-nitro.js";
import { patchVercelOutputConfig } from "./patch-vercel-config.js";
import { copyVercelFunctionAssets } from "./copy-vercel-function-assets.js";
import { isVercelBuildEnvironment, vercelOutputDir } from "./paths.js";
import { preparePlasmHost } from "./prepare-host.js";
import { writePlasmNitroCatchAllRoute } from "./write-nitro-entry.js";
import { ensureWorkflowBuilderDirs } from "./ensure-workflow-builder-dirs.js";
import { writeNitroScheduleTasks } from "./write-nitro-schedule-tasks.js";
import { writeWorkflowDispatchRoute } from "./write-workflow-dispatch-route.js";

export interface PlasmApplicationBuildResult {
  outputDir: string;
  agentSummaryPath: string;
  vercelOutput: boolean;
}

export async function buildPlasmApplication(options: {
  projectRoot: string;
  agentRoot: string;
  discovery: ProjectDiscovery;
  compiledSlots: CompiledSlotMap;
  packageName?: string;
}): Promise<PlasmApplicationBuildResult> {
  const { projectRoot, agentRoot, discovery, compiledSlots, packageName } = options;

  const host = await preparePlasmHost({
    projectRoot,
    agentRoot,
    compiledSlots,
    discovery,
  });

  await writePlasmNitroCatchAllRoute(projectRoot, compiledSlots);

  const loadedSlotsForTasks = await loadAuthoredSlots({
    discovery,
    agentRoot,
    projectRoot,
    compiledSlots,
  });
  if (loadedSlotsForTasks.schedules.length > 0) {
    await writeNitroScheduleTasks({
      projectRoot,
      agentRoot,
      schedules: loadedSlotsForTasks.schedules,
      compiledSlots,
    });
  }

  if (host.workflowEnabled) {
    await ensureWorkflowBuilderDirs(projectRoot);
    await writeWorkflowDispatchRoute(projectRoot);
  }

  const nitro = await createPlasmNitro(host, false);
  try {
    const outputDir = await buildNitroOutput(nitro);

    if (isVercelBuildEnvironment()) {
      await copyVercelFunctionAssets({ projectRoot, agentRoot });
      await patchVercelOutputConfig(projectRoot, host);
    }

    const loadedSlots = await loadAuthoredSlots({
      discovery,
      agentRoot,
      projectRoot,
      compiledSlots,
    });

    const summary = buildPlasmAgentSummary({
      projectRoot,
      agentRoot,
      packageName,
      definition: host.definition,
      discovery,
      loadedSlots,
    });
    const agentSummaryPath = await emitPlasmAgentSummary({ projectRoot, summary });

    const vercelOutput = isVercelBuildEnvironment();
    const resolvedOutput = vercelOutput ? vercelOutputDir(projectRoot) : outputDir;

    return {
      outputDir: resolvedOutput,
      agentSummaryPath,
      vercelOutput,
    };
  } finally {
    await nitro.close();
  }
}

export async function startPlasmNitroDev(options: {
  projectRoot: string;
  agentRoot: string;
  discovery: ProjectDiscovery;
  compiledSlots: CompiledSlotMap;
}): Promise<Awaited<ReturnType<typeof import("nitro/builder").createDevServer>>> {
  const { createDevServer } = await import("nitro/builder");

  const host = await preparePlasmHost(options);
  await writePlasmNitroCatchAllRoute(options.projectRoot, options.compiledSlots);

  const loadedSlotsForTasks = await loadAuthoredSlots({
    discovery: options.discovery,
    agentRoot: options.agentRoot,
    projectRoot: options.projectRoot,
    compiledSlots: options.compiledSlots,
  });
  if (loadedSlotsForTasks.schedules.length > 0) {
    await writeNitroScheduleTasks({
      projectRoot: options.projectRoot,
      agentRoot: options.agentRoot,
      schedules: loadedSlotsForTasks.schedules,
      compiledSlots: options.compiledSlots,
    });
  }

  if (host.workflowEnabled) {
    await ensureWorkflowBuilderDirs(options.projectRoot);
    await writeWorkflowDispatchRoute(options.projectRoot);
  }

  const nitro = await createPlasmNitro(host, true);
  const server = createDevServer(nitro);

  const port = Number(process.env.PORT ?? 3000);
  const listenHost = process.env.HOST ?? "127.0.0.1";
  server.listen({ port, hostname: listenHost });
  console.log(`[plasm-agent] Nitro dev → http://${listenHost}:${port}`);

  return server;
}
