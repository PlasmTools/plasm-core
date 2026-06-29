import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { CompiledSlotMap } from "../cli/compile-authored-slots.js";
import type { LoadedSchedule } from "../authoring/slot-loader.js";
import { PLASM_NITRO_BUILD_DIR } from "./paths.js";

function compiledScheduleImport(
  schedule: LoadedSchedule,
  agentRoot: string,
  compiledSlots: CompiledSlotMap,
  projectRoot: string,
): string {
  const relFromAgent = path.relative(agentRoot, schedule.sourcePath);
  const compiled = compiledSlots[relFromAgent];
  if (!compiled) {
    throw new Error(
      `missing compiled schedule slot for ${schedule.definition.name} (${relFromAgent})`,
    );
  }
  const relImport = path
    .relative(path.join(projectRoot, PLASM_NITRO_BUILD_DIR, "tasks"), path.join(projectRoot, compiled))
    .replace(/\\/g, "/");
  return relImport.startsWith(".") ? relImport : `./${relImport}`;
}

/** Emit Nitro scheduled tasks (Eve-aligned — no HTTP /internal/cron routes). */
export async function writeNitroScheduleTasks(options: {
  projectRoot: string;
  agentRoot: string;
  schedules: LoadedSchedule[];
  compiledSlots: CompiledSlotMap;
}): Promise<void> {
  const { projectRoot, agentRoot, schedules, compiledSlots } = options;
  const tasksDir = path.join(projectRoot, PLASM_NITRO_BUILD_DIR, "tasks");
  await mkdir(tasksDir, { recursive: true });

  for (const schedule of schedules) {
    const name = schedule.definition.name;
    const importPath = compiledScheduleImport(schedule, agentRoot, compiledSlots, projectRoot);
    const source = `import path from "node:path";

import agentDefinition from "../../../agent/agent.js";
import { createPlasmApp } from "@plasm_lang/vercel-agent/server";
import scheduleSlot from "${importPath}";

const agentRoot = path.join(process.cwd(), "agent");

export default {
  meta: {
    description: ${JSON.stringify(`Plasm schedule: ${name}`)},
  },
  async run() {
    const app = await createPlasmApp({
      agentRoot,
      definition: agentDefinition,
      mode: "prod",
      sessions: false,
    });
    const definition = scheduleSlot.default ?? scheduleSlot;
    await definition.handler(app.getAuthoringContext());
  },
};
`;
    await writeFile(path.join(tasksDir, `${name}.ts`), source, "utf8");
  }
}
