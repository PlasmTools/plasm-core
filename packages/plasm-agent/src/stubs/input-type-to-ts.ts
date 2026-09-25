import type {
  CapabilityIntrospectionJson,
  CatalogIntrospectionJson,
  FieldTypeJson,
  InputFieldSchemaJson,
  InputTypeJson,
  NamedValueSchemaJson,
} from "./catalog-introspection.js";
import type { CapabilityInvokeShape } from "./capability-invoke-shape.js";
import {
  invokeBodyFields,
  objectFieldsFromCap,
  searchFieldName,
} from "./capability-invoke-shape.js";

export type BrandRegistry = Map<string, string>;

export function entityTypeName(entityName: string): string {
  return entityName.replace(/[^a-zA-Z0-9_]/g, "_");
}

export function refBrandName(entityName: string): string {
  return `Ref${entityTypeName(entityName)}`;
}

export function idBrandName(entityName: string): string {
  return `${entityTypeName(entityName)}Id`;
}

export function buildBrandRegistry(catalog: CatalogIntrospectionJson): BrandRegistry {
  const out = new Map<string, string>();
  for (const entity of catalog.entities) {
    out.set(entity.name, refBrandName(entity.name));
  }
  return out;
}

function resolveFieldType(
  field: InputFieldSchemaJson,
  values: Record<string, NamedValueSchemaJson>,
): FieldTypeJson | InputTypeJson | undefined {
  if (field.value_ref) {
    return values[field.value_ref]?.field_type;
  }
  return field.input_type;
}

function namedValueToTs(value: NamedValueSchemaJson | undefined, values: Record<string, NamedValueSchemaJson>, brands: BrandRegistry, input: boolean, seen = new Set<string>()): string {
  if (value?.field_type === "array" && value.array_items) {
    const item = value.array_items;
    if (item.value_ref) {
      if (seen.has(item.value_ref)) throw new Error(`Cyclic array value ${item.value_ref}`);
      return `(${namedValueToTs(values[item.value_ref], values, brands, input, new Set([...seen, item.value_ref]))})[]`;
    }
    return `(${fieldTypeToTs(item.field_type, values, brands, input, item.allowed_values)})[]`;
  }
  return fieldTypeToTs(value?.field_type, values, brands, input, value?.allowed_values);
}

function inputFieldToTs(field: InputFieldSchemaJson, values: Record<string, NamedValueSchemaJson>, brands: BrandRegistry, input: boolean): string {
  return field.value_ref ? namedValueToTs(values[field.value_ref], values, brands, input) : fieldTypeToTs(field.input_type, values, brands, input);
}

function fieldTypeToTs(
  ft: FieldTypeJson | InputTypeJson | undefined,
  values: Record<string, NamedValueSchemaJson>,
  brands: BrandRegistry,
  /** Input positions brand entity_ref; output uses wire string. */
  inputContext: boolean,
  allowedValues?: string[],
): string {
  if (!ft) return "unknown";

  if (typeof ft === "object" && "type" in ft) {
    return inputTypeToTs(ft, values, brands, inputContext);
  }

  const fieldType = ft as FieldTypeJson;
  if (typeof fieldType === "object" && "entity_ref" in fieldType) {
    const target = fieldType.entity_ref.target;
    if (inputContext && brands.has(target)) {
      return brands.get(target)!;
    }
    return "string";
  }

  switch (fieldType) {
    case "boolean":
      return "boolean";
    case "integer":
    case "number":
      return "number";
    case "string":
    case "digit_id":
    case "money":
    case "uuid":
    case "date":
      return "string";
    case "blob":
    case "json":
      return "unknown";
    case "select": {
      const allowed = allowedValues ?? [];
      if (!allowed.length) return "string";
      return allowed.map((v) => JSON.stringify(v)).join(" | ");
    }
    case "multi_select":
      return "string[]";
    case "array":
      return "unknown[]";
    default:
      return "unknown";
  }
}

