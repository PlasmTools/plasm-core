#!/usr/bin/env node
/**
 * Smoke: npm-pack bootstrap — init with --npm → install from packed tarballs → build.
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
const engineRoot = path.resolve(packageRoot, "../plasm-engine");
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

async function packPackage(cwd: string): Promise<string> {
  const { stdout } = await run("npm", ["pack", "--pack-destination", cwd], cwd);
  const tarball = stdout.trim().split("\n").pop()?.trim();
  if (!tarball) {
    throw new Error(`npm pack produced no tarball in ${cwd}`);
  }
  return path.join(cwd, tarball);
}

async function main(): Promise<void> {
  const packDir = await mkdtemp(path.join(tmpdir(), "plasm-npm-pack-"));
  const workDir = await mkdtemp(path.join(tmpdir(), "plasm-bootstrap-npm-"));
  try {
    console.log(`npm pack dir: ${packDir}`);
    console.log(`bootstrap temp: ${workDir}`);

    const agentTgz = await packPackage(packageRoot);
    const engineTgz = await packPackage(engineRoot);

    await runPlasm(["init", "--template", "mcp-radar", workDir], packageRoot);

    const pkgPath = path.join(workDir, "package.json");
    const pkg = JSON.parse(await readFile(pkgPath, "utf8")) as {
      dependencies?: Record<string, string>;
    };
    pkg.dependencies = {
      ...pkg.dependencies,
      "@plasm_lang/vercel-agent": `file:${agentTgz}`,
      "@plasm_lang/engine": `file:${engineTgz}`,
    };
    const { writeFile } = await import("node:fs/promises");
    await writeFile(pkgPath, `${JSON.stringify(pkg, null, 2)}\n`, "utf8");

    await run("npm", ["install", "--no-audit", "--no-fund"], workDir);

    const buildOut = await runPlasm(["build"], workDir);
    if (!buildOut.includes("Built 2 stub")) {
      throw new Error(`build missing expected stubs:\n${buildOut}`);
    }

    await run("node", [tsxCli, path.join(packageRoot, "scripts/smoke-vercel-build.ts"), workDir], packageRoot);

    console.log("OK: npm-pack bootstrap devx (pack → init → install tarballs → build → vercel-build smoke)");
  } finally {
    await rm(packDir, { recursive: true, force: true });
    await rm(workDir, { recursive: true, force: true });
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
