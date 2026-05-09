//! Explorer / operator projection: same semantics as dynamic CLI (`cli_builder`) plus
//! [`render_domain_prompt_bundle`](plasm_core::prompt_render::render_domain_prompt_bundle) for DOMAIN metadata.

use plasm_compile::{
    pagination_config_for_capability, parse_capability_template, path_var_names_from_request,
    template_var_names, CapabilityTemplate, PaginationConfig, PaginationLocation, PaginationParam,
};
use plasm_core::discovery::CatalogEntryMeta;
use plasm_core::prompt_render::{
    render_domain_bundle, DomainLineKind, DomainLineMeta, DomainPromptModel, DomainPromptSettings,
    DomainPromptSource,
};
use plasm_core::schema::{
    input_variant_body_type, AuthScheme, EntityDef, FieldSchema, InputFieldSchema, InputFieldWire,
    InputType, OauthExtension, OutputType, RelationMaterialization, RelationSchema,
    StringSemantics, CGS,
};
use plasm_core::symbol_tuning::FocusSpec;
use plasm_core::{capability_method_label_kebab, CapabilityKind, CapabilitySchema, FieldType};
use plasm_core::{catalog_connect_profile, CatalogConnectProfile};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

use crate::subcommand_util::{
    field_subcommand_kebab, path_param_long_flag, pluralize_entity, relation_subcommand_kebab,
};

mod entity_param {
    use serde::{Deserialize, Deserializer};

    /// Axum's query parser may supply one `entity=` as a scalar or repeated keys as a sequence.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }

        match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(s) => Ok(vec![s]),
            OneOrMany::Many(v) => Ok(v),
        }
    }
}

/// Query string for `GET /v1/registry/:entry_id/tool-model`.
#[derive(Debug, serde::Deserialize)]
pub struct ToolModelQuery {
    /// `all` | `single` | `seeds`
    #[serde(default = "default_focus")]
    pub focus: String,
    /// Repeated `entity=` (one for `single`, one or more for `seeds`).
    #[serde(default, deserialize_with = "entity_param::deserialize")]
    pub entity: Vec<String>,
}

fn default_focus() -> String {
    "all".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolModelFocusMode {
    All,
    Single,
    Seeds,
}

impl ToolModelFocusMode {
    fn parse(raw: &str) -> Result<Self, ToolModelBuildError> {
        if raw.eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }
        if raw.eq_ignore_ascii_case("single") {
            return Ok(Self::Single);
        }
        if raw.eq_ignore_ascii_case("seeds") {
            return Ok(Self::Seeds);
        }
        Err(ToolModelBuildError::BadRequest(
            "focus must be all, single, or seeds".into(),
        ))
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Single => "single",
            Self::Seeds => "seeds",
        }
    }
}

#[derive(Debug)]
pub enum ToolModelBuildError {
    BadRequest(String),
}

