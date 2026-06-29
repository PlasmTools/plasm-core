import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import * as esbuild from "esbuild";

import type { ProjectDiscovery } from "../discovery/project-walker.js";

const ESBUILD_BANNER =
  'import { createRequire as __createRequire } from "module"; const require = __createRequire(import.meta.url);';

export type CompiledSlotMap = Record<string, string>;

export interface CompileAuthoredSlotsResult {
  /** agentRoot-relative source path → projectRoot-relative compiled `.mjs`. */
  compiledSlots: CompiledSlotMap;
  compiledDir: string;
}

function slotFiles(
  discovery: ProjectDiscovery,
): Array<{ filePath: string }> {
  const out: Array<{ filePath: string }> = [];
  for (const file of discovery.channels) {
    if (file.kind === "typescript") out.push({ filePath: file.path });
  }
  for (const file of discovery.schedules) {
    if (file.kind === "typescript") out.push({ filePath: file.path });
  }
  for (const file of discovery.hooks) {
    if (file.kind === "typescript") out.push({ filePath: file.path });
  }
  return out;
}

/** Compile authored TS slots to `.plasm/compiled/` for Vercel runtime (no dynamic `.ts` import). */
export async function compileAuthoredSlots(
  projectRoot: string,
  agentRoot: string,
  discovery: ProjectDiscovery,
): Promise<CompileAuthoredSlotsResult> {
  const compiledRoot = path.join(agentRoot, ".plasm", "compiled");
  await mkdir(compiledRoot, { recursive: true });

  const compiledSlots: CompiledSlotMap = {};

  await Promise.all(
    slotFiles(discovery).map(async ({ filePath }) => {
      const relFromAgent = path.relative(agentRoot, filePath);
      const outFile = path.join(
        agentRoot,
        ".plasm",
        "compiled",
        relFromAgent.replace(/\.ts$/, ".mjs"),
      );
      await mkdir(path.dirname(outFile), { recursive: true });

      await esbuild.build({
        entryPoints: [filePath],
        bundle: true,
        platform: "node",
        target: "node22",
        format: "esm",
        outfile: outFile,
        banner: { js: ESBUILD_BANNER },
        external: [
          "@plasm_lang/vercel-agent",
          "@plasm_lang/vercel-agent/*",
          "@plasm_lang/engine",
          "@vercel/functions",
          "@vercel/blob",
          "workflow",
          "workflow/api",
        ],
        logLevel: "silent",
      });

      compiledSlots[relFromAgent] = path.relative(projectRoot, outFile);
    }),
  );

  await writeFile(
    path.join(compiledRoot, "package.json"),
    `${JSON.stringify({ type: "module" }, null, 2)}\n`,
    "utf8",
  );

  return { compiledSlots, compiledDir: compiledRoot };
}
