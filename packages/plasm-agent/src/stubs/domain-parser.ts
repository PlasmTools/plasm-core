import yaml from "js-yaml";

export interface CgsValueDomain {
  key: string;
  type: string;
  target?: string;
  allowedValues?: string[];
  description?: string;
  stringSemantics?: string;
}

export interface CgsField {
  name: string;
  valueRef: string;
  required: boolean;
  description?: string;
}

export interface CgsEntity {
  name: string;
  description?: string;
  idField: string;
  fields: CgsField[];
}

export interface CgsParameter {
  name: string;
  valueRef: string;
  required: boolean;
  role?: string;
  description?: string;
}

export interface CgsCapability {
  name: string;
  entity: string;
  kind: string;
  description?: string;
  parameters: CgsParameter[];
  provides: string[];
}

export interface ParsedCgsDomain {
  entryId: string;
  authScheme?: string;
  entities: CgsEntity[];
  capabilities: CgsCapability[];
  values: Map<string, CgsValueDomain>;
}

type RawDomain = {
  entry_id?: string;
  auth?: { scheme?: string };
  entities?: Record<
    string,
    {
      description?: string;
      id_field?: string;
      fields?: Record<
        string,
        {
          value_ref?: string;
          required?: boolean;
          description?: string;
        }
      >;
    }
  >;
  capabilities?: Record<
    string,
    {
      description?: string;
      kind?: string;
      entity?: string;
      parameters?: Array<{
        name?: string;
        value_ref?: string;
        required?: boolean;
        role?: string;
        description?: string;
      }>;
      provides?: string[];
    }
  >;
  values?: Record<
    string,
    {
      type?: string;
      target?: string;
      allowed_values?: string[];
      description?: string;
      string_semantics?: string;
    }
  >;
};

function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/** Parse `domain.yaml` into a CGS-shaped model for stub generation. */
export function parseCgsDomain(raw: string, fallbackEntryId: string): ParsedCgsDomain {
  const doc = yaml.load(raw) as RawDomain | null;
  if (!doc || typeof doc !== "object") {
    throw new Error("domain.yaml: expected mapping at root");
  }

  const entryId = asString(doc.entry_id)?.trim() || fallbackEntryId;
  const authScheme = asString(doc.auth?.scheme);

  const values = new Map<string, CgsValueDomain>();
  for (const [key, row] of Object.entries(doc.values ?? {})) {
    if (!row || typeof row !== "object") continue;
    const type = asString(row.type);
    if (!type) continue;
    values.set(key, {
      key,
      type,
      target: asString(row.target),
      allowedValues: Array.isArray(row.allowed_values)
        ? row.allowed_values.filter((v): v is string => typeof v === "string")
        : undefined,
      description: asString(row.description),
      stringSemantics: asString(row.string_semantics),
    });
  }

  const entities: CgsEntity[] = [];
  for (const [name, entity] of Object.entries(doc.entities ?? {})) {
    if (!entity || typeof entity !== "object") continue;
    const fields: CgsField[] = [];
    for (const [fieldName, field] of Object.entries(entity.fields ?? {})) {
      if (!field || typeof field !== "object") continue;
      const valueRef = asString(field.value_ref);
      if (!valueRef) continue;
      fields.push({
        name: fieldName,
        valueRef,
        required: field.required === true,
        description: asString(field.description),
      });
    }
    entities.push({
      name,
      description: asString(entity.description),
      idField: asString(entity.id_field) ?? "id",
      fields,
    });
  }

  const capabilities: CgsCapability[] = [];
  for (const [name, cap] of Object.entries(doc.capabilities ?? {})) {
    if (!cap || typeof cap !== "object") continue;
    const entity = asString(cap.entity);
    const kind = asString(cap.kind);
    if (!entity || !kind) continue;
    const parameters: CgsParameter[] = [];
    for (const param of cap.parameters ?? []) {
      if (!param || typeof param !== "object") continue;
      const paramName = asString(param.name);
      const valueRef = asString(param.value_ref);
      if (!paramName || !valueRef) continue;
      parameters.push({
        name: paramName,
        valueRef,
        required: param.required === true,
        role: asString(param.role),
        description: asString(param.description),
      });
    }
    capabilities.push({
      name,
      entity,
      kind,
      description: asString(cap.description),
      parameters,
      provides: Array.isArray(cap.provides)
        ? cap.provides.filter((v): v is string => typeof v === "string")
        : [],
    });
  }

  return { entryId, authScheme, entities, capabilities, values };
}

/** Legacy shape for operator routes (entity/capability counts). */
export function toLegacyParsedDomain(domain: ParsedCgsDomain): {
  entryId: string;
  authScheme?: string;
  entities: Array<{ name: string }>;
  capabilities: Array<{ name: string; entity: string; kind?: string }>;
} {
  return {
    entryId: domain.entryId,
    authScheme: domain.authScheme,
    entities: domain.entities.map((e) => ({ name: e.name })),
    capabilities: domain.capabilities.map((c) => ({
      name: c.name,
      entity: c.entity,
      kind: c.kind,
    })),
  };
}
