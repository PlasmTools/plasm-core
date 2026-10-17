import { asSchema, JSONParseError, type ToolSet } from "ai";
import { z } from "zod";

// Project validation facts only: SDK error messages can embed rejected input.
const issueSchema = z.object({
  code: z.string(),
  path: z.array(z.union([z.string(), z.number()])),
  expected: z.string().optional(),
  keys: z.array(z.string()).optional(),
  minimum: z.number().optional(),
  maximum: z.number().optional(),
  inclusive: z.boolean().optional(),
  exact: z.boolean().optional(),
  type: z.string().optional(),
});
const issuesSchema = z.object({ issues: z.array(issueSchema) });

export function toolValidationFeedback(error: unknown, toolName: string, tools: ToolSet) {
  const seen = new Set<unknown>();
  let current = error;
  let issues: z.infer<typeof issueSchema>[] = [];
  let kind: "json" | "schema" | "unavailable" = "unavailable";
  while (current instanceof Error && !seen.has(current)) {
    seen.add(current);
    if (JSONParseError.isInstance(current)) kind = "json";
    const parsed = issuesSchema.safeParse(current);
    if (parsed.success) { issues = parsed.data.issues; kind = "schema"; break; }
    current = current.cause;
  }
  const definition = tools[toolName];
  return {
    kind,
    issues,
    // The registered schema is the authority; do not maintain a repair schema.
    ...(definition ? { acceptedArguments: asSchema(definition.inputSchema).jsonSchema } : {}),
  };
}
