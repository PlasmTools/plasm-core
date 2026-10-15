import { failureObservation, plasmModelOutput } from "../runtime/execution-failure.js";
import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { AgentRuntime } from "../runtime/agent-runtime.js";
import {
  PLASM_CONTEXT_TOOL_DESCRIPTION,
  PLASM_RUN_TOOL_DESCRIPTION,
  PLASM_TOOL_DESCRIPTION,
} from "./descriptions.js";
import { loadPromptAsset } from "../prompts/index.js";
import { toolInput } from "./tool-input.js";
import { logicalSessionRefSchema, runReferenceSchema, workflowIntentSchema } from "../runtime/session-contract.js";

const contextIntent = workflowIntentSchema.describe(
  "The current intent for this discovery turn. The runtime appends it to the workflow provenance chain and retains ancestor context; explicit revisions may replace earlier goals.",
);
const plasmContextInputSchema = z.discriminatedUnion("session_mode", [
  z.object({ intent: contextIntent, session_mode: z.literal("new") }).strict(),
  z.object({
    intent: contextIntent,
    session_mode: z.literal("extend"),
    logical_session_ref: logicalSessionRefSchema.describe(
      "Copy the existing workflow reference verbatim; do not reconstruct or abbreviate it.",
    ),
  }).strict(),
]);

const plasmInputSchema = z.object({
  logical_session_ref: logicalSessionRefSchema
    .describe("Copy the logical_session_ref returned by plasm_context verbatim; do not reconstruct or abbreviate it."),
  program: z
    .string()
    .min(1)
    .describe(loadPromptAsset("program_param.txt")),
}).strict();

const plasmRunInputSchema = z.object({
  logical_session_ref: logicalSessionRefSchema
    .describe("Copy the logical_session_ref returned by plasm_context verbatim; do not reconstruct or abbreviate it."),
  run_ref: runReferenceSchema
    .describe("pcN from plasm dry-run, or page handle from a prior plasm_run more-pages line"),
}).strict();

export function createPlasmTools(
  runtime: Pick<AgentRuntime, "plasmContext" | "plasm" | "plasmRun" | "recordExecutionFailure">,
): ToolSet {
  const tools: ToolSet = {};
  async function invoke(name: string, run: () => Promise<string>) {
    try { return await run(); } catch (error) {
      const observation = failureObservation(error);
      await runtime.recordExecutionFailure(name, observation.failure);
      return observation;
    }
  }

  tools.plasm_context = tool({
    toModelOutput: plasmModelOutput,
    description: PLASM_CONTEXT_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmContextInputSchema),
    execute: async (args) => invoke("plasm_context", () => runtime.plasmContext({
      intent: args.intent,
      sessionMode: args.session_mode,
      logicalSessionRef: args.session_mode === "extend" ? args.logical_session_ref : undefined,
    })),
  });

  tools.plasm = tool({
    toModelOutput: plasmModelOutput,
    description: PLASM_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmInputSchema),
    execute: async ({ logical_session_ref, program }) =>
      invoke("plasm", () => runtime.plasm({
        logicalSessionRef: logical_session_ref,
        program,
      })),
  });

  tools.plasm_run = tool({
    toModelOutput: plasmModelOutput,
    description: PLASM_RUN_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmRunInputSchema),
    execute: async ({ logical_session_ref, run_ref }) =>
      invoke("plasm_run", () => runtime.plasmRun({
        logicalSessionRef: logical_session_ref,
        runRef: run_ref,
      })),
  });

  return tools;
}

export type PlasmTools = ReturnType<typeof createPlasmTools>;
