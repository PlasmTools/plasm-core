import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { CompiledSlotMap } from "../cli/compile-authored-slots.js";
import { plasmNitroRoutesDir } from "./paths.js";

function slotImportId(relFromAgent: string): string {
  return relFromAgent.replace(/[^a-zA-Z0-9]+/g, "_");
}

/** Generate Nitro catch-all route that boots PlasmApp (eve-style programmatic host). */
export async function writePlasmNitroCatchAllRoute(
  projectRoot: string,
  compiledSlots: CompiledSlotMap = {},
): Promise<string> {
  const routesDir = plasmNitroRoutesDir(projectRoot);
  await mkdir(routesDir, { recursive: true });
  const routePath = path.join(routesDir, "[[path]].ts");

  const slotImports = Object.entries(compiledSlots)
    .map(([relFromAgent, projectRelOut]) => {
      const id = slotImportId(relFromAgent);
      const importPath = `../../../${projectRelOut.replace(/\\/g, "/")}`;
      return `import __slot_${id} from "${importPath}";`;
    })
    .join("\n");

  const slotEntries = Object.keys(compiledSlots)
    .map((relFromAgent) => {
      const id = slotImportId(relFromAgent);
      return `  ${JSON.stringify(relFromAgent)}: __slot_${id},`;
    })
    .join("\n");

  const preloadBlock =
    Object.keys(compiledSlots).length > 0
      ? `
${slotImports}

(globalThis as typeof globalThis & {
  __PLASM_PRELOADED_SLOTS?: Record<string, unknown>;
}).__PLASM_PRELOADED_SLOTS = {
${slotEntries}
};
`
      : "";

  const source = `import path from "node:path";
import type { IncomingMessage, ServerResponse } from "node:http";

import agentDefinition from "../../../agent/agent.js";
import { createPlasmApp, vercelPlasmHandler } from "@plasm_lang/vercel-agent/server";

import "../../../agent/instrumentation.js";
${preloadBlock}
const agentRoot = path.join(process.cwd(), "agent");

let appPromise: ReturnType<typeof createPlasmApp> | undefined;

async function plasmApp() {
  appPromise ??= createPlasmApp({
    agentRoot,
    definition: agentDefinition,
    mode: "prod",
    sessions: false,
  });
  return appPromise;
}

/** Nitro wraps routes with \`toEventHandler\` — node req/res live on \`event.node\`. */
type NitroNodeEvent = {
  node: { req: IncomingMessage; res: ServerResponse };
};

export default async (event: NitroNodeEvent) => {
  const app = await plasmApp();
  const { req, res } = event.node;
  await new Promise<void>((resolve, reject) => {
    res.once("finish", () => resolve());
    res.once("error", reject);
    void vercelPlasmHandler(app)(req, res).catch(reject);
  });
};
`;
  await writeFile(routePath, source, "utf8");
  return routePath;
}
