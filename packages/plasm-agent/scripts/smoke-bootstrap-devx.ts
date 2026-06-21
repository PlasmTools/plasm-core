#!/usr/bin/env node
/**
 * Smoke: bootstrap devx — init --template mcp-radar → install → build → info → vercel scaffold → channel.
 */
import { spawn } from "node:child_process";
import { access, readFile } from "node:fs/promises";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const plasmNode = path.join(packageRoot, "scripts/plasm-node.mjs");
const plasmCli = path.join(packageRoot, "scripts/plasm-cli.ts");
const smokeVercelHandler = path.join(packageRoot, "scripts/smoke-vercel-handler.ts");

const nodeArgs = ["--experimental-strip-types", "--experimental-transform-types", plasmNode];

async function run(
  command: string,
  commandArgs: string[],
  cwd: string,
): Promise<{ stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, commandArgs, {
      cwd,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve({ stdout, stderr });
        return;
      }
      reject(
        new Error(
          `${command} ${commandArgs.join(" ")} failed (${code})\n${stdout}\n${stderr}`,
        ),
      );
    });
  });
}

async function runPlasm(plasmArgs: string[], cwd: string): Promise<string> {
  const { stdout } = await run("node", [...nodeArgs, plasmCli, ...plasmArgs], cwd);
  return stdout;
}

async function assertVercelScaffold(workDir: string): Promise<void> {
  const vercelJsonPath = path.join(workDir, "vercel.json");
  const apiHandlerPath = path.join(workDir, "api", "[[...path]].ts");
  await access(vercelJsonPath);
  await access(apiHandlerPath);

  const vercelJson = JSON.parse(await readFile(vercelJsonPath, "utf8")) as {
    crons?: Array<{ path: string; schedule: string }>;
    buildCommand?: string;
  };
  if (!vercelJson.buildCommand?.includes("plasm-agent build")) {
    throw new Error(`vercel.json missing buildCommand: ${JSON.stringify(vercelJson)}`);
  }
  const cron = vercelJson.crons?.find((c) => c.path.includes("mcp-radar-scan"));
  if (!cron) {
    throw new Error(`vercel.json missing mcp-radar-scan cron: ${JSON.stringify(vercelJson.crons)}`);
  }
}

async function main(): Promise<void> {
  const workDir = await mkdtemp(path.join(tmpdir(), "plasm-bootstrap-"));
  try {
    console.log(`bootstrap temp: ${workDir}`);

    await runPlasm(["init", "--template", "mcp-radar", workDir], packageRoot);
    await assertVercelScaffold(workDir);

    await run("npm", ["install", "--no-audit", "--no-fund"], workDir);

    const buildOut = await runPlasm(["build"], workDir);
    if (!buildOut.includes("hackernews") || !buildOut.includes("tavily")) {
      throw new Error(`build missing expected stubs:\n${buildOut}`);
    }

    const infoOut = await runPlasm(["info", "--json"], workDir);
    const info = JSON.parse(infoOut) as {
      loadedSlots?: {
        channels?: Array<{ name: string }>;
        diagnostics?: Array<{ level: string; message: string }>;
      };
      diagnostics?: Array<{ level: string; message: string }>;
    };

    if (!info.loadedSlots?.channels?.some((c) => c.name === "mcp-radar")) {
      throw new Error(`mcp-radar channel not loaded: ${JSON.stringify(info, null, 2)}`);
    }

    const errors = [
      ...(info.diagnostics ?? []),
      ...(info.loadedSlots?.diagnostics ?? []),
    ].filter((d) => d.level === "error");
    if (errors.length) {
      throw new Error(
        `slot diagnostics:\n${errors.map((e) => e.message).join("\n")}`,
      );
    }

    await run("npm", ["run", "smoke:channel"], workDir);

    await run("node", [...nodeArgs, smokeVercelHandler, workDir], packageRoot);

    console.log(
      "OK: bootstrap devx (init → vercel scaffold → install → build → info → smoke:channel → prod handler)",
    );
  } finally {
    await rm(workDir, { recursive: true, force: true });
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
