//! CGS (Capability Graph Schema) load path: `domain.yaml` + `mappings.yaml`, combined YAML, or CGS interchange.
//!
//! **Tracing:** set `RUST_LOG=plasm_core::loader=trace` (or `=debug`) and install a `tracing-subscriber`
//! (e.g. `plasm-eval` / `dump_prompt` binaries do this). Phases logged: file read, `serde_yaml` parse,
//! [`assemble_cgs`], [`CGS::validate`].
//! For CML template parsing after load: `plasm_compile::transport=trace`.

use crate::identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityName, RelationName,
};
use crate::schema::{
    FieldValueKind, LegacyViaParamPatch, NamedValueSchema, ValueDomainKey, ViewDefinition,
};
use crate::{
    capability_template_all_var_names, AgentPresentation, ArrayItemsSchema, AttachmentMediaKind,
    AuthScheme, BackendSelectionSchema, CapabilityInputs,
    CapabilityKind, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson, Cardinality,
    FieldDeriveRule, FieldSchema, FieldType, InputFieldSchema, InputSchema, InputType,
    InvocationControlsSchema, OauthExtension, ParentScopeSchema, RelationSchema, ResourceSchema,
    ScopeAggregateKeyPolicy, CGS,
};
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};
use tracing::{debug, info, trace, warn};

fn deserialize_forbidden_invoke_preflight_key<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<serde::de::IgnoredAny>::deserialize(deserializer)? {
        None => Ok(()),
        Some(_) => Err(serde::de::Error::custom(
            "invoke_preflight was removed; use preflight: [{ kind: hydrate_invoke_target, get: <get_cap>, prefix: <env_prefix> }]",
        )),
    }
}

/// Hard cap for `domain.yaml` / `mappings.yaml` / combined CGS YAML (defense in depth).
const MAX_SCHEMA_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// When `PLASM_CGS_FAST_LOAD=1`, skip expression-surface / teaching table bundle synthesis at load (structural validate only).
pub fn plasm_cgs_fast_load_enabled() -> bool {
    std::env::var("PLASM_CGS_FAST_LOAD").ok().as_deref() == Some("1")
}

/// Read a schema YAML file as UTF-8 text. Refuses FIFOs/sockets and oversized files so we never
/// block forever on `read_to_string` (e.g. `mkfifo domain.yaml`) or allocate pathological buffers.
fn read_schema_text_file(path: &Path, label: &str) -> Result<String, String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("Failed to stat {label} {}: {e}", path.display()))?;
    if !is_regular_schema_file(&meta) {
        return Err(format!(
            "{} is not a regular file (or is a pipe/socket); refusing to read",
            path.display()
        ));
    }
    let len = meta.len();
    if len > MAX_SCHEMA_FILE_BYTES {
        return Err(format!(
            "{label} {} is too large ({} bytes; max {})",
            path.display(),
            len,
            MAX_SCHEMA_FILE_BYTES
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {label} {}: {e}", path.display()))?;
    trace!(
        path = %path.display(),
        label,
        chars = text.len(),
        "read_schema_text_file"
    );
    Ok(text)
}

fn is_regular_schema_file(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let ft = meta.file_type();
        if ft.is_fifo() || ft.is_socket() {
            return false;
        }
    }
    true
}

/// Domain model file (domain.yaml)
#[derive(Debug, Deserialize)]
pub struct DomainFile {
    /// Reusable value domains (`value_ref` targets); catalog-local.
    #[serde(default)]
    pub values: IndexMap<String, DomainNamedValue>,
    /// Information-flow class registry (`data_class` / `sink_class` / `sanitizes` must reference these keys).
    #[serde(default)]
    pub data_classes: IndexMap<crate::DataClassName, crate::DataClassSchema>,
    pub entities: IndexMap<String, DomainEntity>,
    pub capabilities: IndexMap<String, DomainCapability>,
    /// Monotonic distribution version for this catalog entry (`0` when omitted).
    #[serde(default)]
    pub version: u64,
    /// Default HTTP(S) origin for CML execution (required; matches [`CGS::http_backend`]).
    pub http_backend: String,
    /// Optional authentication scheme for all requests in this schema.
    #[serde(default)]
    pub auth: Option<AuthScheme>,
    /// Optional declarative OAuth scope implications (see [`OauthExtension`]).
    #[serde(default)]
    pub oauth: Option<OauthExtension>,
    /// Composed read-only capabilities (`transport: view` in mappings).
    #[serde(default)]
    pub views: IndexMap<String, ViewDefinition>,
    /// Declarative runtime schema overlay (`schema_overlay:` in domain.yaml).
    #[serde(default)]
    pub schema_overlay: Option<crate::schema_overlay::SchemaOverlaySpec>,
    /// Alternate registry ids accepted by discovery / MCP seed resolution (e.g. `pokemon` → `pokeapi`).
    #[serde(default)]
    pub registry_aliases: Vec<String>,
    /// When true, non-idempotent mutators must declare `identity_key` (PLT workflow identity).
    #[serde(default)]
    pub workflow_identity: bool,
}

#[derive(Debug, Deserialize)]
pub struct DomainEntity {
    #[serde(default)]
    pub description: String,
    /// Primary id field name. Optional when `key_vars` is provided — the first
    /// key var is used as the `id_field` for compound-key entities.
    #[serde(default)]
    pub id_field: Option<String>,
    #[serde(default)]
    pub id_format: Option<crate::IdFormat>,
    /// JSON path of object keys for row identity when there is no top-level id field.
    /// YAML: `[a, b]` or dotted string `a.b`.
    #[serde(default, deserialize_with = "deserialize_optional_id_from")]
    pub id_from: Option<Vec<String>>,
    /// Compound-key variable names (e.g. `[owner, repo, number]`). When present,
    /// `id_field` defaults to the first var if not explicitly set.
    #[serde(default)]
    pub key_vars: Vec<String>,
    pub fields: IndexMap<String, DomainField>,
    #[serde(default)]
    pub relations: IndexMap<String, DomainRelation>,
    /// Alternate entity tokens accepted by the path parser (e.g. `Workspace` for `Team`).
    #[serde(default)]
    pub expression_aliases: Vec<String>,
    /// See [`plasm_core::ResourceSchema::implicit_request_identity`].
    #[serde(default)]
    pub implicit_request_identity: bool,
    /// Relation/embed-only entity — no top-level capabilities (YAML: `abstract: true`).
    #[serde(default, rename = "abstract")]
    pub abstract_entity: bool,
    /// When false, teaching table omits projection bracket exemplars (default: true).
    #[serde(default = "default_domain_projection_examples")]
    pub domain_projection_examples: bool,
    /// Optional Get capability id for projection exemplar field order (`provides` / default order).
    #[serde(default)]
    pub primary_read: Option<String>,
    /// Optional Query capability id when the entity declares 2+ unscoped Queries.
    #[serde(default)]
    pub primary_query: Option<String>,
    /// Optional Search capability id when the entity declares 2+ unscoped Searches.
    #[serde(default)]
    pub primary_search: Option<String>,
    #[serde(default)]
    pub discovery: Option<crate::DiscoveryEntityHints>,
}

fn default_domain_projection_examples() -> bool {
    true
}

/// `values.*.enum` — token list, or token→English gloss map for teaching Meaning.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EnumMembershipYaml {
    List(Vec<String>),
    /// Keys are wire tokens; values are optional English glosses (empty = token only).
    Map(IndexMap<String, String>),
}

