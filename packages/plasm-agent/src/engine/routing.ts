import { z } from "zod";
import { workflowIntentSchema, intentProvenanceSchema } from "../runtime/session-contract.js";

const capability = z.object({ catalog: z.string().min(1), capability: z.string().min(1) }).strict();
const slot = z.object({ id: z.string().min(1), statement: workflowIntentSchema }).strict();
export const discoveryCoverageSchema = z.object({
  obligations: z.array(z.object({ slot, matched_capabilities: z.array(capability) }).strict()),
}).strict().superRefine((coverage, ctx) => {
  const statements = new Set<string>();
  coverage.obligations.forEach((entry, index) => {
    if (entry.slot.id !== `s${index}` || statements.has(entry.slot.statement)
      || new Set(entry.matched_capabilities.map(capabilityKey)).size !== entry.matched_capabilities.length) {
      ctx.addIssue({ code: "custom", message: "coverage has invalid slot identity or duplicate statements/matches" });
    }
    statements.add(entry.slot.statement);
  });
});
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
  slot_id: z.string().min(1), capability_id: z.string().min(1),
  choice: z.enum(["direct_match", "does_not_match", "uncertain"]),
  probabilities: z.record(z.string(), z.number()), confidence: z.number().min(0).max(1),
}).strict();

const inputPath = z.object({
  lane: z.enum(["scope", "selection", "controls", "arguments", "payload"]),
  path: z.array(z.string().min(1)).min(1),
}).strict();

const inputSourceBinding = z.object({
  consumer: capability,
  input: inputPath,
  provider: capability,
  output_field: z.string().min(1),
  collect: z.boolean(),
}).strict();

const inputSourceCandidate = z.object({
  provider: capability,
  bindings: z.array(inputSourceBinding).min(1),
}).strict();

