import { createRequire } from "node:module";
import { cp, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

import { isVercelBuildEnvironment } from "./paths.js";

const ENGINE_PACKAGES = [
  "@plasm_lang/engine",
  "@plasm_lang/engine-linux-x64-gnu",
  "@plasm_lang/engine-linux-x64-musl",
  "@plasm_lang/engine-darwin-arm64",
  "@plasm_lang/engine-darwin-x64",
] as const;

const VERCEL_ENGINE_PACKAGES = [
  "@plasm_lang/engine",
  "@plasm_lang/engine-linux-x64-gnu",
  "@plasm_lang/engine-linux-x64-musl",
] as const;

function scopedPackageDest(funcDir: string, pkg: string): string {
  const slash = pkg.indexOf("/");
  const scope = pkg.slice(0, slash);
  const name = pkg.slice(slash + 1);
  return path.join(funcDir, "node_modules", scope, name);
}

/** Copy native engine packages into the traced Vercel function `node_modules`. */
export async function copyNativeEnginePackages(
  projectRoot: string,
  funcDir: string,
): Promise<void> {
  const require = createRequire(path.join(projectRoot, "package.json"));

  let enginePkgJson: string;
  try {
    enginePkgJson = require.resolve("@plasm_lang/engine/package.json");
  } catch {
    return;
  }

  const engineDir = path.dirname(enginePkgJson);
  const engineRequire = createRequire(path.join(engineDir, "package.json"));
  const packagesToCopy = isVercelBuildEnvironment()
    ? [...VERCEL_ENGINE_PACKAGES]
    : [...ENGINE_PACKAGES];
  const copied: Array<{ pkg: string; version: string; dest: string }> = [];

  for (const pkg of packagesToCopy) {
    let pkgJsonPath: string;
    try {
      pkgJsonPath =
        pkg === "@plasm_lang/engine"
          ? enginePkgJson
          : engineRequire.resolve(`${pkg}/package.json`);
    } catch {
      continue;
    }

    const srcDir = path.dirname(pkgJsonPath);
    const destDir = scopedPackageDest(funcDir, pkg);
    await mkdir(path.dirname(destDir), { recursive: true });
    await cp(srcDir, destDir, { recursive: true, force: true });

    const raw = await readFile(pkgJsonPath, "utf8");
    const version = (JSON.parse(raw) as { version?: string }).version ?? "0.0.0";
    copied.push({ pkg, version, dest: destDir });
  }

  if (!copied.some((entry) => entry.pkg === "@plasm_lang/engine")) {
    return;
  }

  const funcPackagePath = path.join(funcDir, "package.json");
  let funcPackage: { dependencies?: Record<string, string> } = {};
  try {
    funcPackage = JSON.parse(await readFile(funcPackagePath, "utf8")) as {
      dependencies?: Record<string, string>;
    };
  } catch {
    funcPackage = { type: "module", private: true, dependencies: {} };
  }

  const dependencies = { ...(funcPackage.dependencies ?? {}) };
  for (const { pkg, version } of copied) {
    dependencies[pkg] = version;
  }
  await writeFile(
    funcPackagePath,
    `${JSON.stringify({ ...funcPackage, dependencies }, null, 2)}\n`,
    "utf8",
  );
}