impl EnumMembershipYaml {
    fn into_tokens_and_glosses(self) -> (Vec<String>, Option<IndexMap<String, String>>) {
        match self {
            EnumMembershipYaml::List(v) => (v, None),
            EnumMembershipYaml::Map(m) => {
                let tokens: Vec<String> = m.keys().cloned().collect();
                let glosses: IndexMap<String, String> = m
                    .into_iter()
                    .filter(|(_, g)| !g.trim().is_empty())
                    .collect();
                let glosses = if glosses.is_empty() {
                    None
                } else {
                    Some(glosses)
                };
                (tokens, glosses)
            }
        }
    }
}

/// `values:` entry — same typing keys as a field, without per-field response metadata.
#[derive(Debug, Deserialize)]
pub struct DomainNamedValue {
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type")]
    pub value_type: String,
    #[serde(default)]
    pub target: Option<String>,
    /// Enum membership for `type: enum` / `multi_enum` (legacy key `allowed_values` still accepted).
    ///
    /// List form: `enum: [a, b]` · Map form: `enum: { a: "gloss", b: "gloss" }` (glosses feed teaching Meaning).
    #[serde(default, alias = "allowed_values")]
    pub enum_values: Option<EnumMembershipYaml>,
    #[serde(default, rename = "enum")]
    pub enum_key: Option<EnumMembershipYaml>,
    #[serde(default)]
    pub items: Option<DomainItems>,
    /// Default ISO-like currency token for [`FieldType::Money`] rows.
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub exclusive_min: Option<f64>,
    #[serde(default)]
    pub exclusive_max: Option<f64>,
    #[serde(default)]
    pub multiple_of: Option<f64>,
    /// Rejected at compile — retired keys (no dual-read).
    #[serde(default)]
    pub value_format: Option<serde_yaml::Value>,
    #[serde(default)]
    pub string_semantics: Option<serde_yaml::Value>,
}

fn deserialize_optional_id_from<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IdFromYaml {
        Str(String),
        Arr(Vec<String>),
    }
    let v = Option::<IdFromYaml>::deserialize(deserializer)?;
    Ok(v.map(|x| match x {
        IdFromYaml::Str(s) => s
            .split('.')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        IdFromYaml::Arr(a) => a,
    }))
}

fn deserialize_optional_wire_path<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum WirePathYaml {
        Str(String),
        Arr(Vec<String>),
    }
    let v = Option::<WirePathYaml>::deserialize(deserializer)?;
    Ok(v.map(|x| match x {
        WirePathYaml::Str(s) => s
            .split('.')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        WirePathYaml::Arr(a) => a,
    }))
}

#[derive(Debug, Deserialize)]
pub struct DomainField {
    /// Catalog-local key into `values:` — **only** way to declare wire shape for this field.
    pub value_ref: String,
    #[serde(default)]
    pub description: String,
    /// Wire path for response decoding (`owner.login` or `["owner","login"]`).
    #[serde(default, deserialize_with = "deserialize_optional_wire_path")]
    pub path: Option<Vec<String>>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub agent_presentation: Option<AgentPresentation>,
    /// Optional MIME for attachment-like fields; copied to [`FieldSchema::mime_type_hint`].
    #[serde(default)]
    pub mime_type_hint: Option<String>,
    #[serde(default)]
    pub attachment_media: Option<AttachmentMediaKind>,
    /// Post-extraction derivation (see [`FieldSchema::derive`]).
    #[serde(default)]
    pub derive: Option<FieldDeriveRule>,
    /// Optional information-flow label for this field (must exist in top-level `data_classes:`).
    #[serde(default)]
    pub data_class: Option<crate::DataClassName>,
    /// Sibling field that supplies currency for a money amount on the same entity.
    #[serde(default)]
    pub currency_field: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DomainRelation {
    #[serde(default)]
    pub description: String,
    pub target: String,
    pub cardinality: String,
    /// How chain traversal resolves this edge (`query_scoped`, `from_parent_get`, …).
    #[serde(default)]
    pub materialize: Option<crate::RelationMaterialization>,
    /// Legacy authoring key; normalized to `materialize.query_scoped` at load.
    #[serde(default)]
    pub via_param: Option<String>,
    #[serde(default)]
    pub discovery: Option<crate::DiscoveryRelationHints>,
}

/// `invoke_preflight` is rejected at deserialize time via [`deserialize_forbidden_invoke_preflight_key`].
/// Abolished `execution:` (RA-5 context frame) is rejected here via `deny_unknown_fields`.
#[allow(clippy::manual_non_exhaustive)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainCapability {
    #[serde(default)]
    pub description: String,
    pub kind: String,
    pub entity: String,
    /// Policy for compound `entity_ref` scope parameters after runtime splat (`retain` default).
    #[serde(default)]
    pub scope_aggregate_key_policy: Option<ScopeAggregateKeyPolicy>,
    /// Parameters derived exclusively from a typed parent row.
    #[serde(default)]
    pub scope: Vec<DomainParameter>,
    /// Query/search source-selection parameters.
    #[serde(default)]
    pub selection: Vec<DomainParameter>,
    /// Pagination, sorting, and response-shape controls.
    #[serde(default)]
    pub controls: Vec<DomainParameter>,
    /// Named invocation arguments which are not payload fields.
    #[serde(default)]
    pub arguments: Option<InputSchema>,
    /// Create/update/action body payload.
    #[serde(default)]
    pub payload: Option<InputSchema>,
    /// Entity fields this capability populates in its response.
    /// When absent, defaults are applied by `CGS::effective_provides` (same ordered field list as
    /// teaching table exemplars when `provides` is empty: `id_field` first, then lexicographic rest).
    #[serde(default)]
    pub provides: Vec<String>,
    /// Data classes this capability declares to sanitize before producing output.
    #[serde(default)]
    pub sanitizes: Vec<crate::DataClassName>,
    /// When false, capability must not be used as a policy sanitizer.
    #[serde(default)]
    pub deterministic: Option<bool>,
    /// Declared response shape for validation (required for `action` unless `provides` is set).
    #[serde(default)]
    pub output: Option<crate::OutputSchema>,
    #[serde(default)]
    pub preflight: Option<crate::preflight::PreflightPlan>,
    #[serde(
        default,
        rename = "invoke_preflight",
        deserialize_with = "deserialize_forbidden_invoke_preflight_key"
    )]
    _invoke_preflight_removed: (),
    #[serde(default)]
    pub discovery: Option<crate::DiscoveryCapabilityHints>,
    /// Natural-key params for PLT workflow identity (required when catalog `workflow_identity: true`).
    #[serde(default)]
    pub identity_key: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainParameter {
    pub name: String,
    /// Catalog-local key into `values:` when this parameter is registry-backed.
    #[serde(default)]
    pub value_ref: String,
    /// Inline structural input (`type: object`, `array`, `union`, …); mutually exclusive with non-empty `value_ref`.
    #[serde(default)]
    pub input_type: Option<Box<InputType>>,
    #[serde(default)]
    pub required: bool,
    /// Human-readable hint for prompts; teaching gloss uses `type · description`, else `type · name`.
    #[serde(default)]
    pub description: String,
    /// Optional sink class for information-flow validation (must exist in top-level `data_classes:`).
    #[serde(default)]
    pub sink_class: Option<crate::SinkClassName>,
}

/// YAML `items:` block for `array` fields and parameters.
#[derive(Debug, Deserialize)]
pub struct DomainItems {
    /// Element shape lives under `values:` (same as `value_ref` on fields).
    pub value_ref: String,
}

/// Load a CGS from split domain.yaml + mappings.yaml files.
pub fn load_split_schema(domain_path: &Path, mappings_path: &Path) -> Result<CGS, String> {
    load_split_schema_internal(domain_path, mappings_path, true)
}

