import path from "node:path";
import { access } from "node:fs/promises";

import workflowNitro from "@workflow/nitro";
import { createNitro } from "nitro/builder";
import type { Nitro, NitroConfig } from "nitro/types";

import type { PreparedPlasmHost } from "./prepare-host.js";
import {
  isVercelBuildEnvironment,
  plasmNitroBuildDir,
  PLASM_NITRO_BUILD_DIR,
  plasmNitroOutputDir,
  plasmNitroRoutesDir,
  vercelOutputDir,
} from "./paths.js";
import { createPlasmVercelOptions } from "./vercel-build-output-config.js";

const SERVER_TRACE_DEPS = [
  "@ai-sdk/otel",
  "@opentelemetry/api",
  "@plasm_lang/engine",
  "@plasm_lang/vercel-agent",
  "@vercel/functions",
  "@vercel/blob",
  "@vercel/otel",
  "ai",
  "workflow",
  "workflow/api",
];

function resolveNitroPreset(dev: boolean): NitroConfig["preset"] {
  if (dev) return "nitro-dev";
  if (isVercelBuildEnvironment()) return "vercel";
  return "node-server";
}

async function publicDirExists(projectRoot: string): Promise<boolean> {
  try {
    await access(path.join(projectRoot, "public"));
    return true;
  } catch {
    return false;
  }
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function plasmNitroHandlers(
  projectRoot: string,
  workflowEnabled: boolean,
): Promise<NonNullable<NitroConfig["handlers"]>> {
  const catchAllHandler = path.join(plasmNitroRoutesDir(projectRoot), "[[path]].ts");
  const handlers: NonNullable<NitroConfig["handlers"]> = [];

  if (workflowEnabled) {
    const dispatchHandler = path.join(
      plasmNitroRoutesDir(projectRoot),
      "internal",
      "workflow",
      "dispatch.post.ts",
    );
    if (await pathExists(dispatchHandler)) {
      handlers.push({
        route: "/internal/workflow/dispatch",
        handler: dispatchHandler,
        method: "post",
      });
    }
  }

  handlers.push({ route: "/**", handler: catchAllHandler });
  return handlers;
}

export async function createPlasmNitro(
  host: PreparedPlasmHost,
  dev: boolean,
): Promise<Nitro> {
  const preset = resolveNitroPreset(dev);
  const isVercel = preset === "vercel";
  const plasmVercel = createPlasmVercelOptions(isVercel);

  const modules: NitroConfig["modules"] = [];
  if (host.workflowEnabled) {
    modules.push(workflowNitro);
  }

  const traceDeps = [...new Set([...SERVER_TRACE_DEPS, ...host.externalDeps])];

  const publicAssets = (await publicDirExists(host.projectRoot))
    ? [
        {
          dir: path.join(host.projectRoot, "public"),
          baseURL: "/",
          maxAge: 3600,
        },
      ]
    : [];

  const vercelConfig = plasmVercel?.config ?? { version: 3 as const };
  const config: NitroConfig = {
    rootDir: host.projectRoot,
    buildDir: plasmNitroBuildDir(host.projectRoot),
    scanDirs: [path.join(host.projectRoot, PLASM_NITRO_BUILD_DIR)],
    preset,
    dev,
    serverDir: false,
    ignore: ["api/**", "server/**", "routes/**", "nitro.config.ts"],
    handlers: await plasmNitroHandlers(host.projectRoot, host.workflowEnabled),
    publicAssets,
    modules,
    experimental: {
      tasks: Object.keys(host.scheduledTasks).length > 0,
    },
    scheduledTasks: host.scheduledTasks,
    workflow: host.workflowEnabled
      ? {
          dirs: ["workflows"],
        }
      : undefined,
    traceDeps: [
      ...traceDeps,
      "./agent/**",
      "./lib/**",
      "./agent/.plasm/compiled/**",
      "./agent/.plasm/stubs/**",
    ],
    vercel: isVercel
      ? {
          entryFormat: "node",
          functions: {
            maxDuration: 300,
            environment: {
              PLASM_RUN_ARTIFACTS_DIR: "/tmp/plasm-archives/runs",
              PLASM_TRACE_ARCHIVE_DIR: "/tmp/plasm-archives/traces",
            },
          },
          config: {
            ...vercelConfig,
          },
        }
      : undefined,
    output: isVercel
      ? { dir: vercelOutputDir(host.projectRoot) }
      : dev
        ? undefined
        : { dir: plasmNitroOutputDir(host.projectRoot) },
  };

  return createNitro(config, dev ? { watch: true } : undefined);
}
