// Domain model extraction from OpenAPI specification summaries.
// The LLM reads the RPC surface and infers the semantic domain graph.

function ExtractDomainModel(api_summary: string) -> DomainModel {
    client DeepExtract
    prompt #"
        {{ _.role("system") }}
        You are an API domain model extractor. You convert OpenAPI/REST
        specifications into semantic domain models. You MUST:
        - Define an entity for EVERY schema in the spec, including simple
          ones like Category or Tag that have no operations
        - Create shared types for enum values that appear in multiple places
        - Ensure every relation target_entity matches a defined entity name
        - Ensure every shared_type_name matches a defined shared type name
        Your output will be mechanically validated. Referential integrity
        violations will cause re-prompting.

        {{ _.role("user") }}
        Extract the domain model from this API specification.

        RULES:
        - An API endpoint is an RPC operation. A domain entity is a business object.
        - Multiple endpoints on the same path prefix operate on the SAME entity.
        - Foreign keys, parent IDs, database_id, workspace_id, and any *Id / *_id that points at
          another resource MUST use field_type ENTITY_REF with ref_target_entity set to that entity.
          Do NOT use INTEGER for those — INTEGER is only for quantities, limits, page sizes, counts,
          and numeric data that is not an entity identifier.
        - Notion-style "query with parent filter" params are ENTITY_REF to the parent entity, not INTEGER.
        - A field like `petId: integer` on Order is a RELATION to Pet, not just a number.
        - Inline enums appearing in both schemas and query params are SHARED TYPES.
        - Path nesting like /pet/{petId}/uploadImage means uploadImage is a CAPABILITY on Pet.
        - $ref in schema = direct relation. Foreign key pattern (xxxId) = inferred relation.
        - Classify capabilities: QUERY (list/filter), GET (by ID), CREATE, UPDATE, DELETE, ACTION.
        - Entities with no operations are valid (e.g. Category referenced by Pet).
        - Timestamps: use RFC3339 for datetime strings, ISO8601_DATE for date-only strings,
          UNIX_MS / UNIX_SEC only when the wire is numeric epoch — never DATE or DATETIME aliases.
        - Enums: field_type ENUM with shared_type_name when values repeat across fields/params.

        API SPECIFICATION:
        ---
        {{ api_summary }}
        ---
    "#
}