/// Load split schema files without running [`finalize_cgs_load`] (for pack paths that validate after mutation).
pub fn load_split_schema_unvalidated(
    domain_path: &Path,
    mappings_path: &Path,
) -> Result<CGS, String> {
    load_split_schema_internal(domain_path, mappings_path, false)
}

fn load_split_schema_internal(
    domain_path: &Path,
    mappings_path: &Path,
    validate: bool,
) -> Result<CGS, String> {
    let span = crate::spans::schema_load_split(domain_path, mappings_path);
    let _enter = span.enter();
    let t0 = std::time::Instant::now();

    debug!("phase: read domain.yaml");
    let domain_content = read_schema_text_file(domain_path, "domain.yaml")?;
    debug!(bytes = domain_content.len(), "phase: read domain.yaml done");

    debug!("phase: read mappings.yaml");
    let mappings_content = read_schema_text_file(mappings_path, "mappings.yaml")?;
    debug!(
        bytes = mappings_content.len(),
        "phase: read mappings.yaml done"
    );

    debug!("phase: serde_yaml parse domain (DomainFile)");
    let domain: DomainFile = serde_yaml::from_str(&domain_content)
        .map_err(|e| format!("Failed to parse domain YAML: {}", e))?;
    debug!(
        entities = domain.entities.len(),
        capabilities = domain.capabilities.len(),
        "phase: domain YAML parsed"
    );

    debug!("phase: serde_yaml parse mappings (IndexMap)");
    let mappings: IndexMap<String, serde_json::Value> = serde_yaml::from_str(&mappings_content)
        .map_err(|e| format!("Failed to parse mappings YAML: {}", e))?;
    debug!(keys = mappings.len(), "phase: mappings YAML parsed");

    debug!("phase: assemble_cgs");
    let mut cgs = assemble_cgs_core(domain, mappings)?;
    if validate {
        finalize_cgs_load(&mut cgs)?;
    }

    info!(
        elapsed_ms = t0.elapsed().as_millis() as u64,
        entities = cgs.entities.len(),
        capabilities = cgs.capabilities.len(),
        "load_split_schema finished"
    );
    Ok(cgs)
}

/// Like [`load_schema_dir`] but skips validation — caller must run [`finalize_cgs_load`] after mutations.
pub fn load_schema_dir_unvalidated(dir: &Path) -> Result<CGS, String> {
    let resolved = resolve_schema_directory_for_load(dir);
    let span = crate::spans::schema_load_directory(&resolved);
    let _g = span.enter();
    load_split_schema_unvalidated(
        &resolved.join("domain.yaml"),
        &resolved.join("mappings.yaml"),
    )
}

/// Run post-assemble normalization, validation, and string-semantics checks.
pub fn finalize_cgs_load(cgs: &mut CGS) -> Result<(), String> {
    let span = crate::spans::schema_validate(cgs.entities.len(), cgs.capabilities.len());
    let _guard = span.enter();
    let legacy_via_param = std::mem::take(&mut cgs.pending_legacy_via_param_patches);
    cgs.normalize_relation_materialization(&legacy_via_param);
    cgs.stamp_entity_ref_catalogs();
    debug!(
        entities = cgs.entities.len(),
        capabilities = cgs.capabilities.len(),
        "assemble_cgs: calling CGS::validate"
    );
    cgs.validate()
        .map_err(|e| format!("CGS validation failed: {}", e))?;

    warn_scope_aggregate_policy_template_mismatches(cgs);
    warn_unlabeled_output_data(cgs);

    trace!("assemble_cgs: validate ok");
    Ok(())
}

/// If `dir/domain.yaml` is missing, resolve known authoring typos to a sibling directory that exists.
fn resolve_schema_directory_for_load(dir: &Path) -> PathBuf {
    if dir.join("domain.yaml").is_file() {
        return dir.to_path_buf();
    }
    // Common mistake: `overshow_tool` vs fixture dir `overshow_tools`.
    if dir.file_name().and_then(|n| n.to_str()) == Some("overshow_tool") {
        let alt = dir.with_file_name("overshow_tools");
        if alt.join("domain.yaml").is_file() {
            info!(
                requested = %dir.display(),
                resolved = %alt.display(),
                "resolve_schema_directory_for_load: using sibling `overshow_tools`"
            );
            return alt;
        }
    }
    dir.to_path_buf()
}

/// Load a CGS from a directory containing domain.yaml and mappings.yaml.
pub fn load_schema_dir(dir: &Path) -> Result<CGS, String> {
    let resolved = resolve_schema_directory_for_load(dir);
    let span = crate::spans::schema_load_directory(&resolved);
    let _g = span.enter();
    load_split_schema(
        &resolved.join("domain.yaml"),
        &resolved.join("mappings.yaml"),
    )
}

/// Load a CGS from a directory (`domain.yaml` + `mappings.yaml`), a single YAML file
/// (serialized [`CGS`] interchange, or combined domain + mappings), or a legacy `.json`
/// path (deprecated; removed — use YAML or a schema directory).
pub fn load_schema(path: &Path) -> Result<CGS, String> {
    let span = crate::spans::schema_load_path(path);
    let _g = span.enter();

    if path.is_dir() {
        debug!("load_schema branch: directory -> load_schema_dir");
        return load_schema_dir(path);
    }
    if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
        debug!("load_schema branch: yaml file");
        let content = read_schema_text_file(path, "schema YAML")?;

        // Full CGS document (e.g. `.cgs.yaml` from extract pipelines)
        debug!("trying serde_yaml -> CGS interchange");
        if let Ok(mut cgs) = serde_yaml::from_str::<CGS>(&content) {
            debug!("CGS interchange parse ok; validating");
            cgs.stamp_entity_ref_catalogs();
            cgs.validate()
                .map_err(|e| format!("CGS validation failed: {}", e))?;
            return Ok(cgs);
        }

        // Combined authoring file: entities + capabilities + optional mappings
        #[derive(Deserialize)]
        struct CombinedFile {
            #[serde(flatten)]
            domain: DomainFile,
            #[serde(default)]
            mappings: IndexMap<String, serde_json::Value>,
        }

        debug!("trying combined DomainFile + mappings YAML");
        let combined: CombinedFile = serde_yaml::from_str(&content).map_err(|e| {
            format!(
                "Failed to parse YAML (expected CGS or domain+mappings): {}",
                e
            )
        })?;
        return assemble_cgs(combined.domain, combined.mappings);
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(crate::catalog_il::CATALOG_IL_BODY_SUFFIX))
    {
        debug!("load_schema branch: compiled catalog JSON IL");
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        return crate::catalog_il::load_catalog_il_bytes(&bytes);
    }
    if path.extension().is_some_and(|e| e == "json") {
        Err(format!(
            "bare CGS JSON is not supported ({}). Use a directory with domain.yaml + mappings.yaml, a .cgs.yaml / .yaml CGS file, or a compiled `{}` catalog artifact.",
            path.display(),
            crate::catalog_il::CATALOG_IL_BODY_SUFFIX
        ))
    } else {
        Err(format!("Unknown schema format: {}", path.display()))
    }
}

/// Pluggable CGS loading (filesystem path, embedded bundle, remote fetch, etc.).
pub trait SchemaSource {
    fn load_cgs(&self) -> Result<CGS, String>;
}

/// Load via [`load_schema`] from a file or directory path.
#[derive(Debug, Clone)]
pub struct PathSchemaSource {
    pub path: std::path::PathBuf,
}

