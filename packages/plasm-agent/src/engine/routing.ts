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

const scopeBudgetRetention = z.object({
  capability_id: z.string().min(1),
  reference: capability,
  capabilities: z.number().int().nonnegative().safe(),
  capability_limit: z.number().int().positive().safe(),
  bytes: z.number().int().nonnegative().safe(),
  byte_limit: z.number().int().positive().safe(),
}).strict().refine(value => value.capabilities > value.capability_limit || value.bytes > value.byte_limit,
  {message: "budget retention requires an exceeded scope limit"});

function capabilityKey(value: z.infer<typeof capability>): string {
  return `${value.catalog}\0${value.capability}`;
}

function sameSet(left: Iterable<string>, right: Iterable<string>): boolean {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((value) => b.has(value));
}

const routingSchema = z.object({
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
    budget_retained: z.array(scopeBudgetRetention).max(0).optional(),
  }).strict(),
  search: z.object({
    policy: z.literal("additive-connected-frontier-current-hybrid-evidence-3-judgments-3-attempts-16000-byte-packet-v42"),
    exposed: z.array(capability),
    considered: z.array(capability),
    unexplored: z.array(capability),
    deferred: z.array(z.object({
      reference: capability,
      reason: z.enum(["requests", "input_bytes", "acquisition_cycle", "reported_input_tokens", "operation_pending", "operation_uncertain", "provider_unavailable"]),
    }).strict()),
    requests: z.number().int().min(0).max(3),
    input_bytes: z.number().int().min(0).max(48000),
    reported_input_tokens: z.number().int().nonnegative().safe(),
    missing_usage: z.number().int().min(0).max(3),
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
  const budgetRetained = routing.matching.budget_retained ?? [];
  if (new Set(budgetRetained.map(value => value.capability_id)).size !== budgetRetained.length
    || budgetRetained.some(value => {
      const candidate = candidates.get(value.capability_id);
      return !candidate || capabilityKey(candidate.reference) !== capabilityKey(value.reference)
        || !matches.some(match => match.capability_id === value.capability_id && match.choice === "uncertain");
    })) {
    ctx.addIssue({code: "custom", path: ["matching", "budget_retained"],
      message: "budget outcomes must uniquely identify retained uncertain candidates"});
  }
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
  if (Boolean(routing.recovery) !== (selected.length === 0 && routing.search.exposed.length === 0
    && routing.search.deferred.length === 0)) {
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
  const considered = new Set(routing.search.considered.map(capabilityKey));
  const exposed = new Set(routing.search.exposed.map(capabilityKey));
  const unexplored = new Set(routing.search.unexplored.map(capabilityKey));
  if (considered.size !== routing.search.considered.length
    || exposed.size !== routing.search.exposed.length
    || routing.retrieval.candidates.some(candidate => exposed.has(capabilityKey(candidate.reference)))
    || routing.search.unexplored.some(ref => exposed.has(capabilityKey(ref)))
    || unexplored.size !== routing.search.unexplored.length
    || routing.retrieval.candidates.some(candidate => unexplored.has(capabilityKey(candidate.reference)))
    || (closure?.prerequisites.some(ref => unexplored.has(capabilityKey(ref))) ?? false)
    || new Set(routing.search.deferred.map(item => capabilityKey(item.reference))).size !== routing.search.deferred.length
    || routing.search.deferred.some(item => !unexplored.has(capabilityKey(item.reference))
      && !(closure?.prerequisites.some(ref => capabilityKey(ref) === capabilityKey(item.reference)) ?? false)
      && !((item.reason === "operation_pending" || item.reason === "operation_uncertain" || item.reason === "provider_unavailable")
        && routing.retrieval.candidates.some(candidate => capabilityKey(candidate.reference) === capabilityKey(item.reference))))
    || routing.retrieval.candidates.some(candidate => !considered.has(capabilityKey(candidate.reference)))
    || routing.search.missing_usage > routing.search.requests
    || (routing.search.requests === 0 ? routing.search.input_bytes !== 0
      : routing.search.input_bytes === 0 || routing.search.input_bytes > routing.search.requests * 16000
        )
    || (closure?.prerequisites.some(ref => !considered.has(capabilityKey(ref))) ?? false)) {
    ctx.addIssue({code: "custom", path: ["search"], message: "bounded search accounting contradicts judged capabilities or required acquisitions"});
  }
  if (routing.retrieval.candidates.some((candidate) => !permitted(candidate.reference))
    || [...routing.search.exposed, ...routing.search.considered, ...routing.search.unexplored,
      ...routing.search.deferred.map(item => item.reference)].some(ref => !permitted(ref))
    || (closure && [...closure.business, ...closure.input_sources, ...closure.prerequisites].some((ref) => !permitted(ref)))) {
    ctx.addIssue({ code: "custom", path: ["authorization"], message: "routing exposed an unauthorized capability" });
  }
});

/** The Rust router owns validation; this decoder also rejects mismatched native packages. */
export const routingPacketSchema = z.object({
  routing: routingSchema,
  teaching: z.object({ prompt: z.string(), declarations_changed: z.boolean(), delta_refs: z.array(z.string()) }).nullable(),
  workflow_examples: z.array(z.object({
    example_ref: z.string().uuid(), purpose: z.string(),
    outputs: z.array(z.string()),
    assurance: z.enum(["agent_attested", "postcondition_verified"]),
  }).strict()).max(3).optional(),
  workflow_recall: z.enum(["completed", "pending", "unavailable", "timed_out"]).optional(),
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
