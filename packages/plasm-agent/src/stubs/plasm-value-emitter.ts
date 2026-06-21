import type { CapabilityIntrospectionJson } from "./catalog-introspection.js";
import type { CapabilityBinding } from "./stub-symbols.js";
import type { CapabilityInvokeShape } from "./capability-invoke-shape.js";
import { invokeBodyFields, searchFieldName } from "./capability-invoke-shape.js";

export interface EmissionField {
  name: string;
  required: boolean;
  access: string;
  kind: "literal" | "number" | "boolean" | "select";
}

export function emissionKindForField(
  field: import("./catalog-introspection.js").InputFieldSchemaJson,
  values: Record<string, import("./catalog-introspection.js").NamedValueSchemaJson>,
): EmissionField["kind"] {
  const nv = field.value_ref ? values[field.value_ref] : undefined;
  const ft = nv?.field_type;
  if (typeof ft === "string") {
    if (ft === "integer" || ft === "number") return "number";
    if (ft === "boolean") return "boolean";
    if (ft === "select") return "select";
  }
  if (field.input_type?.type === "value") {
    const inner = field.input_type.field_type;
    if (inner === "integer" || inner === "number") return "number";
    if (inner === "boolean") return "boolean";
  }
  return "literal";
}

export function buildEmissionFields(
  cap: CapabilityIntrospectionJson,
  catalogValues: Record<string, import("./catalog-introspection.js").NamedValueSchemaJson>,
  shape: CapabilityInvokeShape,
  entityIdField: string,
  inputVar: string,
): EmissionField[] {
  const body = invokeBodyFields(cap, shape, entityIdField);
  return body.map((f) => ({
    name: f.name,
    required: !!f.required,
    access: `${inputVar}.${f.name}`,
    kind: emissionKindForField(f, catalogValues),
  }));
}

export interface DottedArgCodegen {
  key: string;
  valueExpr: string;
  kind: "literal" | "number" | "boolean" | "select";
  optional: boolean;
}

export function dottedArgsCodegen(
  cap: CapabilityIntrospectionJson,
  catalogValues: Record<string, import("./catalog-introspection.js").NamedValueSchemaJson>,
  shape: CapabilityInvokeShape,
  entityIdField: string,
  inputVar: string,
): DottedArgCodegen[] {
  return buildEmissionFields(cap, catalogValues, shape, entityIdField, inputVar).map((f) => ({
    key: f.name,
    valueExpr: f.access,
    kind: f.kind,
    optional: !f.required,
  }));
}

function renderBuildDottedArgsCall(args: DottedArgCodegen[]): string {
  if (!args.length) return '""';
  const entries = args
    .map(
      (a) =>
        `{ key: ${JSON.stringify(a.key)}, value: ${a.valueExpr}, kind: ${JSON.stringify(a.kind)}${a.optional ? ", optional: true" : ""} }`,
    )
    .join(", ");
  return `buildDottedArgs([${entries}])`;
}

/** Generated TypeScript statements that set `program` (uses `buildDottedArgs`, `plasmLiteral`, …). */
export function renderProgramStatements(
  binding: CapabilityBinding,
  cap: CapabilityIntrospectionJson,
  catalogValues: Record<string, import("./catalog-introspection.js").NamedValueSchemaJson>,
  shape: CapabilityInvokeShape,
  entityIdField: string,
  inputVar: string,
): string {
  const sym = binding.entitySymbol;
  const dotted = renderBuildDottedArgsCall(
    dottedArgsCodegen(cap, catalogValues, shape, entityIdField, inputVar),
  );

  switch (shape) {
    case "RootQuery":
      return `const program = ${JSON.stringify(sym)};`;
    case "ScopedQuery": {
      const args = dottedArgsCodegen(cap, catalogValues, shape, entityIdField, inputVar);
      const dottedCall = renderBuildDottedArgsCall(args);
      return `const preds = ${dottedCall};
  const program = preds ? \`${sym}{\${preds}}\` : ${JSON.stringify(sym)};`;
    }
    case "GetById":
      return `const program = \`${sym}(\${plasmLiteral(${inputVar}.${entityIdField})})\`;`;
    case "SearchText": {
      const q = searchFieldName(cap) ?? "q";
      return `const program = \`${sym}~\${plasmLiteral(${inputVar}.${q})}\`;`;
    }
    case "SearchFiltered": {
      const q = searchFieldName(cap) ?? "q";
      const args = dottedArgsCodegen(cap, catalogValues, shape, entityIdField, inputVar);
      const dottedCall = renderBuildDottedArgsCall(args);
      return `const filterArgs = ${dottedCall};
  const program = filterArgs
    ? \`${sym}~\${plasmLiteral(${inputVar}.${q})}{\${filterArgs}}\`
    : \`${sym}~\${plasmLiteral(${inputVar}.${q})}\`;`;
    }
    case "RootCreate":
      return `const args = ${dotted};
  const program = \`${sym}.create(\${args})\`;`;
    case "ScopedUpdate":
      return `const args = ${dotted};
  const program = \`${sym}(\${plasmLiteral(${inputVar}.${entityIdField})}).update(\${args})\`;`;
    case "ScopedAction": {
      const wire = binding.methodWire;
      const method = binding.methodSymbol ? `.${binding.methodSymbol}` : `.${wire}`;
      return `const program = \`${sym}(\${plasmLiteral(${inputVar}.${entityIdField})})${method}()\`;`;
    }
    case "ScopedDelete":
      return `const program = \`${sym}(\${plasmLiteral(${inputVar}.${entityIdField})}).delete()\`;`;
    case "MethodUnion": {
      const m = binding.methodSymbol ?? "m1";
      return `const program = \`${sym}.${m}(\${plasmLiteral("v1")})\`;`;
    }
    case "MethodObject": {
      const m = binding.methodSymbol ?? binding.methodWire;
      return `const args = ${dotted};
  const program = \`${sym}.${m}(\${args})\`;`;
    }
    default:
      return `const program = ${JSON.stringify(sym)};`;
  }
}

/** @deprecated Prefer {@link renderProgramStatements} — avoids nested-template syntax errors. */
export function renderProgramExpr(
  binding: CapabilityBinding,
  cap: CapabilityIntrospectionJson,
  catalogValues: Record<string, import("./catalog-introspection.js").NamedValueSchemaJson>,
  shape: CapabilityInvokeShape,
  entityIdField: string,
  inputVar: string,
): string {
  void binding;
  void cap;
  void catalogValues;
  void shape;
  void entityIdField;
  void inputVar;
  return '""';
}