impl SchemaSource for PathSchemaSource {
    fn load_cgs(&self) -> Result<CGS, String> {
        load_schema(&self.path)
    }
}

fn compile_domain_named_values(
    domain_values: &IndexMap<String, DomainNamedValue>,
) -> Result<IndexMap<String, NamedValueSchema>, String> {
    let mut out = IndexMap::new();
    for (name, dv) in domain_values.iter() {
        let ctx = format!("values['{name}']");
        let schema = compile_one_named_value(dv, &ctx, &out)?;
        out.insert(name.clone(), schema);
    }
    Ok(out)
}

fn compile_one_named_value(
    d: &DomainNamedValue,
    ctx: &str,
    prior: &IndexMap<String, NamedValueSchema>,
) -> Result<NamedValueSchema, String> {
    if d.string_semantics.is_some() {
        return Err(format!(
            "{ctx}: `string_semantics` was removed; use profile types (`markdown`, `document`, `json_text`, `html`) or bare `string`"
        ));
    }
    if d.value_format.is_some() {
        return Err(format!(
            "{ctx}: `value_format` was removed; use temporal profiles (`rfc3339`, `iso8601_date`, `unix_ms`, `unix_sec`) or money as decimal-string kernel"
        ));
    }
    let vt = d.value_type.trim();
    if vt.is_empty() {
        return Err(format!("{ctx}: missing `type`"));
    }
    let (kernel, profile) = crate::value_domain::parse_type_name(vt, d.target.as_deref())
        .map_err(|e| format!("{ctx}: {e}"))?;

    let enum_membership = {
        let membership_yaml = d.enum_key.clone().or_else(|| d.enum_values.clone());
        match membership_yaml {
            None => None,
            Some(m) => {
                let (tokens, glosses) = m.into_tokens_and_glosses();
                if tokens.is_empty() {
                    None
                } else {
                    Some(
                        crate::value_domain::EnumMembership::try_new(tokens, glosses)
                            .map_err(|e| format!("{ctx}: {e}"))?,
                    )
                }
            }
        }
    };

    if matches!(profile, Some(crate::value_domain::ProfileId::MultiEnum))
        && enum_membership
            .as_ref()
            .is_none_or(|m| m.tokens().is_empty())
    {
        return Err(format!(
            "{ctx}: type 'multi_enum' requires non-empty `enum:` membership list"
        ));
    }
    if matches!(profile, Some(crate::value_domain::ProfileId::Enum))
        && enum_membership
            .as_ref()
            .is_none_or(|m| m.tokens().is_empty())
    {
        return Err(format!(
            "{ctx}: type 'enum' requires non-empty `enum:` membership list"
        ));
    }

    let array_items = if matches!(kernel, crate::value_domain::KernelKind::Array) {
        let Some(ref it) = d.items else {
            return Err(format!(
                "{ctx}: type 'array' requires `items:` describing element types"
            ));
        };
        Some(parse_domain_array_items(
            it,
            &format!("{ctx}, items"),
            Some(prior),
        )?)
    } else {
        if d.items.is_some() {
            return Err(format!(
                "{ctx}: 'items:' is only valid when type is 'array'"
            ));
        }
        None
    };

    let constraints = crate::value_domain::Constraints {
        min_length: d.min_length,
        max_length: d.max_length,
        pattern: d.pattern.clone(),
        min: d.min,
        max: d.max,
        exclusive_min: d.exclusive_min,
        exclusive_max: d.exclusive_max,
        multiple_of: d.multiple_of,
    };

    let currency = d
        .currency
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let domain = crate::value_domain::ValueDomain::new(
        kernel,
        profile,
        constraints,
        enum_membership,
        currency,
    )
    .map_err(|e| format!("{ctx}: {e}"))?;

    Ok(NamedValueSchema::from_domain(
        d.description.clone(),
        domain,
        array_items,
    ))
}
fn field_schema_from_domain_field(
    fname: &str,
    entity_name: &str,
    f: &DomainField,
    values: &IndexMap<String, NamedValueSchema>,
) -> Result<FieldSchema, String> {
    let ctx = format!("entity '{entity_name}', field '{fname}'");
    let vr = f.value_ref.trim();
    if vr.is_empty() {
        return Err(format!(
            "{ctx}: `value_ref` is required — declare the wire shape under top-level `values:`"
        ));
    }
    let nv = values
        .get(vr)
        .ok_or_else(|| format!("{ctx}: unknown `value_ref` '{vr}'"))?;
    let description = if f.description.trim().is_empty() {
        nv.description.clone()
    } else {
        f.description.clone()
    };
    let vdk = ValueDomainKey::new(vr.to_string()).map_err(|e| format!("{ctx}: {e}"))?;
    Ok(FieldSchema {
        name: EntityFieldName::from(fname),
        kind: FieldValueKind::Registry(vdk),
        description,
        required: f.required,
        agent_presentation: f.agent_presentation,
        mime_type_hint: f.mime_type_hint.clone(),
        data_class: f.data_class.clone(),
        attachment_media: f.attachment_media,
        wire_path: f.path.clone(),
        derive: f.derive.clone(),
        currency_field: f
            .currency_field
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    })
}

fn input_field_schema_from_domain_parameter(
    cap_name: &str,
    p: &DomainParameter,
    values: &IndexMap<String, NamedValueSchema>,
) -> Result<InputFieldSchema, String> {
    let ctx = format!("capability '{cap_name}', parameter '{}'", p.name);
    let vr = p.value_ref.trim();
    match (vr.is_empty(), p.input_type.as_ref()) {
        (false, None) => {
            let nv = values
                .get(vr)
                .ok_or_else(|| format!("{ctx}: unknown `value_ref` '{vr}'"))?;
            let description = if p.description.trim().is_empty() {
                let nd = nv.description.trim();
                if nd.is_empty() {
                    None
                } else {
                    Some(nv.description.clone())
                }
            } else {
                Some(p.description.clone())
            };
            let vdk = ValueDomainKey::new(vr.to_string()).map_err(|e| format!("{ctx}: {e}"))?;
            Ok(InputFieldSchema {
                name: p.name.clone(),
                wire: crate::InputFieldWire::Registry(vdk),
                required: p.required,
                description,
                default: None,
                sink_class: p.sink_class.clone(),
                wire_json_path: None,
                wire_array_element_key: None,
            })
        }
        (true, Some(ty)) => Ok(InputFieldSchema {
            name: p.name.clone(),
            wire: crate::InputFieldWire::Inline(ty.clone()),
            required: p.required,
            description: if p.description.trim().is_empty() {
                None
            } else {
                Some(p.description.clone())
            },
            default: None,
            sink_class: p.sink_class.clone(),
            wire_json_path: None,
            wire_array_element_key: None,
        }),
        (false, Some(_)) => Err(format!(
            "{ctx}: set exactly one of `value_ref` or `input_type`, not both"
        )),
        (true, None) => Err(format!(
            "{ctx}: missing `value_ref` and `input_type` — declare a `values:` key or inline `input_type`"
        )),
    }
}

fn input_fields_from_domain_parameters(
    cap_name: &str,
    params: &[DomainParameter],
    values: &IndexMap<String, NamedValueSchema>,
) -> Result<Vec<InputFieldSchema>, String> {
    params
        .iter()
        .map(|p| input_field_schema_from_domain_parameter(cap_name, p, values))
        .collect()
}

