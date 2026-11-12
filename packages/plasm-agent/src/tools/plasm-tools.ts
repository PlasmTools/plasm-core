import { AgentExecutionFailure, failureObservation, plasmModelOutput } from "../runtime/execution-failure.js";
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
import { logicalSessionRefSchema, workflowIntentSchema } from "../runtime/session-contract.js";
import { failureResultsFor } from "./failure-results.js";

const contextIntent = workflowIntentSchema.describe(
  "Self-contained current discovery request, including relevant conditions and amendments. Prior requests remain audit/search state, not judgment context.",
);
const plasmContextInputSchema = z.object({
    intent: contextIntent,
    session_mode: z.literal("extend"),
    logical_session_ref: logicalSessionRefSchema.describe(
      "Copy the existing workflow reference verbatim; do not reconstruct or abbreviate it.",
    ),
  }).strict();

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
  run_ref: z.string()
    .describe("Copy the exact run_ref returned by plasm, or a page handle from a prior result; never guess a reference."),
}).strict();

export function createPlasmTools(
  runtime: Pick<AgentRuntime, "plasmContext" | "plasm" | "plasmRun" | "recordExecutionFailure"> & Partial<Pick<AgentRuntime, "workflowExampleServingEnabled" | "plasmExample">>,
): ToolSet {
  const tools: ToolSet = {};
  if (runtime.workflowExampleServingEnabled?.()) {
    const fetchExample = runtime.plasmExample?.bind(runtime);
    if (!fetchExample) throw new Error("Example serving requires native fetch support");
    tools.plasm_example = tool({
      description: "Optionally fetch a teaching example offered by discovery. Adapt it to your task: distinguish relevant operations from unrelated effects and do not copy unrequested effects. Fetching checks the source and adds its required declarations to this session; examples do not authorize execution.",
      inputSchema: toolInput(z.object({ logical_session_ref: logicalSessionRefSchema, example_ref: z.string().uuid() }).strict()),
      execute: ({ logical_session_ref, example_ref }) => fetchExample({ logicalSessionRef: logical_session_ref, revisionRef: example_ref }),
    });
  }
  const failures = failureResultsFor(runtime);
  async function invoke(name: string, run: () => Promise<string>) {
    try { return await run(); } catch (error) {
      const observation = failureObservation(error);
      await runtime.recordExecutionFailure(name, observation.failure);
      const diagnosticIncluded = observation.summary.includes(observation.failure.diagnostic);
      // A complete no-effect failure has no ledger to expand; its compact
      // summary already contains the diagnostic and recovery instruction.
      if (!observation.failure.effects_unresolved
        && !observation.failure.input_occurrence && observation.failure.effects.length === 0 && observation.failure.dispatches.length === 0) {
        const action = {
          repair_program: "Correct the program before retrying.",
          obtain_authorization: "Obtain authorization before retrying.",
          reconcile_effects: "Check the prior execution before further writes.",
          replan_after_rejection: "Consider another authorized plan.",
          stop: "Stop and resolve the reported blocker.",
        }[observation.failure.recovery];
        const diagnostic = diagnosticIncluded ? "" : `\n${observation.failure.diagnostic}`;
        return {...observation, summary: `${observation.summary}${diagnostic}\n${action}`};
      }
      const ref = failures.record(observation.failure);
      // Deliver evidence already held by the host in the failing call itself.
      // The existing bounded page supplies a continuation only when needed.
      return {...observation, summary: `${observation.summary}\n${failures.read(ref, 0, diagnosticIncluded)}`};
    }
  }

  tools.plasm_context = tool({
    toModelOutput: plasmModelOutput,
    description: PLASM_CONTEXT_TOOL_DESCRIPTION,
    inputSchema: toolInput(plasmContextInputSchema),
    execute: async (args) => invoke("plasm_context", () => {
      if (args.session_mode !== "extend") throw new AgentExecutionFailure({
        cause: "program", recovery: "repair_program", code: "invalid_context_input",
        diagnostic: "The host already opened this workflow; extend its logical_session_ref.",
        node: null, occurrence_path: [], catalog_digest: null, effects: [], dispatches: [], effects_unresolved: false,
      }, "Extend the host-opened workflow using its logical_session_ref.");
      return runtime.plasmContext({
        intent: args.intent,
        sessionMode: args.session_mode,
        logicalSessionRef: args.logical_session_ref,
      });
    }),
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

  tools.plasm_failure = tool({
    description: "Read all recorded partial outcomes of a failed Plasm call. Use the failure_ref returned by that failure; follow returned offsets for more entries. Does not retry, execute, or observe the backend. References belong to this runtime and its most recent 128 failures.",
    inputSchema: toolInput(z.object({failure_ref: z.string().regex(/^failure_[0-9a-f-]{36}$/), offset: z.number().int().nonnegative().default(0)}).strict()),
    execute: async ({failure_ref, offset}) => failures.read(failure_ref, offset ?? 0),
  });

  return tools;
}

export type PlasmTools = ReturnType<typeof createPlasmTools>;
