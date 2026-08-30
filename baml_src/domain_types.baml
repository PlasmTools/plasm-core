// Domain model types extracted from an API specification.
// The LLM infers the semantic domain graph from the RPC surface.
// FieldType names align with CGS kernel/profile vocabulary — no retired date/select aliases.

enum FieldType {
    STRING
    INTEGER @description("Counts, sizes, ordinals — NOT foreign keys. Use UNIX_MS only when the API wire is epoch milliseconds, not entity ids.")
    FLOAT
    BOOLEAN
    RFC3339 @description("Instant timestamps as RFC3339 / ISO-8601 datetime strings")
    ISO8601_DATE @description("Calendar dates without time (YYYY-MM-DD)")
    UNIX_MS @description("Unix epoch milliseconds as integer")
    UNIX_SEC @description("Unix epoch seconds as integer")
    ENUM
    ARRAY
    ENTITY_REF @description("Value is another entity's primary key (FK / parent scope / database_id / xxxId). Never INTEGER for these.")
}

enum Cardinality {
    ONE
    MANY
}

enum CapabilityKind {
    QUERY   @description("List/filter a collection of entities")
    GET     @description("Retrieve a single entity by ID or key")
    CREATE  @description("Create a new entity")
    UPDATE  @description("Modify an existing entity")
    DELETE  @description("Remove an entity")
    ACTION  @description("Any other entity-scoped operation")
}

enum ParamLocation {
    PATH
    QUERY
    BODY
    HEADER
}

class DomainModel {
    entities DomainEntity[] @description("The domain entities inferred from the API")
    shared_types SharedType[] @description("Named enum/value types shared across entities")
}

class DomainEntity {
    name string @description("PascalCase entity name, e.g. Pet, Order, User")
    description string? @description("What this entity represents in the domain")
    id_field string @description("Primary identifier field name")
    id_type FieldType @description("Type of the ID field")
    alternate_keys string[] @description("Other fields that can uniquely identify this entity, e.g. username")
    fields DomainField[] @description("All data fields on this entity")
    relations DomainRelation[] @description("Relationships to other entities")
    capabilities DomainCapability[] @description("Operations available on this entity")
}

class DomainField {
    name string @description("Field name as it appears in the API")
    field_type FieldType
    required bool
    description string?
    shared_type_name string? @description("If this field uses a shared enum type, reference it by name")
    array_element_type FieldType? @description("If field_type is ARRAY, the element type")
    ref_target_entity string? @description("Required when field_type is ENTITY_REF: PascalCase entity this ID references (e.g. Team for team_id)")
}

class DomainRelation {
    name string @description("Relation name, e.g. 'category', 'tags', 'pet'")
    target_entity string @description("Target entity name this relation points to")
    cardinality Cardinality
    foreign_key_field string? @description("If the relation is via a foreign key field, name it, e.g. 'petId'")
    evidence string @description("Why you infer this relation exists")
}

class DomainCapability {
    name string @description("Short verb name, e.g. 'query', 'get', 'create', 'update', 'delete', 'uploadImage'")
    kind CapabilityKind
    description string?
    http_method string @description("GET, POST, PUT, PATCH, DELETE")
    path_template string @description("URL path with {param} placeholders, e.g. /pet/{petId}")
    parameters DomainParameter[] @description("All parameters for this operation")
    request_body_fields DomainField[] @description("Fields in the request body, if any")
    response_entity string? @description("Entity type returned, if any")
    response_is_array bool @description("Whether the response is an array of entities")
}

class DomainParameter {
    name string
    location ParamLocation
    field_type FieldType
    required bool
    shared_type_name string? @description("If this param uses a shared enum, reference it by name")
    description string?
    ref_target_entity string? @description("When field_type is ENTITY_REF: PascalCase entity this param scopes or references")
}

class SharedType {
    name string @description("PascalCase type name, e.g. PetStatus, OrderStatus")
    description string?
    values string[] @description("The enum values")
    used_by string[] @description("Which entities/capabilities reference this type")
}
