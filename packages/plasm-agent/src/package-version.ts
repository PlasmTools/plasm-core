import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

let cachedVersion: string | undefined;

/** Published semver from @plasm_lang/vercel-agent package.json. */
export function frameworkPackageVersion(): string {
  if (cachedVersion) return cachedVersion;
  const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const raw = readFileSync(path.join(packageRoot, "package.json"), "utf8");
  cachedVersion = (JSON.parse(raw) as { version: string }).version;
  return cachedVersion;
}
