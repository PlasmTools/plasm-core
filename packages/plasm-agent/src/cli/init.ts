import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import {
  blankPackageJsonScaffold,
  exists,
  patchProjectPackageJson,
  plasmAgentPackageRoot,
  runTemplateInit,
  writeDeployScaffold,
} from "./init-scaffold.js";
import type { ResolvedAgentProject } from "./project-root.js";

export type { InitOptions } from "./init-scaffold.js";

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

const ENV_EXAMPLE = `# Vercel AI Gateway
# On Vercel: OIDC auth is automatic — no key required for gateway model slugs.
# Local / self-host: run \`plasm-agent link\` or set AI_GATEWAY_API_KEY.
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

export async function runPlasmInit(
  targetDir: string,
  options?: import("./init-scaffold.js").InitOptions,
): Promise<ResolvedAgentProject> {
  if (options?.template) {
    return runTemplateInit(targetDir, options.template, options);
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
      `${JSON.stringify(blankPackageJsonScaffold(), null, 2)}\n`,
      "utf8",
    );
    await patchProjectPackageJson(projectRoot, plasmAgentPackageRoot(), { npm: options?.npm });
  }

  await writeDeployScaffold(projectRoot);
  return { projectRoot, agentRoot };
}

export function formatInitSuccess(
  project: ResolvedAgentProject,
  options?: import("./init-scaffold.js").InitOptions,
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
