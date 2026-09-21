import http from "node:http";
import { timingSafeEqual } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { IMAGE_PIN_RE, MAX_INPUT_BYTES, runSandboxTransform, validateTransformRequest } from "../src/tools/artifact-sandbox.mjs";
const token = process.env.PLASM_ARTIFACT_TOKEN;
const image = process.env.PLASM_ARTIFACT_IMAGE;
if (!token || !IMAGE_PIN_RE.test(image ?? "")) throw Error("Broker requires token and pinned image");
let active = 0;
const server = http.createServer(async (req, res) => {
  const given = Buffer.from(req.headers.authorization ?? "");
  const expected = Buffer.from(`Bearer ${token}`);
  const reply = (status, value) => { res.writeHead(status, {"content-type":"application/json"}); res.end(JSON.stringify(value)); };
  if (given.length !== expected.length || !timingSafeEqual(given, expected)) return reply(401, {error:"Unauthorized"});
  if (req.url !== "/transform" || req.method !== "POST") return reply(404, {error:"Unknown operation"});
  if (active >= 10) return reply(429, {error:"Artifact worker capacity reached; retry after active transforms complete"});
  active++;
  try {
    const chunks = []; let size = 0;
    for await (const chunk of req) { size += chunk.length; if (size > MAX_INPUT_BYTES) throw Error("Artifact inputs exceed 32 MiB"); chunks.push(chunk); }
    const request = validateTransformRequest(JSON.parse(Buffer.concat(chunks).toString()));
    reply(200, {result: await runSandboxTransform(request, image)});
  } catch (error) { reply(400, {error: error.message}); }
  finally { active--; }
});
server.requestTimeout = 55000;
server.listen(0, "0.0.0.0", async () => {
  await writeFile(process.argv[2], JSON.stringify({port:server.address().port}), {mode:0o600});
});
for (const signal of ["SIGTERM", "SIGINT"]) process.on(signal, () => server.close(() => process.exit(0)));
