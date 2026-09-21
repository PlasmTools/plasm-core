import { execFileSync } from "node:child_process";
import { lstat, readFile, realpath } from "node:fs/promises";
import path from "node:path";
import { IMAGE_PIN_RE, MAX_INPUT_BYTES, runSandboxTransform, validateTransformRequest } from "./artifact-sandbox.mjs";
export const ARTIFACT_IMAGE_PIN_RE = IMAGE_PIN_RE;
export function pinnedArtifactImage(image = process.env.PLASM_ARTIFACT_IMAGE): string | null {
  const value = image?.trim() ?? "";
  return IMAGE_PIN_RE.test(value) ? value : null;
}
export function artifactRuntimeAvailable(): boolean {
  return Boolean((process.env.PLASM_ARTIFACT_ENDPOINT && process.env.PLASM_ARTIFACT_TOKEN) || pinnedArtifactImage());
}
/** Node 22+ supplies native TypeScript stripping. Never pull implicitly. */
export function pinLocalArtifactImageSync(): string | null {
  const current = pinnedArtifactImage();
  if (current) return current;
  for (const name of ["node:22-alpine", "node:22-bookworm-slim", "node:24-alpine", "node:24-bookworm-slim"]) {
    try {
      const image = execFileSync("docker", ["image", "inspect", "--format", "{{index .RepoDigests 0}}", name],
        { encoding: "utf8", timeout: 5000, stdio: ["ignore", "pipe", "ignore"] }).trim();
      if (pinnedArtifactImage(image)) { process.env.PLASM_ARTIFACT_IMAGE = image; return image; }
    } catch { /* Local candidate absent. */ }
  }
  return null;
}
async function readArtifact(root: string, relative: string, remaining: number): Promise<{ value: unknown; bytes: number }> {
  if (path.isAbsolute(relative) || relative.split(/[\\/]/).some(part => !part || part === ".." || part === ".")) throw Error("Artifact path must be workspace-relative");
  let cursor = root;
  for (const part of relative.split(/[\\/]/)) {
    cursor = path.join(cursor, part);
    if ((await lstat(cursor)).isSymbolicLink()) throw Error("Artifact symlinks are not allowed");
  }
  const stat = await lstat(cursor);
  if (!stat.isFile() || stat.size > remaining) throw Error("Artifact must be a JSON file below 32 MiB");
  const body = await readFile(cursor, "utf8");
  const bytes = Buffer.byteLength(body);
  if (bytes > remaining) throw Error("Artifact inputs exceed 32 MiB");
  return { value: JSON.parse(body), bytes };
}
/** Only named artifacts are transferred; no host paths or credentials reach the sandbox. */
export async function runArtefactTransform(workspaceRoot: string, code: string, paths: string[]): Promise<string> {
  if (!paths.length || paths.length > 16) throw Error("Choose 1–16 artifact paths");
  const root = await realpath(workspaceRoot);
  const artifacts: unknown[] = [];
  let remaining = MAX_INPUT_BYTES;
  for (const locator of paths) {
    const input = await readArtifact(root, locator, remaining);
    artifacts.push(input.value);
    remaining -= input.bytes;
  }
  const request = validateTransformRequest({ code, artifacts });
  const endpoint = process.env.PLASM_ARTIFACT_ENDPOINT;
  if (endpoint) {
    const token = process.env.PLASM_ARTIFACT_TOKEN;
    if (!token) throw Error("Artifact broker token is missing");
    const response = await fetch(endpoint, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${token}` }, body: JSON.stringify(request), signal: AbortSignal.timeout(50000) });
    const result = await response.json() as { result?: string; error?: string };
    if (!response.ok || typeof result.result !== "string") throw Error(result.error ?? "Artifact broker failed");
    if (Buffer.byteLength(result.result) > 8192) throw Error("Derived result exceeds 8192 bytes");
    JSON.parse(result.result);
    return result.result;
  }
  const image = pinnedArtifactImage();
  if (!image) throw Error("Artifact execution requires a pinned Node 22+ image or an authenticated artifact broker");
  return runSandboxTransform(request, image);
}
