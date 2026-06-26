#!/usr/bin/env node
/**
 * TypeScript CLI for @plasm_lang/vercel-agent (not the Rust plasm-server TUI).
 */
import path from "node:path";

import { runPlasmBuild } from "../src/cli/build.js";
import {
  installDevServerShutdown,
  startDevServerForProject,
} from "../src/cli/dev.js";
import { startNitroDevForProject } from "../src/cli/nitro-dev.js";
import { runDevTui } from "../src/dev/client/repl.js";
import { collectProjectInfo, formatPlasmInfoHuman } from "../src/cli/info.js";
import { runPlasmInit, formatInitSuccess } from "../src/cli/init.js";
import { runPlasmLink } from "../src/cli/link.js";
import {
  requireAgentProject,
  readPackageName,
  resolveAgentProject,
} from "../src/cli/project-root.js";
import { loadAgentEnv } from "../src/load-env.js";

loadAgentEnv();

const args = process.argv.slice(2);
const command = args[0];

function flag(name: string): boolean {
  return args.includes(name);
}

function optionValue(flagName: string): string | undefined {
  const index = args.indexOf(flagName);
  if (index < 0) return undefined;
  const value = args[index + 1];
  if (!value || value.startsWith("-")) return undefined;
  return value;
}

function initTargetDir(): string {
  for (let i = 1; i < args.length; i++) {
    const arg = args[i];
    if (arg.startsWith("-")) {
      if (arg === "--template") i++;
      continue;
    }
    return arg;
  }
  return process.cwd();
}

async function cmdInit(): Promise<void> {
  const template = optionValue("--template");
  const target = initTargetDir();
  const project = await runPlasmInit(target, { template });
  console.log(formatInitSuccess(project, { template }));
}

async function cmdInfo(): Promise<void> {
  const project = await requireAgentProject();
  const packageName = await readPackageName(project.projectRoot);
  const info = await collectProjectInfo({
    projectRoot: project.projectRoot,
    agentRoot: project.agentRoot,
    packageName,
  });
  if (flag("--json")) {
    console.log(JSON.stringify(info, null, 2));
    return;
  }
  console.log(formatPlasmInfoHuman(info));
}

async function cmdLink(): Promise<void> {
  const project = await requireAgentProject();
  const result = await runPlasmLink(project.projectRoot);
  for (const line of result.messages) console.log(line);
  if (!result.linked && !result.envPulled) process.exitCode = 1;
}

async function cmdBuild(): Promise<void> {
  const project = await requireAgentProject();
  const result = await runPlasmBuild(project);
  console.log(`Built ${result.stubs.length} stub(s)`);
  console.log(`manifest: ${result.manifestPath}`);
  for (const stub of result.stubs) {
    console.log(`  - ${stub.entryId} → ${path.relative(project.projectRoot, stub.outPath)}`);
  }
}

async function cmdDev(): Promise<void> {
  const project = await requireAgentProject();
  if (flag("--interactive")) {
    const handle = await startDevServerForProject({
      project,
      tui: flag("--no-tui") ? false : "auto",
    });
    installDevServerShutdown(handle);
    return;
  }
  await startNitroDevForProject(project);
}

async function cmdChat(): Promise<void> {
  const urlFlag = args.find((a, i) => args[i - 1] === "--url");
  const baseUrl = urlFlag ?? process.env.PLASM_DEV_URL ?? "http://127.0.0.1:3000";
  await runDevTui({ baseUrl });
}

async function main(): Promise<void> {
  if (!command || command === "--help" || command === "-h") {
    console.log(`plasm-agent — @plasm_lang/vercel-agent CLI

Commands:
  init [dir] [--template NAME]  Scaffold agent project (templates: mcp-radar)
  info [--json] Project + catalog diagnostics
  link          vercel link + env pull (AI Gateway key)
  build         CGS stubs + .plasm/discovery/manifest.json
  dev [--interactive] [--no-tui]  Nitro dev (Vercel routing parity; default). --interactive for TUI/sessions.
  chat [--url URL]  Terminal client for a running dev server
`);
    return;
  }

  switch (command) {
    case "init":
      await cmdInit();
      break;
    case "info": {
      const resolved = await resolveAgentProject();
      if (!resolved) {
        console.error("No agent project found. Run `plasm-agent init` first.");
        process.exitCode = 1;
        return;
      }
      await cmdInfo();
      break;
    }
    case "link":
      await cmdLink();
      break;
    case "build":
      await cmdBuild();
      break;
    case "dev":
      await cmdDev();
      break;
    case "chat":
      await cmdChat();
      break;
    default:
      console.error(`Unknown command: ${command}`);
      process.exitCode = 1;
  }
}

await main().catch((err) => {
  console.error(err);
  process.exit(1);
});
