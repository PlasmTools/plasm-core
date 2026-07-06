import type { IncomingMessage, ServerResponse } from "node:http";

import { waitUntil } from "@vercel/functions";
import { defineChannel } from "@plasm_lang/vercel-agent";

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
        const body = (await readJsonBody(req)) as { force?: boolean; reset?: boolean };
        const options = {
          force: body.force === true,
          reset: body.reset === true,
          channelKind: "channel:mcp-radar" as const,
        };

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
      method: "POST",
      path: "/channel/mcp-radar/reset",
      handler: async (_req, res, _ctx) => {
        sendJson(res, 200, {
          ok: true,
          message: 'Trigger POST /channel/mcp-radar/run with {"reset":true}',
        });
      },
    },
    {
      method: "GET",
      path: "/channel/mcp-radar/proof",
      handler: async (_req, res, _ctx) => {
        sendJson(res, 200, {
          note: "Proof document lives in the proof catalog; read/write via agent Plasm session.",
          sessions: "/operator/sessions",
          runs: "/operator/runs",
        });
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
