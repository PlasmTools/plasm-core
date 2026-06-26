import { spawn, type ChildProcess } from "node:child_process";
import { access } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { nitroDevNodeOptions } from "./node-dev-imports.js";
import type { ResolvedAgentProject } from "./project-root.js";

function plasmAgentPackageRoot(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
}

async function assertNitroScaffold(projectRoot: string): Promise<void> {
  const required = [
    path.join(projectRoot, "nitro.config.ts"),
    path.join(projectRoot, "routes", "[...path].ts"),
  ];
  for (const filePath of required) {
    try {
      await access(filePath);
    } catch {
      throw new Error(
        `Missing ${path.relative(projectRoot, filePath)} — run \`plasm-agent init\` to scaffold Nitro dev`,
      );
    }
  }
}

function nitroBin(projectRoot: string): string {
  return path.join(projectRoot, "node_modules", ".bin", "nitro");
}

export async function startNitroDevForProject(project: ResolvedAgentProject): Promise<void> {
  await assertNitroScaffold(project.projectRoot);

  const nodeOptions = nitroDevNodeOptions(plasmAgentPackageRoot());

  const bin = nitroBin(project.projectRoot);
  try {
    await access(bin);
  } catch {
    throw new Error(
      "nitropack is not installed — run `npm install` in the project root (init adds nitropack as a devDependency)",
    );
  }

  const child: ChildProcess = spawn(bin, ["dev"], {
    cwd: project.projectRoot,
    env: {
      ...process.env,
      NODE_OPTIONS: nodeOptions,
    },
    stdio: "inherit",
  });

  const wait = new Promise<number>((resolve, reject) => {
    child.on("error", reject);
    child.on("close", (code) => resolve(code ?? 0));
  });

  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.on(signal, () => {
      child.kill(signal);
    });
  }

  const exitCode = await wait;
  process.exit(exitCode);
}
