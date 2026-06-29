import path from "node:path";
import { mkdir, writeFile } from "node:fs/promises";

const WORKFLOW_DISPATCH_ROUTE = "/internal/workflow/dispatch";

/** Emit workflow dispatch route for durable steps (no Plasm imports in workflow bundles). */
export async function writeWorkflowDispatchRoute(projectRoot: string): Promise<string> {
  const routesDir = path.join(projectRoot, ".plasm", "nitro", "routes", "internal", "workflow");
  await mkdir(routesDir, { recursive: true });
  const routePath = path.join(routesDir, "dispatch.post.ts");
  const source = `import path from "node:path";
import type { IncomingMessage, ServerResponse } from "node:http";

import agentDefinition from "../../../../../agent/agent.js";
import { createPlasmApp } from "@plasm_lang/vercel-agent/server";

const agentRoot = path.join(process.cwd(), "agent");

type NitroNodeEvent = {
  node: { req: IncomingMessage; res: ServerResponse };
};

async function readJson(req: IncomingMessage): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(Buffer.from(chunk));
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) return {};
  return JSON.parse(raw) as Record<string, unknown>;
}

export default async (event: NitroNodeEvent) => {
  const { req, res } = event.node;
  if ((req.method ?? "GET").toUpperCase() !== "POST") {
    res.statusCode = 405;
    res.end("Method Not Allowed");
    return;
  }

  const body = await readJson(req);
  const job = String(body.job ?? "");
  const force = body.force === true;

  if (job !== "mcp-radar-scan") {
    res.statusCode = 404;
    res.setHeader("content-type", "application/json; charset=utf-8");
    res.end(JSON.stringify({ error: "unknown_job", job }));
    return;
  }

  const app = await createPlasmApp({
    agentRoot,
    definition: agentDefinition,
    mode: "prod",
    sessions: false,
  });
  const { runRadar } = await import("../../../../../lib/run-radar.js");
  const result = await runRadar(app.getAuthoringContext(), { force });

  res.statusCode = 200;
  res.setHeader("content-type", "application/json; charset=utf-8");
  res.end(JSON.stringify(result));
};
`;
  await writeFile(routePath, source, "utf8");
  return WORKFLOW_DISPATCH_ROUTE;
}

export { WORKFLOW_DISPATCH_ROUTE };
