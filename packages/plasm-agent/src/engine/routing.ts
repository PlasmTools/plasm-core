import { z } from "zod";
import { workflowIntentSchema, intentProvenanceSchema } from "../runtime/session-contract.js";

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
  probabilities: z.record(z.string(), z.number()), confidence: z.number().min(0).max(1),
}).strict();

const probabilityChoices = {
  capability: ["relevant", "unrelated", "uncertain"],
} as const;

function capabilityKey(value: z.infer<typeof capability>): string {
  return `${value.catalog}\0${value.capability}`;
}

function sameSet(left: Iterable<string>, right: Iterable<string>): boolean {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((value) => b.has(value));
}

function validateProbabilities(
  probabilities: Record<string, number>,
  choices: readonly string[],
  ctx: z.RefinementCtx,
  path: Array<string | number>,
): void {
  if (!sameSet(Object.keys(probabilities), choices)) {
    ctx.addIssue({ code: "custom", path, message: "probability keys do not match choices" });
    return;
  }
  const values = choices.map((choice) => probabilities[choice] ?? Number.NaN);
  if (values.some((value) => !Number.isFinite(value) || value < 0 || value > 1)) {
    ctx.addIssue({ code: "custom", path, message: "probabilities must be finite values in [0, 1]" });
  }
  const total = values.reduce((sum, value) => sum + value, 0);
  if (Math.abs(total - 1) > 0.01) {
    ctx.addIssue({ code: "custom", path, message: "probabilities must sum to one" });
  }
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
  }).strict(),
  closure: prerequisiteClosureSchema.nullable(),
  recovery: z.object({
    candidates: z.array(z.object({ reference: capability, choice: z.enum(["relevant", "unrelated", "uncertain"]), relevance_probability: z.number().min(0).max(1) }).strict()),
    available_catalogs: z.array(z.object({
      entry_id: z.string().min(1),
      description: z.string().min(1),
    }).strict()),
    guidance: z.string().min(1),
  }).strict().optional().nullable(),
}).strict().superRefine((routing, ctx) => {
  const candidates = new Map(routing.retrieval.candidates.map(c => [c.id, c]));
  const matches = routing.matching.matches;
  const selected = matches.filter(m => m.choice === "relevant").flatMap(m => {
    const c = candidates.get(m.capability_id); return c ? [capabilityKey(c.reference)] : [];
  });
  if (candidates.size !== routing.retrieval.candidates.length
    || new Set(routing.retrieval.candidates.map(c => capabilityKey(c.reference))).size !== candidates.size
    || new Set(matches.map(m => m.capability_id)).size !== matches.length
    || !sameSet(matches.map(m => m.capability_id), candidates.keys())) {
    ctx.addIssue({code:"custom",path:["matching"],message:"relevance decisions must cover each candidate exactly once"});
  }
  matches.forEach((m,i) => validateProbabilities(m.probabilities, probabilityChoices.capability, ctx, ["matching","matches",i,"probabilities"]));
  if (!sameSet(selected, routing.closure?.business.map(capabilityKey) ?? [])
    || (routing.closure?.input_sources.length ?? 0) !== 0) {
    ctx.addIssue({code:"custom",path:["closure"],message:"closure must retain exactly the current relevant capabilities"});
  }
  if (Boolean(routing.recovery) !== (selected.length === 0)) {
    ctx.addIssue({code:"custom",path:["recovery"],message:"empty-selection diagnostic contradicts relevance decisions"});
  }
  if (routing.recovery) {
    const expected = matches.flatMap(m => { const c = candidates.get(m.capability_id); return c ? [JSON.stringify([capabilityKey(c.reference),m.choice,m.probabilities.relevant])] : []; });
    const actual = routing.recovery.candidates.map(c => JSON.stringify([capabilityKey(c.reference),c.choice,c.relevance_probability]));
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
  teaching: z.object({ tsv: z.string(), delta_refs: z.array(z.string()) }).nullable(),
});
export type RoutingPacket = z.infer<typeof routingPacketSchema>;
export type PrerequisiteClosure = z.infer<typeof prerequisiteClosureSchema>;

export function routingExplanationLines(matching: RoutingPacket["routing"]["matching"]): string[] {
  return matching.matches.some(m => m.choice === "relevant") ? [] : ["No relevant capability selected from the current bounded packet."];
}
export function routingRecoveryMarkdown(routing: RoutingPacket["routing"]): string | null {
  const recovery=routing.recovery;
  if (!recovery) return null;
  return [recovery.guidance,...[...recovery.candidates].sort((a,b)=>b.relevance_probability-a.relevance_probability).slice(0,3).map(c=>`- \`${c.reference.catalog}/${c.reference.capability}\`: ${c.choice} (relevance ${c.relevance_probability.toFixed(2)})`),...recovery.available_catalogs.map(c=>`- \`${c.entry_id}\`: ${c.description}`)].join("\n\n");
}
