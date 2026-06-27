import { compileAuthoredSlots } from "./compile-authored-slots.js";
import { walkAgentProject } from "../discovery/project-walker.js";
import { generateAllStubs } from "../stubs/generator.js";
import { startPlasmNitroDev } from "../nitro/build-application.js";
import type { ResolvedAgentProject } from "./project-root.js";

export async function startNitroDevForProject(project: ResolvedAgentProject): Promise<void> {
  await generateAllStubs(project.agentRoot);
  const discovery = await walkAgentProject(project.agentRoot);
  const { compiledSlots } = await compileAuthoredSlots(
    project.projectRoot,
    project.agentRoot,
    discovery,
  );

  const server = await startPlasmNitroDev({
    projectRoot: project.projectRoot,
    agentRoot: project.agentRoot,
    discovery,
    compiledSlots,
  });

  await new Promise<void>((resolve) => {
    const shutdown = () => {
      void server.close().finally(() => resolve());
    };
    process.on("SIGINT", shutdown);
    process.on("SIGTERM", shutdown);
  });
}
