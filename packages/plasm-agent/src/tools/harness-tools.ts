import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { SkillDefinition } from "../authoring/define-skill.js";
import type { SubagentRegistry } from "../authoring/subagent-loader.js";
import { toolInput } from "./tool-input.js";

const readSkillInputSchema = z.object({
  name: z.string().min(1).describe("Skill name from the index"),
});

export function createHarnessTools(options: {
  skills?: SkillDefinition[];
  subagents?: SubagentRegistry;
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
