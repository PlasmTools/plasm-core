import path from "node:path";

import { fromNodeMiddleware } from "h3";

import agentDefinition from "../agent/agent.js";
import { createPlasmApp, vercelPlasmHandler } from "@plasm_lang/vercel-agent";

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

export default fromNodeMiddleware(async (req, res) => {
  const app = await plasmApp();
  await new Promise<void>((resolve, reject) => {
    res.once("finish", () => resolve());
    res.once("error", reject);
    void vercelPlasmHandler(app)(req, res).catch(reject);
  });
});
