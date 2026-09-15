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

const requirementCoverage = z
  .object({
    requirement: z.string().min(1),
    supporting_capability_ids: z.array(z.string()),
    unresolved_reason: z.string().default(""),
  })
  .strict()
  .refine(
    (entry) => {
      const supporting = entry.supporting_capability_ids.length > 0;
      const unresolved = entry.unresolved_reason.trim().length > 0;
      return (supporting && !unresolved) || (!supporting && unresolved);
    },
    "requirement coverage must associate supporting IDs or an unresolved reason, not both or neither",
  );

/** The Rust router owns validation; this decoder also rejects mismatched native packages. */
export const routingPacketSchema = z.object({
  routing: z.object({
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
        (selection.requirement_coverage.some((c) => c.unresolved_reason.trim().length > 0)
          ? "insufficient"
          : "ready"),
      "routing status contradicts sufficiency",
    ),
    closure: closure.nullable(),
    recovery: z.object({
      unresolved: z.array(z.object({ requirement: z.string().min(1), reason: z.string().min(1) }).strict()),
      requirement_coverage: z.array(requirementCoverage).optional().default([]),
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
  return selection.requirement_coverage
    .filter((c) => c.unresolved_reason.trim().length > 0)
    .map((work) => `Unresolved: ${work.requirement} — ${work.unresolved_reason}`);
}

/** Lead with unresolved needs + coverage audit + available integration descriptions. */
export function routingRecoveryMarkdown(routing: RoutingPacket["routing"]): string | null {
  const recovery = routing.recovery;
  if (!recovery) return null;
  const coverage = recovery.requirement_coverage?.length
    ? recovery.requirement_coverage
    : routing.selection.requirement_coverage;
  const lines = [
    `**plasm_context:** ${routing.selection.status}`,
    recovery.unresolved.length
      ? "**Unresolved** (intent not fully covered by presented capabilities):"
      : "",
    ...recovery.unresolved.map((work) => `- \`${work.requirement}\` — ${work.reason}`),
    coverage.length ? "**Requirement coverage** (audit; not execution approval):" : "",
    ...coverage.map((entry) =>
      entry.unresolved_reason.trim()
        ? `- \`${entry.requirement}\` — unresolved: ${entry.unresolved_reason}`
        : `- \`${entry.requirement}\` — supporting: ${entry.supporting_capability_ids.join(", ")}`,
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
