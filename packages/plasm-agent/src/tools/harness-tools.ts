import { artifactRuntimeAvailable, runArtefactTransform } from "./artifact-process.js";
export { artifactRuntimeAvailable, pinnedArtifactImage, runArtefactTransform } from "./artifact-process.js";

import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { SkillDefinition } from "../authoring/define-skill.js";
import type { SubagentRegistry } from "../authoring/subagent-loader.js";
import { COMPLETE_TASK_TOOL_NAME, SUBMIT_ANSWER_TOOL_NAME } from "./format.js";
import { createTaskLedgerTool, type TaskLedgerStore } from "./task-ledger.js";
import { toolInput } from "./tool-input.js";

const readSkillInputSchema = z.object({
  name: z.string().min(1).describe("Skill name from the index"),
});

const artefactTransformInputSchema = z.object({
  paths: z.array(z.string()).min(1).max(16).describe("File locators returned by plasm_read_run_artifact, in argument order"),
  code: z.string().min(1).describe("TypeScript module exporting a default function: export default (artifacts: any[]) => derivedResult"),
});

const completeTaskInputSchema = z.object({});

const submitAnswerInputSchema = z.object({
  answer: z.string().min(1).describe("Reportable value."),
});

/** Domain-general AppWorld complete_task — successful call grades explicit null. */
export const COMPLETE_TASK_TOOL_DESCRIPTION =
  "End the task when the instruction asked for no reportable value. " +
  "Call this after the work is done and nothing must be reported " +
  "(no number, name, amount, or other asked-for value). " +
  "A successful call grades explicit null. Do not pass an answer field. " +
  "Do not call this when the instruction asked you to report a value. " +
  "Requires an initial plasm_context discovery for this workflow first.";

/** Domain-general AppWorld submit_answer — verbatim asked-for value, then terminate. */
export const SUBMIT_ANSWER_TOOL_DESCRIPTION =
  "End the task when the instruction asked for a reportable value. " +
  "`answer` is that value exactly as requested — a number, name, or other exact string. " +
  "Pass it verbatim. When the answer is a monetary value, submit only the numeric part — no currency symbols or comma groupings. " +
  "When the instruction asks how much, how many, a total, a count, or another single value, submit exactly one bare scalar; do not provide a breakdown, labels, units, or multiple values. " +
  "Not a done-summary, not a table, not surrounding prose. " +
  "Do not call this when the instruction asked for no value (use complete_task instead). " +
  "An empty or missing answer is invalid and does not finish the task. " +
  "Requires an initial plasm_context discovery for this workflow first.";

/** Structural gate: environment-action evals must discover before abandon / no-access claims. */
export const EVAL_TERMINAL_REQUIRES_DISCOVERY =
  "Initial discovery is required before ending an environment-action evaluation. " +
  "Call plasm_context with session_mode new and the task intent first. " +
  "Do not claim unsupported access or abandon before discovery.";

export type EvalTerminalGate = {
  /**
   * Return true once initial `plasm_context` has opened a workflow session.
   * When omitted, terminals are ungated (non-eval or tests that inject tools directly).
   */
  discoveryCompleted?: () => boolean;
};

/**
 * Discovery refusal must take the tool-error path (throw), never ordinary
 * success text — otherwise the loop grades a blocked terminal as completion.
 */
function assertDiscoveryCompleted(gate?: EvalTerminalGate): void {
  if (!gate?.discoveryCompleted) return;
  if (gate.discoveryCompleted()) return;
  throw new Error(EVAL_TERMINAL_REQUIRES_DISCOVERY);
}

export function createCompleteTaskTool(gate?: EvalTerminalGate): ToolSet {
  return {
    [COMPLETE_TASK_TOOL_NAME]: tool({
      description: COMPLETE_TASK_TOOL_DESCRIPTION,
      inputSchema: toolInput(completeTaskInputSchema),
      execute: async () => {
        assertDiscoveryCompleted(gate);
        return "Task marked complete.";
      },
    }),
  };
}

export function createSubmitAnswerTool(gate?: EvalTerminalGate): ToolSet {
  return {
    [SUBMIT_ANSWER_TOOL_NAME]: tool({
      description: SUBMIT_ANSWER_TOOL_DESCRIPTION,
      inputSchema: toolInput(submitAnswerInputSchema),
      execute: async () => {
        assertDiscoveryCompleted(gate);
        return "Answer submitted.";
      },
    }),
  };
}

export function createEvalTerminalTools(gate?: EvalTerminalGate): ToolSet {
  return {
    ...createCompleteTaskTool(gate),
    ...createSubmitAnswerTool(gate),
  };
}

