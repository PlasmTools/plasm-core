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
  | "program_param.txt"
  | "plasm_tool.txt"
  | "python-plasm-dag.txt"
  | "plasm_run_tool_base.txt";

function readPromptAsset(name: PromptAssetName): string {
  const monorepo = path.join(monorepoAssetsDir, name);
  const vendored = path.join(vendoredDir, name);
  const file = existsSync(monorepo) ? monorepo : vendored;
  if (!existsSync(file)) {
    throw new Error(`missing Plasm prompt asset: ${name} (looked in ${monorepo} and ${vendored})`);
  }
  return readFileSync(file, "utf8");
}

export function loadPromptAsset(name: PromptAssetName): string {
  return readPromptAsset(name).trim();
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
 * Named completion-contract slot in `initialize_workflow.txt`.
 * Overlay replaces this exact paragraph — never the first paragraph by index.
 */
const RECEIPT_COMPLETION =
  "Successful execution receipts are evidence of their confirmed effects. " +
  "Do not repeat a confirmed write or reread solely to reconfirm it; inspect state when a requested condition or an effect remains unresolved. ";

export const WORKFLOW_COMPLETION_SLOT =
  "Plan from the current requirement, taught symbols and observed results. " +
  RECEIPT_COMPLETION +
  "Stop with no tool call when observations establish every requested effect. " +
  "Intermediate results alone are not completion.";

/**
 * Eval overlay for `WORKFLOW_COMPLETION_SLOT` when terminal tools are registered.
 * Voice B: end through `complete_task` (no reportable value) or
 * `submit_answer` (reportable value). T144504 those tasks used `complete_task`.
 * T214120 31dc/325 followed Voice A prose-stop after `plasm_run`.
 */
export const EVAL_TERMINAL_COMPLETION =
  "Plan from the current requirement, taught symbols and observed results. " +
  RECEIPT_COMPLETION +
  "After observations establish every requested effect, end through complete_task " +
  "when the instruction asked for no reportable value, or submit_answer with that " +
  "value when it did. Writing a value into an app or sending it to another person is an effect, not an answer to the requester. A request to return a single number, amount, count, name, or other " +
  "value requires that exact one value only: never a labelled breakdown, explanation, " +
  "unit, or table. Intermediate results alone are not completion.";

export const WORKFLOW_INITIALIZATION_SLOT =
  'Open `plasm_context` with `session_mode: "new"` and the original `intent`.';

export const EVAL_WORKFLOW_INITIALIZATION =
  'The host supplies initial discovery from the task description. Reuse its `logical_session_ref`; call `plasm_context` with `session_mode: "extend"` for further discovery.';

/**
 * Replace exactly one named slot. Missing or duplicate is a hard error —
 * never first-paragraph / index surgery.
 */
export function overlayNamedSlot(
  source: string,
  slot: string,
  replacement: string,
  slotName: string,
): string {
  const parts = source.split(slot);
  if (parts.length !== 2) {
    throw new Error(
      `initialize_workflow.txt must contain exactly one ${slotName}; found ${parts.length - 1}`,
    );
  }
  return `${parts[0]}${replacement}${parts[1]}`;
}

/**
 * Overlay `completion` onto the named `WORKFLOW_COMPLETION_SLOT`.
 * Missing or duplicate slot is a hard error — not first-paragraph surgery.
 */
export function overlayWorkflowCompletion(workflow: string, completion: string): string {
  return overlayNamedSlot(workflow, WORKFLOW_COMPLETION_SLOT, completion, "WORKFLOW_COMPLETION_SLOT");
}


/**
 * Framework system liturgy for every PlasmAgent turn.
 * Plan→Act→Observe cycle; language semantics belong to tool descriptions —
 * not product/AppWorld overlays. Eval-terminal completion is gated:
 * without `includeEvalTerminals`, do not invent `complete_task` / `submit_answer`.
 */
export function buildDefaultSystemLiturgy(options: SystemLiturgyOptions = {}): string {
  const workflowAsset = loadPromptAsset("initialize_workflow.txt");
  let workflow = options.includeEvalTerminals
    ? overlayWorkflowCompletion(workflowAsset, EVAL_TERMINAL_COMPLETION)
    : workflowAsset;
  workflow = overlayNamedSlot(workflow, WORKFLOW_INITIALIZATION_SLOT,
    EVAL_WORKFLOW_INITIALIZATION, "WORKFLOW_INITIALIZATION_SLOT");
  return workflow;
}

export function buildPlasmToolDescription(): string {
  // Same assembly as plasm-core's PLASM_MCP_TOOL_DESCRIPTION.
  return `${readPromptAsset("plasm_tool.txt")}\n\n${readPromptAsset("python-plasm-dag.txt")}`.trim();
}

export function buildPlasmContextToolDescription(): string {
  return loadPromptAsset("plasm_context_tool.txt");
}

export function buildDiscoverToolDescription(): string {
  return loadPromptAsset("discover_tool.txt");
}

export function buildPlasmRunToolDescription(): string {
  return loadPromptAsset("plasm_run_tool_base.txt");
}
