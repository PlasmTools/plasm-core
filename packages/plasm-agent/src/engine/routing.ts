import { z } from "zod";
import { workflowIntentSchema, intentProvenanceSchema } from "../runtime/session-contract.js";
import { AgentExecutionFailure } from "../runtime/execution-failure.js";

const capability = z.object({ catalog: z.string().min(1), capability: z.string().min(1) }).strict();
export const prerequisiteClosureSchema = z.object({
  business: z.array(capability),
  input_sources: z.array(capability),
  prerequisites: z.array(capability),
  acquisitions: z.array(z.object({
    id: z.string(), provider_catalog: z.string(), provider: z.string(), capability,
    arguments: z.record(z.string(), z.unknown()),
  })),
  edges: z.array(z.object({
    consumer: capability, consumer_instance: z.string().nullable(), provider_instance: z.string(),
  }).passthrough()),
});

const match = z.object({
  capability_id: z.string().min(1),
  choice: z.enum(["relevant", "unrelated", "uncertain"]),
}).strict();

function capabilityKey(value: z.infer<typeof capability>): string {
  return `${value.catalog}\0${value.capability}`;
}

function sameSet(left: Iterable<string>, right: Iterable<string>): boolean {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((value) => b.has(value));
}

export const environmentSupportSchema = z.object({
  choice: z.enum(["established", "not_established", "undetermined"]),
  request_hash: z.string(),
}).strict();

const routingSchema = z.object({
  environment_support: environmentSupportSchema.optional().nullable(),
  intent_provenance: intentProvenanceSchema,
  intent_analysis: z.string().optional(),
  intent: workflowIntentSchema,
  pin_id: z.string().uuid(),
  authorization: z.object({ catalogs: z.array(z.string()), capabilities: z.record(z.string(), z.array(z.string())) }).strict(),
  retrieval: z.object({
    generation: z.string(),
    candidates: z.array(z.object({
      id: z.string(), reference: capability,
      document: z.object({ entity: z.string() }).passthrough(),
    }).passthrough()).max(128),
  }).passthrough(),
  matching: z.object({
    matches: z.array(match),
  }).strict(),
  closure: prerequisiteClosureSchema.nullable(),
  recovery: z.object({
    candidates: z.array(z.object({ reference: capability, choice: z.enum(["relevant", "unrelated", "uncertain"]) }).strict()),
    available_catalogs: z.array(z.object({
      entry_id: z.string().min(1),
      description: z.string().min(1),
    }).strict()),
    guidance: z.string().min(1),
  }).strict().optional().nullable(),
}).strict().superRefine((routing, ctx) => {
  const candidates = new Map(routing.retrieval.candidates.map(c => [c.id, c]));
  const matches = routing.matching.matches;
  const selected = matches.filter(m => m.choice !== "unrelated").flatMap(m => {
    const c = candidates.get(m.capability_id); return c ? [capabilityKey(c.reference)] : [];
  });
  if (candidates.size !== routing.retrieval.candidates.length
    || new Set(routing.retrieval.candidates.map(c => capabilityKey(c.reference))).size !== candidates.size
    || new Set(matches.map(m => m.capability_id)).size !== matches.length
    || !sameSet(matches.map(m => m.capability_id), candidates.keys())) {
    ctx.addIssue({code:"custom",path:["matching"],message:"relevance decisions must cover each candidate exactly once"});
  }
  if (!sameSet(selected, routing.closure?.business.map(capabilityKey) ?? [])
    || (routing.closure?.input_sources.length ?? 0) !== 0) {
    ctx.addIssue({code:"custom",path:["closure"],message:"closure must retain exactly the current relevant and uncertain capabilities"});
  }
  if (Boolean(routing.recovery) !== (selected.length === 0)) {
    ctx.addIssue({code:"custom",path:["recovery"],message:"empty-selection diagnostic contradicts relevance decisions"});
  }
  if (routing.recovery) {
    const expected = matches.flatMap(m => { const c = candidates.get(m.capability_id); return c ? [JSON.stringify([capabilityKey(c.reference),m.choice])] : []; });
    const actual = routing.recovery.candidates.map(c => JSON.stringify([capabilityKey(c.reference),c.choice]));
    if (!sameSet(expected,actual)) ctx.addIssue({code:"custom",path:["recovery"],message:"diagnostics contradict relevance decisions"});
  }
  const permitted = (ref: z.infer<typeof capability>): boolean => routing.authorization.catalogs.includes(ref.catalog)
    && (routing.authorization.capabilities[ref.catalog]?.includes(ref.capability) ?? true);
  const closure = routing.closure;
  if (routing.retrieval.candidates.some((candidate) => !permitted(candidate.reference))
    || (closure && [...closure.business, ...closure.input_sources, ...closure.prerequisites].some((ref) => !permitted(ref)))) {
    ctx.addIssue({ code: "custom", path: ["authorization"], message: "routing exposed an unauthorized capability" });
  }
});

