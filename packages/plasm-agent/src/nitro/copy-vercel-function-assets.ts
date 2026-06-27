import { access, cp } from "node:fs/promises";
import path from "node:path";

import { vercelOutputDir } from "./paths.js";

async function pathExists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

/** Copy agent project files into the Vercel serverless bundle (runtime slot discovery). */
export async function copyVercelFunctionAssets(options: {
  projectRoot: string;
  agentRoot: string;
}): Promise<void> {
  const funcDir = path.join(vercelOutputDir(options.projectRoot), "functions", "__server.func");
  if (!(await pathExists(funcDir))) {
    return;
  }

  await cp(options.agentRoot, path.join(funcDir, "agent"), { recursive: true });

  const libDir = path.join(options.projectRoot, "lib");
  if (await pathExists(libDir)) {
    await cp(libDir, path.join(funcDir, "lib"), { recursive: true });
  }
}
