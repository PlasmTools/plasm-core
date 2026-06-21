import { access, readFile, readdir, stat } from "node:fs/promises";
import path from "node:path";

const SLOT_DIRS = [
  "catalogs",
  "skills",
  "channels",
  "schedules",
  "hooks",
  "subagents",
] as const;

export type AuthoredSlot = (typeof SLOT_DIRS)[number];

export interface DiscoveryDiagnostic {
  level: "error" | "warning";
  slot: AuthoredSlot | "instructions" | "agent";
  path: string;
  message: string;
}

export interface DiscoveredCatalog {
  name: string;
  path: string;
  entryId?: string;
}

export interface DiscoveredNamedFile {
  name: string;
  path: string;
  kind: "typescript" | "markdown";
}

export interface DiscoveredSubagent {
  name: string;
  path: string;
  agentPath: string;
}

export interface DiscoveredInstructions {
  path: string;
  kind: "markdown" | "typescript";
}

export interface ProjectDiscovery {
  agentRoot: string;
  instructions?: DiscoveredInstructions;
  catalogs: DiscoveredCatalog[];
  skills: DiscoveredNamedFile[];
  channels: DiscoveredNamedFile[];
  schedules: DiscoveredNamedFile[];
  hooks: DiscoveredNamedFile[];
  subagents: DiscoveredSubagent[];
  diagnostics: DiscoveryDiagnostic[];
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

function fileKind(filename: string): "typescript" | "markdown" | null {
  if (filename.endsWith(".ts")) return "typescript";
  if (filename.endsWith(".md")) return "markdown";
  return null;
}

function pathDerivedName(filePath: string): string {
  return path.basename(filePath, path.extname(filePath));
}

async function readCatalogEntryId(catalogDir: string): Promise<string | undefined> {
  const domainPath = path.join(catalogDir, "domain.yaml");
  if (!(await pathExists(domainPath))) return undefined;
  const raw = await readFile(domainPath, "utf8");
  const match = raw.match(/^entry_id:\s*["']?([^"'\n]+)["']?/m);
  return match?.[1]?.trim();
}

async function scanNamedFiles(
  slotDir: string,
  slot: Exclude<AuthoredSlot, "catalogs" | "subagents">,
): Promise<DiscoveredNamedFile[]> {
  if (!(await pathExists(slotDir))) return [];
  const entries = await readdir(slotDir, { withFileTypes: true });
  const out: DiscoveredNamedFile[] = [];
  for (const entry of entries) {
    if (!entry.isFile()) continue;
    const kind = fileKind(entry.name);
    if (!kind) continue;
    out.push({
      name: pathDerivedName(entry.name),
      path: path.join(slotDir, entry.name),
      kind,
    });
  }
  out.sort((a, b) => a.name.localeCompare(b.name));
  return out;
}

async function scanCatalogs(
  agentRoot: string,
  diagnostics: DiscoveryDiagnostic[],
): Promise<DiscoveredCatalog[]> {
  const catalogsDir = path.join(agentRoot, "catalogs");
  if (!(await pathExists(catalogsDir))) return [];

  const entries = await readdir(catalogsDir, { withFileTypes: true });
  const out: DiscoveredCatalog[] = [];

    for (const entry of entries) {
      const catalogDir = path.join(catalogsDir, entry.name);
      const dirStat = await stat(catalogDir).catch(() => null);
      if (!dirStat?.isDirectory()) continue;
    const domainYaml = path.join(catalogDir, "domain.yaml");
    const mappingsYaml = path.join(catalogDir, "mappings.yaml");

    if (!(await pathExists(domainYaml))) {
      diagnostics.push({
        level: "error",
        slot: "catalogs",
        path: catalogDir,
        message: "catalog directory is missing domain.yaml",
      });
      continue;
    }
    if (!(await pathExists(mappingsYaml))) {
      diagnostics.push({
        level: "error",
        slot: "catalogs",
        path: catalogDir,
        message: "catalog directory is missing mappings.yaml",
      });
      continue;
    }

    const entryId = await readCatalogEntryId(catalogDir);
    out.push({
      name: entry.name,
      path: catalogDir,
      entryId,
    });
  }

  out.sort((a, b) => a.name.localeCompare(b.name));
  return out;
}

async function scanSubagents(
  agentRoot: string,
  diagnostics: DiscoveryDiagnostic[],
): Promise<DiscoveredSubagent[]> {
  const subagentsDir = path.join(agentRoot, "subagents");
  if (!(await pathExists(subagentsDir))) return [];

  const entries = await readdir(subagentsDir, { withFileTypes: true });
  const out: DiscoveredSubagent[] = [];

    for (const entry of entries) {
      const subagentDir = path.join(subagentsDir, entry.name);
      const dirStat = await stat(subagentDir).catch(() => null);
      if (!dirStat?.isDirectory()) continue;
    const agentPath = path.join(subagentDir, "agent.ts");
    if (!(await pathExists(agentPath))) {
      diagnostics.push({
        level: "warning",
        slot: "subagents",
        path: subagentDir,
        message: "subagent directory is missing agent.ts",
      });
      continue;
    }
    out.push({
      name: entry.name,
      path: subagentDir,
      agentPath,
    });
  }

  out.sort((a, b) => a.name.localeCompare(b.name));
  return out;
}

async function scanInstructions(
  agentRoot: string,
  diagnostics: DiscoveryDiagnostic[],
): Promise<DiscoveredInstructions | undefined> {
  const markdownPath = path.join(agentRoot, "instructions.md");
  const typescriptPath = path.join(agentRoot, "instructions.ts");

  const hasMarkdown = await pathExists(markdownPath);
  const hasTypescript = await pathExists(typescriptPath);

  if (hasMarkdown && hasTypescript) {
    diagnostics.push({
      level: "warning",
      slot: "instructions",
      path: agentRoot,
      message: "both instructions.md and instructions.ts exist; markdown wins",
    });
  }

  if (hasMarkdown) {
    return { path: markdownPath, kind: "markdown" };
  }
  if (hasTypescript) {
    return { path: typescriptPath, kind: "typescript" };
  }

  diagnostics.push({
    level: "warning",
    slot: "instructions",
    path: agentRoot,
    message: "no instructions.md or instructions.ts found",
  });
  return undefined;
}

/** Walk `agent/` and collect eve-shaped discovery metadata (catalogs replace tools/). */
export async function walkAgentProject(agentRoot: string): Promise<ProjectDiscovery> {
  const resolvedRoot = path.resolve(agentRoot);
  const rootStat = await stat(resolvedRoot).catch(() => null);
  const diagnostics: DiscoveryDiagnostic[] = [];

  if (!rootStat?.isDirectory()) {
    diagnostics.push({
      level: "error",
      slot: "agent",
      path: resolvedRoot,
      message: "agent root is not a directory",
    });
    return {
      agentRoot: resolvedRoot,
      catalogs: [],
      skills: [],
      channels: [],
      schedules: [],
      hooks: [],
      subagents: [],
      diagnostics,
    };
  }

  const [instructions, catalogs, skills, channels, schedules, hooks, subagents] =
    await Promise.all([
      scanInstructions(resolvedRoot, diagnostics),
      scanCatalogs(resolvedRoot, diagnostics),
      scanNamedFiles(path.join(resolvedRoot, "skills"), "skills"),
      scanNamedFiles(path.join(resolvedRoot, "channels"), "channels"),
      scanNamedFiles(path.join(resolvedRoot, "schedules"), "schedules"),
      scanNamedFiles(path.join(resolvedRoot, "hooks"), "hooks"),
      scanSubagents(resolvedRoot, diagnostics),
    ]);

  return {
    agentRoot: resolvedRoot,
    instructions,
    catalogs,
    skills,
    channels,
    schedules,
    hooks,
    subagents,
    diagnostics,
  };
}

export const authoredSlots: readonly AuthoredSlot[] = SLOT_DIRS;