export function inputTypeToTs(
  input: InputTypeJson,
  values: Record<string, NamedValueSchemaJson>,
  brands: BrandRegistry,
  inputContext = true,
): string {
  switch (input.type) {
    case "none":
      return "void";
    case "value": {
      const allowed = input.allowed_values;
      return fieldTypeToTs(input.field_type, values, brands, inputContext, allowed);
    }
    case "object": {
      const lines = input.fields.map((f) => {
        const ft = resolveFieldType(f, values);
        const allowed =
          typeof ft === "string" || (typeof ft === "object" && !("type" in ft))
            ? values[f.value_ref ?? ""]?.allowed_values
            : undefined;
        const ts = inputFieldToTs(f, values, brands, inputContext);
        return `${JSON.stringify(f.name)}${f.required ? "" : "?"}: ${ts};`;
      });
      return `{\n  ${lines.join("\n  ")}\n}`;
    }
    case "array":
      return `(${inputTypeToTs(input.element_type, values, brands, inputContext)})[]`;
    case "union": {
      const variants = input.variants.map((v) => {
        const body = inputTypeToTs(
          { type: "object", fields: v.fields },
          values,
          brands,
          inputContext,
        );
        const tag = v.wire;
        return `{ readonly ${JSON.stringify(tag.field)}: ${JSON.stringify(tag.value)}; ${body.slice(1, -1).trim()} }`;
      });
      return variants.join(" | ");
    }
    default:
      return "unknown";
  }
}

export function capabilityInputTypeName(capName: string): string {
  const base = capName
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean)
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("");
  return `${base}Input`;
}

export function renderBrandTypes(catalog: CatalogIntrospectionJson): string {
  const lines = [
    "type Brand<B extends string, T> = T & { readonly __brand: B };",
  ];
  for (const entity of catalog.entities) {
    const id = idBrandName(entity.name);
    const ref = refBrandName(entity.name);
    lines.push(`export type ${id} = Brand<${JSON.stringify(id)}, string>;`);
    lines.push(`export type ${ref} = ${id};`);
  }
  return lines.join("\n");
}

export function renderEntityRowType(
  entityName: string,
  fieldNames: string[],
  catalog: CatalogIntrospectionJson,
): string {
  const entity = catalog.entities.find((e) => e.name === entityName);
  if (!entity) return `export type ${entityTypeName(entityName)} = Record<string, unknown>;`;

  const values = catalog.values;
  const lines: string[] = [];
  for (const fieldName of fieldNames) {
    const field = entity.fields.find((f) => f.name === fieldName);
    if (!field) {
      lines.push(`${fieldName}?: unknown;`);
      continue;
    }
    const nv = values[field.value_ref];
    const ts = namedValueToTs(nv, values, buildBrandRegistry(catalog), false);
    lines.push(`${fieldName}${field.required ? "" : "?"}: ${ts};`);
  }
  const typeName = entityTypeName(entityName);
  return `export type ${typeName} = {\n  ${lines.join("\n  ")}\n};`;
}

export function renderCapabilityInputType(
  cap: CapabilityIntrospectionJson,
  catalog: CatalogIntrospectionJson,
  shape: CapabilityInvokeShape,
): string | null {
  const entity = catalog.entities.find((e) => e.name === cap.entity);
  if (!entity) return null;

  const brands = buildBrandRegistry(catalog);
  const values = catalog.values;
  const lines: string[] = [];

  if (cap.kind === "get" || cap.python.receiver) {
    for (const key of cap.python.identity) {
      const field = entity.fields.find(f => f.name === key);
      const value = field ? values[field.value_ref] : undefined;
      const type = namedValueToTs(value, values, brands, true);
      lines.push(`${JSON.stringify(key)}: ${type};`);
    }
  }

  const bodyFields = objectFieldsFromCap(cap).filter(f => !(cap.python.receiver || cap.kind === "get") || !cap.python.identity.includes(f.name));
  for (const field of bodyFields) {
    const ft = resolveFieldType(field, values);
    const allowed = field.value_ref ? values[field.value_ref]?.allowed_values : undefined;
    const ts = inputFieldToTs(field, values, brands, true);
    lines.push(`${field.name}${field.required ? "" : "?"}: ${ts};`);
  }

  const unions = [cap.inputs.payload, cap.inputs.arguments]
    .filter(schema => schema?.input_type.type === "union")
    .map(schema => `(${inputTypeToTs(schema!.input_type, values, brands, true)})`);
  if (!lines.length && !unions.length) return null;
  const common = lines.length ? [`{\n  ${lines.join("\n  ")}\n}`] : [];
  return `export type ${capabilityInputTypeName(cap.name)} = ${[...common, ...unions].join(" & ")};`;
}
