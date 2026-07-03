import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** Repo root (`plasm/`) from `packages/plasm-agent`. */
export function defaultEnvFileCandidates(): string[] {
  return [
    path.join(packageRoot, ".env"),
    path.join(packageRoot, "../../.env"),
    path.join(packageRoot, "../../../.env"),
    path.join(packageRoot, "agent", ".env"),
  ];
}

/** Load project env by walking ancestors; nearer files override, empty values are ignored. */
export function loadProjectAgentEnv(projectRoot: string): string | undefined {
  const files = [...projectEnvFileCandidates(projectRoot)].reverse();
  let loaded: string | undefined;
  const pending = new Map<string, string>();
  for (const file of files) {
    if (!existsSync(file)) continue;
    const text = readFileSync(file, "utf8");
    for (const line of text.split(/\r?\n/)) {
      const parsed = parseEnvLine(line);
      if (!parsed || !parsed.value.trim()) continue;
      pending.set(parsed.key, parsed.value);
    }
    loaded = file;
  }
  for (const [key, value] of pending) {
    if (process.env[key] === undefined) {
      process.env[key] = value;
    }
  }
  if (!process.env.AI_GATEWAY_API_KEY?.trim()) {
    const alias =
      process.env.AI_API_GATEWAY_KEY?.trim() ??
      process.env.AI_GATEWAY_KEY?.trim();
    if (alias) {
      process.env.AI_GATEWAY_API_KEY = alias;
    }
  }
  return loaded;
}

/** `.env` candidates for an agent project root and ancestor directories. */
export function projectEnvFileCandidates(projectRoot: string): string[] {
  const resolved = path.resolve(projectRoot);
  const files = [
    path.join(resolved, ".env.local"),
    path.join(resolved, ".env"),
  ];
  let dir = resolved;
  for (let depth = 0; depth < 8; depth++) {
    const parent = path.dirname(dir);
    if (parent === dir) break;
    files.push(path.join(parent, ".env"));
    dir = parent;
  }
  return files;
}

function parseEnvLine(line: string): { key: string; value: string } | null {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("#")) return null;
  const withoutExport = trimmed.startsWith("export ")
    ? trimmed.slice("export ".length).trim()
    : trimmed;
  const eq = withoutExport.indexOf("=");
  if (eq <= 0) return null;
  const key = withoutExport.slice(0, eq).trim();
  let value = withoutExport.slice(eq + 1).trim();
  if (
    (value.startsWith('"') && value.endsWith('"')) ||
    (value.startsWith("'") && value.endsWith("'"))
  ) {
    value = value.slice(1, -1);
  }
  return { key, value };
}

/** Load `.env` into `process.env` without overriding existing variables. Empty values are skipped. */
export function loadAgentEnv(files: string[] = defaultEnvFileCandidates()): string | undefined {
  let loaded: string | undefined;
  for (const file of files) {
    if (!existsSync(file)) continue;
    const text = readFileSync(file, "utf8");
    for (const line of text.split(/\r?\n/)) {
      const parsed = parseEnvLine(line);
      if (!parsed || !parsed.value.trim()) continue;
      if (process.env[parsed.key] === undefined) {
        process.env[parsed.key] = parsed.value;
      }
    }
    loaded = file;
  }

  // User alias → Vercel AI Gateway canonical name.
  if (!process.env.AI_GATEWAY_API_KEY?.trim()) {
    const alias =
      process.env.AI_API_GATEWAY_KEY?.trim() ??
      process.env.AI_GATEWAY_KEY?.trim();
    if (alias) {
      process.env.AI_GATEWAY_API_KEY = alias;
    }
  }

  return loaded;
}
