/**
 * Canonical Plasm prompt assets (vendored from plasm-core prompt_render/assets).
 * Prefer monorepo crates path when present so agent + MCP stay byte-identical.
 */
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const vendoredDir = path.join(here, "assets");
const monorepoAssetsDir = path.resolve(
  here,
  "../../../../crates/plasm-core/src/prompt_render/assets",
);

export type PromptAssetName =
  | "discover_tool.txt"
  | "initialize_workflow.txt"
  | "plasm_context_tool.txt"
  | "plasm_tool.txt"
  | "plasm_run_tool_base.txt"
  | "plasm_run_tool_artifact_tool.txt"
  | "plasm_read_run_artifact_tool.txt";

export function loadPromptAsset(name: PromptAssetName): string {
  const monorepo = path.join(monorepoAssetsDir, name);
  const vendored = path.join(vendoredDir, name);
  const file = existsSync(monorepo) ? monorepo : vendored;
  if (!existsSync(file)) {
    throw new Error(`missing Plasm prompt asset: ${name} (looked in ${monorepo} and ${vendored})`);
  }
  return readFileSync(file, "utf8").trim();
}

/**
 * Framework system liturgy for every PlasmAgent turn.
 * Plan→Act→Observe cycle + language law + tool-only resource rite —
 * not product/AppWorld overlays.
 */
export function buildDefaultSystemLiturgy(): string {
  return [
    loadPromptAsset("initialize_workflow.txt"),
    "",
    "## Resource handling (tool-only host)",
    loadPromptAsset("plasm_run_tool_artifact_tool.txt"),
    loadPromptAsset("plasm_read_run_artifact_tool.txt"),
    "",
    "## Plasm language (`plasm` tool)",
    loadPromptAsset("plasm_tool.txt"),
  ].join("\n");
}

export function buildPlasmToolDescription(): string {
  return loadPromptAsset("plasm_tool.txt");
}

export function buildPlasmContextToolDescription(): string {
  return loadPromptAsset("plasm_context_tool.txt");
}

export function buildDiscoverToolDescription(): string {
  return loadPromptAsset("discover_tool.txt");
}

export function buildPlasmRunToolDescription(): string {
  return [
    loadPromptAsset("plasm_run_tool_base.txt"),
    "",
    loadPromptAsset("plasm_run_tool_artifact_tool.txt"),
  ].join("\n");
}

export function buildPlasmReadRunArtifactToolDescription(): string {
  return loadPromptAsset("plasm_read_run_artifact_tool.txt");
}
