import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { AgentRuntime } from "../runtime/agent-runtime.js";
import {
  PLASM_CONTEXT_TOOL_DESCRIPTION,
  PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION,
  PLASM_RUN_TOOL_DESCRIPTION,
  PLASM_TOOL_DESCRIPTION,
} from "./descriptions.js";
import { toolInput } from "./tool-input.js";

const plasmContextInputSchema = z.object({
  intent: z
    .string()
    .min(1)
    .describe(
      "The current business need. Session continuity uses logical_session_ref.",
    ),
  session_mode: z
    .enum(["new", "extend"])
    .optional()
    .describe(
      'Use "new" once per workflow; "extend" on later turns with the same logical_session_ref. Defaults to new when omitted.',
    ),
  logical_session_ref: z
    .string()
    .optional()
    .describe("Required continuity handle on session_mode extend (from plasm_context)."),
}).strict();

const plasmInputSchema = z.object({
  logical_session_ref: z
    .string()
    .describe("Same logical_session_ref returned by plasm_context"),
  program: z
    .string()
    .min(1)
    .describe("Plasm source program using e#/m#/p#/r# from the session teaching TSV"),
  reasoning: z
    .string()
    .optional()
    .describe("Optional short note explaining the intent of this call"),
});

const plasmRunInputSchema = z.object({
  logical_session_ref: z
    .string()
    .describe("Same logical_session_ref returned by plasm_context"),
  run_ref: z
    .string()
    .describe("pcN from plasm dry-run, or page handle from a prior plasm_run more-pages line"),
  reasoning: z
    .string()
    .optional()
    .describe("Optional short note explaining the intent of this call"),
});

const plasmReadRunArtifactInputSchema = z
  .object({
    logical_session_ref: z
      .string()
      .describe("Same logical_session_ref returned by plasm_context"),
    run_id: z
      .string()
      .min(1)
      .optional()
      .describe("Run snapshot id (pr… hex) from _meta.plasm.steps[] / markdown"),
    artifact_uri: z
      .string()
      .min(1)
      .optional()
      .describe("plasm://… snapshot URI from _meta.plasm.steps[] / resource_link"),
    reasoning: z
      .string()
      .optional()
      .describe("Optional short note explaining the intent of this call"),
  })
  .strict()
  .refine(
    (value) =>
      [Boolean(value.run_id?.trim()), Boolean(value.artifact_uri?.trim())].filter(Boolean)
        .length === 1,
    { message: "provide exactly one of run_id or artifact_uri" },
  );

export function createPlasmTools(runtime: AgentRuntime): ToolSet {
  const tools: ToolSet = {};

  tools.plasm_context = tool({
    description: PLASM_CONTEXT_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmContextInputSchema),
    execute: async (args) =>
      runtime.plasmContext({
        intent: args.intent,
        sessionMode: args.session_mode ?? "new",
        logicalSessionRef: args.logical_session_ref,
      }),
  });

  tools.plasm = tool({
    description: PLASM_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmInputSchema),
    execute: async ({ logical_session_ref, program, reasoning }) => {
      try {
        return await runtime.plasm({
          logicalSessionRef: logical_session_ref,
          program,
          reasoning,
        });
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        return `**plasm** error — revise \`program\` on the same logical_session_ref:\n\n${msg}`;
      }
    },
  });

  tools.plasm_run = tool({
    description: PLASM_RUN_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmRunInputSchema),
    execute: async ({ logical_session_ref, run_ref, reasoning }) => {
      try {
        return await runtime.plasmRun({
          logicalSessionRef: logical_session_ref,
          runRef: run_ref,
          reasoning,
        });
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        return `**plasm_run** error:\n\n${msg}`;
      }
    },
  });

  tools.plasm_read_run_artifact = tool({
    description: PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmReadRunArtifactInputSchema),
    execute: async ({ logical_session_ref, run_id, artifact_uri, reasoning }) =>
      runtime.readRunArtifact({
        logicalSessionRef: logical_session_ref,
        runId: run_id,
        artifactUri: artifact_uri,
        reasoning,
      }),
  });

  return tools;
}

export type PlasmTools = ReturnType<typeof createPlasmTools>;