fn capability_inputs_from_domain(
    cap_name: &str,
    cap: &DomainCapability,
    values: &IndexMap<String, NamedValueSchema>,
) -> Result<CapabilityInputs, String> {
    Ok(CapabilityInputs {
        scope: ParentScopeSchema(input_fields_from_domain_parameters(
            cap_name, &cap.scope, values,
        )?),
        selection: BackendSelectionSchema(input_fields_from_domain_parameters(
            cap_name,
            &cap.selection,
            values,
        )?),
        controls: InvocationControlsSchema(input_fields_from_domain_parameters(
            cap_name,
            &cap.controls,
            values,
        )?),
        arguments: cap.arguments.clone(),
        payload: cap.payload.clone(),
    })
}

fn assemble_cgs_core(
    mut domain: DomainFile,
    mut mappings: IndexMap<String, serde_json::Value>,
) -> Result<CGS, String> {
    let span = crate::spans::schema_assemble(domain.entities.len(), domain.capabilities.len());
    let _g = span.enter();
    trace!("assemble_cgs: building entity resources");

    let mut cgs = CGS::new();
    cgs.http_backend = domain.http_backend;
    cgs.auth = domain.auth;
    cgs.oauth = domain.oauth;
    cgs.version = domain.version;
    cgs.schema_overlay = domain.schema_overlay;
    cgs.registry_aliases = domain.registry_aliases;
    cgs.workflow_identity = domain.workflow_identity;
    cgs.data_classes = domain.data_classes;
    cgs.values = compile_domain_named_values(&domain.values)?;

    let mut legacy_via_param_patches: Vec<LegacyViaParamPatch> = Vec::new();

    for (name, entity) in &domain.entities {
        validate_compound_entity_identity(name, entity)?;

        let fields: Vec<FieldSchema> = entity
            .fields
            .iter()
            .map(|(fname, f)| field_schema_from_domain_field(fname, name, f, &cgs.values))
            .collect::<Result<Vec<_>, String>>()?;

        let source_id_field = entity
            .id_field
            .clone()
            .or_else(|| entity.key_vars.first().cloned())
            .map(EntityFieldName::from)
            .unwrap_or_else(|| EntityFieldName::from("id"));

        let relations: Vec<RelationSchema> = entity
            .relations
            .iter()
            .map(|(rname, r)| {
                if r.cardinality == "many" && r.materialize.is_none() {
                    if let Some(via_param) = r.via_param.as_ref() {
                        legacy_via_param_patches.push(LegacyViaParamPatch {
                            entity: EntityName::from(name.as_str()),
                            relation: RelationName::from(rname.as_str()),
                            via_param: CapabilityParamName::from(via_param.as_str()),
                            source_id_field: source_id_field.clone(),
                        });
                    }
                }
                RelationSchema {
                    name: RelationName::from(rname.as_str()),
                    description: r.description.clone(),
                    target_resource: EntityName::from(r.target.clone()),
                    cardinality: if r.cardinality == "many" {
                        Cardinality::Many
                    } else {
                        Cardinality::One
                    },
                    materialize: r.materialize.clone(),
                    discovery: r.discovery.clone(),
                }
            })
            .collect();

        // Resolve id_field: explicit > first key_var > fallback "id"
        let id_field = entity
            .id_field
            .clone()
            .or_else(|| entity.key_vars.first().cloned())
            .map(EntityFieldName::from)
            .unwrap_or_else(|| EntityFieldName::from("id"));

        let resource = ResourceSchema {
            name: EntityName::from(name.clone()),
            description: entity.description.clone(),
            id_field,
            id_format: entity.id_format,
            id_from: entity.id_from.clone(),
            fields,
            relations,
            expression_aliases: entity.expression_aliases.clone(),
            implicit_request_identity: entity.implicit_request_identity,
            key_vars: entity
                .key_vars
                .iter()
                .map(|s| EntityFieldName::from(s.as_str()))
                .collect(),
            abstract_entity: entity.abstract_entity,
            domain_projection_examples: entity.domain_projection_examples,
            primary_read: entity.primary_read.clone(),
            primary_query: entity.primary_query.clone(),
            primary_search: entity.primary_search.clone(),
            discovery: entity.discovery.clone(),
        };

        cgs.add_resource(resource)
            .map_err(|e| format!("Failed to add entity '{}': {}", name, e))?;
    }

    trace!(
        n = cgs.entities.len(),
        "assemble_cgs: entities added; building capabilities"
    );

    for (cap_name, cap) in &domain.capabilities {
        let kind = parse_capability_kind(&cap.kind);

        let template = mappings.swap_remove(cap_name).ok_or_else(|| {
            format!(
                "Capability '{cap_name}' is listed in domain.yaml but has no entry in mappings.yaml"
            )
        })?;

        let inputs = capability_inputs_from_domain(cap_name, cap, &cgs.values)?;

        let capability = CapabilitySchema {
            name: CapabilityName::from(cap_name.clone()),
            description: cap.description.clone(),
            kind,
            domain: EntityName::from(cap.entity.clone()),
            mapping: CapabilityMapping {
                template: CapabilityTemplateJson(template),
            },
            inputs,
            output_schema: cap.output.clone(),
            provides: cap.provides.clone(),
            sanitizes: cap.sanitizes.clone(),
            invalidates_entities: vec![],
            deterministic: cap.deterministic,
            scope_aggregate_key_policy: cap.scope_aggregate_key_policy.unwrap_or_default(),
            preflight: cap.preflight.clone(),
            discovery: cap.discovery.clone(),
            identity_key: cap.identity_key.clone(),
        };

        cgs.add_capability(capability)
            .map_err(|e| format!("Failed to add capability '{}': {}", cap_name, e))?;
    }

    cgs.views = std::mem::take(&mut domain.views);

    cgs.pending_legacy_via_param_patches = legacy_via_param_patches;

    Ok(cgs)
}

fn assemble_cgs(
    domain: DomainFile,
    mappings: IndexMap<String, serde_json::Value>,
) -> Result<CGS, String> {
    let mut cgs = assemble_cgs_core(domain, mappings)?;
    finalize_cgs_load(&mut cgs)?;
    Ok(cgs)
}

/// Compound-key entities must declare how row-level extraction finds a primary slot before
/// [`plasm_compile::build_decoded_reference`] assembles the compound [`Ref`]. Without an explicit
/// `id_field`, `id_from`, or `implicit_request_identity`, the loader used to default `id_field` to
/// the first `key_var` — which is often absent on the wire (e.g. `owner` on GitHub commit JSON).
fn validate_compound_entity_identity(
    entity_name: &str,
    entity: &DomainEntity,
) -> Result<(), String> {
    if entity.key_vars.len() < 2 {
        return Ok(());
    }
    let has_explicit = entity.id_field.is_some();
    let has_id_from = entity.id_from.as_ref().is_some_and(|p| !p.is_empty());
    let implicit = entity.implicit_request_identity;
    if has_explicit || has_id_from || implicit {
        return Ok(());
    }
    Err(format!(
        "entity '{entity_name}': compound key_vars {:?} require an explicit `id_field`, non-empty `id_from`, or `implicit_request_identity: true` (do not rely on implicit default to the first key var)",
        entity.key_vars
    ))
}

/// Warn when a catalog that declares `data_classes:` leaves structured/multiline read outputs
/// without a `data_class` (plan-flow cannot label that data).
fn warn_unlabeled_output_data(cgs: &CGS) {
    for msg in cgs.unlabeled_output_data_warnings() {
        warn!(target: "plasm_core::loader", violation = %msg, "unlabeled output data");
    }
}

