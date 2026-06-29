import type { IncomingMessage, ServerResponse } from "node:http";

import { waitUntil } from "@vercel/functions";
import { defineChannel } from "@plasm_lang/vercel-agent";

import { readProofMarkdown } from "../../lib/proof-store.js";
import { radarStatus } from "../../lib/run-radar.js";
import { startMcpRadarRun } from "../../lib/start-mcp-radar.js";

function readJsonBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let body = "";
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      if (!body.trim()) {
        resolve({});
        return;
      }
      try {
        resolve(JSON.parse(body));
      } catch (err) {
        reject(err);
      }
    });
    req.on("error", reject);
  });
}

function sendJson(res: ServerResponse, status: number, payload: unknown): void {
  res.statusCode = status;
  res.setHeader("content-type", "application/json; charset=utf-8");
  res.end(JSON.stringify(payload, null, 2));
}

export default defineChannel({
  name: "mcp-radar",
  routes: [
    {
      method: "POST",
      path: "/channel/mcp-radar/run",
      handler: async (req, res, ctx) => {
        const body = (await readJsonBody(req)) as { force?: boolean };
        const options = { force: body.force === true };

        if (process.env.VERCEL === "1") {
          waitUntil(
            startMcpRadarRun(ctx, options).catch((err: unknown) => {
              console.error("[mcp-radar] workflow start failed:", err);
            }),
          );
          sendJson(res, 202, { accepted: true, workflow: true });
          return;
        }

        const run = await startMcpRadarRun(ctx, options);
        sendJson(res, 200, { accepted: true, workflow: true, run });
      },
    },
    {
      method: "GET",
      path: "/channel/mcp-radar/proof",
      handler: async (req, res, ctx) => {
        const url = new URL(req.url ?? "/", "http://localhost");
        const format = url.searchParams.get("format");
        const markdown = await readProofMarkdown(ctx.agentRoot);
        if (format === "json") {
          sendJson(res, 200, { markdown, path: "agent/research/mcp-innovations-proof.md" });
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "text/markdown; charset=utf-8");
        res.end(markdown);
      },
    },
    {
      method: "GET",
      path: "/channel/mcp-radar/status",
      handler: async (_req, res, ctx) => {
        sendJson(res, 200, await radarStatus(ctx.agentRoot));
      },
    },
  ],
});
