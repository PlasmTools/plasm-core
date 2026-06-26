import { zodSchema, type FlexibleSchema } from "ai";
import type { z } from "zod";

/** Wrap Zod for AI SDK `tool()` — avoids TS2589 deep instantiation during Vercel typecheck. */
export function toolInput<T extends z.ZodTypeAny>(
  schema: T,
): FlexibleSchema<z.infer<T>> {
  return zodSchema(schema);
}
