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

/** Generate one complete Python DAG using the compiler's declared binding. */
export function renderProgramStatements(
  binding: CapabilityBinding,
  cap: CapabilityIntrospectionJson,
  catalogValues: Record<string, import("./catalog-introspection.js").NamedValueSchemaJson>,
  shape: CapabilityInvokeShape,
  entityIdField: string,
  inputVar: string,
): string {
  const api = cap.python;
  if (api.unavailable) return `const program: string = (() => { throw new Error(${JSON.stringify(`${cap.name}: ${api.unavailable}`)}); })();`;
  const parameters = [...api.parameters];
  for (const schema of [cap.inputs.payload, cap.inputs.arguments]) {
    if (schema?.input_type.type !== "union") continue;
    for (const variant of schema.input_type.variants) {
      // Preserve supplied fields; Python rejects mixtures of alternatives.
      for (const field of [{name: variant.wire.field, required: false,
        input_type: {type: "value" as const, field_type: "string" as const}}, ...variant.fields]) {
        if (!parameters.some(existing => existing.name === field.name)) {
          parameters.push({...field, required: false});
        }
      }
    }
  }
  const args = parameters
    .filter(field => !((api.receiver || cap.kind === "get") && api.identity.includes(field.name)))
    .map(field => ({ key: field.name, valueExpr: `${inputVar}[${JSON.stringify(field.name)}]`,
      kind: emissionKindForField(field, catalogValues), optional: !field.required }));
  const identity = api.identity.map(key => ({ key, valueExpr: `${inputVar}[${JSON.stringify(key)}]`, kind: "literal" as const, optional: false }));
  const identityCode = identity.length === 1 ? `plasmLiteral(${identity[0]!.valueExpr})` : renderBuildDottedArgsCall(identity);
  const receiver = api.receiver ? `${api.entity_symbol}.get(\${identity})` : api.entity_symbol;
  const call = cap.kind === "get" ? `${api.entity_symbol}.${api.method}(\${[identity, args].filter(Boolean).join(", ")})` : `${receiver}.${api.method}(\${args})`;
  return [
    `const identity = ${cap.kind === "get" || api.receiver ? identityCode : '""'};`,
    `const args = ${renderBuildDottedArgsCall(args)};`,
    'const program = `class Invoke(Program):\\n    def build(self):\\n        return ' + call + '\\n`;',
  ].join("\n  ");
}
