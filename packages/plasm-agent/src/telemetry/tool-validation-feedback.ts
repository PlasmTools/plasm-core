import { asSchema, JSONParseError, type ToolSet } from "ai";
import { z } from "zod";

// Project validation facts only: SDK error messages can embed rejected input.
const issueSchema = z.object({
  code: z.string(),
  path: z.array(z.union([z.string(), z.number()])),
  expected: z.string().optional(),
  validation: z.union([
    z.string(),
    z.object({ includes: z.string(), position: z.number().optional() }),
    z.object({ startsWith: z.string() }),
    z.object({ endsWith: z.string() }),
  ]).optional(),
  keys: z.array(z.string()).optional(),
  minimum: z.number().optional(),
  maximum: z.number().optional(),
  inclusive: z.boolean().optional(),
  exact: z.boolean().optional(),
  type: z.string().optional(),
});
const issuesSchema = z.object({ issues: z.array(issueSchema) });

// Tool descriptions are already supplied with the registered schema. Keep
// machine constraints in repair feedback without sending their prose again.
const schemaMaps = new Set(["properties", "patternProperties", "$defs", "definitions", "dependentSchemas"]);
const schemaLists = new Set(["allOf", "anyOf", "oneOf", "prefixItems"]);
const schemaChildren = new Set(["items", "additionalProperties", "not", "if", "then", "else", "contains", "propertyNames"]);

function withoutDescriptions(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(withoutDescriptions);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value).flatMap(([key, nested]) => {
    if (key === "description") return [];
    if (schemaMaps.has(key) && nested && typeof nested === "object" && !Array.isArray(nested)) {
      return [[key, Object.fromEntries(Object.entries(nested).map(([name, child]) => [name, withoutDescriptions(child)]))]];
    }
    if (schemaLists.has(key) && Array.isArray(nested)) return [[key, nested.map(withoutDescriptions)]];
    if (schemaChildren.has(key)) return [[key, withoutDescriptions(nested)]];
    return [[key, nested]];
  }));
}

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
    ...(definition ? { acceptedArguments: withoutDescriptions(asSchema(definition.inputSchema).jsonSchema) } : {}),
  };
}