/// Warn when `omit_when_redundant` is set but the HTTP template still references the aggregate
/// scope variable (e.g. `repository`) instead of splatted `key_vars`.
fn warn_scope_aggregate_policy_template_mismatches(cgs: &CGS) {
    for (cap_name, cap) in &cgs.capabilities {
        if cap.scope_aggregate_key_policy != ScopeAggregateKeyPolicy::OmitWhenRedundant {
            continue;
        }
        let vars = capability_template_all_var_names(&cap.mapping.template.0);
        for param in cap.scope_params() {
            let Ok(nv) = param.named_value(cgs) else {
                continue;
            };
            if !matches!(nv.field_type, FieldType::EntityRef { .. }) {
                continue;
            }
            if vars.contains(&param.name) {
                warn!(
                    target: "plasm_core::loader",
                    capability = %cap_name,
                    param = %param.name,
                    "CML template still references aggregate scope var while scope_aggregate_key_policy is omit_when_redundant; prefer splatted key_vars in path/query/body"
                );
            }
        }
    }
}

fn parse_domain_array_items(
    items: &DomainItems,
    context: &str,
    named_values: Option<&IndexMap<String, NamedValueSchema>>,
) -> Result<ArrayItemsSchema, String> {
    let name = items.value_ref.trim();
    if name.is_empty() {
        return Err(format!(
            "{context}: `items.value_ref` is required (element shape lives under `values:`)"
        ));
    }
    let Some(map) = named_values else {
        return Err(format!(
            "{context}: array `items` require top-level `values:` in domain.yaml"
        ));
    };
    let nv = map
        .get(name)
        .ok_or_else(|| format!("{context}: unknown `items.value_ref` '{name}'"))?;
    if matches!(nv.field_type, FieldType::Array) {
        return Err(format!(
            "{context}: `items.value_ref` '{name}' must not reference an array-typed value domain"
        ));
    }
    let vdk = ValueDomainKey::new(name.to_string()).map_err(|e| format!("{context}: {e}"))?;
    Ok(ArrayItemsSchema {
        kind: FieldValueKind::Registry(vdk),
        field_type: nv.field_type.clone(),
        value_format: nv.value_format,
        allowed_values: nv.allowed_values.clone(),
    })
}

fn parse_capability_kind(s: &str) -> CapabilityKind {
    match s {
        "query" => CapabilityKind::Query,
        "get" => CapabilityKind::Get,
        "create" => CapabilityKind::Create,
        "update" => CapabilityKind::Update,
        "delete" => CapabilityKind::Delete,
        "action" => CapabilityKind::Action,
        "search" => CapabilityKind::Search,
        // e.g. GET /user — no row id; treated like Get for typing and tooling.
        "singleton" => CapabilityKind::Get,
        _ => CapabilityKind::Action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    #[test]
    fn entity_ref_stamp_distinguishes_same_target_across_catalogs() {
        fn stamped(entry_id: &str) -> FieldType {
            let field_type = FieldType::EntityRef {
                entry_id: Default::default(),
                target: EntityName::from("SharedTarget"),
            };
            let mut cgs = CGS::new();
            cgs.bind_registry_entry_id(entry_id);
            cgs.values.insert(
                "shared_ref".into(),
                NamedValueSchema::from_domain(
                    String::new(),
                    crate::value_domain::ValueDomain::from_legacy(
                        &field_type,
                        None,
                        None,
                        None,
                        None,
                    ),
                    None,
                ),
            );
            cgs.stamp_entity_ref_catalogs();
            cgs.values["shared_ref"].field_type.clone()
        }

        let alpha = stamped("alpha");
        let beta = stamped("beta");

        assert_eq!(alpha.entity_ref_target(), Some("SharedTarget"));
        assert_eq!(beta.entity_ref_target(), Some("SharedTarget"));
        assert_eq!(alpha.entity_ref_entry_id(), Some("alpha"));
        assert_eq!(beta.entity_ref_entry_id(), Some("beta"));
        assert_ne!(
            alpha.entity_ref_entry_id(),
            beta.entity_ref_entry_id(),
            "same wire target in different catalogs must retain distinct ownership"
        );
    }

    fn init_loader_tracing_test() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                        tracing_subscriber::EnvFilter::new("plasm_core::loader=trace,info")
                    }),
                )
                .with_test_writer()
                .try_init();
        });
    }

    #[test]
    fn unlabeled_output_data_warnings_when_catalog_opts_into_data_classes() {
        let dir = Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).expect("github");
        let warnings = cgs.unlabeled_output_data_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Commit") && w.contains("message")),
            "structured provided fields without data_class must warn: {warnings:?}"
        );
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("Repository") && w.contains("description")),
            "labeled Repository.description must not warn: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .all(|w| w.contains("data_class") && w.contains("unlabeled")),
            "warning copy must mention data_class / unlabeled: {warnings:?}"
        );
    }

    #[test]
    fn unlabeled_output_data_warnings_skipped_without_data_classes_registry() {
        let dir = Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).expect("matrix");
        assert!(
            cgs.data_classes.is_empty(),
            "matrix fixture must not declare data_classes"
        );
        assert!(
            cgs.unlabeled_output_data_warnings().is_empty(),
            "catalogs without data_classes: must not warn about unlabeled outputs"
        );
    }

    #[test]
    fn language_matrix_loads_money_offer() {
        let dir = Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).expect("language matrix with money");
        let offer = cgs.get_entity("LangOffer").expect("LangOffer");
        let price = offer.fields.get("price").expect("price");
        assert_eq!(price.currency_field.as_deref(), Some("quote_currency"));
        let nv = price.named_value(&cgs).expect("price nv");
        assert_eq!(nv.field_type, FieldType::Money);
    }

    #[test]
    fn accepts_money_without_value_format() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_price:
    type: money
entities:
  Offer:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      price:
        value_ref: nv_price
        required: true
capabilities:
  q:
    kind: query
    entity: Offer
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        load_schema_dir(dir.path()).expect("money needs no value_format");
    }

    #[test]
    fn rejects_string_semantics_on_money() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_price:
    type: money
    string_semantics: short
entities:
  Offer:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      price:
        value_ref: nv_price
        required: true
