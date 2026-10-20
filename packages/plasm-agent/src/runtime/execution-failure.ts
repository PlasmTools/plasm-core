import { z } from "zod";

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
const jsonValueSchema: z.ZodType<JsonValue> = z.lazy(() => z.union([
  z.null(), z.boolean(), z.number().finite(), z.string(),
  z.array(jsonValueSchema), z.record(jsonValueSchema),
]));

const computeInferenceDiagnosticSchema = z.object({
  code: z.string(),
  unmaterializable_type: z.enum(["any", "unknown", "unsupported", "bytes", "enum", "protocol", "new_type", "record", "truth_refinement"]).nullable(),
  message: z.string(),
  compute_line: z.number().int().nonnegative().nullable(),
  compute_column: z.number().int().nonnegative().nullable(),
  source_excerpt: z.string().nullable(),
}).strict();

const collectionReferenceSchema = z.object({
  catalog: z.array(z.number().int().min(0).max(255)).length(32),
  expression: z.array(z.number().int().min(0).max(255)).length(32),
  epoch: z.number().int().nonnegative(),
}).strict();
export const resolvedEffectEvidenceSchema = z.object({
  invocation: z.string().uuid(),
  operation: z.object({ entry_id: z.string(), capability: z.string() }).strict(),
  entity: z.string(),
  target_kind: z.enum(["create", "receiver", "capability"]),
  origin: z.object({
    execution: z.string().uuid(), scope_path: z.array(z.string()),
    occurrence_path: z.array(z.number().int().nonnegative()),
    inputs: z.array(z.object({ binding: z.string(), producer: z.string(), collection: collectionReferenceSchema }).strict()),
    selection: z.object({ collection: collectionReferenceSchema, index: z.number().int().nonnegative() }).strict().nullable(),
  }).strict().nullable(),
}).strict();

export const executionFailureSchema = z.object({
  cause: z.enum(["program", "response_contract", "catalog", "runtime", "upstream", "transport", "authorization", "cancelled", "unclassified"]),
  recovery: z.enum(["repair_program", "obtain_authorization", "reconcile_effects", "stop"]),
  code: z.string(),
  // Diagnostic prose is visible to the agent, never recovery authority.
  diagnostic: z.string(),
  node: z.string().nullable(),
  occurrence_path: z.array(z.number().int().nonnegative()),
  catalog_digest: z.string().nullable(),
  input_occurrence: z.object({
    scope_path: z.array(z.string()), occurrence_path: z.array(z.number().int().nonnegative()),
    source_index: z.number().int().nonnegative(), source_identity: z.string().nullable(),
  }).strict().optional(),
  effects: z.array(z.object({ resolved_effects: z.array(resolvedEffectEvidenceSchema).optional(), entry_id: z.string(), capability: z.string(), completed: z.number().int().nonnegative(), failed: z.number().int().nonnegative(), occurrences: z.array(z.object({ scope_path: z.array(z.string()), occurrence_path: z.array(z.number().int().nonnegative()), source_index: z.number().int().nonnegative(), source_identity: z.string().nullable(), status: z.enum(["completed", "failed"]) }).strict()) }).strict()),
  dispatches: z.array(z.object({
    resolved_effect: resolvedEffectEvidenceSchema.optional(),
    operation: z.object({ entry_id: z.string(), capability: z.string() }).strict(),
    request_fingerprint: z.string(), status: z.enum(["unresolved", "response_received"]),
  }).strict()),
  effects_unresolved: z.boolean(),
  // Original provider envelope is evidence, never recovery authority.
  mcp_tool_result: jsonValueSchema.optional(),
  input_budget: z.object({
    unit: z.enum(["rows", "accounted_bytes"]),
    limit: z.number().int().nonnegative(),
    observed_at_least: z.number().int().nonnegative(),
    input: z.string(),
  }).strict().refine(evidence => evidence.observed_at_least > evidence.limit,
    "budget evidence must establish a violation").optional(),
  program_rejection: z.object({
    category: z.enum(["parse", "type", "plan", "flow"]),
    span_offset: z.number().int().nonnegative().nullable(),
    detail: z.discriminatedUnion("kind", [
      z.object({kind: z.literal("compute_inference"), diagnostics: z.array(computeInferenceDiagnosticSchema)}).strict(),
      z.object({kind: z.literal("union_columns_differ"), left: z.array(z.string()), right: z.array(z.string())}).strict(),
      z.object({kind: z.literal("reserved_binding"), name: z.string()}).strict(),
      z.object({kind: z.literal("static_iteration_required")}).strict(),
      z.object({kind: z.literal("build_mutation")}).strict(),
    ]).nullable(),
  }).strict().optional(),
}).strict().refine(failure => {
  const effects = failure.effects_unresolved || failure.dispatches.length > 0 || failure.effects.some(effect => effect.completed > 0);
  if (failure.recovery === "repair_program") return failure.cause === "program" && !effects;
  if (failure.recovery === "obtain_authorization") return failure.cause === "authorization" && !effects;
  return true;
}, "recovery authority must agree with cause and effect evidence");
export type ExecutionFailure = z.infer<typeof executionFailureSchema>;
export class AgentExecutionFailure extends Error {
  constructor(readonly failure: ExecutionFailure, readonly summary: string) { super(summary); }
}
export function failureObservation(error: unknown) {
  const admitted = error instanceof AgentExecutionFailure && executionFailureSchema.safeParse(error.failure).success;
  const failure: ExecutionFailure = admitted ? error.failure : {
    cause: "unclassified", recovery: "stop", code: "unclassified_execution_failure", diagnostic: error instanceof Error ? error.message : typeof error === "string" ? error : "Unclassified host failure", node: null,
    occurrence_path: [], catalog_digest: null, effects: [], dispatches: [], effects_unresolved: true,
  };
  return {
    status: "execution_failed" as const,
    failure,
    summary: admitted
      ? error.summary
      : "Execution stopped because the host reported an unclassified failure. Effects may be unresolved. Stop this execution.",
  };
}
export const failureObservationSchema = z.object({
  status: z.literal("execution_failed"),
  failure: executionFailureSchema,
  summary: z.string().min(1),
}).strict();

/** The model receives only presentation; host tool results retain typed evidence. */
export function plasmModelOutput({ output }: { output: unknown }) {
  return { type: "text" as const, value: typeof output === "string" ? output : failureObservationSchema.parse(output).summary };
}
/** Only structured host observations own recovery authority; prose cannot grant it. */
export function terminalExecutionFailure(results: readonly { toolName: string; output: unknown }[]): ExecutionFailure | undefined {
  for (const part of results) {
    if (!["plasm", "plasm_run", "plasm_context"].includes(part.toolName)) continue;
    if (!part.output || typeof part.output !== "object" || !("status" in part.output) || part.output.status !== "execution_failed") continue;
    const result = failureObservationSchema.safeParse(part.output);
    if (!result.success) return failureObservation(undefined).failure;
    if (!["repair_program", "reconcile_effects"].includes(result.data.failure.recovery)) return result.data.failure;
  }
  return undefined;
}
