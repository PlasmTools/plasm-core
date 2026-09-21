import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";
const execute = promisify(execFile);
let cleanupFailed = false;
export const IMAGE_PIN_RE = /^[-a-zA-Z0-9./_:]+@sha256:[a-f0-9]{64}$/;
export const MAX_INPUT_BYTES = 32 * 1024 * 1024;
export const MAX_RESULT_BYTES = 8192;
export function validateTransformRequest(value) {
  if (!value || typeof value !== "object" || Object.keys(value).some(k => !["code", "artifacts"].includes(k)) || typeof value.code !== "string" || !value.code.trim() || Buffer.byteLength(value.code) > 128 * 1024 || !Array.isArray(value.artifacts) || !value.artifacts.length || value.artifacts.length > 16)
    throw Error("Expected a TypeScript function and 1–16 JSON artifacts");
  if (Buffer.byteLength(JSON.stringify(value)) > MAX_INPUT_BYTES) throw Error("Artifact inputs exceed 32 MiB");
  return value;
}
/** The Docker container, not a JS VM, is the security boundary. */
export async function runSandboxTransform(request, image) {
  validateTransformRequest(request);
  if (!IMAGE_PIN_RE.test(image ?? "")) throw Error("Artifact image must be pinned by digest");
  if (cleanupFailed) throw Error("Artifact sandbox disabled after cleanup failure");
  const root = await mkdtemp(path.join(tmpdir(), "plasm-transform-"));
  const name = `plasm-artifact-${randomUUID()}`;
  try {
    await writeFile(path.join(root, "transform.ts"), request.code);
    await writeFile(path.join(root, "input.json"), JSON.stringify(request.artifacts));
    await writeFile(path.join(root, "runner.mjs"), `import fs from 'node:fs/promises';
const emit = process.stdout.write.bind(process.stdout);
console.log = () => { throw Error('Return a bounded result instead of printing artifacts'); };
const {default: transform} = await import('./transform.ts');
if (typeof transform !== 'function') throw Error('Export a default TypeScript function');
const result = await transform(JSON.parse(await fs.readFile('/input/input.json', 'utf8')));
const text = JSON.stringify(result);
if (text === undefined) throw Error('Return a JSON value');
if (Buffer.byteLength(text) > ${MAX_RESULT_BYTES}) throw Error('Derived result exceeds 8192 bytes; return a smaller summary');
emit(text);`);
    const { stdout } = await execute("docker", ["run", "--rm", "--pull=never", "--name", name,
      "--network=none", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
      "--memory=128m", "--memory-swap=128m", "--cpus=1", "--pids-limit=64", "--ipc=none",
      "--tmpfs", "/tmp:rw,noexec,nosuid,size=8m",
      "--mount", `type=bind,src=${root},dst=/input,readonly`, "--workdir=/input",
      "--entrypoint=timeout", image, "-s", "KILL", "30", "node", "--experimental-strip-types", "/input/runner.mjs"],
      { timeout: 35000, killSignal: "SIGKILL", maxBuffer: MAX_RESULT_BYTES + 4096 });
    if (Buffer.byteLength(stdout) > MAX_RESULT_BYTES) throw Error("Derived result exceeds 8192 bytes");
    JSON.parse(stdout);
    return stdout;
  } catch (error) {
    throw Error(error.killed || error.code === 137 ? "Artifact execution exceeded its resource limit"
      : `Artifact execution failed: ${(error.stderr ?? error.message).slice(0, 2000)}`);
  } finally {
    try {
      await execute("docker", ["rm", "--force", name], { timeout: 10000 }).catch(error => {
        if (!String(error.stderr).includes("No such container")) { cleanupFailed = true; throw Error("Artifact container cleanup failed"); }
      });
    } finally { await rm(root, { recursive: true, force: true }); }
  }
}
