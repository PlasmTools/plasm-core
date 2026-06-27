import { build, copyPublicAssets, prepare, prerender } from "nitro/builder";
import type { Nitro } from "nitro/types";

export async function buildNitroOutput(nitro: Nitro): Promise<string> {
  await prepare(nitro);
  await copyPublicAssets(nitro);
  await prerender(nitro);
  await build(nitro);
  const outDir = nitro.options.output.dir;
  if (!outDir) {
    throw new Error("Nitro build completed without output.dir");
  }
  return outDir;
}
