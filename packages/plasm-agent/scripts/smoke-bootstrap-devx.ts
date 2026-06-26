#!/usr/bin/env node
/**
 * Smoke: bootstrap devx — init --template mcp-radar → install → build → info → channel.
 * Uses monorepo file: deps (default when plasm-engine sibling exists).
 */
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { access, readFile } from "node:fs/promises";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const runPlasmCli = path.join(packageRoot, "scripts", "run-plasm-cli.mjs");
const tsxCli = require.resolve("tsx/cli");

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
  const { stdout } = await run("node", [runPlasmCli, ...plasmArgs], cwd);
  return stdout;
}

async function runTsScript(script: string, args: string[], cwd: string): Promise<void> {
  await run("node", [tsxCli, script, ...args], cwd);
}

async function assertNitroScaffold(workDir: string): Promise<void> {
  await access(path.join(workDir, "nitro.config.ts"));
  await access(path.join(workDir, "routes", "[...path].ts"));

  const pkg = JSON.parse(await readFile(path.join(workDir, "package.json"), "utf8")) as {
    scripts?: Record<string, string>;
    devDependencies?: Record<string, string>;
  };
  if (pkg.scripts?.dev !== "plasm-agent dev") {
    throw new Error(`package.json dev script must be plasm-agent dev, got ${pkg.scripts?.dev}`);
  }
  if (!pkg.devDependencies?.nitropack) {
    throw new Error("package.json missing devDependencies.nitropack");
  }
  if (!pkg.devDependencies?.tsx) {
    throw new Error("package.json missing devDependencies.tsx");
  }
}

async function assertVercelScaffold(workDir: string): Promise<void> {
  const vercelJsonPath = path.join(workDir, "vercel.json");
  const apiHandlerPath = path.join(workDir, "api", "[[...path]].ts");
  const publicIndex = path.join(workDir, "public", "index.html");
  await access(vercelJsonPath);
  await access(apiHandlerPath);
  await access(publicIndex);

  const vercelJson = JSON.parse(await readFile(vercelJsonPath, "utf8")) as {
    crons?: Array<{ path: string; schedule: string }>;
    buildCommand?: string;
  };
  if (vercelJson.buildCommand !== "plasm-agent build") {
    throw new Error(`vercel.json buildCommand must be plasm-agent build: ${JSON.stringify(vercelJson)}`);
  }
  const cron = vercelJson.crons?.find((c) => c.path.includes("mcp-radar-scan"));
  if (!cron) {
    throw new Error(`vercel.json missing mcp-radar-scan cron: ${JSON.stringify(vercelJson.crons)}`);
  }

  const handlerSrc = await readFile(apiHandlerPath, "utf8");
  if (!handlerSrc.includes("@plasm_lang/vercel-agent/server")) {
    throw new Error("api handler must import @plasm_lang/vercel-agent/server");
  }

  const pkg = JSON.parse(await readFile(path.join(workDir, "package.json"), "utf8")) as {
    scripts?: Record<string, string>;
  };
  if (pkg.scripts?.["vercel-build"]) {
    throw new Error("package.json must not define vercel-build (use vercel.json buildCommand only)");
  }
}

async function main(): Promise<void> {
  const workDir = await mkdtemp(path.join(tmpdir(), "plasm-bootstrap-"));
  try {
    console.log(`bootstrap temp: ${workDir}`);

    await runPlasm(["init", "--template", "mcp-radar", workDir], packageRoot);
    await assertVercelScaffold(workDir);
    await assertNitroScaffold(workDir);

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

    await runTsScript(path.join(packageRoot, "scripts/smoke-hn-preflight.ts"), [workDir], packageRoot);
    await runTsScript(path.join(packageRoot, "scripts/smoke-vercel-build.ts"), [workDir], packageRoot);
    await runTsScript(path.join(packageRoot, "scripts/smoke-vercel-handler.ts"), [workDir], packageRoot);

    console.log(
      "OK: bootstrap devx (init → vercel+nitro scaffold → install → build → info → smoke:channel → HN → vercel-build → prod handler)",
    );
  } finally {
    await rm(workDir, { recursive: true, force: true });
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
