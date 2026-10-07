import { failureObservation } from "../runtime/execution-failure.js";
import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { AgentRuntime } from "../runtime/agent-runtime.js";
import {
  PLASM_CONTEXT_TOOL_DESCRIPTION,
  PLASM_RUN_TOOL_DESCRIPTION,
  PLASM_TOOL_DESCRIPTION,
} from "./descriptions.js";
import { toolInput } from "./tool-input.js";
import { logicalSessionRefSchema } from "../runtime/session-contract.js";

const plasmContextInputSchema = z.object({
  intent: z
    .string()
    .min(1)
    .describe(
      "The current intent for this discovery turn. The runtime appends it to the workflow provenance chain and retains ancestor context; explicit revisions may replace earlier goals.",
    ),
  session_mode: z
    .enum(["new", "extend"])
    .optional()
    .describe(
      'Use "new" once per workflow; "extend" on later turns with the same logical_session_ref. Defaults to new when omitted.',
    ),
  logical_session_ref: logicalSessionRefSchema
    .optional()
    .describe("Required on session_mode extend. Copy the logical_session_ref from plasm_context verbatim; do not reconstruct or abbreviate it."),
}).strict();

const plasmInputSchema = z.object({
  logical_session_ref: logicalSessionRefSchema
    .describe("Copy the logical_session_ref returned by plasm_context verbatim; do not reconstruct or abbreviate it."),
  program: z
    .string()
    .min(1)
    .describe("Python Program subclass using e#/m#/r# and wire fields from the session domain declarations"),
  reasoning: z
    .string()
    .optional()
    .describe("Optional short note explaining the intent of this call"),
});

const plasmRunInputSchema = z.object({
  logical_session_ref: logicalSessionRefSchema
    .describe("Copy the logical_session_ref returned by plasm_context verbatim; do not reconstruct or abbreviate it."),
  run_ref: z
    .string()
    .describe("pcN from plasm dry-run, or page handle from a prior plasm_run more-pages line"),
  reasoning: z
    .string()
    .optional()
    .describe("Optional short note explaining the intent of this call"),
});

export function createPlasmTools(
  runtime: Pick<AgentRuntime, "plasmContext" | "plasm" | "plasmRun">,
): ToolSet {
  const tools: ToolSet = {};
  async function invoke(run: () => Promise<string>) {
    try { return await run(); } catch (error) {
      return failureObservation(error);
    }
  }

  tools.plasm_context = tool({
    description: PLASM_CONTEXT_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmContextInputSchema),
    execute: async (args) => invoke(() => runtime.plasmContext({
      intent: args.intent,
      sessionMode: args.session_mode ?? "new",
      logicalSessionRef: args.logical_session_ref,
    })),
  });

  tools.plasm = tool({
    description: PLASM_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmInputSchema),
    execute: async ({ logical_session_ref, program, reasoning }) =>
      invoke(() => runtime.plasm({
        logicalSessionRef: logical_session_ref,
        program,
        reasoning,
      })),
  });

  tools.plasm_run = tool({
    description: PLASM_RUN_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmRunInputSchema),
    execute: async ({ logical_session_ref, run_ref, reasoning }) =>
      invoke(() => runtime.plasmRun({
        logicalSessionRef: logical_session_ref,
        runRef: run_ref,
        reasoning,
      })),
  });

  return tools;
}

export type PlasmTools = ReturnType<typeof createPlasmTools>;