impl std::fmt::Display for ToolModelBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolModelBuildError::BadRequest(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for ToolModelBuildError {}

/// Focus echoed back with resolved entity names for the slice.
#[derive(Debug, Serialize)]
pub struct ToolModelFocusBlock {
    pub mode: String,
    pub resolved_entities: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ToolModelOverview {
    pub entity_count: usize,
    pub relation_edge_count: usize,
    pub verb_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ToolModelDomainBlock {
    pub model: DomainPromptModel,
}

#[derive(Debug, Serialize)]
pub struct ToolModelResponse {
    pub entry: CatalogEntryMeta,
    pub focus: ToolModelFocusBlock,
    pub overview: ToolModelOverview,
    pub auth: ToolModelAuthBlock,
    pub entities: Vec<ExplorerEntityProjection>,
    pub domain: ToolModelDomainBlock,
}

#[derive(Debug, Serialize)]
pub struct ToolModelAuthBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthScheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OauthExtension>,
    /// Typed connect-eligibility projection for control-plane policy.
    pub connect_profile: CatalogConnectProfile,
}

#[derive(Debug, Serialize)]
pub struct ExplorerProjectionField {
    pub name: String,
    pub type_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_entity: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ref_entity_navigable: bool,
    /// CGS field prose (`FieldSchema::description`); may be empty when YAML omits it.
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct ExplorerEntityProjection {
    pub name: String,
    pub description: String,
    /// GET / DOMAIN projection fields only ([`CGS::default_ordered_entity_field_names`] on `entity.fields` — not `relations`).
    pub projection: Vec<ExplorerProjectionField>,
    pub verbs: Vec<ExplorerVerb>,
    /// DOMAIN lines paired with REPL-style parameters (prompt-shaped, not CLI-shaped).
    pub capabilities: Vec<ExplorerCapabilityRow>,
    pub relations: Vec<ExplorerRelation>,
    pub reverse_traversals: Vec<ExplorerReverseTraversal>,
    pub entity_ref_links: Vec<ExplorerEntityRefLink>,
    pub domain_lines: Vec<DomainLineMeta>,
}

/// One DOMAIN prompt line plus parameters for the matching graph affordance (REPL / execute surface).
#[derive(Debug, Clone, Serialize)]
pub struct ExplorerCapabilityRow {
    pub expression: String,
    pub line_kind: String,
    /// CGS capability id when the DOMAIN line is bound to a single capability (from [`DomainLineMeta::source_capability`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_name: Option<String>,
    /// CGS `description:` on the bound capability when non-empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub parameters: Vec<ExplorerVerbArg>,
    pub returns: ExplorerReturn,
}

/// Declared capability output (from `output_schema`), with optional link target for CGS entities.
#[derive(Debug, Clone, Serialize)]
pub struct ExplorerReturn {
    /// `entity` | `collection` | `side_effect` | `status` | `custom` | `unspecified`
    pub kind: String,
    /// Short label for the tool UI (entity name, collection summary, etc.).
    pub label: String,
    /// Extra prose from CGS when present (e.g. `output.type: side_effect` description).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// When true, `entity` names a row in this catalog’s entity list (safe to deep-link in explorer).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub entity_navigable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExplorerVerbArg {
    /// Name in path / predicate / input (snake_case); not a CLI `--flag`.
    pub binding: String,
    /// `positional`, `path`, `key`, `template`, `predicate`, `input`, `pagination`, `summary`.
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// CGS-facing prose (field `description` when present).
    pub description: String,
    /// Short type summary for the UI (e.g. `→ PokemonColor`, `string`, `select […]`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub type_label: String,
    /// When set, explorer may link to this entity (e.g. `EntityRef` parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_entity: Option<String>,
    /// Optional CLI mirror for operators (`--long`); omitted when empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cli_flag: String,
}

#[derive(Debug, Serialize)]
pub struct ExplorerVerb {
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_name: Option<String>,
    pub about: String,
    pub has_pagination: bool,
    pub has_summary: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<ExplorerVerbArg>,
    pub returns: ExplorerReturn,
}

fn explorer_return_unspecified() -> ExplorerReturn {
    ExplorerReturn {
        kind: "unspecified".into(),
        label: "(not declared)".into(),
        description: String::new(),
        entity: None,
        entity_navigable: false,
    }
}

/// When YAML omits `output:`, infer the usual Plasm semantics: **Get** → one `domain` entity;
/// **Query** / **Search** → a list of `domain` rows. Other kinds stay [`explorer_return_unspecified`].
fn explorer_return_inferred_when_no_output_schema(
    cgs: &CGS,
    cap: &CapabilitySchema,
) -> ExplorerReturn {
    let domain = cap.domain.to_string();
    let navigable = cgs.entities.contains_key(domain.as_str());
    match cap.kind {
        CapabilityKind::Get => ExplorerReturn {
            kind: "entity".into(),
            label: domain.clone(),
            description: String::new(),
            entity: Some(domain),
            entity_navigable: navigable,
        },
        CapabilityKind::Query | CapabilityKind::Search => {
            let label = format!("{domain}[]");
            ExplorerReturn {
                kind: "collection".into(),
                label,
                description: String::new(),
                entity: Some(domain),
                entity_navigable: navigable,
            }
        }
        _ => explorer_return_unspecified(),
    }
}

fn explorer_return_for_cap(cgs: &CGS, cap: &CapabilitySchema) -> ExplorerReturn {
    match cap.output_schema.as_ref() {
        Some(os) => explorer_return_for_output_type(cgs, &os.output_type),
        None => explorer_return_inferred_when_no_output_schema(cgs, cap),
    }
}

fn explorer_return_for_output_type(cgs: &CGS, ot: &OutputType) -> ExplorerReturn {
    match ot {
        OutputType::SideEffect { description } => {
            let full = description.trim().to_string();
            let label = trim_tool_desc(description);
            let description = if full == label { String::new() } else { full };
            ExplorerReturn {
                kind: "side_effect".into(),
                label,
                description,
                entity: None,
                entity_navigable: false,
            }
        }
        OutputType::Entity { entity_type } => {
            let navigable = cgs.entities.contains_key(entity_type.as_str());
            ExplorerReturn {
                kind: "entity".into(),
                label: entity_type.clone(),
                description: String::new(),
                entity: Some(entity_type.clone()),
                entity_navigable: navigable,
            }
        }
        OutputType::Collection {
            entity_type,
            max_count,
        } => {
            let navigable = cgs.entities.contains_key(entity_type.as_str());
            let label = if let Some(m) = max_count {
                format!("{entity_type}[] (max {m})")
            } else {
                format!("{entity_type}[]")
            };
            ExplorerReturn {
                kind: "collection".into(),
                label,
                description: String::new(),
                entity: Some(entity_type.clone()),
                entity_navigable: navigable,
            }
        }
        OutputType::Status { .. } => ExplorerReturn {
            kind: "status".into(),
            label: "status".into(),
            description: String::new(),
            entity: None,
            entity_navigable: false,
        },
        OutputType::Custom { .. } => ExplorerReturn {
            kind: "custom".into(),
            label: "custom".into(),
            description: String::new(),
            entity: None,
            entity_navigable: false,
        },
    }
}

fn trim_tool_desc(s: &str) -> String {
    let t = s.trim();
    let mut it = t.chars();
    let short: String = it.by_ref().take(120).collect();
    if it.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

/// Prefer CGS `CapabilitySchema::description` for operator-facing copy; use `fallback` when empty.
fn capability_about_from_cgs(cap: &CapabilitySchema, fallback: impl Into<String>) -> String {
    let t = cap.description.trim();
    if t.is_empty() {
        fallback.into()
    } else {
        t.to_string()
    }
}

/// Tool Explorer / operator copy: LLM execute uses opaque `page(pg#)` continuations, not CLI flags.
fn append_llm_pagination_execute_note(about: String) -> String {
    const NOTE: &str = " For MCP `plasm`, additional list pages use namespaced `page(s0_pg#)` handles from tool results; HTTP execute uses plain `page(pg#)`. Not raw API pagination parameters.";
    if about.is_empty() {
        NOTE.trim_start().to_string()
    } else {
        format!("{about}{NOTE}")
    }
}

/// Compact list of allowed values for select/multiselect labels (pipe-separated, bracketed).
fn format_allowed_domain(values: &[String], max_visible: usize) -> String {
    if values.is_empty() {
        return "(no values)".into();
    }
    if values.len() <= max_visible {
        return values.join(" | ");
    }
    format!(
        "{} | … (+{})",
        values[..max_visible].join(" | "),
        values.len() - max_visible
    )
}

/// Returns `None` for plain short strings (default semantics).
fn string_subtype_keyword_from_semantics(sem: StringSemantics) -> Option<&'static str> {
    match sem {
        StringSemantics::Short => None,
        sem => sem.gloss_type_keyword(),
    }
}

fn type_label_from_parts(
    field_type: &FieldType,
    allowed_values: Option<&[String]>,
    string_semantics: StringSemantics,
    array_items: Option<&plasm_core::schema::ArrayItemsSchema>,
) -> String {
    match field_type {
        FieldType::EntityRef { target } => {
            format!("entity_ref → {target}")
        }
        FieldType::Select => {
            if let Some(av) = allowed_values {
                let inner = format_allowed_domain(av, 12);
                format!("select · one of [ {inner} ]")
            } else {
                "select".into()
            }
        }
        FieldType::MultiSelect => {
            if let Some(av) = allowed_values {
                let inner = format_allowed_domain(av, 12);
                format!("multi-select · any of [ {inner} ]")
            } else {
                "multi-select".into()
            }
        }
        FieldType::Boolean => "boolean".into(),
        FieldType::Number => "number · f64".into(),
        FieldType::Integer => "integer · i64".into(),
        FieldType::Uuid => "uuid".into(),
        FieldType::String => match string_subtype_keyword_from_semantics(string_semantics) {
            None => "string".into(),
            Some(kw) => format!("string · {kw}"),
        },
        FieldType::Blob => "blob · binary".into(),
        FieldType::Date => "date".into(),
        FieldType::Array => {
            if let Some(items) = array_items {
                format!("array[{}]", field_type_compact_label(&items.field_type))
            } else {
                "array".into()
            }
        }
        FieldType::Json => "json · object".into(),
    }
}

fn input_type_tool_label(ty: &InputType, cgs: &CGS) -> String {
    match ty {
        InputType::None => "none".into(),
        InputType::Value {
            field_type,
            allowed_values,
        } => type_label_from_parts(
            field_type,
            allowed_values.as_deref(),
            StringSemantics::Short,
            None,
        ),
        InputType::Object { .. } => "object".into(),
        InputType::Array { element_type, .. } => {
            format!("array[{}]", input_type_tool_label(element_type, cgs))
        }
        InputType::Union { variants } => {
            let labels: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
            format!("union · {}", labels.join(" | "))
        }
    }
}

fn ref_entity_for_input_type(cgs: &CGS, ty: &InputType) -> Option<String> {
    match ty {
        InputType::Value { field_type: ft, .. } => navigable_entity_ref_target(cgs, ft),
        InputType::Object { fields, .. } => fields
            .iter()
            .find_map(|f| ref_entity_for_input_field(cgs, f)),
        InputType::Array { element_type, .. } => {
            ref_entity_for_input_type(cgs, element_type.as_ref())
        }
        InputType::Union { variants } => variants
            .iter()
            .find_map(|v| ref_entity_for_input_type(cgs, &input_variant_body_type(v))),
        _ => None,
    }
}

fn input_field_type_label(field: &InputFieldSchema, cgs: &CGS) -> String {
    match &field.wire {
        InputFieldWire::Inline(ty) => input_type_tool_label(ty.as_ref(), cgs),
        InputFieldWire::Registry(_) => {
            let nv = field.named_value(cgs).expect("param value_ref");
            type_label_from_parts(
                &nv.field_type,
                nv.allowed_values.as_deref(),
                field.effective_string_semantics(cgs),
                field.resolved_array_items(cgs),
            )
        }
    }
}

fn navigable_entity_ref_target(cgs: &CGS, field_type: &FieldType) -> Option<String> {
    match field_type {
        FieldType::EntityRef { target } if cgs.entities.contains_key(target.as_str()) => {
            Some(target.to_string())
        }
        _ => None,
    }
}

fn ref_entity_for_input_field(cgs: &CGS, field: &InputFieldSchema) -> Option<String> {
    match &field.wire {
        InputFieldWire::Registry(_) => {
            let nv = field.named_value(cgs).ok()?;
            navigable_entity_ref_target(cgs, &nv.field_type)
        }
        InputFieldWire::Inline(ty) => ref_entity_for_input_type(cgs, ty.as_ref()),
    }
}

fn field_type_compact_label(ft: &FieldType) -> String {
    match ft {
        FieldType::Boolean => "boolean".into(),
        FieldType::Number => "number · f64".into(),
        FieldType::Integer => "integer · i64".into(),
        FieldType::Uuid => "uuid".into(),
        FieldType::String => "string".into(),
        FieldType::Blob => "blob".into(),
        FieldType::Select => "select".into(),
        FieldType::MultiSelect => "multi-select".into(),
        FieldType::Date => "date".into(),
        FieldType::Array => "array".into(),
        FieldType::Json => "json · object".into(),
        FieldType::EntityRef { target } => format!("entity_ref → {target}"),
    }
}

fn schema_field_type_label(field: &FieldSchema, cgs: &CGS) -> String {
    let nv = cgs
        .named_value_for_slot(field)
        .expect("entity field value_ref");
    type_label_from_parts(
        &nv.field_type,
        nv.allowed_values.as_deref(),
        field.effective_string_semantics(cgs),
        field.resolved_array_items(cgs),
    )
}

fn ref_entity_for_field_schema(cgs: &CGS, field: &FieldSchema) -> Option<String> {
    let nv = cgs.named_value_for_slot(field).ok()?;
    navigable_entity_ref_target(cgs, &nv.field_type)
}

fn build_projection_fields(cgs: &CGS, entity: &EntityDef) -> Vec<ExplorerProjectionField> {
    let mut out = Vec::new();
    for fname in CGS::default_ordered_entity_field_names(entity) {
        let Some(f) = entity.fields.get(fname.as_str()) else {
            continue;
        };
        let type_label = schema_field_type_label(f, cgs);
        let ref_entity = ref_entity_for_field_schema(cgs, f);
        let ref_entity_navigable = ref_entity.is_some();
        let description = f.description.trim().to_string();
        out.push(ExplorerProjectionField {
            name: fname,
            type_label,
            ref_entity,
            ref_entity_navigable,
            description,
        });
    }
    out
}

fn explorer_arg_from_input_field(
    cgs: &CGS,
    field: &InputFieldSchema,
    role: &str,
) -> ExplorerVerbArg {
    let type_label = input_field_type_label(field, cgs);
    let ref_entity = ref_entity_for_input_field(cgs, field);
    let description = field
        .description
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_default();
    ExplorerVerbArg {
        binding: field.name.clone(),
        role: role.to_string(),
        required: Some(field.required),
        description,
        type_label,
        ref_entity,
        cli_flag: format!("--{}", field.name),
    }
}

fn predicate_args_from_capability(cgs: &CGS, cap: &CapabilitySchema) -> Vec<ExplorerVerbArg> {
    cap.object_params()
        .map(|fields| {
            fields
                .iter()
                .map(|f| explorer_arg_from_input_field(cgs, f, "predicate"))
                .collect()
        })
        .unwrap_or_default()
}

fn invoke_args_from_capability(
    cgs: &CGS,
    entity: &EntityDef,
    cap: &CapabilitySchema,
    include_positional_id: bool,
) -> Vec<ExplorerVerbArg> {
    let mut out = Vec::new();
    if include_positional_id {
        let b = entity
            .key_vars
            .last()
            .map(|k| k.as_str().to_string())
            .unwrap_or_else(|| "id".into());
        out.push(ExplorerVerbArg {
            binding: b,
            role: "positional".into(),
            required: Some(true),
            description: format!("{} node id", entity.name),
            type_label: "id".into(),
            ref_entity: None,
            cli_flag: String::new(),
        });
    }
    if let Some(fields) = cap.object_params() {
        for f in fields {
            out.push(explorer_arg_from_input_field(cgs, f, "input"));
        }
    }
    out
}

fn explorer_args_from_pagination(pconf: &PaginationConfig) -> Vec<ExplorerVerbArg> {
    let mut out = vec![
        ExplorerVerbArg {
            binding: "limit".into(),
            role: "pagination".into(),
            required: Some(false),
            description: "Page size / batch size".into(),
            type_label: "usize".into(),
            ref_entity: None,
            cli_flag: "--limit".into(),
        },
        ExplorerVerbArg {
            binding: "all".into(),
            role: "pagination".into(),
            required: Some(false),
            description: "Drain every page".into(),
            type_label: "flag".into(),
            ref_entity: None,
            cli_flag: "--all".into(),
        },
    ];

    if pconf.location == PaginationLocation::BlockRange {
        out.extend([
            ExplorerVerbArg {
                binding: "from_block".into(),
                role: "pagination".into(),
                required: Some(false),
                description: "EVM range start".into(),
                type_label: "u64".into(),
                ref_entity: None,
                cli_flag: "--from-block".into(),
            },
            ExplorerVerbArg {
                binding: "to_block".into(),
                role: "pagination".into(),
                required: Some(false),
                description: "EVM range end".into(),
                type_label: "u64".into(),
                ref_entity: None,
                cli_flag: "--to-block".into(),
            },
        ]);
        return out;
    }

    let mut added_offset = false;
    let mut added_page = false;
    let mut has_from_response = false;
    for (name, param) in &pconf.params {
        match param {
            PaginationParam::Counter { .. } => {
                let name_lower = name.to_lowercase();
                if name_lower.contains("offset") {
                    if !added_offset {
                        added_offset = true;
                        out.push(ExplorerVerbArg {
                            binding: "offset".into(),
                            role: "pagination".into(),
                            required: Some(false),
                            description: format!("Start offset (`{name}`)"),
                            type_label: "i64".into(),
                            ref_entity: None,
                            cli_flag: "--offset".into(),
                        });
                    }
                } else if !added_page {
                    added_page = true;
                    out.push(ExplorerVerbArg {
                        binding: "page".into(),
                        role: "pagination".into(),
                        required: Some(false),
                        description: format!("Start page (`{name}`)"),
                        type_label: "i64".into(),
                        ref_entity: None,
                        cli_flag: "--page".into(),
                    });
                }
            }
            PaginationParam::FromResponse { .. } => {
                if !has_from_response {
                    has_from_response = true;
                    out.push(ExplorerVerbArg {
                        binding: "cursor".into(),
                        role: "pagination".into(),
                        required: Some(false),
                        description: format!("Resume from cursor (`{name}`)"),
                        type_label: "string".into(),
                        ref_entity: None,
                        cli_flag: "--cursor".into(),
                    });
                }
            }
            PaginationParam::Fixed { .. } => {}
        }
    }

    out
}

fn explorer_args_for_get(entity: &EntityDef, get_cap: &CapabilitySchema) -> Vec<ExplorerVerbArg> {
    let key_names: Vec<&str> = entity.key_vars.iter().map(|k| k.as_str()).collect();
    let keys_human = key_names.join(", ");
    let leaf_binding = entity
        .key_vars
        .last()
        .map(|k| k.as_str().to_string())
        .unwrap_or_else(|| "id".into());

    let mut out = vec![ExplorerVerbArg {
        binding: leaf_binding.clone(),
        role: "positional".into(),
        required: Some(true),
        description: if entity.key_vars.len() > 1 {
            format!("Leaf segment · keys {keys_human}")
        } else {
            format!("Path tail id · {}", entity.name)
        },
        type_label: "id".into(),
        ref_entity: None,
        cli_flag: String::new(),
    }];

    let Ok(template) = parse_capability_template(&get_cap.mapping.template) else {
        return out;
    };

    let http_cml = match &template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => Some(cml),
        _ => None,
    };

    if let Some(cml) = http_cml {
        let names = path_var_names_from_request(cml);
        for var_name in names.iter().take(names.len().saturating_sub(1)) {
            out.push(ExplorerVerbArg {
                binding: var_name.clone(),
                role: "path".into(),
                required: Some(true),
                description: format!("Before `{leaf_binding}` in the URL path"),
                type_label: "path".into(),
                ref_entity: None,
                cli_flag: format!("--{}", path_param_long_flag(var_name)),
            });
        }
    }

    if entity.key_vars.len() > 1 {
        let on_path: HashSet<String> = http_cml
            .map(|c| path_var_names_from_request(c).into_iter().collect())
            .unwrap_or_default();
        for kv in &entity.key_vars {
            if on_path.contains(kv.as_str()) {
                continue;
            }
            let b = kv.as_str().to_string();
            out.push(ExplorerVerbArg {
                binding: b.clone(),
                role: "key".into(),
                required: Some(true),
                description: "Compound key · not on path".to_string(),
                type_label: "key".into(),
                ref_entity: None,
                cli_flag: format!("--{}", path_param_long_flag(kv.as_str())),
            });
        }
    }

    let http_path_vars: Vec<String> = match &template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            path_var_names_from_request(cml)
        }
        CapabilityTemplate::View(_)
        | CapabilityTemplate::EvmCall(_)
        | CapabilityTemplate::EvmLogs(_) => Vec::new(),
    };

    for var_name in template_var_names(&template) {
        if var_name == "id" || http_path_vars.contains(&var_name) {
            continue;
        }
        out.push(ExplorerVerbArg {
            binding: var_name.clone(),
            role: "template".into(),
            required: Some(false),
            description: format!("CML template `{var_name}`"),
            type_label: "template".into(),
            ref_entity: None,
            cli_flag: format!("--{}", path_param_long_flag(&var_name)),
        });
    }

    out
}

