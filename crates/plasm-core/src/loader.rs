//! CGS (Capability Graph Schema) load path: `domain.yaml` + `mappings.yaml`, combined YAML, or CGS interchange.
//!
//! **Tracing:** set `RUST_LOG=plasm_core::loader=trace` (or `=debug`) and install a `tracing-subscriber`
//! (e.g. `plasm-eval` / `dump_prompt` binaries do this). Phases logged: file read, `serde_yaml` parse,
//! [`assemble_cgs`], [`CGS::validate`].
//! For CML template parsing after load: `plasm_compile::transport=trace`.

use crate::identity::{CapabilityName, EntityFieldName, EntityName, RelationName};
use crate::schema::{FieldValueKind, NamedValueSchema, ValueDomainKey, ViewDefinition};
use crate::{
    capability_template_all_var_names, AgentPresentation, ArrayItemsSchema, AttachmentMediaKind,
    AuthScheme, CapabilityKind, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson,
    Cardinality, FieldDeriveRule, FieldSchema, FieldType, InputFieldSchema, InputSchema, InputType,
    InputValidation, OauthExtension, ParameterRole, RelationSchema, ResourceSchema,
    ScopeAggregateKeyPolicy, StringSemantics, ValueWireFormat, CGS,
};
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, trace, warn};

/// Hard cap for `domain.yaml` / `mappings.yaml` / combined CGS YAML (defense in depth).
const MAX_SCHEMA_FILE_BYTES: u64 = 50 * 1024 * 1024;

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
    /// When false, DOMAIN omits projection bracket exemplars (default: true).
    #[serde(default = "default_domain_projection_examples")]
    pub domain_projection_examples: bool,
    /// Optional Get capability id for projection exemplar field order (`provides` / default order).
    #[serde(default)]
    pub primary_read: Option<String>,
    #[serde(default)]
    pub discovery: Option<crate::DiscoveryEntityHints>,
}

fn default_domain_projection_examples() -> bool {
    true
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
    #[serde(default)]
    pub allowed_values: Option<Vec<String>>,
    #[serde(default)]
    pub value_format: Option<ValueWireFormat>,
    #[serde(default)]
    pub items: Option<DomainItems>,
    #[serde(default)]
    pub string_semantics: Option<StringSemantics>,
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
    #[serde(default)]
    pub discovery: Option<crate::DiscoveryRelationHints>,
}

#[derive(Debug, Deserialize)]
pub struct DomainCapability {
    #[serde(default)]
    pub description: String,
    pub kind: String,
    pub entity: String,
    /// Policy for compound `entity_ref` scope parameters after runtime splat (`retain` default).
    #[serde(default)]
    pub scope_aggregate_key_policy: Option<ScopeAggregateKeyPolicy>,
    #[serde(default)]
    pub parameters: Option<Vec<DomainParameter>>,
    /// Entity fields this capability populates in its response.
    /// When absent, defaults are applied by `CGS::effective_provides` (same ordered field list as
    /// DOMAIN exemplars when `provides` is empty: `id_field` first, then lexicographic rest).
    #[serde(default)]
    pub provides: Vec<String>,
    /// Declared response shape for validation (required for `action` unless `provides` is set).
    #[serde(default)]
    pub output: Option<crate::OutputSchema>,
    /// Optional structured input beyond `parameters` (same shape as CGS [`InputSchema`]).
    ///
    /// **Merge when `parameters` is also set:** `input_schema.input_type` must be [`InputType::Object`].
    /// Field order is **parameter-derived fields first** (stable HTTP-ish order from `parameters:`),
    /// then **`input_schema.input_type.fields`** (body-only / extra slots). `additional_fields` on
    /// that object is carried into the merged schema; `validation` / `description` / `examples` on
    /// this block apply to the merged [`InputSchema`]. A parameter `name` that also appears in
    /// `input_schema` object `fields` is a **load error** (no silent override).
    #[serde(default)]
    pub input_schema: Option<InputSchema>,
    #[serde(default)]
    pub invoke_preflight: Option<crate::InvokePreflight>,
    #[serde(default)]
    pub discovery: Option<crate::DiscoveryCapabilityHints>,
}

#[derive(Debug, Deserialize)]
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
    /// Semantic role of this parameter. One of:
    /// `filter` (default), `search`, `sort`, `sort_direction`, `response_control`, `scope`.
    #[serde(default)]
    pub role: Option<String>,
    /// Human-readable hint for prompts; DOMAIN gloss uses `type · description`, else `type · name`.
    #[serde(default)]
    pub description: String,
}

/// YAML `items:` block for `array` fields and parameters.
#[derive(Debug, Deserialize)]
pub struct DomainItems {
    /// Element shape lives under `values:` (same as `value_ref` on fields).
    pub value_ref: String,
}