/** The Rust router owns validation; this decoder also rejects mismatched native packages. */
export const routingPacketSchema = z.object({
  routing: routingSchema,
  teaching: z.object({ prompt: z.string(), declarations_changed: z.boolean(), delta_refs: z.array(z.string()) }).nullable(),
});
export type RoutingPacket = z.infer<typeof routingPacketSchema>;
export type PrerequisiteClosure = z.infer<typeof prerequisiteClosureSchema>;

/** Decode the native discovery boundary without disguising contract errors as execution faults. */
export function decodeRoutingPacket(raw: string): RoutingPacket {
  let value: unknown;
  try { value = JSON.parse(raw); } catch {
    throw routingContractFailure("Native discovery response is not valid JSON.");
  }
  const decoded = routingPacketSchema.safeParse(value);
  if (decoded.success) return decoded.data;
  throw routingContractFailure(decoded.error.issues.map(issue => `${issue.path.join(".")}: ${issue.message}`).join("; "));
}

function routingContractFailure(diagnostic: string): AgentExecutionFailure {
  return new AgentExecutionFailure({
    cause: "runtime", recovery: "stop", code: "discovery_response_contract",
    diagnostic, node: null, occurrence_path: [], catalog_digest: null,
    effects: [], dispatches: [], effects_unresolved: false,
  }, `Discovery response failed contract validation: ${diagnostic}`);
}

export function routingExplanationLines(matching: RoutingPacket["routing"]["matching"]): string[] {
  return matching.matches.some(m => m.choice !== "unrelated") ? [] : ["No relevant or uncertain capability selected from the current bounded packet."];
}
export function routingRecoveryMarkdown(routing: RoutingPacket["routing"]): string | null {
  const recovery=routing.recovery;
  if (!recovery) return null;
  return [recovery.guidance,...[...recovery.candidates].sort((a,b)=>capabilityKey(a.reference).localeCompare(capabilityKey(b.reference))).slice(0,3).map(c=>`- \`${c.reference.catalog}/${c.reference.capability}\`: ${c.choice}`),...recovery.available_catalogs.map(c=>`- \`${c.entry_id}\`: ${c.description}`)].join("\n\n");
}

/** Advisory capability evidence; it never grants execution or recovery authority. */
export function routingSupportMarkdown(routing: RoutingPacket["routing"]): string | null {
  const support = routing.environment_support;
  if (!support) return null;
  const guidance: Record<z.infer<typeof environmentSupportSchema>["choice"], string> = {
    established: "The support classifier assessed the supplied operation contracts as sufficient for the current intent. This is an advisory model judgment, not a verified executable plan or a guarantee of matching data, successful execution or permission. Check that the exposed operations cover the requested effects; extend the same logical_session_ref for unresolved needs.",
    not_established: "The supplied operation contracts do not establish a plan for the current intent. Relevant capabilities remain available. Inspect their inputs and outputs, then extend the same logical_session_ref for unresolved needs. This does not mean the task is impossible or that a capability is absent from the catalog.",
    undetermined: "Support for the current intent remains undetermined from the supplied contracts. Relevant capabilities remain available. Inspect the evidence and extend the same logical_session_ref for unresolved needs; do not treat this as proof of impossibility.",
  };
  return guidance[support.choice];
}
