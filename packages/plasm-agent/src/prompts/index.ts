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
  /**
   * Same gate as `createHarnessTools({ includeTaskLedger })`.
   * Eval/A-B factor only — product default is unset (corrected current loop).
   */
  includeTaskLedger?: boolean;
};

/**
 * Named completion-contract slot in `initialize_workflow.txt`.
 * Overlay replaces this exact paragraph — never the first paragraph by index.
 */
export const WORKFLOW_COMPLETION_SLOT =
  "Plan from the current requirement, taught symbols and observed results. " +
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
  "Call plasm_context before claiming unsupported access or ending the task. " +
  "After observations establish every requested effect, end through complete_task " +
  "when the instruction asked for no reportable value, or submit_answer with that " +
  "value when it did. A request for a single number, amount, count, name, or other " +
  "value requires that exact one value only: never a labelled breakdown, explanation, " +
  "unit, or table. Intermediate results alone are not completion.";

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
 * Named plan-memory sentence in `initialize_workflow.txt` (already in the
 * crates asset). Overlay only when the task-ledger experiment is on.
 */
export const TASK_LEDGER_PLAN_SLOT =
  "If requested effects remain absent, revise the plan within the same session.";

export const TASK_LEDGER_PLAN_OVERLAY =
  "If requested effects remain absent, revise the host task ledger; do not reconstruct scope from conversation alone.";

/**
 * Static ledger rite — composed as its own section, not spliced into a
 * paragraph index. The current record is *not* system law; each loop
 * iteration presents a separate model-authored state echo.
 */
export const TASK_LEDGER_LITURGY = [
  "## Task ledger",
  "",
  "`task_ledger` writes the host record for this instruction: requested outcomes and scope, conditions that authorize alternatives, observed facts, interpretations, completed effects (with evidence refs), unresolved questions, and remaining work.",
  "",
  "The host stores your write and echoes it. It does not add outcomes, remaining work, or authorized alternatives, and it does not decide that the instruction is satisfied.",
  "",
  "Write `observed_facts` only for tool-cited observations (`kind` is `fact`). Every fact requires `evidence_ref` citing a run_id, plasm:// URI, run_ref, or tool name. The host checks that the citation is a known shape; it does not treat the citation as proof. Write `interpretations` separately (`kind` is `interpretation`). `completed_effects.satisfaction` is always an interpretation.",
  "",
  "An alternative action is authorized only when the user’s stated condition is met. A failed preferred action is not that condition.",
  "",
  "Each decision sees a host echo of your last accepted write as model-authored state — not host law and not a completion contract. Revise the record when evidence changes. Completion still follows the contract above — the ledger is not a terminal.",
].join("\n");

/**
 * Framework system liturgy for every PlasmAgent turn.
 * Plan→Act→Observe cycle + language law + tool-only resource rite —
 * not product/AppWorld overlays. Eval-terminal completion is gated:
 * without `includeEvalTerminals`, do not invent `complete_task` / `submit_answer`.
 */
export function buildDefaultSystemLiturgy(options: SystemLiturgyOptions = {}): string {
  const workflowAsset = loadPromptAsset("initialize_workflow.txt");
  let workflow = options.includeEvalTerminals
    ? overlayWorkflowCompletion(workflowAsset, EVAL_TERMINAL_COMPLETION)
    : workflowAsset;
  if (options.includeTaskLedger) {
    workflow = overlayNamedSlot(
      workflow,
      TASK_LEDGER_PLAN_SLOT,
      TASK_LEDGER_PLAN_OVERLAY,
      "TASK_LEDGER_PLAN_SLOT",
    );
  }
  const sections = [
    workflow,
    "",
    "## Resource handling (tool-only host)",
    loadPromptAsset("plasm_run_tool_artifact_tool.txt"),
    loadPromptAsset("plasm_read_run_artifact_tool.txt"),
    "",
    "## Plasm language (`plasm` tool)",
    loadPromptAsset("plasm_tool.txt"),
  ];
  if (options.includeTaskLedger) {
    sections.push("", TASK_LEDGER_LITURGY);
  }
  return sections.join("\n");
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
