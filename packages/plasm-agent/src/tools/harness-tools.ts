import { runArtefactTransform } from "./artifact-process.js";
export { runArtefactTransform } from "./artifact-process.js";

import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { SkillDefinition } from "../authoring/define-skill.js";
import type { SubagentRegistry } from "../authoring/subagent-loader.js";
import { COMPLETE_TASK_TOOL_NAME, SUBMIT_ANSWER_TOOL_NAME } from "./format.js";
import { toolInput } from "./tool-input.js";

const readSkillInputSchema = z.object({
  name: z.string().min(1).describe("Skill name from the index"),
});

const artefactTransformInputSchema = z.object({
  code: z
    .string()
    .min(1)
    .describe(
      "JavaScript (not TypeScript syntax) async function body or expression. " +
        "Helpers: readText(rel), readJson(rel), writeText(rel, s), writeJson(rel, v), list(rel?). " +
        "Workspace is the artefact dir only. Return a value or use console.log.",
    ),
  reasoning: z.string().optional().describe("Optional short note"),
});

export const PLASM_ARTEFACT_TRANSFORM_TOOL_DESCRIPTION = `Harness **artefact transform** (data manipulation only).

Run sandboxed JavaScript against files under the task artefact workspace
(\`PLASM_RUN_ARTIFACTS_DIR\` / agent artefact root). After \`plasm_read_run_artifact\`,
snapshots land in \`artefacts/<run_id>.json\` and \`artefacts/latest.json\`.

Allowed: read/write relative paths under the workspace; JSON/CSV/string compute.
Forbidden: fetch/network, child_process, absolute paths outside workspace, AppWorld HTTP.

Return free-form stdout / returned value. Not a Plasm language feature.`;

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
  "Do not call this when the instruction asked you to report a value.";

/** Domain-general AppWorld submit_answer — verbatim asked-for value, then terminate. */
export const SUBMIT_ANSWER_TOOL_DESCRIPTION =
  "End the task when the instruction asked for a reportable value. " +
  "`answer` is that value exactly as requested — a number, name, or other exact string. " +
  "Pass it verbatim. When the answer is a monetary value, submit only the numeric part — no currency symbols or comma groupings. " +
  "Not a done-summary, not a table, not surrounding prose. " +
  "Do not call this when the instruction asked for no value (use complete_task instead). " +
  "An empty or missing answer is invalid and does not finish the task.";

export function createCompleteTaskTool(): ToolSet {
  return {
    [COMPLETE_TASK_TOOL_NAME]: tool({
      description: COMPLETE_TASK_TOOL_DESCRIPTION,
      inputSchema: toolInput(completeTaskInputSchema),
      execute: async () => "Task marked complete.",
    }),
  };
}

export function createSubmitAnswerTool(): ToolSet {
  return {
    [SUBMIT_ANSWER_TOOL_NAME]: tool({
      description: SUBMIT_ANSWER_TOOL_DESCRIPTION,
      inputSchema: toolInput(submitAnswerInputSchema),
      execute: async () => "Answer submitted.",
    }),
  };
}

export function createEvalTerminalTools(): ToolSet {
  return {
    ...createCompleteTaskTool(),
    ...createSubmitAnswerTool(),
  };
}

export function createArtefactTransformTool(workspaceRoot: string): ToolSet {
  return {
    plasm_artefact_transform: tool({
      description: PLASM_ARTEFACT_TRANSFORM_TOOL_DESCRIPTION,
      inputSchema: toolInput(artefactTransformInputSchema),
      execute: async ({ code }) => {
        try {
          return await runArtefactTransform(workspaceRoot, code);
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
  /** When set, always register plasm_artefact_transform against this workspace. */
  artefactWorkspaceRoot?: string;
  includeArtefactTransform?: boolean;
  /**
   * Register `complete_task` / `submit_answer`. Same gate as
   * `buildDefaultSystemLiturgy({ includeEvalTerminals })`.
   */
  includeEvalTerminals?: boolean;
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
    options.includeArtefactTransform ?? Boolean(options.artefactWorkspaceRoot);
  if (includeTransform && options.artefactWorkspaceRoot) {
    Object.assign(tools, createArtefactTransformTool(options.artefactWorkspaceRoot));
  }

  if (options.includeEvalTerminals) {
    Object.assign(tools, createEvalTerminalTools());
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
