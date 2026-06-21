import { createFixtureMockTransport } from "../../src/engine/fixture-mock-transport.js";
import { defineChannel } from "../../src/authoring/define-channel.js";
import { dryRunProgram } from "../../src/stubs/catalog-client.js";

import executeTiny from "../.plasm/stubs/execute_tiny.js";

function readJsonBody(req: import("node:http").IncomingMessage): Promise<unknown> {
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

export default defineChannel({
  name: "execute-tiny-webhook",
  routes: [
    {
      method: "POST",
      path: "/channel/execute-tiny/products",
      handler: async (req, res) => {
        const body = (await readJsonBody(req)) as { dryRun?: boolean };
        const useDryRun = body.dryRun !== false;

        if (useDryRun) {
          const dry = await dryRunProgram(executeTiny.builder, "e2");
          res.statusCode = 200;
          res.setHeader("content-type", "application/json; charset=utf-8");
          res.end(
            JSON.stringify({
              mode: "dry_run",
              planCommitRef: dry.planCommitRef,
            }),
          );
          return;
        }

        const mock = createFixtureMockTransport();
        const rows = await executeTiny.Product.product_list({ transport: mock });
        res.statusCode = 200;
        res.setHeader("content-type", "application/json; charset=utf-8");
        res.end(JSON.stringify({ mode: "live", count: rows.length, rows }));
      },
    },
  ],
});
