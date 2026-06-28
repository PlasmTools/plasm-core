import { tool, type ToolSet } from "ai";
import { z } from "zod";

import type { AgentRuntime } from "../runtime/agent-runtime.js";
import {
  DISCOVER_TOOL_DESCRIPTION,
  PLASM_CONTEXT_TOOL_DESCRIPTION,
  PLASM_RUN_TOOL_DESCRIPTION,
  PLASM_TOOL_DESCRIPTION,
} from "./descriptions.js";
import { toolInput } from "./tool-input.js";

const seedSchema = z.object({
  api: z.string().describe("Registry entry_id / catalog api"),
  entity: z.string().describe("CGS entity name from discovery or prior knowledge"),
});

const discoverInputSchema = z.object({
  intent: z
    .string()
    .min(1)
    .describe(
      "One plain-language task description for the whole user goal. Returns catalog api/entity picks — not program symbols.",
    ),
});

const plasmContextInputSchema = z.object({
  intent: z
    .string()
    .min(1)
    .describe(
      "Stable string for one user goal (same value every turn for that goal — do not rotate per message).",
    ),
  seeds: z
    .array(seedSchema)
    .min(1)
    .describe("Non-empty array of {api, entity} capability picks"),
  ranked_capabilities: z
    .array(z.string())
    .nullable()
    .optional()
    .describe(
      "Optional capability wire names from discover_capabilities. Omit on expand; null or [] clears.",
    ),
});

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

export function createPlasmTools(runtime: AgentRuntime): ToolSet {
  return {
    discover_capabilities: tool({
      description: DISCOVER_TOOL_DESCRIPTION,
      inputSchema: toolInput(discoverInputSchema),
      execute: async ({ intent }) => runtime.discoverCapabilities({ intent }),
    }),

    plasm_context: tool({
      description: PLASM_CONTEXT_TOOL_DESCRIPTION,
      inputSchema: toolInput(plasmContextInputSchema),
      execute: async ({ intent, seeds, ranked_capabilities }) =>
        runtime.plasmContext({
          intent,
          seeds: seeds as Array<{ api: string; entity: string }>,
          rankedCapabilities: ranked_capabilities,
        }),
    }),

    plasm: tool({
      description: PLASM_TOOL_DESCRIPTION,
      inputSchema: toolInput(plasmInputSchema),
      execute: async ({ logical_session_ref, program, reasoning }) =>
        runtime.plasm({ logicalSessionRef: logical_session_ref, program, reasoning }),
    }),

    plasm_run: tool({
      description: PLASM_RUN_TOOL_DESCRIPTION,
      inputSchema: toolInput(plasmRunInputSchema),
      execute: async ({ logical_session_ref, run_ref, reasoning }) =>
        runtime.plasmRun({
          logicalSessionRef: logical_session_ref,
          runRef: run_ref,
          reasoning,
        }),
    }),
  };
}

export type PlasmTools = ReturnType<typeof createPlasmTools>;
