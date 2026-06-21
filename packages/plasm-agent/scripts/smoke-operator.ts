import path from "node:path";
import { fileURLToPath } from "node:url";

import { defineAgent } from "../src/define-agent.js";
import { createDevServer } from "../src/dev/server.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const def = defineAgent({ model: "vercel/google/gemini-2.5-flash" });
const handle = await createDevServer({
  agentRoot: path.join(packageRoot, "agent"),
  definition: def,
  port: 0,
});

const base = handle.url;
for (const route of ["/operator", "/operator/catalogs", "/operator/ops"]) {
  const res = await fetch(`${base}${route}`);
  const text = await res.text();
  console.log(route, res.status, text.slice(0, 100).replace(/\s+/g, " "));
}

await handle.close();
console.log("OK: operator routes");
