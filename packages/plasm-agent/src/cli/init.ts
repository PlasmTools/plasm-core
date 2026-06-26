import { access, cp, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { FRAMEWORK_VERSION } from "../project-info.js";
import type { ResolvedAgentProject } from "./project-root.js";

export interface InitOptions {
  template?: string;
}

const SKIP_TEMPLATE_DIRS = new Set(["node_modules", ".plasm", ".nitro", ".output", "server"]);
const SKIP_TEMPLATE_FILES = new Set(["package-lock.json"]);

function plasmAgentPackageRoot(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
}

function resolveTemplateDir(template: string): string {
  const templates: Record<string, string> = {
    "mcp-radar": path.join(plasmAgentPackageRoot(), "../../../examples/mcp-radar-agent"),
  };
  const dir = templates[template];
  if (!dir) {
    throw new Error(
      `Unknown template "${template}". Available: ${Object.keys(templates).join(", ")}`,
    );
  }
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

async function copyTemplate(templateRoot: string, projectRoot: string): Promise<void> {
  await cp(templateRoot, projectRoot, {
    recursive: true,
    filter: (src) => src === templateRoot || shouldCopyTemplateEntry(src, templateRoot),
  });
}

async function patchBootstrapPackageJson(
  projectRoot: string,
  packageRoot: string,
): Promise<void> {
  const pkgPath = path.join(projectRoot, "package.json");
  const raw = await readFile(pkgPath, "utf8");
  const pkg = JSON.parse(raw) as {
    name?: string;
    scripts?: Record<string, string>;
    dependencies?: Record<string, string>;
    devDependencies?: Record<string, string>;
  };
  pkg.name = path.basename(projectRoot);
  const engineRoot = path.resolve(packageRoot, "../plasm-engine");
  pkg.dependencies = {
    ...pkg.dependencies,
    "@plasm_lang/vercel-agent": `file:${path.resolve(packageRoot)}`,
    "@plasm_lang/engine": `file:${engineRoot}`,
    "@vercel/blob": "^0.27.3",
    "@vercel/functions": "^3.4.3",
    "@vercel/kv": "^3.0.0",
  };
  const nodeRunner =
    "node --experimental-strip-types --experimental-transform-types ./node_modules/@plasm_lang/vercel-agent/scripts/plasm-node.mjs";
  pkg.scripts = {
    build: "plasm-agent build",
    "vercel-build": "plasm-agent build",
    dev: "plasm-agent dev",
    "dev:interactive": "plasm-agent dev --interactive",
    info: "plasm-agent info",
    deploy: "vercel deploy",
    ...pkg.scripts,
    eval: `${nodeRunner} scripts/run-evals.ts`,
    "smoke:channel": `${nodeRunner} scripts/smoke-channel.ts`,
  };
  pkg.devDependencies = {
    ...pkg.devDependencies,
    nitropack: "^2.13.4",
  };
  await writeFile(pkgPath, `${JSON.stringify(pkg, null, 2)}\n`, "utf8");
}

async function runTemplateInit(
  targetDir: string,
  template: string,
): Promise<ResolvedAgentProject> {
  const projectRoot = path.resolve(targetDir);
  const agentRoot = path.join(projectRoot, "agent");
  if (await exists(path.join(agentRoot, "agent.ts"))) {
    throw new Error(`agent/agent.ts already exists in ${projectRoot}`);
  }
  const templateRoot = resolveTemplateDir(template);
  if (!(await exists(templateRoot))) {
    throw new Error(`Template source missing: ${templateRoot}`);
  }
  await mkdir(projectRoot, { recursive: true });
  await copyTemplate(templateRoot, projectRoot);
  await patchBootstrapPackageJson(projectRoot, plasmAgentPackageRoot());
  await writeVercelScaffold(projectRoot, template);
  await writeNitroScaffold(projectRoot);
  return { projectRoot, agentRoot };
}

const AGENT_TS = `import path from "node:path";
import { fileURLToPath } from "node:url";

import { createAgentFromProject, defineAgent } from "@plasm_lang/vercel-agent";
import { loadAgentEnv } from "@plasm_lang/vercel-agent";

loadAgentEnv();

const agentRoot = path.dirname(fileURLToPath(import.meta.url));

const agentDefinition = defineAgent({
  model: process.env.PLASM_AGENT_MODEL ?? "anthropic/claude-sonnet-4.6",
  compaction: { thresholdPercent: 0.75 },
  experimental: { skills: true },
});

export default agentDefinition;

export async function createPlasmAgent() {
  return createAgentFromProject(agentDefinition, {
    agentRoot,
    tenantScope: process.env.PLASM_TENANT_SCOPE ?? "local",
    maxSteps: 20,
    telemetry: process.env.PLASM_AGENT_TELEMETRY !== "0",
  });
}
`;

const INSTRUCTIONS_MD = `# Catalog-native Plasm agent

Use **discover_capabilities → plasm_context → plasm → plasm_run** for catalog-backed work.
Keep one stable \`intent\` per user goal.
`;

const DOMAIN_YAML = `version: 1
auth:
  scheme: none
http_backend: http://example.test
entities:
  Item:
    id_field: id
    description: Starter entity
    fields:
      id:
        required: true
        value_ref: nv_item_id
      name:
        required: false
        value_ref: nv_item_name
    relations: {}
capabilities:
  item_list:
    description: List items
    kind: query
    entity: Item
    provides:
    - id
    - name
values:
  nv_item_id:
    type: string
    string_semantics: short
  nv_item_name:
    type: string
    string_semantics: short
`;

const MAPPINGS_YAML = `version: 1
capabilities:
  item_list:
    transport:
      kind: http
      method: GET
      path: /items
`;

const ENV_EXAMPLE = `# Vercel AI Gateway (run \`plasm-agent link\` to pull from a linked project)
AI_GATEWAY_API_KEY=

# Vercel Cron auth (production)
CRON_SECRET=

# Durable state on Vercel (optional locally)
KV_REST_API_URL=
KV_REST_API_TOKEN=
BLOB_READ_WRITE_TOKEN=
PLASM_STATE_BACKEND=kv
PLASM_ARCHIVE_BACKEND=vercel

# Optional overrides
PLASM_AGENT_MODEL=anthropic/claude-sonnet-4.6
PLASM_TENANT_SCOPE=local
PORT=3000
`;

const VERCEL_JSON_DEFAULT = `{
  "buildCommand": "plasm-agent build",
  "rewrites": [{ "source": "/(.*)", "destination": "/api/$1" }]
}
`;

const VERCEL_JSON_MCP_RADAR = `{
  "buildCommand": "plasm-agent build",
  "rewrites": [{ "source": "/(.*)", "destination": "/api/$1" }],
  "crons": [
    {
      "path": "/internal/cron/mcp-radar-scan",
      "schedule": "0 */6 * * *"
    }
  ],
  "functions": {
    "api/**": {
      "maxDuration": 300
    }
  }
}
`;

const VERCELIGNORE = `node_modules
.env*
agent/.plasm/research
`;

const API_HANDLER_TS = `import path from "node:path";
import { fileURLToPath } from "node:url";

import agentDefinition from "../agent/agent.js";
import { createPlasmApp, vercelPlasmHandler } from "@plasm_lang/vercel-agent";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");

let app: Awaited<ReturnType<typeof createPlasmApp>> | undefined;

export default async function handler(
  req: import("node:http").IncomingMessage,
  res: import("node:http").ServerResponse,
): Promise<void> {
  app ??= await createPlasmApp({
    agentRoot,
    definition: agentDefinition,
    mode: "prod",
    sessions: false,
  });
  await vercelPlasmHandler(app)(req, res);
}
`;

const NITRO_CONFIG_TS = `import { defineNitroConfig } from "nitropack/config";

export default defineNitroConfig({
  compatibilityDate: "2026-06-26",
  srcDir: ".",
  ignore: ["api/**"],
  devServer: {
    port: Number(process.env.PORT ?? 3000),
    host: process.env.HOST ?? "127.0.0.1",
  },
  typescript: {
    strict: false,
  },
  externals: {
    inline: ["@plasm_lang/engine"],
  },
});
`;

const NITRO_CATCHALL_ROUTE_TS = `import path from "node:path";

import { fromNodeMiddleware } from "h3";

import agentDefinition from "../agent/agent.js";
import { createPlasmApp, vercelPlasmHandler } from "@plasm_lang/vercel-agent";

const agentRoot = path.join(process.cwd(), "agent");

let appPromise: ReturnType<typeof createPlasmApp> | undefined;

async function plasmApp() {
  appPromise ??= createPlasmApp({
    agentRoot,
    definition: agentDefinition,
    mode: "prod",
    sessions: false,
  });
  return appPromise;
}

export default fromNodeMiddleware(async (req, res) => {
  const app = await plasmApp();
  await new Promise<void>((resolve, reject) => {
    res.once("finish", () => resolve());
    res.once("error", reject);
    void vercelPlasmHandler(app)(req, res).catch(reject);
  });
});
`;

const PACKAGE_JSON_TEMPLATE = {
  name: "my-plasm-agent",
  private: true,
  type: "module",
  scripts: {
    dev: "plasm-agent dev",
    "dev:interactive": "plasm-agent dev --interactive",
    build: "plasm-agent build",
    "vercel-build": "plasm-agent build",
    info: "plasm-agent info",
    deploy: "vercel deploy",
  },
  dependencies: {} as Record<string, string>,
  devDependencies: {
    nitropack: "^2.13.4",
  },
};

async function writeVercelScaffold(
  projectRoot: string,
  template?: string,
): Promise<void> {
  const vercelJson =
    template === "mcp-radar" ? VERCEL_JSON_MCP_RADAR : VERCEL_JSON_DEFAULT;
  await mkdir(path.join(projectRoot, "api"), { recursive: true });
  await writeFile(path.join(projectRoot, "vercel.json"), vercelJson, "utf8");
  await writeFile(path.join(projectRoot, ".vercelignore"), VERCELIGNORE, "utf8");
  await writeFile(
    path.join(projectRoot, "api", "[[...path]].ts"),
    API_HANDLER_TS,
    "utf8",
  );
}

async function writeNitroScaffold(projectRoot: string): Promise<void> {
  await mkdir(path.join(projectRoot, "routes"), { recursive: true });
  await writeFile(path.join(projectRoot, "nitro.config.ts"), NITRO_CONFIG_TS, "utf8");
  await writeFile(
    path.join(projectRoot, "routes", "[...path].ts"),
    NITRO_CATCHALL_ROUTE_TS,
    "utf8",
  );
}

function packageJsonScaffold(): Record<string, unknown> {
  return {
    ...PACKAGE_JSON_TEMPLATE,
    dependencies: {
      "@plasm_lang/vercel-agent": `^${FRAMEWORK_VERSION}`,
    },
  };
}

async function exists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

export async function runPlasmInit(
  targetDir: string,
  options?: InitOptions,
): Promise<ResolvedAgentProject> {
  if (options?.template) {
    return runTemplateInit(targetDir, options.template);
  }

  const projectRoot = path.resolve(targetDir);
  const agentRoot = path.join(projectRoot, "agent");

  if (await exists(path.join(agentRoot, "agent.ts"))) {
    throw new Error(`agent/agent.ts already exists in ${projectRoot}`);
  }

  await mkdir(path.join(agentRoot, "catalogs", "starter"), { recursive: true });
  await mkdir(path.join(agentRoot, "skills"), { recursive: true });
  await mkdir(path.join(agentRoot, "channels"), { recursive: true });
  await mkdir(path.join(agentRoot, "schedules"), { recursive: true });
  await mkdir(path.join(agentRoot, "hooks"), { recursive: true });
  await mkdir(path.join(projectRoot, "evals"), { recursive: true });

  await writeFile(path.join(agentRoot, "agent.ts"), AGENT_TS, "utf8");
  await writeFile(path.join(agentRoot, "instructions.md"), INSTRUCTIONS_MD, "utf8");
  await writeFile(path.join(agentRoot, "catalogs", "starter", "domain.yaml"), DOMAIN_YAML, "utf8");
  await writeFile(path.join(agentRoot, "catalogs", "starter", "mappings.yaml"), MAPPINGS_YAML, "utf8");
  await writeFile(path.join(projectRoot, ".env.example"), ENV_EXAMPLE, "utf8");

  if (!(await exists(path.join(projectRoot, "package.json")))) {
    await writeFile(
      path.join(projectRoot, "package.json"),
      `${JSON.stringify(packageJsonScaffold(), null, 2)}\n`,
      "utf8",
    );
    await patchBootstrapPackageJson(projectRoot, plasmAgentPackageRoot());
  }

  await writeVercelScaffold(projectRoot);
  await writeNitroScaffold(projectRoot);

  return { projectRoot, agentRoot };
}

export function formatInitSuccess(
  project: ResolvedAgentProject,
  options?: InitOptions,
): string {
  if (options?.template === "mcp-radar") {
    return [
      `Initialized MCP Radar agent from template at ${project.projectRoot}`,
      "",
      "Next:",
      `  cd ${project.projectRoot}`,
      "  npm install",
      "  plasm-agent link          # pull AI_GATEWAY_API_KEY from Vercel",
      "  plasm-agent build",
      "  npm run smoke:channel",
      "  plasm-agent dev           # Nitro dev server (Vercel routing parity)",
      "  plasm-agent dev --interactive  # optional TUI + sessions + hot reload",
      "  vercel deploy             # production (channels + cron)",
    ].join("\n");
  }

  return [
    `Initialized Plasm agent project at ${project.projectRoot}`,
    "",
    "Next:",
    `  cd ${project.projectRoot}`,
    "  npm install",
    "  plasm-agent link      # pull AI_GATEWAY_API_KEY from Vercel",
    "  plasm-agent build     # generate CGS stubs + discovery manifest",
    "  plasm-agent dev       # Nitro dev server (Vercel routing parity)",
    "  plasm-agent dev --interactive  # optional TUI + sessions",
    "  vercel deploy         # production on Vercel",
  ].join("\n");
}
