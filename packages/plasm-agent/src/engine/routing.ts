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
      unsupported: z.array(z.object({ intent_quote: z.string().min(1), reason: z.string().min(1) }).strict()),
    }).strict().refine((selection) => selection.status === (selection.unsupported.length ? "insufficient" : "ready"), "routing status contradicts sufficiency"),
    closure: closure.nullable(),
  }),
  teaching: z.object({ tsv: z.string(), delta_refs: z.array(z.string()) }).nullable(),
});
export type RoutingPacket = z.infer<typeof routingPacketSchema>;
export type PrerequisiteClosure = z.infer<typeof closure>;

export function routingExplanationLines(selection: RoutingPacket["routing"]["selection"]): string[] {
  return selection.unsupported.map((work) => `Unsupported: ${work.intent_quote} — ${work.reason}`);
}
