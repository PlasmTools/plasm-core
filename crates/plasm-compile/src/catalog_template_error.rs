//! Compile-owned failures for catalog recipes, views, embedded identity and OpenAPI contracts.
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CatalogTemplateError {
    #[error(transparent)]
    Cml(#[from] crate::CmlError),
    #[error("decode compiled request recipes: {source}")]
    RecipeJson {
        #[source]
        source: Arc<serde_json::Error>,
    },
    #[error("compiled catalog has no request recipe for `{capability}`")]
    RecipeMissing { capability: String },
    #[error("compiled request recipes target CGS {expected}, loaded CGS is {actual}")]
    RevisionMismatch { expected: String, actual: String },
    #[error("compiled request recipe capability set does not match the CGS: expected {expected:?}, actual {actual:?}")]
    RecipeCapabilitySet {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    #[error("compiled request recipe manifest: {source}")]
    Manifest {
        #[source]
        source: Arc<plasm_core::catalog_il::CatalogIlError>,
    },
    #[error("read compiled request recipes at {path:?}: {source}")]
    RecipeRead {
        path: PathBuf,
        #[source]
        source: Arc<std::io::Error>,
    },
    #[error("compiled request recipe digest mismatch")]
    RecipeDigestMismatch,
    #[error("capability `{capability}`: service error bodies are opaque; conflict_rules are not supported; declare read-backed reconciliation")]
    ConflictRulesUnsupported { capability: String },
    #[error("capability `{capability}`: {source}")]
    CapabilityTemplate {
        capability: String,
        #[source]
        source: crate::CmlError,
    },
    #[error("capability `{capability}` CML template: unknown bind reference(s) {wires:?} — allowed: bind.catalog_http_origin")]
    UnknownBindingWires {
        capability: String,
        wires: Vec<String>,
    },
    #[error("capability `{capability}`: credential binding requires a create or action capability (got {kind:?})")]
    CredentialCapabilityKind {
        capability: String,
        kind: plasm_core::CapabilityKind,
    },
    #[error("{source}")]
    PathEnvironment {
        #[source]
        source: plasm_core::path_env::PathEnvProofError,
    },
    #[error("capability `{capability}`: pagination param(s) {parameters:?} also appear as CML template vars (path/query/body/headers/multipart) — dual-wire is forbidden; remove the manual fields and let `pagination:` drive the wire")]
    PaginationDualWire {
        capability: String,
        parameters: Vec<String>,
    },
    #[error("capability `{capability}`: initial-only query key `{field}` must be a field of the CML query object")]
    InitialOnlyQueryKey { capability: String, field: String },
    #[error("capability `{capability}`: parameter(s) {parameters:?} are declared in domain.yaml but not referenced in CML (path/query/body/headers/multipart/pagination). Fabricated filters/params that never hit the wire are forbidden — wire them in mappings.yaml or remove them from the capability.")]
    UnwiredCapabilityParameters {
        capability: String,
        parameters: Vec<String>,
    },
    #[error("{label}: template exceeds 32KiB ({actual} bytes, maximum {maximum})")]
    ViewTemplateSize {
        label: String,
        actual: usize,
        maximum: usize,
    },
    #[error("{label}: {source}")]
    ViewTemplateSyntax {
        label: String,
        #[source]
        source: Arc<minijinja::Error>,
    },
    #[error("view `{view}` references unknown capability `{capability}`")]
    ViewCapabilityMissing { view: String, capability: String },
    #[error("view `{view}`: capability `{capability}` maps to view `{mapped_view}`")]
    ViewMappingMismatch {
        view: String,
        capability: String,
        mapped_view: String,
    },
    #[error("view `{view}` capability `{capability}` must use transport: view")]
    ViewTransportRequired { view: String, capability: String },
    #[error("view `{view}` targets unknown entity `{entity}`")]
    ViewEntityMissing { view: String, entity: String },
    #[error("view `{view}` scope `{scope}` cannot be both required and inject")]
    ViewRequiredInjectedScope { view: String, scope: String },
    #[error("view `{view}` has duplicate node id `{node}`")]
    ViewDuplicateNode { view: String, node: String },
    #[error("view `{view}` traversal `{node}` cannot also declare capability, bind, or when")]
    ViewTraversalDeclarations { view: String, node: String },
    #[error("view `{view}` node `{node}` references unknown capability `{capability}`")]
    ViewNodeCapabilityMissing {
        view: String,
        node: String,
        capability: String,
    },
    #[error("view `{view}` node `{node}`: unsupported capability kind {kind:?}")]
    ViewNodeCapabilityKind {
        view: String,
        node: String,
        kind: plasm_core::CapabilityKind,
    },
    #[error("view `{view}` node `{node}`: nested view capabilities are not supported")]
    NestedView { view: String, node: String },
    #[error("view `{view}` node `{node}` bind `{parameter}` references `{referenced_node}` before it runs")]
    ViewForwardBinding {
        view: String,
        node: String,
        parameter: String,
        referenced_node: String,
    },
    #[error("view `{view}` {kind} `{field}` references unknown node `{node}`")]
    ViewOutputNodeMissing {
        view: String,
        kind: &'static str,
        field: String,
        node: String,
    },
    #[error("view `{view}` relation `{relation}` identity union needs nonempty nodes and a many relation")]
    ViewIdentityUnionShape {
        view: String,
        relation: String,
        nodes: Vec<String>,
        cardinality: plasm_core::Cardinality,
    },
    #[error("view `{view}` relation `{relation}` references unknown node `{node}`")]
    ViewRelationNodeMissing {
        view: String,
        relation: String,
        node: String,
    },
    #[error(
        "view `{view}` relation `{relation}` expects {expected}, node `{node}` produces {actual}"
    )]
    ViewRelationEntityMismatch {
        view: String,
        relation: String,
        node: String,
        expected: String,
        actual: String,
    },
    #[error("view `{view}` relation `{relation}` targets unknown entity `{entity}`")]
    ViewRelationEntityMissing {
        view: String,
        relation: String,
        entity: String,
    },
    #[error("capability `{capability}` maps to unknown view `{view}`")]
    CapabilityViewMissing { capability: String, view: String },
    #[error("{parent}.{relation}: missing embedded entity {entity}")]
    EmbeddedEntityMissing {
        parent: String,
        relation: String,
        entity: String,
    },
    #[error("{parent}.{relation}: embedded identity slot {entity}.{field} has no declared field")]
    EmbeddedIdentityFieldMissing {
        parent: String,
        relation: String,
        entity: String,
        field: String,
    },
    #[error("embedded identity slot {entity}.{field}: {source}")]
    EmbeddedIdentitySchema {
        entity: String,
        field: String,
        #[source]
        source: Arc<plasm_core::SchemaError>,
    },
    #[error("{parent}.{relation}: identity slot {entity}.{field} has an empty wire path")]
    EmbeddedIdentityEmptyPath {
        parent: String,
        relation: String,
        entity: String,
        field: String,
    },
    #[error("{parent}.{relation}: inherited identity slot {field} has incompatible parent and child types")]
    InheritedIdentityTypeMismatch {
        parent: String,
        relation: String,
        field: String,
        parent_type: plasm_core::FieldType,
        child_type: plasm_core::FieldType,
        parent_format: Option<plasm_core::ValueWireFormat>,
        child_format: Option<plasm_core::ValueWireFormat>,
    },
    #[error("capability `{capability}`: OpenAPI GET {path} declares pagination but CML omits `pagination:`")]
    OpenApiPaginationMissing { capability: String, path: String },
    #[error("read {path:?}: {source}")]
    OpenApiRead {
        path: PathBuf,
        #[source]
        source: Arc<std::io::Error>,
    },
    #[error("parse {path:?}: {source}")]
    OpenApiJson {
        path: PathBuf,
        #[source]
        source: Arc<serde_json::Error>,
    },
}
