/** JSON shape from `PlasmEngine.introspectCatalog()` (post-merge CGS). */

export type FieldTypeJson =
  | "boolean"
  | "number"
  | "integer"
  | "uuid"
  | "blob"
  | "string"
  | "select"
  | "multi_select"
  | "date"
  | "array"
  | "json"
  | { entity_ref: { target: string } };

export interface NamedValueSchemaJson {
  description?: string;
  field_type: FieldTypeJson;
  value_format?: string;
  allowed_values?: string[];
  string_semantics?: string;
  array_items?: {
    value_ref?: string;
    field_type?: FieldTypeJson;
    allowed_values?: string[];
  };
}

export interface InputFieldSchemaJson {
  name: string;
  value_ref?: string;
  input_type?: InputTypeJson;
  required?: boolean;
  description?: string;
  role?: ParameterRoleJson;
}

export type ParameterRoleJson =
  | "filter"
  | "search"
  | "sort"
  | "sort_direction"
  | "response_control"
  | "scope";

export type InputTypeJson =
  | { type: "none" }
  | {
      type: "value";
      field_type: FieldTypeJson;
      allowed_values?: string[];
    }
  | {
      type: "object";
      fields: InputFieldSchemaJson[];
      additional_fields?: boolean;
    }
  | {
      type: "array";
      element_type: InputTypeJson;
      min_length?: number;
      max_length?: number;
    }
  | {
      type: "union";
      variants: InputVariantSchemaJson[];
    };

export interface InputVariantSchemaJson {
  name: string;
  description?: string;
  constructor_symbol?: string;
  fields: InputFieldSchemaJson[];
  wire: { field: string; value: string };
}

export interface InputSchemaJson {
  input_type: InputTypeJson;
  description?: string;
}

export interface OutputSchemaJson {
  type: "side_effect" | "entity" | "collection" | "status" | "custom";
  description?: string;
  entity_type?: string;
}

export interface CapabilityIntrospectionJson {
  name: string;
  kind: string;
  entity: string;
  invoke_wire_name: string;
  input_schema: InputSchemaJson | null;
  provides: string[];
  output_schema: OutputSchemaJson | null;
}

export interface EntityFieldIntrospectionJson {
  name: string;
  value_ref: string;
  required: boolean;
}

export interface EntityIntrospectionJson {
  name: string;
  id_field: string;
  fields: EntityFieldIntrospectionJson[];
}

export interface CatalogIntrospectionJson {
  entry_id: string;
  catalog_cgs_hash: string;
  entities: EntityIntrospectionJson[];
  values: Record<string, NamedValueSchemaJson>;
  capabilities: CapabilityIntrospectionJson[];
}

export function parseCatalogIntrospection(json: string): CatalogIntrospectionJson {
  return JSON.parse(json) as CatalogIntrospectionJson;
}
