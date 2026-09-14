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

export type SystemLiturgyOptions = {
  /**
   * Same gate as `createHarnessTools({ includeEvalTerminals })`.
   * When those tools are registered, teach Voice B completion
   * (`complete_task` / `submit_answer`) — never “Stop with no tool call.”
   */
  includeEvalTerminals?: boolean;
};

/**
 * Opening paragraph when eval terminal tools are registered (T214120).
 * Voice B: end through `complete_task` (no reportable value) or
 * `submit_answer` (reportable value). T144504 those tasks used `complete_task`.
 * T214120 31dc/325 followed Voice A prose-stop after `plasm_run`.
 */
const EVAL_TERMINAL_OPENING =
  "Plan from the current requirement, taught symbols and observed results. " +
  "After observations establish every requested effect, end through complete_task " +
  "when the instruction asked for no reportable value, or submit_answer with that " +
  "value when it did. Intermediate results alone are not completion.";

function withEvalTerminalCompletion(workflow: string): string {
  const lines = workflow.split("\n");
  const title = lines[0] ?? "";
  let i = 1;
  while (i < lines.length && lines[i] === "") i += 1;
  while (i < lines.length && lines[i] !== "") i += 1;
  return [title, "", EVAL_TERMINAL_OPENING, ...lines.slice(i)].join("\n");
}

/**
 * Framework system liturgy for every PlasmAgent turn.
 * Plan→Act→Observe cycle + language law + tool-only resource rite —
 * not product/AppWorld overlays. Eval-terminal completion is gated:
 * without `includeEvalTerminals`, do not invent `complete_task` / `submit_answer`.
 */
export function buildDefaultSystemLiturgy(options: SystemLiturgyOptions = {}): string {
  const workflow = options.includeEvalTerminals
    ? withEvalTerminalCompletion(loadPromptAsset("initialize_workflow.txt"))
    : loadPromptAsset("initialize_workflow.txt");
  return [
    workflow,
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