#[derive(Debug, Serialize)]
pub struct ExplorerRelation {
    pub name: String,
    pub subcommand: String,
    pub target_entity: String,
    /// When true, tool explorer may link to `target_entity` in this catalog.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub target_entity_navigable: bool,
    pub cardinality: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization: Option<ExplorerRelationMaterialization>,
    pub about: String,
    pub scoped_hidden_params: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplorerRelationMaterialization {
    pub kind: &'static str,
    /// CGS `capabilities` key for `query_scoped` / `query_scoped_bindings` materialization (target `query` / `search`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_keys: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ExplorerReverseTraversal {
    pub subcommand: String,
    pub source_entity: String,
    /// When true, `source_entity` names a row in this catalog (link subcommand + type in the explorer).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_entity_navigable: bool,
    pub via_param: String,
    pub about: String,
    pub has_pagination: bool,
    pub has_summary: bool,
}

#[derive(Debug, Serialize)]
pub struct ExplorerEntityRefLink {
    pub field: String,
    pub subcommand: String,
    pub target_entity: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub target_entity_navigable: bool,
    /// CGS `FieldSchema::description` when non-empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub about: String,
}

fn materialization_view(m: &RelationMaterialization) -> ExplorerRelationMaterialization {
    match m {
        RelationMaterialization::Unavailable => ExplorerRelationMaterialization {
            kind: "unavailable",
            capability: None,
            param: None,
            binding_keys: None,
        },
        RelationMaterialization::FromParentGet { .. } => ExplorerRelationMaterialization {
            kind: "from_parent_get",
            capability: None,
            param: None,
            binding_keys: None,
        },
        RelationMaterialization::QueryScoped { capability, param } => {
            ExplorerRelationMaterialization {
                kind: "query_scoped",
                capability: Some(capability.to_string()),
                param: Some(param.to_string()),
                binding_keys: None,
            }
        }
        RelationMaterialization::QueryScopedBindings {
            capability,
            bindings,
        } => ExplorerRelationMaterialization {
            kind: "query_scoped_bindings",
            capability: Some(capability.to_string()),
            param: None,
            binding_keys: Some(bindings.keys().map(|k| k.to_string()).collect()),
        },
    }
}

fn validate_entity_names(cgs: &CGS, names: &[String]) -> Result<(), ToolModelBuildError> {
    for n in names {
        if !cgs.entities.contains_key(n.as_str()) {
            return Err(ToolModelBuildError::BadRequest(format!(
                "unknown entity `{n}` for this catalog"
            )));
        }
    }
    Ok(())
}

fn render_bundle_for_tool_model(
    cgs: &CGS,
    q: &ToolModelQuery,
) -> Result<(plasm_core::prompt_render::DomainPromptBundle, &'static str), ToolModelBuildError> {
    let mode = ToolModelFocusMode::parse(&q.focus)?;
    match mode {
        ToolModelFocusMode::All => {
            validate_entity_names(cgs, &q.entity)?;
            if !q.entity.is_empty() {
                return Err(ToolModelBuildError::BadRequest(
                    "focus=all does not accept entity= parameters".into(),
                ));
            }
            Ok((
                render_domain_bundle(
                    cgs,
                    DomainPromptSource::Catalog {
                        focus: FocusSpec::All,
                    },
                    DomainPromptSettings {
                        include_domain_execution_model: true,
                        symbolic: false,
                        symbol_map_cross_cache: None,
                    },
                ),
                mode.as_str(),
            ))
        }
        ToolModelFocusMode::Single => {
            if q.entity.len() != 1 {
                return Err(ToolModelBuildError::BadRequest(
                    "focus=single requires exactly one entity= parameter".into(),
                ));
            }
            validate_entity_names(cgs, &q.entity)?;
            Ok((
                render_domain_bundle(
                    cgs,
                    DomainPromptSource::Catalog {
                        focus: FocusSpec::Single(q.entity[0].as_str()),
                    },
                    DomainPromptSettings {
                        include_domain_execution_model: true,
                        symbolic: false,
                        symbol_map_cross_cache: None,
                    },
                ),
                mode.as_str(),
            ))
        }
        ToolModelFocusMode::Seeds => {
            if q.entity.is_empty() {
                return Err(ToolModelBuildError::BadRequest(
                    "focus=seeds requires at least one entity= parameter".into(),
                ));
            }
            validate_entity_names(cgs, &q.entity)?;
            let refs: Vec<&str> = q.entity.iter().map(|s| s.as_str()).collect();
            Ok((
                render_domain_bundle(
                    cgs,
                    DomainPromptSource::Catalog {
                        focus: FocusSpec::Seeds(&refs),
                    },
                    DomainPromptSettings {
                        include_domain_execution_model: true,
                        symbolic: false,
                        symbol_map_cross_cache: None,
                    },
                ),
                mode.as_str(),
            ))
        }
    }
}

/// Build JSON for the explorer UI; mirrors CLI affordances in [`crate::cli_builder`] and DOMAIN model in prompt render.
pub fn build_tool_model(
    cgs: &CGS,
    meta: &CatalogEntryMeta,
    q: &ToolModelQuery,
) -> Result<ToolModelResponse, ToolModelBuildError> {
    let (bundle, mode_label) = render_bundle_for_tool_model(cgs, q)?;

    let resolved_entities: Vec<String> = bundle
        .model
        .entities
        .iter()
        .map(|e| e.entity.clone())
        .collect();

    let invoke_by_domain = invoke_capabilities_by_domain(cgs);

    let mut entities_out = Vec::new();
    let mut relation_edge_count: usize = 0;
    let mut verb_count: usize = 0;

    for edp in &bundle.model.entities {
        let name = edp.entity.as_str();
        let Some(entity) = cgs.entities.get(name) else {
            continue;
        };
        if entity.abstract_entity {
            continue;
        }

        let invoke_caps = invoke_by_domain
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or_default();

        let proj = project_entity(cgs, entity, edp.lines.clone(), invoke_caps);
        relation_edge_count +=
            proj.relations.len() + proj.reverse_traversals.len() + proj.entity_ref_links.len();
        verb_count += proj.verbs.len();
        entities_out.push(proj);
    }

    let auth = cgs.auth.clone();
    let oauth = cgs.oauth.clone();
    let scheme = auth.as_ref().map(auth_scheme_name).map(str::to_string);
    let connect_profile = catalog_connect_profile(auth.as_ref(), oauth.as_ref());

    Ok(ToolModelResponse {
        entry: meta.clone(),
        focus: ToolModelFocusBlock {
            mode: mode_label.to_string(),
            resolved_entities,
        },
        overview: ToolModelOverview {
            entity_count: entities_out.len(),
            relation_edge_count,
            verb_count,
        },
        auth: ToolModelAuthBlock {
            scheme,
            auth,
            oauth,
            connect_profile,
        },
        entities: entities_out,
        domain: ToolModelDomainBlock {
            model: bundle.model,
        },
    })
}

fn auth_scheme_name(scheme: &AuthScheme) -> &'static str {
    match scheme {
        AuthScheme::None => "none",
        AuthScheme::ApiKeyHeader { .. } => "api_key_header",
        AuthScheme::ApiKeyQuery { .. } => "api_key_query",
        AuthScheme::BearerToken { .. } => "bearer_token",
        AuthScheme::Oauth2ClientCredentials { .. } => "oauth2_client_credentials",
    }
}