export const PLASM_ARTEFACT_TRANSFORM_TOOL_DESCRIPTION = `Process materialized artifacts with TypeScript. Pass their file locators in paths and export a default function receiving the parsed JSON artifacts in the same order. Example: export default ([snapshot]: any[]) => Object.keys(snapshot). The function may be async. Return only the needed JSON summary (maximum 8192 bytes). Inputs stay outside model context. No network or external side effects; execution is isolated and bounded to 30 seconds. This is harness computation, not Plasm language.`;

export function createArtefactTransformTool(workspaceRoot: string): ToolSet {
  return {
    plasm_artefact_transform: tool({
      description: PLASM_ARTEFACT_TRANSFORM_TOOL_DESCRIPTION,
      inputSchema: toolInput(artefactTransformInputSchema),
      execute: async ({ code, paths }) => {
        try {
          return await runArtefactTransform(workspaceRoot, code, paths);
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          // Tool errors must stay in-band — never throw out of execute (kills the agent process).
          return (
            `**plasm_artefact_transform** failed: ${msg}\n` +
            `Use a relative path under the artefact workspace (e.g. \`latest.json\` after plasm_read_run_artifact), ` +
            `or continue with \`plasm\` / \`plasm_run\`.`
          );
        }
      },
    }),
  };
}

export function createHarnessTools(options: {
  skills?: SkillDefinition[];
  subagents?: SubagentRegistry;
  /** When set, register plasm_artefact_transform only if an artifact runtime is configured. */
  artefactWorkspaceRoot?: string;
  includeArtefactTransform?: boolean;
  /**
   * Register `complete_task` / `submit_answer`. Same gate as
   * `buildDefaultSystemLiturgy({ includeEvalTerminals })`.
   */
  includeEvalTerminals?: boolean;
  /**
   * When eval terminals are registered, require initial discovery before
   * `complete_task` / `submit_answer` succeed. Pass `() => runtime.hasOpenWorkflow()`.
   */
  discoveryCompleted?: () => boolean;
  /**
   * Register `task_ledger`. Same gate as
   * `buildDefaultSystemLiturgy({ includeTaskLedger })`.
   * Requires `taskLedgerStore` — host persist only, no invented obligations.
   */
  includeTaskLedger?: boolean;
  taskLedgerStore?: TaskLedgerStore;
}): ToolSet {
  const tools: ToolSet = {};
  const skillByName = new Map((options.skills ?? []).map((s) => [s.name, s]));

  if (skillByName.size > 0) {
    tools.read_skill = tool({
      description:
        "Load full text for a filesystem skill by name. Use when the skill index in the system prompt is not enough.",
      inputSchema: toolInput(readSkillInputSchema),
      execute: async ({ name }: { name: string }) => {
        const skill = skillByName.get(name);
        if (!skill) {
          return `Unknown skill "${name}". Available: ${[...skillByName.keys()].join(", ")}`;
        }
        return skill.body.trim();
      },
    });
  }

  const subagentNames = options.subagents?.list().map((s) => s.name) ?? [];
  if (subagentNames.length > 0 && options.subagents) {
    const subagents = options.subagents;
    tools.delegate_subagent = tool({
      description:
        "Delegate a sub-task to a filesystem-isolated child agent. Each subagent has its own catalogs and session scope.",
      inputSchema: toolInput(
        z.object({
          name: z
            .string()
            .min(1)
            .describe(`Subagent name. One of: ${subagentNames.join(", ")}`),
          message: z.string().min(1).describe("User message for the child agent turn"),
        }),
      ),
      execute: async ({ name, message }: { name: string; message: string }) => {
        const result = await subagents.delegate(name, message);
        return `${result.text}\n\n(steps: ${result.steps})`;
      },
    });
  }

  const includeTransform =
    (options.includeArtefactTransform ?? Boolean(options.artefactWorkspaceRoot)) &&
    artifactRuntimeAvailable();
  if (includeTransform && options.artefactWorkspaceRoot) {
    Object.assign(tools, createArtefactTransformTool(options.artefactWorkspaceRoot));
  }

  if (options.includeEvalTerminals) {
    Object.assign(
      tools,
      createEvalTerminalTools(
        options.discoveryCompleted
          ? { discoveryCompleted: options.discoveryCompleted }
          : undefined,
      ),
    );
  }

  if (options.includeTaskLedger) {
    if (!options.taskLedgerStore) {
      throw new Error("includeTaskLedger requires taskLedgerStore");
    }
    Object.assign(tools, createTaskLedgerTool(options.taskLedgerStore));
  }

  return tools;
}

export function renderSkillIndex(skills: SkillDefinition[]): string {
  if (!skills.length) return "";
  const lines = skills.map((skill) => {
    const desc = skill.description ?? "Filesystem skill";
    return `- **${skill.name}** — ${desc} (use read_skill to load)`;
  });
  return ["## Skill index", ...lines].join("\n");
}
