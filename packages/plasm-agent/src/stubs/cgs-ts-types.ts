import type {
  CgsCapability,
  CgsEntity,
  CgsField,
  CgsParameter,
  CgsValueDomain,
  ParsedCgsDomain,
} from "./domain-parser.js";

export function valueDomainToTsType(
  domain: CgsValueDomain | undefined,
  entityTypes: Map<string, string>,
): string {
  if (!domain) return "unknown";
  switch (domain.type) {
    case "string":
    case "uuid":
    case "date":
      return "string";
    case "integer":
    case "number":
      return "number";
    case "boolean":
      return "boolean";
    case "json":
      return "unknown";
    case "entity_ref": {
      const target = domain.target;
      if (target && entityTypes.has(target)) {
        return entityTypes.get(target)!;
      }
      return "string";
    }
    case "select": {
      const allowed = domain.allowedValues ?? [];
      if (!allowed.length) return "string";
      return allowed.map((v) => JSON.stringify(v)).join(" | ");
    }
    case "array":
      return "unknown[]";
    default:
      return "unknown";
  }
}

function fieldByName(entity: CgsEntity, name: string): CgsField | undefined {
  return entity.fields.find((f) => f.name === name);
}

function resolveFieldTsType(
  fieldName: string,
  entity: CgsEntity,
  values: Map<string, CgsValueDomain>,
  entityTypes: Map<string, string>,
  /** Wire row JSON uses string ids for entity_ref fields, not nested objects. */
  outputField = false,
): { type: string; optional: boolean } {
  const field = fieldByName(entity, fieldName);
  if (!field) return { type: "unknown", optional: true };
  const domain = values.get(field.valueRef);
  if (outputField && domain?.type === "entity_ref") {
    return { type: "string", optional: !field.required };
  }
  return {
    type: valueDomainToTsType(domain, entityTypes),
    optional: !field.required,
  };
}

export function entityTypeName(entityName: string): string {
  return entityName.replace(/[^a-zA-Z0-9_]/g, "_");
}

export function buildEntityTypeMap(domain: ParsedCgsDomain): Map<string, string> {
  const out = new Map<string, string>();
  for (const entity of domain.entities) {
    out.set(entity.name, entityTypeName(entity.name));
  }
  return out;
}

export function renderEntityType(
  entity: CgsEntity,
  domain: ParsedCgsDomain,
  fieldNames: string[],
): string {
  const entityTypes = buildEntityTypeMap(domain);
  const lines: string[] = [];
  for (const fieldName of fieldNames) {
    const { type, optional } = resolveFieldTsType(
      fieldName,
      entity,
      domain.values,
      entityTypes,
      true,
    );
    lines.push(`${fieldName}${optional ? "?" : ""}: ${type};`);
  }
  const typeName = entityTypeName(entity.name);
  return `export type ${typeName} = {\n  ${lines.join("\n  ")}\n};`;
}

export function effectiveProvides(cap: CgsCapability, entity: CgsEntity): string[] {
  if (cap.provides.length) return cap.provides;
  const names = entity.fields.map((f) => f.name);
  const idIdx = names.indexOf(entity.idField);
  if (idIdx > 0) {
    names.splice(idIdx, 1);
    names.unshift(entity.idField);
  }
  return names;
}

export function capabilityInputTypeName(cap: CgsCapability): string {
  const base = cap.name
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean)
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("");
  return `${base}Input`;
}

export function renderCapabilityInputType(
  cap: CgsCapability,
  entity: CgsEntity,
  domain: ParsedCgsDomain,
): string | null {
  const entityTypes = buildEntityTypeMap(domain);
  const kind = cap.kind.toLowerCase();
  const lines: string[] = [];

  if (kind === "get") {
    const idField = fieldByName(entity, entity.idField);
    const idType = idField
      ? valueDomainToTsType(domain.values.get(idField.valueRef), entityTypes)
      : "string";
    lines.push(`${entity.idField}: ${idType};`);
  } else {
    for (const param of cap.parameters) {
      const domainRow = domain.values.get(param.valueRef);
      const optional = !param.required;
      lines.push(
        `${param.name}${optional ? "?" : ""}: ${valueDomainToTsType(domainRow, entityTypes)};`,
      );
    }
  }

  if (!lines.length) return null;
  return `export type ${capabilityInputTypeName(cap)} = {\n  ${lines.join("\n  ")}\n};`;
}

export function capabilityNeedsInput(cap: CgsCapability): boolean {
  const kind = cap.kind.toLowerCase();
  if (kind === "get") return true;
  return cap.parameters.length > 0;
}

export function parameterTsType(
  param: CgsParameter,
  domain: ParsedCgsDomain,
): string {
  const entityTypes = buildEntityTypeMap(domain);
  return valueDomainToTsType(domain.values.get(param.valueRef), entityTypes);
}
