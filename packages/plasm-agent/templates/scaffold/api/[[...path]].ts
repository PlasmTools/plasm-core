import path from "node:path";
import { fileURLToPath } from "node:url";

import agentDefinition from "../agent/agent.js";
import { createPlasmApp, vercelPlasmHandler } from "@plasm_lang/vercel-agent/server";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");

let app: Awaited<ReturnType<typeof createPlasmApp>> | undefined;

export default async function handler(
  req: import("node:http").IncomingMessage,
  res: import("node:http").ServerResponse,
): Promise<void> {
  app ??= await createPlasmApp({
    agentRoot,
    definition: agentDefinition,
    mode: "prod",
    sessions: false,
  });
  await vercelPlasmHandler(app)(req, res);
}