capabilities:
  q:
    kind: query
    entity: Offer
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(err.contains("string_semantics"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_dangling_currency_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_price:
    type: money
entities:
  Offer:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      price:
        value_ref: nv_price
        required: true
        currency_field: quote_currency
capabilities:
  q:
    kind: query
    entity: Offer
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("quote_currency") && err.contains("currency_field"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_non_string_currency_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_qty:
    type: integer
  nv_price:
    type: money
entities:
  Offer:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      qty:
        value_ref: nv_qty
        required: true
      price:
        value_ref: nv_price
        required: true
        currency_field: qty
capabilities:
  q:
    kind: query
    entity: Offer
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("qty") && err.contains("string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_split_schema() {
        init_loader_tracing_test();
        let dir = Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return; // Skip if not generated yet
        }
        let cgs = load_schema_dir(dir).unwrap();
        assert!(!cgs.entities.is_empty());
        assert!(!cgs.capabilities.is_empty());
        assert!(cgs.get_entity("Pet").is_some());
    }

    #[test]
    fn test_load_flow_matrix_split_schema_preserves_flow_annotations() {
        init_loader_tracing_test();
        let dir = Path::new("../../fixtures/schemas/flow_matrix");
        if !dir.join("domain.yaml").is_file() {
            return;
        }
        let cgs = load_schema_dir(dir).expect("flow_matrix");
        assert!(cgs
            .data_classes
            .contains_key(&crate::DataClassName::new("untrusted").expect("untrusted")));
        let body = cgs
            .field_data_class("Message", "body")
            .expect("body data_class");
        assert_eq!(body.as_str(), "untrusted");
        let sanitize = cgs
            .capabilities
            .get(&crate::CapabilityName::from("sanitize_body"))
            .expect("sanitize_body");
        assert_eq!(sanitize.sanitizes.len(), 1);
        assert_eq!(sanitize.sanitizes[0].as_str(), "untrusted");
        let send = cgs
            .capabilities
            .get(&crate::CapabilityName::from("send"))
            .expect("send");
        let sinks = cgs.capability_sink_params(send);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].name, "body");
        assert_eq!(
            sinks[0].sink_class.as_ref().map(|s| s.as_str()),
            Some("outbound_body")
        );
    }

    #[test]
    fn load_schema_dir_resolves_overshow_tool_typo_to_overshow_tools() {
        init_loader_tracing_test();
        let typo = Path::new("../../fixtures/schemas/overshow_tool");
        assert!(
            !typo.join("domain.yaml").is_file(),
            "typo path should not carry domain.yaml so sibling resolution is exercised"
        );
        let canonical = Path::new("../../fixtures/schemas/overshow_tools");
        if !canonical.join("domain.yaml").is_file() {
            return;
        }
        let cgs = load_schema_dir(typo).expect("resolves to sibling overshow_tools");
        assert!(cgs.get_entity("CaptureItem").is_some());
    }

    #[test]
    fn test_load_cgs_yaml_fallback() {
        let path = Path::new("../../fixtures/schemas/test_schema.cgs.yaml");
        if !path.exists() {
            return;
        }
        let Ok(cgs) = load_schema(path) else {
            return;
        };
        assert!(!cgs.entities.is_empty());
        let Some(blob) = cgs.get_entity("BlobAsset") else {
            return;
        };
        let Some(payload) = blob.fields.get("payload") else {
            return;
        };
        let Ok(payload_nv) = cgs.named_value_for_slot(payload) else {
            return;
        };
        assert!(matches!(payload_nv.field_type, crate::FieldType::Blob));
        if let Some(hint) = payload.mime_type_hint.as_deref() {
            assert_eq!(hint, "application/octet-stream");
        }
        if payload.attachment_media.is_some() {
            assert_eq!(
                payload.attachment_media,
                Some(crate::schema::AttachmentMediaKind::Generic)
            );
        }
        let icon = blob.fields.get("icon_png").expect("icon_png field");
        assert_eq!(icon.mime_type_hint.as_deref(), Some("image/png"));
        assert_eq!(
            icon.attachment_media,
            Some(crate::schema::AttachmentMediaKind::Image)
        );
    }

    #[test]
    fn test_entity_ref_yaml_and_reverse_caps() {
        init_loader_tracing_test();
        let dir = Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        assert!(cgs.get_entity("Space").is_some());
        let caps = cgs.find_reverse_traversal_caps("Team");
        assert!(
            caps.iter()
                .any(|(c, p)| c.name == "space_query" && *p == "team_id"),
            "expected space_query.team_id: {:?}",
            caps
        );
    }

    #[test]
    fn load_schema_dir_rejects_relation_unknown_target_entity() {
        let dir = Path::new("../../fixtures/schemas/relation_unknown_target_test");
        if !dir.join("domain.yaml").is_file() {
            return;
        }
        let err = load_schema_dir(dir).expect_err("broken relation target should fail validate");
        assert!(
            err.contains("MissingEntity") && err.contains("peer"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_evm_erc20_fixture() {
        let dir = Path::new("../../fixtures/schemas/evm_erc20");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        assert!(cgs.get_entity("Balance").is_some());
        assert!(cgs.get_entity("Transfer").is_some());
        assert!(cgs.get_capability("balance_get").is_some());
        assert!(cgs.get_capability("transfer_query").is_some());
    }

    /// Smoke: standard split schemas under `apis/` that load with the current domain YAML shape.
    #[test]
    fn test_apis_split_schemas_smoke() {
        init_loader_tracing_test();
        const NAMES: &[&str] = &[
            "clickup",
            "dnd5e",
            "evm-erc20",
            "github",
            "gitlab",
            "gmail",
            "google-calendar",
            "google-sheets",
            "graphqlzero",
            "jira",
            "linear",
            "musixmatch",
            "notion",
            "nytimes",
            "omdb",
            "openbrewerydb",
            "openmeteo",
            "pokeapi",
            "rawg",
            "rickandmorty",
            "slack",
            "spotify",
            "tau2_retail",
            "tavily",
            "themealdb",
            "xkcd",
        ];
        let root = Path::new("../../apis");
        if !root.is_dir() {
            return;
        }
        for name in NAMES {
            let dir = root.join(name);
            if !dir.join("domain.yaml").exists() || !dir.join("mappings.yaml").exists() {
                continue;
            }
            let cgs = load_schema_dir(&dir).unwrap_or_else(|e| panic!("load apis/{name}: {e}"));
            cgs.validate()
                .unwrap_or_else(|e| panic!("validate apis/{name}: {e}"));
        }
        let pet_dir = Path::new("../../fixtures/schemas/petstore");
        if pet_dir.join("domain.yaml").exists() && pet_dir.join("mappings.yaml").exists() {
            let cgs =
                load_schema_dir(pet_dir).unwrap_or_else(|e| panic!("load fixtures/petstore: {e}"));
            cgs.validate()
                .unwrap_or_else(|e| panic!("validate fixtures/petstore: {e}"));
        }
        let poke_mini_dir = Path::new("../../fixtures/schemas/pokeapi_mini");
        if poke_mini_dir.join("domain.yaml").exists()
            && poke_mini_dir.join("mappings.yaml").exists()
        {
            let cgs = load_schema_dir(poke_mini_dir)
                .unwrap_or_else(|e| panic!("load fixtures/pokeapi_mini: {e}"));
            cgs.validate()
                .unwrap_or_else(|e| panic!("validate fixtures/pokeapi_mini: {e}"));
        }
    }

    /// Pack embeds CGS via `serde_yaml`; must round-trip (same as `plasm-pack-catalogs`).
    #[test]
    fn test_cgs_serde_yaml_roundtrip_smoke() {
        use crate::schema::CGS;
        const NAMES: &[&str] = &[
            "clickup",
            "dnd5e",
            "evm-erc20",
            "github",
            "gitlab",
            "gmail",
            "google-calendar",
            "google-sheets",
            "graphqlzero",
            "jira",
            "linear",
            "musixmatch",
            "notion",
            "nytimes",
            "omdb",
            "openbrewerydb",
            "openmeteo",
            "pokeapi",
            "rawg",
            "rickandmorty",
            "slack",
            "spotify",
            "tau2_retail",
            "tavily",
            "themealdb",
            "xkcd",
        ];
        let root = Path::new("../../apis");
        if !root.is_dir() {
            return;
        }
        for name in NAMES {
            let dir = root.join(name);
            if !dir.join("domain.yaml").exists() || !dir.join("mappings.yaml").exists() {
                continue;
            }
            let cgs = load_schema_dir(&dir).unwrap_or_else(|e| panic!("load apis/{name}: {e}"));
            let yaml = serde_yaml::to_string(&cgs).expect("serde_yaml::to_string");
            let _: CGS = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("serde_yaml round-trip apis/{name}: {e}\n---\n{yaml}"));
        }
        let pet_dir = Path::new("../../fixtures/schemas/petstore");
        if pet_dir.join("domain.yaml").exists() && pet_dir.join("mappings.yaml").exists() {
            let cgs =
                load_schema_dir(pet_dir).unwrap_or_else(|e| panic!("load fixtures/petstore: {e}"));
            let yaml = serde_yaml::to_string(&cgs).expect("serde_yaml::to_string");
            let _: CGS = serde_yaml::from_str(&yaml).unwrap_or_else(|e| {
                panic!("serde_yaml round-trip fixtures/petstore: {e}\n---\n{yaml}")
            });
        }
        let poke_mini_dir = Path::new("../../fixtures/schemas/pokeapi_mini");
        if poke_mini_dir.join("domain.yaml").exists()
            && poke_mini_dir.join("mappings.yaml").exists()
        {
            let cgs = load_schema_dir(poke_mini_dir)
                .unwrap_or_else(|e| panic!("load fixtures/pokeapi_mini: {e}"));
            let yaml = serde_yaml::to_string(&cgs).expect("serde_yaml::to_string");
            let _: CGS = serde_yaml::from_str(&yaml).unwrap_or_else(|e| {
                panic!("serde_yaml round-trip fixtures/pokeapi_mini: {e}\n---\n{yaml}")
            });
        }
    }

    #[test]
    fn rejects_capability_array_param_without_items() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id_str:
    type: string
  nv_x_bad:
    type: array
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id_str
        required: true
capabilities:
  q:
    kind: query
    entity: E
    selection:
      - name: x
        value_ref: nv_x_bad
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("requires") && err.contains("items"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn accepts_bare_string_without_string_semantics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_body:
    type: string
entities:
  Widget:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      body:
        value_ref: nv_body
        required: false
capabilities:
  q:
    kind: query
    entity: Widget
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        load_schema_dir(dir.path()).expect("bare string ok");
    }

    #[test]
    fn rejects_entity_array_field_without_items() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_tags_bad:
    type: array
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      tags:
        value_ref: nv_tags_bad
        required: false
capabilities: {}
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "{}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("requires") && err.contains("items"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_multi_enum_with_empty_enum_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_ms_bad:
    type: multi_enum
    enum: []
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  q:
    kind: query
    entity: E
    selection:
      - name: s
        value_ref: nv_ms_bad
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("multi_enum") && (err.contains("enum") || err.contains("non-empty")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_items_block_on_non_array_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id_bad:
    type: string
    items:
      value_ref: nv_inner
  nv_inner:
    type: string
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id_bad
        required: true
capabilities: {}
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "{}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("only valid when type is 'array'") || err.contains("items"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn loads_minimal_array_param_with_string_items() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_x_elem:
    type: string
  nv_x:
    type: array
    items:
      value_ref: nv_x_elem
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  q:
    kind: query
    entity: E
    selection:
      - name: x
        value_ref: nv_x
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        load_schema_dir(dir.path()).unwrap();
    }

    #[test]
    fn loads_structurally_disjoint_query_input_lanes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_filter_q:
    type: string
  nv_page:
    type: integer
entities:
  Widget:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  q:
    kind: query
    entity: Widget
    selection:
      - name: filter_q
        value_ref: nv_filter_q
        required: true
    controls:
      - name: page
        value_ref: nv_page
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let cgs = load_schema_dir(dir.path()).unwrap();
        let cap = cgs.get_capability("q").expect("cap q");
        assert_eq!(cap.selection_params()[0].name, "filter_q");
        assert_eq!(cap.control_params()[0].name, "page");
        assert!(cap.scope_params().is_empty());
    }

    #[test]
    fn enum_map_authoring_loads_token_glosses_for_teaching() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_status:
    type: enum
    enum:
      pending: awaiting settlement
      approved: fully settled
      denied: refused end-to-end
entities:
  Widget:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
      status:
        value_ref: nv_status
        required: false
capabilities:
  q:
    kind: query
    entity: Widget
    selection:
      - name: status
        value_ref: nv_status
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let cgs = load_schema_dir(dir.path()).unwrap();
        let nv = cgs.values.get("nv_status").expect("nv_status");
        let membership = nv.domain.enum_membership.as_ref().expect("membership");
        assert_eq!(
            membership.tokens(),
            &[
                "pending".to_string(),
                "approved".to_string(),
                "denied".to_string()
            ][..]
        );
        let glosses = membership.glosses().expect("glosses");
        assert_eq!(
            glosses.get("pending").map(String::as_str),
            Some("awaiting settlement")
        );
        let meta = crate::symbol_tuning::IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: crate::identity::EntityName::from("Widget".to_string()),
            role: crate::symbol_tuning::IdentRegistryRole::EntityField,
            value_registry_key: crate::schema::ValueDomainKey::new("nv_status").expect("key"),
            field_type: crate::FieldType::Select,
            profile: None,
            array_items: None,
            allowed_values: nv.allowed_values.clone(),
            wire_name: "status".into(),
            description: String::new(),
        };
        let meaning = meta
            .render_value_domain_row_gloss("", None, Some(&cgs))
            .expect("meaning");
        assert_eq!(
            meaning,
            "enum · pending: awaiting settlement; approved: fully settled; denied: refused end-to-end"
        );
    }

    #[test]
    fn rejects_enum_gloss_with_reserved_delimiter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_status:
    type: enum
    enum:
      pending: awaiting settlement; not yet
