import type {
  CapabilityIntrospectionJson,
  InputFieldSchemaJson,
  InputTypeJson,
} from "./catalog-introspection.js";

export type CapabilityInvokeShape =
  | "RootQuery"
  | "ScopedQuery"
  | "GetById"
  | "SearchText"
  | "SearchFiltered"
  | "RootCreate"
  | "ScopedUpdate"
  | "ScopedAction"
  | "ScopedDelete"
  | "MethodObject"
  | "MethodUnion";

export function inputTypeRoot(input: InputTypeJson | undefined): InputTypeJson | undefined {
  return input;
}

export function objectFieldsFromCap(
  cap: CapabilityIntrospectionJson,
): InputFieldSchemaJson[] {
  const schema = cap.input_schema?.input_type;
  if (!schema || schema.type !== "object") return [];
  return schema.fields ?? [];
}

function hasScopeOrFilterParams(fields: InputFieldSchemaJson[]): boolean {
  return fields.some((f) => {
    const role = f.role ?? "filter";
    return role === "scope" || role === "filter";
  });
}

function searchParam(fields: InputFieldSchemaJson[]): InputFieldSchemaJson | undefined {
  return fields.find((f) => (f.role ?? "filter") === "search") ?? fields[0];
}

function nonSearchParams(fields: InputFieldSchemaJson[]): InputFieldSchemaJson[] {
  const search = searchParam(fields);
  return fields.filter((f) => f !== search);
}

export function classifyInvokeShape(cap: CapabilityIntrospectionJson): CapabilityInvokeShape {
  const kind = cap.kind.toLowerCase();
  const inputType = cap.input_schema?.input_type;

  if (inputType?.type === "union") {
    return "MethodUnion";
  }

  const fields = objectFieldsFromCap(cap);

  switch (kind) {
    case "query":
      return hasScopeOrFilterParams(fields) ? "ScopedQuery" : "RootQuery";
    case "get":
      return "GetById";
    case "search": {
      const extra = nonSearchParams(fields);
      return extra.length > 0 ? "SearchFiltered" : "SearchText";
    }
    case "create":
      return "RootCreate";
    case "update":
      return "ScopedUpdate";
    case "action":
      return "ScopedAction";
    case "delete":
      return "ScopedDelete";
    default:
      return "MethodObject";
  }
}

export function capabilityReturnsVoid(cap: CapabilityIntrospectionJson): boolean {
  if (cap.output_schema?.type === "side_effect") return true;
  if (cap.kind.toLowerCase() === "delete" && cap.provides.length === 0) return true;
  return false;
}

export function capabilityReturnsScalar(cap: CapabilityIntrospectionJson): boolean {
  const kind = cap.kind.toLowerCase();
  return kind === "get";
}

export function capabilityNeedsInput(
  cap: CapabilityIntrospectionJson,
  shape: CapabilityInvokeShape,
  idField?: string,
): boolean {
  if (shape === "RootQuery") return false;
  if (shape === "GetById" || shape === "ScopedUpdate" || shape === "ScopedAction" || shape === "ScopedDelete") {
    return true;
  }
  const fields = objectFieldsFromCap(cap);
  if (fields.length > 0) return true;
  if (shape === "ScopedQuery" || shape === "SearchText" || shape === "SearchFiltered") {
    return fields.some((f) => f.required);
  }
  void idField;
  return false;
}

/** Body fields for dotted-arg emission (excludes scoped receiver id). */
export function invokeBodyFields(
  cap: CapabilityIntrospectionJson,
  shape: CapabilityInvokeShape,
  entityIdField: string,
): InputFieldSchemaJson[] {
  const fields = objectFieldsFromCap(cap);
  switch (shape) {
    case "GetById":
    case "ScopedUpdate":
    case "ScopedAction":
    case "ScopedDelete":
      return fields.filter((f) => f.name !== entityIdField);
    case "SearchText":
    case "SearchFiltered": {
      const search = searchParam(fields);
      return fields.filter((f) => f !== search);
    }
    default:
      return fields;
  }
}

export function searchFieldName(cap: CapabilityIntrospectionJson): string | undefined {
  const fields = objectFieldsFromCap(cap);
  return searchParam(fields)?.name;
}
