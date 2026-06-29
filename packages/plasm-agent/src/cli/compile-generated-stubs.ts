import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import * as esbuild from "esbuild";

import type { StubGenerationResult } from "../stubs/generator.js";

const ESBUILD_BANNER =
  'import { createRequire as __createRequire } from "module"; const require = __createRequire(import.meta.url);';

const STUB_RUNTIME_SOURCE = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "../stubs/stub-runtime.ts",
);

const STUB_RUNTIME_BASENAME = "_stub-runtime.mjs";

/** Native engine stays external; runtime is a shared sibling module on Vercel. */
const STUB_RUNTIME_EXTERNAL = ["@plasm_lang/engine"];

export interface CompileGeneratedStubsResult {
  /** entry_id → projectRoot-relative compiled `.mjs` path */
  compiledStubs: Record<string, string>;
}

function stubRuntimeExternalPlugin(): esbuild.Plugin {
  return {
    name: "stub-runtime-sibling",
    setup(build) {
      build.onResolve({ filter: /^\.\/_stub-runtime\.js$/ }, () => ({
        path: "./_stub-runtime.mjs",
        external: true,
      }));
    },
  };
}

async function compileStubRuntime(stubsDir: string): Promise<string> {
  const runtimePath = path.join(stubsDir, STUB_RUNTIME_BASENAME);
  await esbuild.build({
    entryPoints: [STUB_RUNTIME_SOURCE],
    bundle: true,
    platform: "node",
    target: "node22",
    format: "esm",
    outfile: runtimePath,
    banner: { js: ESBUILD_BANNER },
    external: STUB_RUNTIME_EXTERNAL,
    logLevel: "silent",
  });
  return runtimePath;
}

/** Compile generated CGS stubs to `.mjs` for serverless dynamic import. */
export async function compileGeneratedStubs(
  projectRoot: string,
  agentRoot: string,
  stubs: StubGenerationResult[],
): Promise<CompileGeneratedStubsResult> {
  const stubsDir = path.join(agentRoot, ".plasm", "stubs");
  await mkdir(stubsDir, { recursive: true });
  await compileStubRuntime(stubsDir);

  const compiledStubs: Record<string, string> = {};

  await Promise.all(
    stubs.map(async (stub) => {
      const entryId = stub.entryId;
      const tsPath = stub.outPath;
      const mjsPath = tsPath.replace(/\.ts$/, ".mjs");

      await esbuild.build({
        entryPoints: [tsPath],
        bundle: true,
        platform: "node",
        target: "node22",
        format: "esm",
        outfile: mjsPath,
        banner: { js: ESBUILD_BANNER },
        external: STUB_RUNTIME_EXTERNAL,
        plugins: [stubRuntimeExternalPlugin()],
        logLevel: "silent",
      });

      compiledStubs[entryId] = path.relative(projectRoot, mjsPath);
    }),
  );

  await writeFile(
    path.join(stubsDir, "package.json"),
    `${JSON.stringify({ type: "module" }, null, 2)}\n`,
    "utf8",
  );

  return { compiledStubs };
};
