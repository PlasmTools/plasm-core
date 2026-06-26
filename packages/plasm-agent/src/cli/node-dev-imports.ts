import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);

/** NODE_OPTIONS imports for Nitro dev (tsx + Plasm .ts resolution). */
export function nitroDevNodeOptions(packageRoot: string): string {
  const tsxImport = require.resolve("tsx/esm");
  const plasmLoader = pathToFileURL(
    path.join(packageRoot, "scripts", "register-plasm-loader.mjs"),
  ).href;
  return [`--import=${tsxImport}`, `--import=${plasmLoader}`].join(" ");
}
