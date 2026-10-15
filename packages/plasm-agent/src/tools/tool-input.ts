import { jsonSchema, zodSchema, type FlexibleSchema } from "ai";
import type { z } from "zod";

/** Wrap Zod for AI SDK `tool()` — avoids TS2589 deep instantiation during Vercel typecheck. */
export function toolInput<T extends z.ZodTypeAny>(
  schema: T,
): FlexibleSchema<z.infer<T>> {
  const converted = zodSchema(schema);
  // Tool arguments are object envelopes, including discriminated object unions.
  // Preserve their branches while making the provider's root contract explicit.
  return jsonSchema<z.infer<T>>(
    () => ({ ...converted.jsonSchema, type: "object" }),
    { validate: converted.validate },
  );
}
