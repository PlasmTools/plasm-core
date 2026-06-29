import { access, cp, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { frameworkPackageVersion } from "../package-version.js";
import type { ResolvedAgentProject } from "./project-root.js";

export interface InitOptions {
  template?: string;
  /** Use npm semver deps instead of monorepo file: links (default: auto-detect). */
  npm?: boolean;
}

const SKIP_TEMPLATE_DIRS = new Set(["node_modules", ".plasm", ".nitro", ".output"]);
const SKIP_TEMPLATE_FILES = new Set(["package-lock.json", "vercel-build.ts"]);

function plasmAgentPackageRoot(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
}

function scaffoldRoot(): string {
  return path.join(plasmAgentPackageRoot(), "templates", "scaffold");
}

function resolveTemplateDir(template: string): string {
  const dir = path.join(plasmAgentPackageRoot(), "templates", template);
  return dir;
}

function shouldCopyTemplateEntry(src: string, templateRoot: string): boolean {
  const rel = path.relative(templateRoot, src);
  if (!rel || rel === "") return true;
  const parts = rel.split(path.sep);
  if (parts.some((part) => SKIP_TEMPLATE_DIRS.has(part))) return false;
  if (SKIP_TEMPLATE_FILES.has(path.basename(src))) return false;
  return true;
}

async function exists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function copyTemplate(templateRoot: string, projectRoot: string): Promise<void> {
  await cp(templateRoot, projectRoot, {
    recursive: true,
    filter: (src) => src === templateRoot || shouldCopyTemplateEntry(src, templateRoot),
  });
}

async function copyScaffoldFile(relativePath: string, projectRoot: string): Promise<void> {
  const src = path.join(scaffoldRoot(), relativePath);
  const dest = path.join(projectRoot, relativePath);
  await mkdir(path.dirname(dest), { recursive: true });
  await cp(src, dest);
}

export async function writeDeployScaffold(
  projectRoot: string,
  template?: string,
): Promise<void> {
  const vercelJsonName =
    template === "mcp-radar" ? "vercel.mcp-radar.json" : "vercel.default.json";
  await copyScaffoldFile("agent/instrumentation.ts", projectRoot);
  await copyScaffoldFile(".vercelignore", projectRoot);
  const publicIndex =
    template === "mcp-radar" ? "public/index.mcp-radar.html" : "public/index.html";
  const publicDest = path.join(projectRoot, "public", "index.html");
  await mkdir(path.dirname(publicDest), { recursive: true });
  await cp(path.join(scaffoldRoot(), publicIndex), publicDest);
  await cp(
    path.join(scaffoldRoot(), vercelJsonName),
    path.join(projectRoot, "vercel.json"),
  );
}

function monorepoEngineRoot(packageRoot: string): string {
  return path.resolve(packageRoot, "../plasm-engine");
}

export async function isMonorepoDevPackage(packageRoot: string): Promise<boolean> {
  return exists(path.join(monorepoEngineRoot(packageRoot), "package.json"));
}

export async function patchProjectPackageJson(
  projectRoot: string,
  packageRoot: string,
  options?: { npm?: boolean },
): Promise<void> {
  const pkgPath = path.join(projectRoot, "package.json");
  const raw = await readFile(pkgPath, "utf8");
  const pkg = JSON.parse(raw) as {
    name?: string;
    scripts?: Record<string, string>;
    dependencies?: Record<string, string>;
    devDependencies?: Record<string, string>;
  };

  const version = frameworkPackageVersion();
  const useNpm =
    options?.npm === true
      ? true
      : options?.npm === false
        ? false
        : !(await isMonorepoDevPackage(packageRoot));

  pkg.name = path.basename(projectRoot);
  if (useNpm) {
    pkg.dependencies = {
      ...pkg.dependencies,
      "@plasm_lang/vercel-agent": `^${version}`,
      "@plasm_lang/engine": `^${version}`,
      "@ai-sdk/otel": "^1.0.3",
      "@vercel/blob": "^0.27.3",
      "@vercel/functions": "^3.4.3",
      "@vercel/otel": "^1.5.0",
      ai: "^6.0.0",
      workflow: "^4.5.0",
    };
  } else {
    pkg.dependencies = {
      ...pkg.dependencies,
      "@plasm_lang/vercel-agent": `file:${path.resolve(packageRoot)}`,
      "@plasm_lang/engine": `file:${monorepoEngineRoot(packageRoot)}`,
      "@ai-sdk/otel": "^1.0.3",
      "@vercel/blob": "^0.27.3",
      "@vercel/functions": "^3.4.3",
      "@vercel/otel": "^1.5.0",
      ai: "^6.0.0",
      workflow: "^4.5.0",
    };
  }

  pkg.scripts = {
    build: "plasm-agent build",
    dev: "plasm-agent dev",
    "dev:interactive": "plasm-agent dev --interactive",
    info: "plasm-agent info",
    deploy: "vercel deploy",
    ...pkg.scripts,
  };
  delete pkg.scripts["vercel-build"];

  pkg.devDependencies = {
    ...pkg.devDependencies,
    tsx: "^4.19.4",
  };
  delete pkg.devDependencies?.nitropack;

  await writeFile(pkgPath, `${JSON.stringify(pkg, null, 2)}\n`, "utf8");
}

export function blankPackageJsonScaffold(): Record<string, unknown> {
  const version = frameworkPackageVersion();
  return {
    name: "my-plasm-agent",
    private: true,
    type: "module",
    scripts: {
      dev: "plasm-agent dev",
      "dev:interactive": "plasm-agent dev --interactive",
      build: "plasm-agent build",
      info: "plasm-agent info",
      deploy: "vercel deploy",
    },
    dependencies: {
      "@plasm_lang/vercel-agent": `^${version}`,
      "@plasm_lang/engine": `^${version}`,
    },
    devDependencies: {
      tsx: "^4.19.4",
    },
  };
}

export async function runTemplateInit(
  targetDir: string,
  template: string,
  options?: InitOptions,
): Promise<ResolvedAgentProject> {
  const projectRoot = path.resolve(targetDir);
  const agentRoot = path.join(projectRoot, "agent");
  if (await exists(path.join(agentRoot, "agent.ts"))) {
    throw new Error(`agent/agent.ts already exists in ${projectRoot}`);
  }
  const templateRoot = resolveTemplateDir(template);
  if (!(await exists(templateRoot))) {
    throw new Error(`Unknown template "${template}" or missing source: ${templateRoot}`);
  }
  await mkdir(projectRoot, { recursive: true });
  await copyTemplate(templateRoot, projectRoot);
  await patchProjectPackageJson(projectRoot, plasmAgentPackageRoot(), { npm: options?.npm });
  await writeDeployScaffold(projectRoot, template);
  return { projectRoot, agentRoot };
}

export { exists, plasmAgentPackageRoot, resolveTemplateDir };
