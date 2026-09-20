import { createHash } from "node:crypto";
import { z } from "zod";
import { parseLogicalSessionWireRef, formatLogicalSessionWireRef } from "./logical-session.js";

/** PostgreSQL text/JSONB and native UTF-8 must agree without replacing characters. */
export const unicodeTextSchema = z.string().refine(
  (value) => !value.includes("\0") && Buffer.from(value, "utf8").toString("utf8") === value,
  "text must contain Unicode scalars without U+0000",
);

export const workflowIntentSchema = unicodeTextSchema
  .refine((value) => /[^\p{White_Space}]/u.test(value), "intent must not be empty")
  .brand<"WorkflowIntent">();
export type WorkflowIntent = z.infer<typeof workflowIntentSchema>;

export const intentProvenanceSchema = z.object({
  nodes: z.array(z.object({
    parent: z.number().int().nonnegative().nullable(),
    intent: workflowIntentSchema,
  }).strict()).min(1),
}).strict().refine(({ nodes }) => nodes.every((node, index) => node.parent === (index === 0 ? null : index - 1)),
  "intent parent must be the preceding node");
export type IntentProvenance = z.infer<typeof intentProvenanceSchema>;

/** Every derivation retains its complete ancestry; no role is privileged by name. */
export function deriveIntent(ancestry: IntentProvenance | undefined, intent: WorkflowIntent): IntentProvenance {
  const nodes = ancestry ? intentProvenanceSchema.parse(ancestry).nodes : [];
  return intentProvenanceSchema.parse({ nodes: [...nodes, { parent: nodes.length ? nodes.length - 1 : null, intent }] });
}

export const logicalSessionRefSchema = z.string().refine((value) => {
  try {
    return formatLogicalSessionWireRef(parseLogicalSessionWireRef(value)) === value;
  } catch { return false; }
}, "expected a canonical logical session ref").brand<"LogicalSessionRef">();
export type LogicalSessionRef = z.infer<typeof logicalSessionRefSchema>;

export const sessionIdentitySchema = z.object({
  tenantScope: unicodeTextSchema.refine((value) => value.length > 0, "tenant must not be empty"),
  logicalSessionRef: logicalSessionRefSchema,
}).strict();
export type SessionIdentity = z.infer<typeof sessionIdentitySchema>;

/** A structured tuple has no delimiter ambiguities; only its digest becomes a key. */
export function sessionStorageKey(identity: SessionIdentity): string {
  const valid = sessionIdentitySchema.parse(identity);
  return createHash("sha256")
    .update(JSON.stringify([valid.tenantScope, valid.logicalSessionRef]), "utf8")
    .digest("hex");
}

export function sessionTenantKey(tenantScope: string): string {
  return createHash("sha256").update(unicodeTextSchema.parse(tenantScope), "utf8").digest("hex");
}