const probabilityChoices = {
  capability: ["direct_match", "does_not_match", "uncertain"],
  inputSource: ["required_source", "not_required", "uncertain"],
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
  coverage: discoveryCoverageSchema,
  retrieval: z.object({
    generation: z.string(),
    candidates: z.array(z.object({
      id: z.string(), reference: capability,
      document: z.object({ entity: z.string() }).passthrough(),
    }).passthrough()),
  }).passthrough(),
  matching: z.object({
    slots: z.array(z.object({ id: z.string().min(1), statement: z.string().min(1) }).strict()).min(1),
    matches: z.array(match),
    complete: z.boolean(),
    unmatched_slot_ids: z.array(z.string().min(1)),
    additional_capability_ids: z.array(z.string()),
  }).strict(),
  input_source_projection: z.array(inputSourceCandidate),
  input_source_matching: z.object({
    matches: z.array(z.object({
      provider: capability,
      choice: z.enum(probabilityChoices.inputSource),
      probabilities: z.record(z.string(), z.number()),
      confidence: z.number().min(0).max(1),
    }).strict()),
    selected: z.array(capability),
  }).strict(),
  closure: prerequisiteClosureSchema.nullable(),
  recovery: z.object({
    unmatched_slots: z.array(z.object({
      slot_id: z.string().min(1), statement: z.string().min(1),
      candidates: z.array(z.object({
        reference: capability,
        choice: z.enum(["does_not_match", "uncertain"]),
        direct_match_probability: z.number().min(0).max(1),
      }).strict()),
    }).strict()),
    available_catalogs: z.array(z.object({
      entry_id: z.string().min(1),
      description: z.string().min(1),
    }).strict()),
    guidance: z.string().min(1),
  }).strict().optional().nullable(),
}).superRefine((routing, ctx) => {
  const slotIds = new Set(routing.matching.slots.map((slot) => slot.id));
  const candidateIds = new Set(routing.retrieval.candidates.map((candidate) => candidate.id));
  const pairKeys = new Set<string>();
  const directSlots = new Set<string>();
  const directCandidates = new Set<string>();
  routing.matching.matches.forEach((candidateMatch, index) => {
    const pairKey = `${candidateMatch.slot_id}\0${candidateMatch.capability_id}`;
    if (!slotIds.has(candidateMatch.slot_id) || !candidateIds.has(candidateMatch.capability_id)) {
      ctx.addIssue({ code: "custom", path: ["matching", "matches", index], message: "match references an unknown slot or candidate" });
    }
    if (pairKeys.has(pairKey)) {
      ctx.addIssue({ code: "custom", path: ["matching", "matches", index], message: "duplicate slot/candidate match" });
    }
    pairKeys.add(pairKey);
    validateProbabilities(candidateMatch.probabilities, probabilityChoices.capability, ctx, ["matching", "matches", index, "probabilities"]);
    if (candidateMatch.choice === "direct_match") {
      directSlots.add(candidateMatch.slot_id);
      directCandidates.add(candidateMatch.capability_id);
    }
  });
  const expectedPairs = slotIds.size * candidateIds.size;
  if (pairKeys.size !== expectedPairs) {
    ctx.addIssue({ code: "custom", path: ["matching", "matches"], message: "match matrix is incomplete" });
  }
  const unmatched = [...slotIds].filter((slotId) => !directSlots.has(slotId));
  if (routing.matching.complete !== (unmatched.length === 0)
    || !sameSet(routing.matching.unmatched_slot_ids, unmatched)) {
    ctx.addIssue({ code: "custom", path: ["matching"], message: "completion fields contradict match choices" });
  }
  if (routing.matching.additional_capability_ids.some((id) => !directCandidates.has(id))) {
    ctx.addIssue({ code: "custom", path: ["matching", "additional_capability_ids"], message: "additional capabilities must be direct matches" });
  }

  const projectedProviders = new Set(routing.input_source_projection.map((entry) => capabilityKey(entry.provider)));
  const matchedProviders = new Set<string>();
  const requiredProviders = new Set<string>();
  routing.input_source_matching.matches.forEach((sourceMatch, index) => {
    const provider = capabilityKey(sourceMatch.provider);
    if (!projectedProviders.has(provider) || matchedProviders.has(provider)) {
      ctx.addIssue({ code: "custom", path: ["input_source_matching", "matches", index], message: "input-source match references an unknown or duplicate provider" });
    }
    matchedProviders.add(provider);
    validateProbabilities(sourceMatch.probabilities, probabilityChoices.inputSource, ctx, ["input_source_matching", "matches", index, "probabilities"]);
    if (sourceMatch.choice === "required_source") requiredProviders.add(provider);
  });
  if (!sameSet(projectedProviders, matchedProviders)) {
    ctx.addIssue({ code: "custom", path: ["input_source_matching", "matches"], message: "input-source match matrix is incomplete" });
  }
  if (!sameSet(routing.input_source_matching.selected.map(capabilityKey), requiredProviders)) {
    ctx.addIssue({ code: "custom", path: ["input_source_matching", "selected"], message: "selected input sources contradict match choices" });
  }
  const obligations = new Map(routing.coverage.obligations.map((entry) => [entry.slot.id, entry]));
  for (const slot of routing.matching.slots) {
    if (obligations.get(slot.id)?.slot.statement !== slot.statement) {
      ctx.addIssue({ code: "custom", path: ["coverage"], message: "matching changed an obligation" });
    }
  }
  for (const entry of routing.matching.matches.filter((entry) => entry.choice === "direct_match")) {
    const candidate = routing.retrieval.candidates.find((candidate) => candidate.id === entry.capability_id);
    if (candidate && !obligations.get(entry.slot_id)?.matched_capabilities.some((ref) => capabilityKey(ref) === capabilityKey(candidate.reference))) {
      ctx.addIssue({ code: "custom", path: ["coverage"], message: "coverage omitted a direct match" });
    }
  }
  const unresolved = routing.coverage.obligations.filter((entry) => entry.matched_capabilities.length === 0);
  if ((unresolved.length > 0) !== Boolean(routing.recovery)
    || !sameSet(unresolved.map((entry) => entry.slot.id), routing.recovery?.unmatched_slots.map((entry) => entry.slot_id) ?? [])) {
    ctx.addIssue({ code: "custom", path: ["recovery"], message: "recovery must preserve every unresolved obligation" });
  }
  for (const entry of routing.recovery?.unmatched_slots ?? []) {
    if (obligations.get(entry.slot_id)?.slot.statement !== entry.statement) {
      ctx.addIssue({ code: "custom", path: ["recovery"], message: "recovery changed an obligation" });
    }
    const judgments = routing.matching.matches.filter((match) => match.slot_id === entry.slot_id);
    const expected = judgments.map((match) => {
      const candidate = routing.retrieval.candidates.find((candidate) => candidate.id === match.capability_id);
      return candidate && JSON.stringify([capabilityKey(candidate.reference), match.choice, match.probabilities.direct_match]);
    });
    if (!sameSet(expected.filter((key): key is string => Boolean(key)), entry.candidates.map((candidate) =>
      JSON.stringify([capabilityKey(candidate.reference), candidate.choice, candidate.direct_match_probability])))) {
      ctx.addIssue({ code: "custom", path: ["recovery"], message: "recovery judgments contradict matching" });
    }
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
  return matching.complete
    ? []
    : matching.unmatched_slot_ids.map((slotId) => `No retrieved capability matched effect slot: ${slotId}`);
}

/** Lead with unresolved needs + coverage audit + available integration descriptions. */
export function routingRecoveryMarkdown(routing: RoutingPacket["routing"]): string | null {
  const recovery = routing.recovery;
  if (!recovery) return null;
  const lines = [
    `**plasm_context:** ${routing.closure ? "teaching available; unresolved discovery slots remain" : "unresolved discovery slots"}`,
    recovery.unmatched_slots.length
      ? "**Unmatched affirmative effect slots** (bounded packet result):"
      : "",
    ...recovery.unmatched_slots.flatMap((slot) => {
      const candidates = [...slot.candidates].sort((left, right) => right.direct_match_probability - left.direct_match_probability
        || capabilityKey(left.reference).localeCompare(capabilityKey(right.reference)));
      return [
        `- \`${slot.slot_id}\` — ${slot.statement}`,
        ...(candidates.length ? candidates.slice(0, 3).map((candidate) =>
          `  - \`${candidate.reference.catalog}/${candidate.reference.capability}\` — ${candidate.choice === "does_not_match" ? "rejected" : "uncertain"} (match probability ${candidate.direct_match_probability.toFixed(2)})`)
          : ["  - No authorized candidates retrieved in this packet."]),
        ...(candidates.length > 3 ? [`  - ${candidates.length - 3} other judgments in the routing receipt.`] : []),
      ];
    }),
    recovery.guidance,
    recovery.available_catalogs.length
      ? "**Available integrations** (descriptions for broader rediscovery):"
      : "",
    ...recovery.available_catalogs.map(
      (catalog) => `- \`${catalog.entry_id}\` — ${catalog.description}`,
    ),
  ].filter(Boolean);
  return lines.join("\n\n");
}