/// Pair each DOMAIN prompt line with parameters for the matching affordance (query/search/get/…).
/// Relation-nav DOMAIN lines are omitted — they are listed under `relations` only.
/// Rows resolve the explorer verb via [`DomainLineMeta::source_capability`] (emitted by prompt render), not line order.
/// Create / delete / update / action capabilities per domain, in global [`CGS::capabilities`] order.
/// Built once per tool-model response so [`project_entity`] does not scan all capabilities per entity.
fn invoke_capabilities_by_domain<'a>(cgs: &'a CGS) -> HashMap<&'a str, Vec<&'a CapabilitySchema>> {
    let mut map: HashMap<&'a str, Vec<&'a CapabilitySchema>> =
        HashMap::with_capacity(cgs.entities.len().min(64));
    for cap in cgs.capabilities.values() {
        match cap.kind {
            CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => continue,
            CapabilityKind::Create
            | CapabilityKind::Delete
            | CapabilityKind::Update
            | CapabilityKind::Action => {}
        }
        map.entry(cap.domain.as_str()).or_default().push(cap);
    }
    map
}

fn build_capability_rows(
    cgs: &CGS,
    domain_lines: &[DomainLineMeta],
    verbs: &[ExplorerVerb],
) -> Vec<ExplorerCapabilityRow> {
    let mut by_cap: HashMap<&str, &ExplorerVerb> = HashMap::with_capacity(verbs.len());
    for v in verbs {
        if let Some(cn) = v.capability_name.as_deref() {
            by_cap.insert(cn, v);
        }
    }

    let mut rows = Vec::new();
    for line in domain_lines {
        if line.kind == DomainLineKind::RelationNav {
            continue;
        }
        let line_kind_str = line.kind.as_str().to_string();
        let verb = line
            .source_capability
            .as_ref()
            .and_then(|cn| by_cap.get(cn.as_str()).copied());

        let (parameters, returns) = match verb {
            Some(v) => (v.arguments.clone(), v.returns.clone()),
            None => (Vec::new(), explorer_return_unspecified()),
        };

        let description = line
            .source_capability
            .as_ref()
            .and_then(|cn| cgs.get_capability(cn.as_str()))
            .map(|cap| cap.description.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();

        rows.push(ExplorerCapabilityRow {
            expression: line.expression.clone(),
            line_kind: line_kind_str,
            capability_name: line.source_capability.clone(),
            description,
            parameters,
            returns,
        });
    }
    rows
}

fn project_entity(
    cgs: &CGS,
    entity: &EntityDef,
    domain_lines: Vec<DomainLineMeta>,
    invoke_caps: &[&CapabilitySchema],
) -> ExplorerEntityProjection {
    let name = entity.name.clone();
    let mut verbs = Vec::new();

    if let Some(query_cap) = cgs.primary_query_capability(&name) {
        let mut arguments = predicate_args_from_capability(cgs, query_cap);
        if cgs.find_capability(&name, CapabilityKind::Get).is_some() {
            arguments.push(ExplorerVerbArg {
                binding: "summary".into(),
                role: "summary".into(),
                required: Some(false),
                description: "List rows only (no per-row GET)".into(),
                type_label: "flag".into(),
                ref_entity: None,
                cli_flag: "--summary".into(),
            });
        }
        let has_pagination = if let Some(ref pconf) = pagination_config_for_capability(query_cap) {
            arguments.extend(explorer_args_from_pagination(pconf));
            true
        } else {
            false
        };
        let about_base =
            capability_about_from_cgs(query_cap, format!("Primary list / query for {}", name));
        verbs.push(ExplorerVerb {
            kind: "query".into(),
            label: "query".into(),
            capability_name: Some(query_cap.name.to_string()),
            about: if has_pagination {
                append_llm_pagination_execute_note(about_base)
            } else {
                about_base
            },
            has_pagination,
            has_summary: cgs.find_capability(&name, CapabilityKind::Get).is_some(),
            arguments,
            returns: explorer_return_for_cap(cgs, query_cap),
        });
    }

    if let Some(search_cap) = cgs.primary_search_capability(&name) {
        let mut arguments = predicate_args_from_capability(cgs, search_cap);
        let has_pagination = if let Some(ref pconf) = pagination_config_for_capability(search_cap) {
            arguments.extend(explorer_args_from_pagination(pconf));
            true
        } else {
            false
        };
        let about_base =
            capability_about_from_cgs(search_cap, format!("Primary search for {}", name));
        verbs.push(ExplorerVerb {
            kind: "search".into(),
            label: "search".into(),
            capability_name: Some(search_cap.name.to_string()),
            about: if has_pagination {
                append_llm_pagination_execute_note(about_base)
            } else {
                about_base
            },
            has_pagination,
            has_summary: false,
            arguments,
            returns: explorer_return_for_cap(cgs, search_cap),
        });
    }

    let mut get_caps: Vec<_> = cgs.find_capabilities(&name, CapabilityKind::Get);
    get_caps.sort_by(|a, b| a.name.cmp(&b.name));
    let multiple_identity_verbs = get_caps.len() > 1;
    for get_cap in get_caps {
        let label = if multiple_identity_verbs {
            capability_method_label_kebab(get_cap)
        } else {
            "id".into()
        };
        let arguments = explorer_args_for_get(entity, get_cap);
        verbs.push(ExplorerVerb {
            kind: "identity".into(),
            label,
            capability_name: Some(get_cap.name.to_string()),
            about: capability_about_from_cgs(
                get_cap,
                format!(
                    "Identity on {}: GET by resource id (positional), same path as the dynamic CLI",
                    name
                ),
            ),
            has_pagination: false,
            has_summary: false,
            arguments,
            returns: explorer_return_for_cap(cgs, get_cap),
        });
    }

    for cap in cgs.named_query_capabilities(&name) {
        let sub_kebab = capability_method_label_kebab(cap);
        let kind_label = match cap.kind {
            CapabilityKind::Search => "named_search",
            _ => "named_query",
        };
        let mut arguments = predicate_args_from_capability(cgs, cap);
        let has_pagination = if let Some(ref pconf) = pagination_config_for_capability(cap) {
            arguments.extend(explorer_args_from_pagination(pconf));
            true
        } else {
            false
        };
        let about_base = capability_about_from_cgs(cap, cap.name.to_string());
        verbs.push(ExplorerVerb {
            kind: kind_label.into(),
            label: sub_kebab,
            capability_name: Some(cap.name.to_string()),
            about: if has_pagination {
                append_llm_pagination_execute_note(about_base)
            } else {
                about_base
            },
            has_pagination,
            has_summary: false,
            arguments,
            returns: explorer_return_for_cap(cgs, cap),
        });
    }

    let mut relations = Vec::new();
    for (rel_name, rel_schema) in &entity.relations {
        let (scoped_hidden, materialization) = relation_scope_meta(rel_schema);
        let tgt = rel_schema.target_resource.to_string();
        relations.push(ExplorerRelation {
            name: rel_name.to_string(),
            subcommand: relation_subcommand_kebab(rel_name),
            target_entity: tgt.clone(),
            target_entity_navigable: cgs.entities.contains_key(tgt.as_str()),
            cardinality: match rel_schema.cardinality {
                plasm_core::Cardinality::One => "one".into(),
                plasm_core::Cardinality::Many => "many".into(),
            },
            materialization,
            about: rel_schema.description.clone(),
            scoped_hidden_params: scoped_hidden,
        });
    }

    let mut reverse_traversals = Vec::new();
    {
        let reverse_caps = cgs.find_reverse_traversal_caps(&name);
        let mut relation_names: HashSet<String> = HashSet::with_capacity(entity.relations.len());
        relation_names.extend(
            entity
                .relations
                .keys()
                .map(|k| relation_subcommand_kebab(k)),
        );

        for (cap, param_name) in &reverse_caps {
            let sub_label = relation_subcommand_kebab(&pluralize_entity(cap.domain.as_str()));
            if relation_names.contains(&sub_label) {
                continue;
            }
            let qcap = cgs.find_capability(cap.domain.as_str(), CapabilityKind::Query);
            let domain = cap.domain.to_string();
            reverse_traversals.push(ExplorerReverseTraversal {
                subcommand: sub_label,
                source_entity: domain.clone(),
                source_entity_navigable: cgs.entities.contains_key(domain.as_str()),
                via_param: param_name.to_string(),
                about: capability_about_from_cgs(
                    cap,
                    format!("Reverse from {} via {}.{}", name, cap.domain, param_name),
                ),
                has_pagination: qcap.and_then(pagination_config_for_capability).is_some(),
                has_summary: cgs
                    .find_capability(cap.domain.as_str(), CapabilityKind::Get)
                    .is_some(),
            });
        }
    }

    let mut entity_ref_links = Vec::new();
    for (field_name, field_schema) in &entity.fields {
        let Ok(nv) = cgs.named_value_for_slot(field_schema) else {
            continue;
        };
        if let FieldType::EntityRef { ref target } = nv.field_type {
            let kebab = field_subcommand_kebab(field_name);
            if entity.relations.contains_key(field_name.as_str()) {
                continue;
            }
            let has_get = cgs
                .find_capability(target.as_str(), CapabilityKind::Get)
                .is_some();
            if !has_get {
                continue;
            }
            let tgt = target.to_string();
            entity_ref_links.push(ExplorerEntityRefLink {
                field: field_name.to_string(),
                subcommand: kebab,
                target_entity: tgt.clone(),
                target_entity_navigable: cgs.entities.contains_key(tgt.as_str()),
                description: field_schema.description.trim().to_string(),
                about: format!("{}.{} → {} (EntityRef)", name, field_name, target),
            });
        }
    }

    for cap in invoke_caps {
        let sub_kebab = capability_method_label_kebab(cap);
        match cap.kind {
            CapabilityKind::Create => {
                let arguments = invoke_args_from_capability(cgs, entity, cap, false);
                verbs.push(ExplorerVerb {
                    kind: "create".into(),
                    label: sub_kebab,
                    capability_name: Some(cap.name.to_string()),
                    about: capability_about_from_cgs(cap, format!("Create {}", name)),
                    has_pagination: false,
                    has_summary: false,
                    arguments,
                    returns: explorer_return_for_cap(cgs, cap),
                });
            }
            CapabilityKind::Delete => {
                let arguments = invoke_args_from_capability(cgs, entity, cap, true);
                verbs.push(ExplorerVerb {
                    kind: "delete".into(),
                    label: sub_kebab,
                    capability_name: Some(cap.name.to_string()),
                    about: capability_about_from_cgs(cap, format!("Delete {}", name)),
                    has_pagination: false,
                    has_summary: false,
                    arguments,
                    returns: explorer_return_for_cap(cgs, cap),
                });
            }
            CapabilityKind::Update | CapabilityKind::Action => {
                let k = if cap.kind == CapabilityKind::Update {
                    "update"
                } else {
                    "action"
                };
                let arguments = invoke_args_from_capability(cgs, entity, cap, true);
                verbs.push(ExplorerVerb {
                    kind: k.into(),
                    label: sub_kebab,
                    capability_name: Some(cap.name.to_string()),
                    about: capability_about_from_cgs(cap, cap.name.to_string()),
                    has_pagination: false,
                    has_summary: false,
                    arguments,
                    returns: explorer_return_for_cap(cgs, cap),
                });
            }
            CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => {}
        }
    }

    let capabilities = build_capability_rows(cgs, &domain_lines, &verbs);
    let projection = build_projection_fields(cgs, entity);

    ExplorerEntityProjection {
        description: entity.description.clone(),
        name: name.to_string(),
        projection,
        verbs,
        capabilities,
        relations,
        reverse_traversals,
        entity_ref_links,
        domain_lines,
    }
}

fn relation_scope_meta(
    rel_schema: &RelationSchema,
) -> (Vec<String>, Option<ExplorerRelationMaterialization>) {
    let mat = rel_schema.materialize.as_ref().map(materialization_view);
    let skip = match rel_schema.materialize.as_ref() {
        Some(RelationMaterialization::QueryScoped { param, .. }) => vec![param.to_string()],
        Some(RelationMaterialization::QueryScopedBindings { bindings, .. }) => {
            bindings.keys().map(|k| k.to_string()).collect()
        }
        _ => Vec::new(),
    };
    (skip, mat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema;
    use plasm_core::loader::load_schema_dir;
    use std::path::Path;

    #[test]
    fn tool_model_query_entity_scalar_urlencoded() {
        let q: ToolModelQuery =
            serde_urlencoded::from_str("focus=single&entity=Order").expect("deserialize");
        assert_eq!(q.focus, "single");
        assert_eq!(q.entity, vec!["Order".to_string()]);
    }

    #[test]
    fn fixture_tool_model_all_smoke() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let meta = CatalogEntryMeta {
            entry_id: "overshow".into(),
            label: "Overshow".into(),
            tags: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let q = ToolModelQuery {
            focus: "all".into(),
            entity: vec![],
        };
        let m = build_tool_model(&cgs, &meta, &q).expect("ok");
        assert!(!m.entities.is_empty());
        assert_eq!(m.entities.len(), m.domain.model.entities.len());
        assert_eq!(m.focus.mode, "all");
        assert!(m.overview.verb_count > 0);
        assert_eq!(m.auth.scheme.as_deref(), Some("none"));
        assert_eq!(
            m.auth.connect_profile.capability,
            plasm_core::CatalogAuthCapability::Public
        );
        assert!(m.auth.connect_profile.has_public_mode);
    }

    #[test]
    fn gmail_tool_model_exposes_cgs_auth_oauth_metadata() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/gmail");
        let cgs = load_schema(&dir).expect("gmail");
        let meta = CatalogEntryMeta {
            entry_id: "gmail".into(),
            label: "Gmail".into(),
            tags: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let q = ToolModelQuery {
            focus: "all".into(),
            entity: vec![],
        };
        let m = build_tool_model(&cgs, &meta, &q).expect("ok");
        assert_eq!(m.auth.scheme.as_deref(), Some("bearer_token"));
        assert_eq!(
            m.auth.connect_profile.capability,
            plasm_core::CatalogAuthCapability::ApiKeyAndOauth
        );
        assert!(m.auth.connect_profile.has_api_key);
        assert!(m.auth.connect_profile.has_oauth);
        assert!(m.auth.connect_profile.oauth.provider_present);
        let oauth = m.auth.oauth.as_ref().expect("gmail oauth block");
        assert_eq!(oauth.provider, "google");
        assert!(oauth
            .default_scope_sets
            .contains_key("plasm_gmail_integrator_bundle"));
        assert!(oauth.requirements.capabilities.contains_key("message_list"));
    }

    #[test]
    fn fixture_projection_carries_field_descriptions() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/pokeapi_mini");
        let cgs = load_schema_dir(&dir).expect("pokeapi_mini");
        let meta = CatalogEntryMeta {
            entry_id: "pokeapi_mini".into(),
            label: "PokeAPI Mini".into(),
            tags: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let q = ToolModelQuery {
            focus: "all".into(),
            entity: vec![],
        };
        let m = build_tool_model(&cgs, &meta, &q).expect("ok");
        let berry = m
            .entities
            .iter()
            .find(|e| e.name == "Berry")
            .expect("Berry entity");
        let name = berry
            .projection
            .iter()
            .find(|p| p.name == "name")
            .expect("name field");
        assert!(
            !name.description.is_empty(),
            "CGS `description:` on entity fields must flow into tool-model projection"
        );
    }

    #[test]
    fn github_pull_request_infers_returns_when_output_omitted() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        let cgs = load_schema(&dir).expect("github");
        let pr_get = cgs.get_capability("pr_get").expect("pr_get");
        assert!(
            pr_get.output_schema.is_none(),
            "GitHub pr_get should omit explicit output: (inference applies)"
        );
        let meta = CatalogEntryMeta {
            entry_id: "github".into(),
            label: "GitHub".into(),
            tags: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let q = ToolModelQuery {
            focus: "all".into(),
            entity: vec![],
        };
        let m = build_tool_model(&cgs, &meta, &q).expect("ok");
        let pr = m
            .entities
            .iter()
            .find(|e| e.name == "PullRequest")
            .expect("PullRequest");
        let get = pr.verbs.iter().find(|v| v.kind == "identity").expect("get");
        assert_eq!(get.returns.kind, "entity");
        assert_eq!(get.returns.entity.as_deref(), Some("PullRequest"));
        // `pr_query` is repository-scoped → surfaced as `named_query`, not primary `query`.
        let list = pr
            .verbs
            .iter()
            .find(|v| v.capability_name.as_deref() == Some("pr_query"))
            .expect("pr_query verb");
        assert_eq!(list.returns.kind, "collection");
        assert!(
            list.returns.label.contains("PullRequest"),
            "label={:?}",
            list.returns.label
        );
    }

    #[test]
    fn notion_page_relation_created_by_targets_user() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/notion");
        let cgs = load_schema(&dir).expect("notion");
        let meta = CatalogEntryMeta {
            entry_id: "notion".into(),
            label: "Notion".into(),
            tags: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let q = ToolModelQuery {
            focus: "all".into(),
            entity: vec![],
        };
        let m = build_tool_model(&cgs, &meta, &q).expect("ok");
        let page = m
            .entities
            .iter()
            .find(|e| e.name == "Page")
            .expect("Page entity");
        assert!(
            !page
                .capabilities
                .iter()
                .any(|c| c.line_kind == "relation_nav"),
            "relation DOMAIN lines should not duplicate the Relations section"
        );
        let rel = page
            .relations
            .iter()
            .find(|r| r.name == "created_by")
            .expect("created_by relation");
        assert_eq!(rel.target_entity, "User");
        assert!(rel.target_entity_navigable);
    }
}