entities:
  Widget:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  q:
    kind: query
    entity: Widget
    selection:
      - name: id
        value_ref: nv_id
        required: true
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("must not contain ';'"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn rejects_duplicate_field_across_structural_lanes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_overlap_str:
    type: string
entities:
  Widget:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  q:
    kind: query
    entity: Widget
    selection:
      - name: overlap
        value_ref: nv_overlap_str
        required: true
    controls:
      - name: overlap
        value_ref: nv_overlap_str
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("overlap") && err.contains("selection") && err.contains("controls"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_removed_execution_context_lane() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
entities:
  Session:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
  Widget:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  session_get:
    kind: get
    entity: Session
  widget_get:
    kind: get
    entity: Widget
  q:
    kind: query
    entity: Widget
    execution:
      context:
        entity: Session
        bindings:
          session_id: id
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("mappings.yaml"),
            "session_get: {}\nwidget_get: {}\nq:\n  query:\n    session:\n      type: var\n      name: session_id\n",
        )
        .unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("execution") || err.contains("unknown field"),
            "expected unknown-field reject for execution:; got: {err}"
        );
    }

    #[test]
    fn rejects_legacy_parameter_role() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  q:
    kind: query
    entity: E
    selection:
      - name: id
        value_ref: nv_id
        role: filter
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(err.contains("role"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_side_effect_with_empty_description() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  do_thing:
    description: "Does something"
    kind: action
    entity: E
    output:
      type: side_effect
      description: "   "
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "do_thing: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("side_effect") && err.contains("description"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_input_validation_predicates_in_domain_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_rev:
    type: number
entities:
  E:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  upd:
    kind: update
    entity: E
    payload:
      input_type:
        type: object
        additional_fields: false
        fields:
          - name: revenue
            value_ref: nv_rev
            required: false
      validation:
        predicates:
          - field_path: revenue
            operator: min_value
            value: 0
            error_message: Revenue must be non-negative
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "upd: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("validation.predicates") && err.contains("values:"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn all_apis_packages_validate() {
        let apis = std::path::Path::new("../../apis");
        if !apis.is_dir() {
            return;
        }
        for entry in std::fs::read_dir(apis).expect("read apis dir") {
            let entry = entry.expect("apis entry");
            if !entry.file_type().expect("file type").is_dir() {
                continue;
            }
            let dir = entry.path();
            if !dir.join("domain.yaml").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            load_schema_dir(&dir).unwrap_or_else(|e| {
                panic!("apis/{name} failed CGS validation: {e}");
            });
        }
    }
}