/// Load a CGS from split domain.yaml + mappings.yaml files.
pub fn load_split_schema(domain_path: &Path, mappings_path: &Path) -> Result<CGS, String> {
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
    let cgs = assemble_cgs_core(domain, mappings)?;
    finalize_cgs_load(&cgs)?;

    info!(
        elapsed_ms = t0.elapsed().as_millis() as u64,
        entities = cgs.entities.len(),
        capabilities = cgs.capabilities.len(),
        "load_split_schema finished"
    );
    Ok(cgs)
}

fn finalize_cgs_load(cgs: &CGS) -> Result<(), String> {
    debug!(
        entities = cgs.entities.len(),
        capabilities = cgs.capabilities.len(),
        "assemble_cgs: calling CGS::validate"
    );
    cgs.validate()
        .map_err(|e| format!("CGS validation failed: {}", e))?;

    warn_scope_aggregate_policy_template_mismatches(cgs);

    let sem_violations = cgs.string_semantics_violations();
    if !sem_violations.is_empty() {
        for msg in &sem_violations {
            error!(target: "plasm_core::cgs", "{}", msg);
        }
        return Err(format!(
            "CGS load requires string_semantics on every string field and string capability parameter ({} issue(s); first: {})",
            sem_violations.len(),
            sem_violations[0]
        ));
    }

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
        if let Ok(cgs) = serde_yaml::from_str::<CGS>(&content) {
            debug!("CGS interchange parse ok; validating");
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
    if path.extension().is_some_and(|e| e == "json") {
        Err(format!(
            "CGS JSON is no longer supported ({}). Use a directory with domain.yaml + mappings.yaml, or a .cgs.yaml / .yaml CGS file.",
            path.display()
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
    let vt = d.value_type.trim();
    if vt.is_empty() {
        return Err(format!("{ctx}: missing `type`"));
    }
    let field_type = parse_domain_field_type(vt, &d.target, ctx)?;
    if matches!(field_type, FieldType::MultiSelect)
        && d.allowed_values.as_ref().is_none_or(|v| v.is_empty())
    {
        return Err(format!(
            "{ctx}: type 'multi_select' requires non-empty allowed_values"
        ));
    }
    let array_items = if matches!(field_type, FieldType::Array) {
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
    let (field_type, string_semantics) = normalize_blob_field_type(field_type, d.string_semantics);
    Ok(NamedValueSchema {
        description: d.description.clone(),
        field_type,
        value_format: d.value_format,
        allowed_values: d.allowed_values.clone(),
        string_semantics,
        array_items,
    })
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
        attachment_media: f.attachment_media,
        wire_path: f.path.clone(),
        derive: f.derive.clone(),
    })
}

fn input_field_schema_from_domain_parameter(
    cap_name: &str,
    p: &DomainParameter,
    values: &IndexMap<String, NamedValueSchema>,
) -> Result<InputFieldSchema, String> {
    let ctx = format!("capability '{cap_name}', parameter '{}'", p.name);
    let vr = p.value_ref.trim();
    let role = p.role.as_deref().map(parse_parameter_role);
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
                role,
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
            role,
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

/// Combine `parameters:` rows with an optional explicit `input_schema:` from domain YAML.
///
/// **Ordering:** all parameter-derived fields first, then explicit object fields.
/// **Duplicates:** same `name` in both sources → [`Err`].
/// **Metadata:** when merging, `validation` / `description` / `examples` come from `input_schema`.
fn merge_domain_capability_input_schema(
    cap_name: &str,
    parameters: Option<&Vec<DomainParameter>>,
    explicit: Option<&InputSchema>,
    values: &IndexMap<String, NamedValueSchema>,
) -> Result<Option<InputSchema>, String> {
    let param_fields = parameters
        .map(|ps| input_fields_from_domain_parameters(cap_name, ps, values))
        .transpose()?;

    Ok(match (param_fields, explicit) {
        (None, None) => None,
        (Some(fields), None) => Some(InputSchema {
            input_type: InputType::Object {
                fields,
                additional_fields: true,
            },
            validation: InputValidation::default(),
            description: None,
            examples: vec![],
        }),
        (None, Some(schema)) => Some(schema.clone()),
        (Some(mut fields), Some(explicit_schema)) => {
            let InputType::Object {
                fields: extra_fields,
                additional_fields,
            } = &explicit_schema.input_type
            else {
                return Err(format!(
                    "capability '{cap_name}': when both `parameters` and `input_schema` are set, `input_schema.input_type` must be `type: object` (cannot merge with non-object input_type)"
                ));
            };
            let names: HashSet<&str> = fields.iter().map(|f| f.name.as_str()).collect();
            for ef in extra_fields {
                if names.contains(ef.name.as_str()) {
                    return Err(format!(
                        "capability '{cap_name}': input field '{}' is declared in both `parameters` and `input_schema.input_type.fields`",
                        ef.name
                    ));
                }
            }
            fields.extend(extra_fields.iter().cloned());
            Some(InputSchema {
                input_type: InputType::Object {
                    fields,
                    additional_fields: *additional_fields,
                },
                validation: explicit_schema.validation.clone(),
                description: explicit_schema.description.clone(),
                examples: explicit_schema.examples.clone(),
            })
        }
    })
}

fn assemble_cgs_core(
    domain: DomainFile,
    mappings: IndexMap<String, serde_json::Value>,
) -> Result<CGS, String> {
    let span = crate::spans::schema_assemble(domain.entities.len(), domain.capabilities.len());
    let _g = span.enter();
    trace!("assemble_cgs: building entity resources");

    let mut cgs = CGS::new();
    cgs.http_backend = domain.http_backend;
    cgs.auth = domain.auth;
    cgs.oauth = domain.oauth;
    cgs.version = domain.version;
    cgs.values = compile_domain_named_values(&domain.values)?;

    for (name, entity) in &domain.entities {
        validate_compound_entity_identity(name, entity)?;

        let fields: Vec<FieldSchema> = entity
            .fields
            .iter()
            .map(|(fname, f)| field_schema_from_domain_field(fname, name, f, &cgs.values))
            .collect::<Result<Vec<_>, String>>()?;

        let relations: Vec<RelationSchema> = entity
            .relations
            .iter()
            .map(|(rname, r)| RelationSchema {
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

        let template = mappings
            .get(cap_name)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Capability '{cap_name}' is listed in domain.yaml but has no entry in mappings.yaml"
                )
            })?;

        let input_schema = merge_domain_capability_input_schema(
            cap_name,
            cap.parameters.as_ref(),
            cap.input_schema.as_ref(),
            &cgs.values,
        )?;

        let capability = CapabilitySchema {
            name: CapabilityName::from(cap_name.clone()),
            description: cap.description.clone(),
            kind,
            domain: EntityName::from(cap.entity.clone()),
            mapping: CapabilityMapping {
                template: CapabilityTemplateJson(template),
            },
            input_schema,
            output_schema: cap.output.clone(),
            provides: cap.provides.clone(),
            scope_aggregate_key_policy: cap.scope_aggregate_key_policy.unwrap_or_default(),
            invoke_preflight: cap.invoke_preflight.clone(),
            discovery: cap.discovery.clone(),
        };

        cgs.add_capability(capability)
            .map_err(|e| format!("Failed to add capability '{}': {}", cap_name, e))?;
    }

    cgs.views = domain.views.clone();

    Ok(cgs)
}

fn assemble_cgs(
    domain: DomainFile,
    mappings: IndexMap<String, serde_json::Value>,
) -> Result<CGS, String> {
    let cgs = assemble_cgs_core(domain, mappings)?;
    finalize_cgs_load(&cgs)?;
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

fn parse_field_type_strict(s: &str, ctx: &str) -> Result<FieldType, String> {
    let t = s.trim();
    match t {
        "uuid" => Ok(FieldType::Uuid),
        "string" => Ok(FieldType::String),
        "blob" => Ok(FieldType::Blob),
        "number" | "float" => Ok(FieldType::Number),
        "integer" | "int" => Ok(FieldType::Integer),
        "boolean" | "bool" => Ok(FieldType::Boolean),
        "select" | "enum" => Ok(FieldType::Select),
        "multi_select" => Ok(FieldType::MultiSelect),
        "date" | "datetime" => Ok(FieldType::Date),
        "array" => Ok(FieldType::Array),
        "json" => Ok(FieldType::Json),
        "" => Err(format!("{ctx}: empty field type")),
        _ => Err(format!("{ctx}: unknown field type {t:?}")),
    }
}

/// `string` + `string_semantics: blob` is normalized to [`FieldType::Blob`] (clear semantics).
fn normalize_blob_field_type(
    field_type: FieldType,
    string_semantics: Option<StringSemantics>,
) -> (FieldType, Option<StringSemantics>) {
    match (field_type, string_semantics) {
        (FieldType::String, Some(StringSemantics::Blob)) => (FieldType::Blob, None),
        (ft, sem) => (ft, sem),
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
        let Some(schema) = cap.input_schema.as_ref() else {
            continue;
        };
        let InputType::Object { fields, .. } = &schema.input_type else {
            continue;
        };
        for param in fields {
            if !matches!(param.role, Some(ParameterRole::Scope)) {
                continue;
            }
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

fn parse_domain_field_type(
    field_type: &str,
    target: &Option<String>,
    context: &str,
) -> Result<FieldType, String> {
    if field_type == "entity_ref" {
        let t = target
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!(
                    "{}: field_type 'entity_ref' requires non-empty 'target'",
                    context
                )
            })?;
        Ok(FieldType::EntityRef {
            target: EntityName::from(t.to_string()),
        })
    } else {
        parse_field_type_strict(field_type, context)
    }
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

fn parse_parameter_role(s: &str) -> ParameterRole {
    match s {
        "search" => ParameterRole::Search,
        "sort" => ParameterRole::Sort,
        "sort_direction" => ParameterRole::SortDirection,
        "response_control" => ParameterRole::ResponseControl,
        "scope" => ParameterRole::Scope,
        _ => ParameterRole::Filter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

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
    fn parse_field_type_uuid() {
        assert_eq!(
            parse_field_type_strict("uuid", "ctx").unwrap(),
            FieldType::Uuid
        );
    }

    #[test]
    fn parse_field_type_blob() {
        assert_eq!(
            parse_field_type_strict("blob", "ctx").unwrap(),
            FieldType::Blob
        );
    }

    #[test]
    fn normalize_string_blob_semantics_to_blob_type() {
        let (ft, sem) = normalize_blob_field_type(FieldType::String, Some(StringSemantics::Blob));
        assert_eq!(ft, FieldType::Blob);
        assert!(sem.is_none());
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
        let cgs = load_schema(path).unwrap();
        assert!(!cgs.entities.is_empty());
        let blob = cgs.get_entity("BlobAsset").expect("BlobAsset entity");
        let payload = blob.fields.get("payload").expect("payload field");
        let payload_nv = cgs
            .named_value_for_slot(payload)
            .expect("payload value_ref");
        assert!(matches!(payload_nv.field_type, crate::FieldType::Blob));
        assert_eq!(
            payload.mime_type_hint.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(
            payload.attachment_media,
            Some(crate::schema::AttachmentMediaKind::Generic)
        );
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

    /// Pack embeds CGS via `serde_yaml`; must round-trip (same as `plasm-pack-plugins`).
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
    string_semantics: short
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
    parameters:
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
    fn rejects_domain_when_string_field_omits_string_semantics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
    string_semantics: short
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
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("string_semantics") && err.contains("Widget") && err.contains("body"),
            "unexpected error: {err}"
        );
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
    string_semantics: short
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
    fn rejects_multi_select_with_empty_allowed_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
    string_semantics: short
  nv_ms_bad:
    type: multi_select
    allowed_values: []
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
    parameters:
      - name: s
        value_ref: nv_ms_bad
        required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("non-empty allowed_values"),
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
    string_semantics: short
    items:
      value_ref: nv_inner
  nv_inner:
    type: string
    string_semantics: short
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
    string_semantics: short
  nv_x_elem:
    type: string
    string_semantics: short
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
    parameters:
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
    fn merges_parameters_with_input_schema_object_fields_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
    string_semantics: short
  nv_filter_q:
    type: string
    string_semantics: short
  nv_body_extra:
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
    parameters:
      - name: filter_q
        value_ref: nv_filter_q
        required: true
    input_schema:
      input_type:
        type: object
        additional_fields: false
        fields:
          - name: body_extra
            value_ref: nv_body_extra
            field_type: integer
            required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let cgs = load_schema_dir(dir.path()).unwrap();
        let cap = cgs.get_capability("q").expect("cap q");
        let fields = cap.object_params().expect("object params");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "filter_q");
        assert_eq!(fields[1].name, "body_extra");
        let InputType::Object {
            additional_fields, ..
        } = &cap.input_schema.as_ref().expect("input").input_type
        else {
            panic!("expected object input");
        };
        assert!(!additional_fields);
    }

    #[test]
    fn rejects_duplicate_field_in_parameters_and_input_schema() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain.yaml"),
            r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
    string_semantics: short
  nv_overlap_str:
    type: string
    string_semantics: short
  nv_overlap_int:
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
    parameters:
      - name: overlap
        value_ref: nv_overlap_str
        required: true
    input_schema:
      input_type:
        type: object
        additional_fields: true
        fields:
          - name: overlap
            value_ref: nv_overlap_int
            field_type: integer
            required: false
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("mappings.yaml"), "q: {}\n").unwrap();
        let err = load_schema_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("overlap") && err.contains("parameters") && err.contains("input_schema"),
            "unexpected error: {err}"
        );
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
    string_semantics: short
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
}
