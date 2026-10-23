import { z } from "zod";

// Names and observed field presence only: never render values (including secrets)
// or infer missing types/bindings from output order or source text.
export const runOutputArtifactSchema = z.object({
  run_id: z.string().regex(/^pr[a-f0-9]{64}$/),
  producer_binding: z.string().nullable(),
  snapshot: z.unknown(),
});

const outputSchema = z.object({
  entry_id: z.string(),
  entity: z.string(),
  row_materialization: z.enum(["Catalog", "Computed"]),
  entities: z.array(z.object({
    completeness: z.string(),
    fields: z.record(z.string(), z.unknown()),
  })),
});

type OutputArtifact = z.infer<typeof runOutputArtifactSchema>;
const text = (value: string): string => value.replace(/[\r\n`]/g, " ");

export function formatRecordedOutputs(artifacts: readonly OutputArtifact[]): string {
  if (!artifacts.length) return "";
  const lines = artifacts.map((artifact) => {
    const parsed = outputSchema.safeParse(artifact.snapshot);
    const binding = artifact.producer_binding === null ? "binding unavailable" : text(artifact.producer_binding);
    if (!parsed.success) return `- \`${artifact.run_id}\` — ${binding}; output schema unavailable`;
    const output = parsed.data;
    const counts = new Map<string, number>();
    for (const row of output.entities) {
      for (const field of Object.keys(row.fields)) counts.set(field, (counts.get(field) ?? 0) + 1);
    }
    const fields = [...counts].sort(([a], [b]) => a.localeCompare(b)).map(([field, count]) =>
      text(field) + (count === output.entities.length ? "" : " (some rows)"));
    const kind = output.row_materialization === "Catalog" ? `${text(output.entry_id)}::${text(output.entity)}` : "computed output (not importable)";
    const complete = output.entities.every(row => row.completeness === "complete");
    return `- \`${artifact.run_id}\` — ${binding}: ${kind}[]; ${output.entities.length} rows; observed fields: ${fields.join(", ") || "none"}${complete ? "" : "; incomplete rows"}`;
  });
  return "\n\n**Recorded outputs (historical):**\n" + lines.join("\n");
}
