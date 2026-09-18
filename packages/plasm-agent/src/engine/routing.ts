import { z } from "zod";

const capability = z.object({ catalog: z.string(), capability: z.string() });
const closure = z.object({
  business: z.array(capability),
  prerequisites: z.array(capability),
  acquisitions: z.array(z.object({
    id: z.string(), provider_catalog: z.string(), provider: z.string(), capability,
    arguments: z.record(z.string(), z.unknown()),
  })),
  edges: z.array(z.object({
    consumer: capability, consumer_instance: z.string().nullable(), provider_instance: z.string(),
  }).passthrough()),
});

const description = z.string().refine((value) => value.trim().length > 0, "expected nonempty description");
const evidenceIds = z.array(z.string().min(1)).refine(
  (ids) => new Set(ids).size === ids.length,
  "duplicate capability evidence ID",
);
const requirementAssessment = z.union([
  z.object({ no_capability_needed: description }).strict(),
  z.object({ supported_by: evidenceIds.refine((ids) => ids.length > 0, "support requires capability evidence") }).strict(),
  z.object({ useful_capabilities: evidenceIds, missing: description }).strict(),
]);
const requirementCoverage = z.object({
  requirement: description,
  assessment: requirementAssessment,
}).strict();

/** The Rust router owns validation; this decoder also rejects mismatched native packages. */
export const routingPacketSchema = z.object({
  routing: z.object({
    intent_analysis: z.string().optional(),
    intent_evidence: z.unknown().optional(),
    intent: z.string(),    pin_id: z.string().uuid(),
    retrieval: z.object({
      generation: z.string(),
      candidates: z.array(z.object({
        id: z.string(), reference: capability,
        document: z.object({ entity: z.string() }).passthrough(),
      }).passthrough()),
    }).passthrough(),
    selection: z.object({
      status: z.enum(["ready", "insufficient"]),
      additional_capability_ids: z.array(z.string()),
      requirement_coverage: z.array(requirementCoverage),
    }).strict().refine(
      (selection) =>
        selection.status ===
        (selection.requirement_coverage.some((c) => "missing" in c.assessment)
          ? "insufficient"
          : "ready"),
      "routing status contradicts sufficiency",
    ),
    closure: closure.nullable(),
    recovery: z.object({
      unresolved: z.array(z.object({ requirement: z.string().min(1), reason: z.string().min(1) }).strict()),
      requirement_coverage: z.array(requirementCoverage),
      available_catalogs: z.array(z.object({
        entry_id: z.string().min(1),
        description: z.string().min(1),
      }).strict()),
      guidance: z.string().min(1),
    }).strict().optional().nullable(),
  }),
  teaching: z.object({ tsv: z.string(), delta_refs: z.array(z.string()) }).nullable(),
});
export type RoutingPacket = z.infer<typeof routingPacketSchema>;
export type PrerequisiteClosure = z.infer<typeof closure>;

export function routingExplanationLines(selection: RoutingPacket["routing"]["selection"]): string[] {
  return selection.requirement_coverage.flatMap((work) =>
    "missing" in work.assessment
      ? [`Unresolved: ${work.requirement} — ${work.assessment.missing}`]
      : [],
  );
}

/** Lead with unresolved needs + coverage audit + available integration descriptions. */
export function routingRecoveryMarkdown(routing: RoutingPacket["routing"]): string | null {
  const recovery = routing.recovery;
  if (!recovery) return null;
  const coverage = recovery.requirement_coverage;
  const lines = [
    `**plasm_context:** ${routing.selection.status}`,
    recovery.unresolved.length
      ? "**Unresolved** (intent not fully covered by presented capabilities):"
      : "",
    ...recovery.unresolved.map((work) => `- \`${work.requirement}\` — ${work.reason}`),
    coverage.length ? "**Requirement coverage** (audit; not execution approval):" : "",
    ...coverage.map((entry) =>
      "missing" in entry.assessment
        ? `- \`${entry.requirement}\` — unresolved: ${entry.assessment.missing}; useful: ${entry.assessment.useful_capabilities.join(", ")}`
        : "no_capability_needed" in entry.assessment
          ? `- ${entry.requirement} — local: ${entry.assessment.no_capability_needed}`
          : `- \`${entry.requirement}\` — supporting: ${entry.assessment.supported_by.join(", ")}`,
    ),
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
