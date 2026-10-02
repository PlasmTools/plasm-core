import { z } from "zod";

export const executionFailureSchema = z.object({
  cause: z.enum(["program", "response_contract", "catalog", "runtime", "upstream", "transport", "authorization", "cancelled", "unclassified"]),
  recovery: z.enum(["repair_program", "obtain_authorization", "reconcile_effects", "stop"]),
  code: z.string(),
  // Diagnostic prose is visible to the agent, never recovery authority.
  diagnostic: z.string(),
  node: z.string().nullable(),
  occurrence_path: z.array(z.number().int().nonnegative()),
  catalog_digest: z.string().nullable(),
  effects: z.array(z.object({ entry_id: z.string(), capability: z.string(), completed: z.number().int().nonnegative(), failed: z.number().int().nonnegative(), occurrences: z.array(z.object({ source_index: z.number().int().nonnegative(), source_identity: z.string().nullable(), status: z.enum(["completed", "failed"]) }).strict()) }).strict()),
  dispatches: z.array(z.object({
    operation: z.object({ entry_id: z.string(), capability: z.string() }).strict(),
    request_fingerprint: z.string(), status: z.enum(["unresolved", "response_received"]),
  }).strict()),
  effects_unresolved: z.boolean(),
}).strict().refine(failure => {
  const effects = failure.effects_unresolved || failure.dispatches.length > 0 || failure.effects.some(effect => effect.completed > 0);
  if (failure.recovery === "repair_program") return failure.cause === "program" && !effects;
  if (failure.recovery === "obtain_authorization") return failure.cause === "authorization" && !effects;
  return true;
}, "recovery authority must agree with cause and effect evidence");
export type ExecutionFailure = z.infer<typeof executionFailureSchema>;
export class AgentExecutionFailure extends Error {
  constructor(readonly failure: ExecutionFailure) { super(`${failure.code}: ${failure.diagnostic}`); }
}
export function failureObservation(error: unknown) {
  const failure: ExecutionFailure = error instanceof AgentExecutionFailure && executionFailureSchema.safeParse(error.failure).success ? error.failure : {
    cause: "unclassified", recovery: "stop", code: "unclassified_execution_failure", diagnostic: error instanceof Error ? error.message : typeof error === "string" ? error : "Unclassified host failure", node: null,
    occurrence_path: [], catalog_digest: null, effects: [], dispatches: [], effects_unresolved: true,
  };
  return {
    status: "execution_failed" as const, failure,
    ...(failure.recovery === "reconcile_effects" ? {
      recovery_instructions: "This execution stopped. Inspect its completed writes and unresolved dispatches before choosing the next program. Failed dispatches may have taken effect; service error bodies prove neither success nor absence. A new execution is permitted and does not resume or roll back this execution.",
    } : {}),
  };
}
/** Only structured host observations own recovery authority; prose cannot grant it. */
export function terminalExecutionFailure(messages: readonly { role: string; content: unknown }[]): ExecutionFailure | undefined {
  for (const message of messages) {
    if (message.role !== "tool" || !Array.isArray(message.content)) continue;
    for (const part of message.content) {
      if (!part || typeof part !== "object" || !["plasm", "plasm_run", "plasm_context"].includes(part.toolName)) continue;
      const output = part.output?.type === "json" ? part.output.value : part.output;
      if (output?.status !== "execution_failed") continue;
      const result = executionFailureSchema.safeParse(output.failure);
      if (!result.success) return failureObservation(undefined).failure;
      if (!["repair_program", "reconcile_effects"].includes(result.data.recovery)) return result.data;
    }
  }
  return undefined;
}
