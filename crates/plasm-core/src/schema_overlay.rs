//! Runtime schema overlay: merge workspace-specific entity definitions into a base CGS.
//!
//! Catalogs declare a `schema_overlay:` block in `domain.yaml`; the host executes
//! `source.capability`, projects rows into entities via Minijinja + JSON paths, and merges
//! the result with [`CGS::with_overlay`].

use crate::error::SchemaError;
use crate::identity::{EntityFieldName, EntityName};
use crate::schema::{
    CapabilityKind, CapabilitySchema, EntityDef, FieldDeriveRule, FieldSchema, FieldValueKind,
    InputType, NamedValueSchema, ValueDomainKey, CGS,
};
use indexmap::IndexMap;
use minijinja::{Environment, UndefinedBehavior, Value as MjValue};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

/// Declarative overlay spec authored in `domain.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaOverlaySpec {
    pub source: OverlaySourceSpec,
    pub projection: OverlayProjectionSpec,
    pub entity: OverlayEntitySpec,
    pub decode: OverlayDecodeSpec,
}

impl SchemaOverlaySpec {
    pub fn is_multi_step_source(&self) -> bool {
        !self.source.steps.is_empty()
    }

    /// Final capability name for logging (last pipeline step, or single-step `source.capability`).
    pub fn source_capability(&self) -> &str {
        if self.is_multi_step_source() {
            self.source
                .steps
                .last()
                .map(|s| s.capability.as_str())
                .unwrap_or("")
        } else {
            self.source.capability.as_str()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlaySourceSpec {
    /// Single-step source (Fibery, Notion). Empty when `steps` is non-empty.
    #[serde(default)]
    pub capability: String,
    #[serde(default)]
    pub bind: IndexMap<String, String>,
    /// API-driven multi-fetch pipeline (ClickUp, Jira). Row-driven `bind` on `for_each` steps only.
    #[serde(default)]
    pub steps: Vec<OverlaySourceStepSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlaySourceStepSpec {
    pub capability: String,
    /// Store response rows under this name for later `for_each` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collect: Option<String>,
    /// Walk this path on the response JSON to collect row objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_path: Option<Vec<String>>,
    /// Iterate rows from a prior `collect` step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<String>,
    #[serde(default)]
    pub bind: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<OverlayStepMergeSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayStepMergeSpec {
    AppendArray { path: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayProjectionSpec {
    #[serde(default)]
    pub mode: OverlayProjectionMode,
    pub items_path: Vec<String>,
    /// When set, each top-level generator row is expanded by walking this path to nested arrays.
    /// Templates receive `{ row, parent }` where `row` is the nested element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_items_path: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayProjectionMode {
    #[default]
    PerScopeEntity,
    AugmentBase,
    ColumnSchema,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayEntitySpec {
    pub from_template: String,
    pub name: OverlayTemplateSpec,
    #[serde(default)]
    pub expression_aliases: Vec<OverlayTemplateSpec>,
    pub scope_key: OverlayTemplateSpec,
    #[serde(default)]
    pub static_fields: IndexMap<String, OverlayStaticFieldSpec>,
    #[serde(default)]
    pub dynamic_fields: Option<OverlayDynamicFieldsSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayTemplateSpec {
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayStaticFieldSpec {
    pub template: String,
    pub value_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayDynamicFieldsSpec {
    pub from: FieldCatalogSource,
    #[serde(default)]
    pub skip: Option<FieldSkipSpec>,
    pub wire_name_path: Vec<String>,
    pub wire_type_path: Vec<String>,
    pub name: OverlayTemplateSpec,
    pub type_map: IndexMap<String, String>,
    #[serde(default)]
    pub default: Option<String>,
    pub extract: FieldExtractSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldCatalogSource {
    Array { path: Vec<String> },
    ObjectMap { path: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSkipSpec {
    pub wire_name_path: Vec<String>,
    #[serde(default)]
    pub values_in: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldExtractSpec {
    TopLevelKey,
    PathSegments {
        segments: Vec<String>,
    },
    NameValueArray {
        array_path: Vec<String>,
        match_key_field: String,
        match_equals: OverlayTemplateSpec,
        #[serde(default = "default_name_value_value_field")]
        value_field: String,
    },
}

fn default_name_value_value_field() -> String {
    "value".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayDecodeSpec {
    pub scope: OverlayDecodeScopeSpec,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayDecodeScopeSpec {
    pub params: Vec<String>,
    pub key: OverlayTemplateSpec,
}

/// Synthesized entities and value slots merged into a bootstrap CGS at session open.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaOverlay {
    pub entities: IndexMap<EntityName, EntityDef>,
    pub values: IndexMap<String, NamedValueSchema>,
    /// Maps scope key (e.g. Fibery `Space/Name`) → Plasm entity name.
    pub scope_index: IndexMap<String, EntityName>,
    pub overlay_hash: String,
}

/// Resolve overlay entity name for a scope parameter value.
pub fn overlay_entity_for_scope<'a>(cgs: &'a CGS, scope_value: &str) -> Option<&'a str> {
    cgs.schema_overlay_scope_index
        .get(scope_value)
        .map(|n| n.as_str())
}

fn capability_input_field_names(cap: &CapabilitySchema) -> Vec<String> {
    cap.input_schema
        .as_ref()
        .and_then(|schema| match &schema.input_type {
            InputType::Object { fields, .. } => {
                Some(fields.iter().map(|f| f.name.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Resolve `bind` templates from an API row (`{ row, parent, bind }`).
pub fn resolve_overlay_row_bind(
    bind: &IndexMap<String, String>,
    row: &JsonValue,
    parent: Option<&JsonValue>,
    session_bind: Option<&IndexMap<String, String>>,
) -> Result<IndexMap<String, String>, String> {
    if bind.is_empty() {
        return Ok(IndexMap::new());
    }
    let ctx = overlay_row_context(row, parent);
    let bind_obj: serde_json::Map<String, JsonValue> = session_bind
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), JsonValue::String(v.clone())))
                .collect()
        })
        .unwrap_or_default();
    let ctx = serde_json::json!({
        "row": ctx.get("row").cloned().unwrap_or(JsonValue::Null),
        "parent": ctx.get("parent").cloned().unwrap_or(JsonValue::Null),
        "bind": bind_obj,
    });
    let mut tmpl_env = overlay_template_environment();
    tmpl_env.set_undefined_behavior(UndefinedBehavior::Chainable);
    let mut out = IndexMap::new();
    for (param, template) in bind {
        let value = if template.contains("{{") {
            render_overlay_template(&tmpl_env, template, &ctx)?
        } else {
            template.clone()
        };
        if value.trim().is_empty() {
            return Err(format!("source bind '{param}' resolved to empty string"));
        }
        out.insert(param.clone(), value);
    }
    Ok(out)
}

/// Extract row objects from a fetch response for a collect step.
pub fn overlay_collect_rows(
    response: &JsonValue,
    items_path: &[String],
) -> Result<Vec<JsonValue>, String> {
    let items = walk_json_path(response, items_path)?;
    let arr = items
        .as_array()
        .ok_or_else(|| format!("collect items_path {items_path:?} must resolve to a JSON array"))?;
    Ok(arr.clone())
}

/// Merge a `for_each` step response into the pipeline accumulator.
pub fn overlay_merge_step_response(
    accumulator: &mut JsonValue,
    merge: &OverlayStepMergeSpec,
    response: &JsonValue,
) -> Result<(), String> {
    match merge {
        OverlayStepMergeSpec::AppendArray { path } => {
            let incoming = walk_json_path(response, path)?;
            let incoming_arr = incoming.as_array().ok_or_else(|| {
                format!("merge append_array path {path:?} must resolve to a JSON array")
            })?;
            if !accumulator.is_object() {
                *accumulator = serde_json::json!({});
            }
            let acc_obj = accumulator.as_object_mut().expect("object");
            let key = path
                .last()
                .ok_or("merge append_array path must be non-empty")?
                .clone();
            let entry = acc_obj
                .entry(key)
                .or_insert_with(|| JsonValue::Array(Vec::new()));
            let acc_arr = entry.as_array_mut().ok_or_else(|| {
                "merge append_array accumulator path must be a JSON array".to_string()
            })?;
            for item in incoming_arr {
                acc_arr.push(item.clone());
            }
        }
    }
    Ok(())
}

/// Stable cache suffix for a multi-step overlay pipeline (ordered fetch bodies).
pub fn overlay_pipeline_cache_suffix(responses: &[JsonValue]) -> String {
    if responses.is_empty() {
        return String::new();
    }
    let bytes = serde_json::to_vec(responses).unwrap_or_default();
    format!(":pipeline:{}", hex::encode(Sha256::digest(bytes)))
}

/// Stable cache suffix for single-step bind args (legacy; empty when bind absent).
pub fn overlay_bind_cache_suffix(bind: &IndexMap<String, String>) -> String {
    if bind.is_empty() {
        return String::new();
    }
    let mut keys: Vec<_> = bind.keys().cloned().collect();
    keys.sort();
    let canonical = serde_json::json!(keys
        .iter()
        .map(|k| (k, bind.get(k).map(String::as_str)))
        .collect::<Vec<_>>());
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    format!(":bind:{}", hex::encode(Sha256::digest(bytes)))
}

struct OverlayGeneratorRow<'a> {
    row: &'a JsonValue,
    parent: Option<&'a JsonValue>,
}

fn overlay_row_context(row: &JsonValue, parent: Option<&JsonValue>) -> JsonValue {
    match parent {
        Some(parent) => serde_json::json!({ "row": row, "parent": parent }),
        None => serde_json::json!({ "row": row }),
    }
}

fn collect_overlay_generator_rows<'a>(
    source_response: &'a JsonValue,
    projection: &OverlayProjectionSpec,
) -> Result<Vec<OverlayGeneratorRow<'a>>, String> {
    let items = walk_json_path(source_response, &projection.items_path)?;
    let top_rows = items
        .as_array()
        .ok_or("projection.items_path must resolve to a JSON array")?;

    if let Some(nested_path) = &projection.nested_items_path {
        let mut out = Vec::new();
        for parent in top_rows {
            let nested = walk_json_path(parent, nested_path)?;
            let child_rows = nested
                .as_array()
                .ok_or("projection.nested_items_path must resolve to a JSON array")?;
            for row in child_rows {
                out.push(OverlayGeneratorRow {
                    row,
                    parent: Some(parent),
                });
            }
        }
        Ok(out)
    } else {
        Ok(top_rows
            .iter()
            .map(|row| OverlayGeneratorRow { row, parent: None })
            .collect())
    }
}

/// Build the canonical decode scope key from ambient capability parameters.
pub fn build_decode_scope_key(
    spec: &OverlayDecodeScopeSpec,
    ambient: &IndexMap<String, String>,
) -> Option<String> {
    for param in &spec.params {
        if !ambient.contains_key(param.as_str()) {
            return None;
        }
    }
    let mut amb = serde_json::Map::new();
    for (k, v) in ambient {
        amb.insert(k.clone(), JsonValue::String(v.clone()));
    }
    let ctx = serde_json::json!({ "ambient": amb });
    let env = overlay_template_environment();
    render_overlay_template(&env, &spec.key.template, &ctx).ok()
}

/// Walk a JSON value along a path of object keys (same semantics as CML `items_path`).
pub fn walk_json_path<'a>(value: &'a JsonValue, path: &[String]) -> Result<&'a JsonValue, String> {
    let mut cur = value;
    for seg in path {
        cur = cur
            .get(seg.as_str())
            .ok_or_else(|| format!("items_path missing key '{seg}'"))?;
    }
    Ok(cur)
}

fn walk_json_path_string(value: &JsonValue, path: &[String]) -> Result<String, String> {
    let v = walk_json_path(value, path)?;
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("expected string at path [{}]", path.join(".")))
}

fn sanitize_identifier_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn sanitize_identifier_from_wire(wire: &str) -> String {
    wire.chars()
        .map(|c| match c {
            '/' | '~' | '?' | ' ' | '-' => '_',
            c if c.is_ascii_alphanumeric() => c,
            _ => '_',
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn overlay_template_environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_filter(
        "join_sanitize",
        |value: MjValue,
         args: minijinja::value::Rest<MjValue>|
         -> Result<String, minijinja::Error> {
            let name = value.as_str().ok_or_else(|| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "join_sanitize: expected string",
                )
            })?;
            let sep = args.0.first().and_then(|v| v.as_str()).unwrap_or("__");
            let split_on = args.0.get(1).and_then(|v| v.as_str()).unwrap_or("/");
            Ok(name
                .split(split_on)
                .map(sanitize_identifier_segment)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(sep))
        },
    );
    env.add_filter(
        "sanitize_identifier",
        |value: MjValue| -> Result<String, minijinja::Error> {
            let wire = value.as_str().ok_or_else(|| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "sanitize_identifier: expected string",
                )
            })?;
            Ok(sanitize_identifier_from_wire(wire))
        },
    );
    env
}

fn render_overlay_template(
    env: &Environment<'_>,
    template: &str,
    ctx: &JsonValue,
) -> Result<String, String> {
    let compiled = env
        .template_from_str(template)
        .map_err(|e| format!("template compile: {e}"))?;
    let mj_ctx = MjValue::from_serialize(ctx);
    compiled
        .render(mj_ctx)
        .map_err(|e| format!("template render: {e}"))
}

fn render_path_segments(
    env: &Environment<'_>,
    segments: &[String],
    ctx: &JsonValue,
) -> Result<Vec<String>, String> {
    segments
        .iter()
        .map(|seg| {
            if seg.contains("{{") {
                render_overlay_template(env, seg, ctx)
            } else {
                Ok(seg.clone())
            }
        })
        .collect()
}

fn is_valid_plasm_entity_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn field_from_value_ref(
    base: &CGS,
    field_name: &str,
    value_ref: &str,
    wire_path: Option<Vec<String>>,
    derive: Option<FieldDeriveRule>,
    description: &str,
) -> Result<FieldSchema, String> {
    if !base.values.contains_key(value_ref) {
        return Err(format!("unknown value_ref '{value_ref}'"));
    }
    Ok(FieldSchema {
        name: EntityFieldName::from(field_name),
        kind: FieldValueKind::Registry(ValueDomainKey::new(value_ref).map_err(|e| e.to_string())?),
        description: description.to_string(),
        required: false,
        agent_presentation: None,
        mime_type_hint: None,
        data_class: None,
        attachment_media: None,
        wire_path,
        derive,
        currency_field: None,
    })
}

fn resolve_type_map_key(
    type_map: &IndexMap<String, String>,
    default: Option<&String>,
    wire_type: &str,
) -> Result<String, String> {
    type_map
        .get(wire_type)
        .cloned()
        .or_else(|| default.cloned())
        .ok_or_else(|| format!("no type_map entry for wire type '{wire_type}'"))
}

fn wire_name_from_field(
    field_row: &JsonValue,
    field_key: Option<&str>,
    wire_name_path: &[String],
) -> Result<String, String> {
    if wire_name_path.is_empty() {
        field_key
            .map(str::to_string)
            .ok_or_else(|| "wire_name_path empty and no object_map key".to_string())
    } else {
        walk_json_path_string(field_row, wire_name_path)
    }
}

fn should_skip_field(field_row: &JsonValue, field_key: Option<&str>, skip: &FieldSkipSpec) -> bool {
    if skip.values_in.is_empty() {
        return false;
    }
    wire_name_from_field(field_row, field_key, &skip.wire_name_path)
        .ok()
        .is_some_and(|n| skip.values_in.iter().any(|s| s == &n))
}

struct FieldCatalogEntry<'a> {
    field_row: &'a JsonValue,
    field_key: Option<&'a str>,
}

fn iter_field_catalog<'a>(
    row: &'a JsonValue,
    from: &FieldCatalogSource,
) -> Result<Vec<FieldCatalogEntry<'a>>, String> {
    match from {
        FieldCatalogSource::Array { path } => {
            if path.is_empty() {
                return Ok(vec![FieldCatalogEntry {
                    field_row: row,
                    field_key: None,
                }]);
            }
            let nested = walk_json_path(row, path)?;
            let field_rows = nested
                .as_array()
                .ok_or("dynamic_fields.from array path must resolve to a JSON array")?;
            Ok(field_rows
                .iter()
                .map(|field_row| FieldCatalogEntry {
                    field_row,
                    field_key: None,
                })
                .collect())
        }
        FieldCatalogSource::ObjectMap { path } => {
            let nested = walk_json_path(row, path)?;
            let map = nested
                .as_object()
                .ok_or("dynamic_fields.from object_map path must resolve to a JSON object")?;
            Ok(map
                .iter()
                .map(|(key, field_row)| FieldCatalogEntry {
                    field_row,
                    field_key: Some(key.as_str()),
                })
                .collect())
        }
    }
}

fn extract_wire_path_and_derive(
    env: &Environment<'_>,
    extract: &FieldExtractSpec,
    field_ctx: &JsonValue,
    wire_name: &str,
) -> Result<(Option<Vec<String>>, Option<FieldDeriveRule>), String> {
    match extract {
        FieldExtractSpec::TopLevelKey => Ok((Some(vec![wire_name.to_string()]), None)),
        FieldExtractSpec::PathSegments { segments } => {
            let path = render_path_segments(env, segments, field_ctx)?;
            Ok((Some(path), None))
        }
        FieldExtractSpec::NameValueArray {
            array_path,
            match_key_field,
            match_equals,
            value_field,
        } => {
            let equals = render_overlay_template(env, &match_equals.template, field_ctx)?;
            Ok((
                Some(array_path.clone()),
                Some(FieldDeriveRule::NameValueArrayLookup {
                    equals,
                    match_key_field: match_key_field.clone(),
                    value_field: value_field.clone(),
                    case_insensitive: false,
                }),
            ))
        }
    }
}

fn project_dynamic_fields(
    env: &Environment<'_>,
    base_cgs: &CGS,
    ent: &mut EntityDef,
    row: &JsonValue,
    df: &OverlayDynamicFieldsSpec,
) -> Result<(), String> {
    let entries = iter_field_catalog(row, &df.from)?;
    for FieldCatalogEntry {
        field_row,
        field_key,
    } in entries
    {
        if df
            .skip
            .as_ref()
            .is_some_and(|sw| should_skip_field(field_row, field_key, sw))
        {
            continue;
        }
        let wire_name = wire_name_from_field(field_row, field_key, &df.wire_name_path)?;
        let wire_type = walk_json_path_string(field_row, &df.wire_type_path)?;
        let field_ctx = match field_key {
            Some(key) => serde_json::json!({ "row": row, "field": field_row, "field_key": key }),
            None => serde_json::json!({ "row": row, "field": field_row }),
        };
        let plasm_fname = render_overlay_template(env, &df.name.template, &field_ctx)?;
        if plasm_fname.is_empty() {
            continue;
        }
        if ent
            .fields
            .contains_key(&EntityFieldName::from(plasm_fname.as_str()))
        {
            continue;
        }
        let value_ref = resolve_type_map_key(&df.type_map, df.default.as_ref(), &wire_type)?;
        let (wire_path, derive) =
            extract_wire_path_and_derive(env, &df.extract, &field_ctx, &wire_name)?;
        let field = field_from_value_ref(
            base_cgs,
            &plasm_fname,
            &value_ref,
            wire_path,
            derive,
            &format!("Dynamic overlay field {wire_name}"),
        )?;
        ent.fields
            .insert(EntityFieldName::from(plasm_fname.as_str()), field);
    }
    Ok(())
}

/// Project overlay entities from a source capability JSON response.
pub fn build_schema_overlay(
    spec: &SchemaOverlaySpec,
    base_cgs: &CGS,
    source_response: &JsonValue,
) -> Result<SchemaOverlay, String> {
    if matches!(spec.projection.mode, OverlayProjectionMode::ColumnSchema) {
        return Err("projection.mode column_schema is not implemented".to_string());
    }

    let template = base_cgs
        .get_entity(&spec.entity.from_template)
        .ok_or_else(|| {
            format!(
                "from_template entity '{}' not found",
                spec.entity.from_template
            )
        })?;

    let generator_rows = collect_overlay_generator_rows(source_response, &spec.projection)?;

    let env = overlay_template_environment();
    let mut entities = IndexMap::new();
    let mut scope_index = IndexMap::new();
    let values = IndexMap::new();

    match spec.projection.mode {
        OverlayProjectionMode::PerScopeEntity => {
            for gen in &generator_rows {
                let row_ctx = overlay_row_context(gen.row, gen.parent);
                let entity_name =
                    render_overlay_template(&env, &spec.entity.name.template, &row_ctx)?;
                if !is_valid_plasm_entity_name(&entity_name) {
                    return Err(format!(
                        "overlay entity name '{entity_name}' is not a valid Plasm identifier"
                    ));
                }
                if base_cgs
                    .entities
                    .contains_key(&EntityName::from(entity_name.as_str()))
                    || entities.contains_key(&EntityName::from(entity_name.as_str()))
                {
                    return Err(format!(
                        "overlay entity name '{entity_name}' collides with an existing entity"
                    ));
                }

                let mut aliases = Vec::new();
                for alias_spec in &spec.entity.expression_aliases {
                    aliases.push(render_overlay_template(
                        &env,
                        &alias_spec.template,
                        &row_ctx,
                    )?);
                }

                let scope_key =
                    render_overlay_template(&env, &spec.entity.scope_key.template, &row_ctx)?;
                if scope_index.contains_key(&scope_key) {
                    return Err(format!("duplicate scope_key '{scope_key}'"));
                }
                scope_index.insert(scope_key.clone(), EntityName::from(entity_name.as_str()));

                let mut ent = template.clone();
                ent.name = EntityName::from(entity_name.as_str());
                ent.expression_aliases = aliases;
                ent.description = format!("Overlay entity for {scope_key}");
                ent.abstract_entity = true;
                ent.primary_read = None;

                for (fname, sf) in &spec.entity.static_fields {
                    let _static_val = render_overlay_template(&env, &sf.template, &row_ctx)?;
                    let field = field_from_value_ref(
                        base_cgs,
                        fname,
                        &sf.value_ref,
                        None,
                        None,
                        &format!("Static overlay field {fname}"),
                    )?;
                    ent.fields
                        .insert(EntityFieldName::from(fname.as_str()), field);
                }

                if let Some(df) = &spec.entity.dynamic_fields {
                    project_dynamic_fields(&env, base_cgs, &mut ent, gen.row, df)?;
                }

                entities.insert(EntityName::from(entity_name.as_str()), ent);
            }
        }
        OverlayProjectionMode::AugmentBase => {
            let entity_name = spec.entity.from_template.clone();
            if !is_valid_plasm_entity_name(&entity_name) {
                return Err(format!(
                    "augment_base from_template '{entity_name}' is not a valid Plasm identifier"
                ));
            }

            let mut ent = template.clone();
            ent.name = EntityName::from(entity_name.as_str());
            ent.description = format!("Overlay-augmented {}", entity_name);
            ent.abstract_entity = true;
            ent.primary_read = None;

            for gen in &generator_rows {
                let row_ctx = overlay_row_context(gen.row, gen.parent);
                let scope_key =
                    render_overlay_template(&env, &spec.entity.scope_key.template, &row_ctx)?;
                scope_index
                    .entry(scope_key)
                    .or_insert_with(|| EntityName::from(entity_name.as_str()));

                for (fname, sf) in &spec.entity.static_fields {
                    if !ent
                        .fields
                        .contains_key(&EntityFieldName::from(fname.as_str()))
                    {
                        let field = field_from_value_ref(
                            base_cgs,
                            fname,
                            &sf.value_ref,
                            None,
                            None,
                            &format!("Static overlay field {fname}"),
                        )?;
                        ent.fields
                            .insert(EntityFieldName::from(fname.as_str()), field);
                    }
                }

                if let Some(df) = &spec.entity.dynamic_fields {
                    project_dynamic_fields(&env, base_cgs, &mut ent, gen.row, df)?;
                }
            }

            entities.insert(EntityName::from(entity_name.as_str()), ent);
        }
        OverlayProjectionMode::ColumnSchema => unreachable!(),
    }

    let overlay_hash = {
        let mut keys: Vec<_> = scope_index.keys().cloned().collect();
        keys.sort();
        let canonical = serde_json::json!({
            "entities": entities.keys().collect::<Vec<_>>(),
            "scope_index": keys,
        });
        let bytes = serde_json::to_vec(&canonical).map_err(|e| e.to_string())?;
        hex::encode(Sha256::digest(bytes))
    };

    Ok(SchemaOverlay {
        entities,
        values,
        scope_index,
        overlay_hash,
    })
}

fn merge_augment_base_entity(existing: &mut EntityDef, overlay: EntityDef) {
    for (fname, field) in overlay.fields {
        existing.fields.entry(fname).or_insert(field);
    }
}

impl CGS {
    /// Declarative overlay spec from `domain.yaml` (when present).
    pub fn schema_overlay_spec(&self) -> Option<&SchemaOverlaySpec> {
        self.schema_overlay.as_ref()
    }

    /// Merge a runtime overlay into a clone of this CGS and validate.
    pub fn with_overlay(&self, overlay: SchemaOverlay) -> Result<CGS, SchemaError> {
        let augment_base = self
            .schema_overlay
            .as_ref()
            .is_some_and(|spec| matches!(spec.projection.mode, OverlayProjectionMode::AugmentBase));
        let mut merged = self.clone();
        for (k, v) in overlay.values {
            if merged.values.contains_key(&k) {
                return Err(SchemaError::DuplicateField {
                    entity: "schema_overlay".into(),
                    field: k,
                });
            }
            merged.values.insert(k, v);
        }
        for (name, def) in overlay.entities {
            if augment_base {
                if let Some(existing) = merged.entities.get_mut(&name) {
                    merge_augment_base_entity(existing, def);
                    continue;
                }
            }
            if merged.entities.contains_key(&name) {
                return Err(SchemaError::DuplicateEntity {
                    name: name.to_string(),
                });
            }
            merged.entities.insert(name, def);
        }
        for (key, ent) in overlay.scope_index {
            merged.schema_overlay_scope_index.insert(key, ent);
        }
        merged.schema_overlay_hash = Some(overlay.overlay_hash);
        merged.validate()?;
        Ok(merged)
    }

    /// Session pin hash including overlay digest when present.
    pub fn effective_catalog_cgs_hash_hex(&self) -> String {
        if let Some(ref overlay_hash) = self.schema_overlay_hash {
            let mut hasher = Sha256::new();
            hasher.update(self.catalog_cgs_hash_hex().as_bytes());
            hasher.update(b":overlay:");
            hasher.update(overlay_hash.as_bytes());
            hex::encode(hasher.finalize())
        } else {
            self.catalog_cgs_hash_hex()
        }
    }

    pub(crate) fn validate_schema_overlay(&self) -> Result<(), SchemaError> {
        let Some(spec) = self.schema_overlay.as_ref() else {
            return Ok(());
        };

        if matches!(spec.projection.mode, OverlayProjectionMode::ColumnSchema) {
            return Err(SchemaError::SchemaOverlayInvalid {
                detail: "projection.mode column_schema is not implemented".into(),
            });
        }

        if spec.source.steps.is_empty() {
            if spec.source.capability.trim().is_empty() {
                return Err(SchemaError::SchemaOverlayInvalid {
                    detail: "source.capability or source.steps is required".into(),
                });
            }
            self.validate_overlay_source_capability(&spec.source.capability)?;
            for (param, template) in &spec.source.bind {
                self.validate_overlay_bind_template(&spec.source.capability, param, template)?;
            }
        } else {
            if !spec.source.capability.is_empty() || !spec.source.bind.is_empty() {
                return Err(SchemaError::SchemaOverlayInvalid {
                    detail:
                        "source.capability and source.bind must be omitted when source.steps is set"
                            .into(),
                });
            }
            let mut seen_collects = IndexMap::<String, ()>::new();
            for (idx, step) in spec.source.steps.iter().enumerate() {
                self.validate_overlay_source_capability(&step.capability)?;
                let is_collect = step.collect.is_some();
                let is_for_each = step.for_each.is_some();
                if is_collect == is_for_each {
                    return Err(SchemaError::SchemaOverlayInvalid {
                        detail: format!(
                            "source.steps[{idx}] must declare exactly one of collect or for_each"
                        ),
                    });
                }
                if is_collect {
                    let name = step.collect.as_ref().expect("collect");
                    if step.items_path.as_ref().is_none_or(|p| p.is_empty()) {
                        return Err(SchemaError::SchemaOverlayInvalid {
                            detail: format!(
                                "source.steps[{idx}] collect '{name}' requires items_path"
                            ),
                        });
                    }
                    if step.merge.is_some() || !step.bind.is_empty() {
                        return Err(SchemaError::SchemaOverlayInvalid {
                            detail: format!(
                                "source.steps[{idx}] collect step must not declare bind or merge"
                            ),
                        });
                    }
                    if seen_collects.contains_key(name) {
                        return Err(SchemaError::SchemaOverlayInvalid {
                            detail: format!("duplicate source collect name '{name}'"),
                        });
                    }
                    seen_collects.insert(name.clone(), ());
                } else {
                    let name = step.for_each.as_ref().expect("for_each");
                    if !seen_collects.contains_key(name) {
                        return Err(SchemaError::SchemaOverlayInvalid {
                            detail: format!(
                                "source.steps[{idx}] for_each '{name}' has no prior collect step"
                            ),
                        });
                    }
                    if step.merge.is_none() {
                        return Err(SchemaError::SchemaOverlayInvalid {
                            detail: format!("source.steps[{idx}] for_each step requires merge"),
                        });
                    }
                    for (param, template) in &step.bind {
                        self.validate_overlay_bind_template(&step.capability, param, template)?;
                    }
                }
            }
        }

        if let Some(nested) = &spec.projection.nested_items_path {
            if nested.is_empty() {
                return Err(SchemaError::SchemaOverlayInvalid {
                    detail: "projection.nested_items_path must be non-empty when set".into(),
                });
            }
        }

        if !self
            .entities
            .contains_key(&EntityName::from(spec.entity.from_template.as_str()))
        {
            return Err(SchemaError::SchemaOverlayInvalid {
                detail: format!(
                    "from_template entity '{}' not found",
                    spec.entity.from_template
                ),
            });
        }

        if spec.decode.scope.params.is_empty() && spec.decode.scope.key.template.contains("ambient")
        {
            return Err(SchemaError::SchemaOverlayInvalid {
                detail:
                    "decode.scope.params must be non-empty when decode.scope.key references ambient"
                        .into(),
            });
        }

        for sf in spec.entity.static_fields.values() {
            if !self.values.contains_key(&sf.value_ref) {
                return Err(SchemaError::SchemaOverlayInvalid {
                    detail: format!("static_fields value_ref '{}' not in values:", sf.value_ref),
                });
            }
        }

        if let Some(df) = &spec.entity.dynamic_fields {
            for vr in df.type_map.values() {
                if !self.values.contains_key(vr) {
                    return Err(SchemaError::SchemaOverlayInvalid {
                        detail: format!("dynamic_fields type_map value_ref '{vr}' not in values:"),
                    });
                }
            }
            if let Some(ref d) = df.default {
                if !self.values.contains_key(d) {
                    return Err(SchemaError::SchemaOverlayInvalid {
                        detail: format!("dynamic_fields default value_ref '{d}' not in values:"),
                    });
                }
            }
        }

        let env = overlay_template_environment();
        for tmpl in std::iter::once(&spec.entity.name.template)
            .chain(spec.entity.expression_aliases.iter().map(|a| &a.template))
            .chain(std::iter::once(&spec.entity.scope_key.template))
            .chain(std::iter::once(&spec.decode.scope.key.template))
        {
            env.template_from_str(tmpl)
                .map_err(|e| SchemaError::SchemaOverlayInvalid {
                    detail: format!("invalid Minijinja template '{tmpl}': {e}"),
                })?;
            crate::bind_wire_validate::validate_bind_wire_refs(
                tmpl,
                "schema_overlay Minijinja template",
            )?;
        }
        if let Some(df) = &spec.entity.dynamic_fields {
            env.template_from_str(&df.name.template).map_err(|e| {
                SchemaError::SchemaOverlayInvalid {
                    detail: format!("invalid dynamic_fields.name template: {e}"),
                }
            })?;
            if let FieldExtractSpec::NameValueArray { match_equals, .. } = &df.extract {
                env.template_from_str(&match_equals.template).map_err(|e| {
                    SchemaError::SchemaOverlayInvalid {
                        detail: format!(
                            "invalid dynamic_fields.extract.match_equals template: {e}"
                        ),
                    }
                })?;
            }
        }

        Ok(())
    }

    fn validate_overlay_source_capability(&self, capability: &str) -> Result<(), SchemaError> {
        let cap =
            self.capabilities
                .get(capability)
                .ok_or_else(|| SchemaError::SchemaOverlayInvalid {
                    detail: format!("source capability '{capability}' not found"),
                })?;
        if !matches!(
            cap.kind,
            CapabilityKind::Query | CapabilityKind::Get | CapabilityKind::Search
        ) {
            return Err(SchemaError::SchemaOverlayInvalid {
                detail: format!(
                    "source capability '{capability}' must be query, get, or search (got {:?})",
                    cap.kind
                ),
            });
        }
        Ok(())
    }

    fn validate_overlay_bind_template(
        &self,
        capability: &str,
        param: &str,
        template: &str,
    ) -> Result<(), SchemaError> {
        let cap =
            self.capabilities
                .get(capability)
                .ok_or_else(|| SchemaError::SchemaOverlayInvalid {
                    detail: format!("source capability '{capability}' not found"),
                })?;
        overlay_template_environment()
            .template_from_str(template)
            .map_err(|e| SchemaError::SchemaOverlayInvalid {
                detail: format!("invalid source bind '{param}' template: {e}"),
            })?;
        crate::bind_wire_validate::validate_bind_wire_refs(
            template,
            &format!("overlay source bind '{param}' on capability '{capability}'"),
        )?;
        let input_fields = capability_input_field_names(cap);
        if !input_fields.is_empty() && !input_fields.iter().any(|name| name == param) {
            return Err(SchemaError::SchemaOverlayInvalid {
                detail: format!(
                    "source bind param '{param}' is not declared on capability '{capability}'"
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use std::env;
    use std::path::Path;

    #[test]
    fn build_schema_overlay_from_fixture() {
        let json: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
        )))
        .expect("fixture JSON");
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/fibery_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        let overlay = build_schema_overlay(spec, &base, &json).expect("build overlay");
        assert!(overlay
            .entities
            .contains_key(&EntityName::from("Cricket__Player")));
        assert_eq!(
            overlay
                .scope_index
                .get("Cricket/Player")
                .map(|n| n.as_str()),
            Some("Cricket__Player")
        );
        let player = overlay
            .entities
            .get(&EntityName::from("Cricket__Player"))
            .unwrap();
        assert!(player
            .fields
            .contains_key(&EntityFieldName::from("Cricket_name")));
    }

    #[test]
    fn build_decode_scope_key_composite() {
        let spec = OverlayDecodeScopeSpec {
            params: vec!["project".into(), "issuetype".into()],
            key: OverlayTemplateSpec {
                template: "{{ ambient.project }}:{{ ambient.issuetype }}".into(),
            },
        };
        let mut ambient = IndexMap::new();
        ambient.insert("project".into(), "MYPROJ".into());
        ambient.insert("issuetype".into(), "Story".into());
        let key = build_decode_scope_key(&spec, &ambient).expect("scope key");
        assert_eq!(key, "MYPROJ:Story");
    }

    #[test]
    fn build_decode_scope_key_static_global() {
        let spec = OverlayDecodeScopeSpec {
            params: vec![],
            key: OverlayTemplateSpec {
                template: "global".into(),
            },
        };
        let ambient = IndexMap::new();
        let key = build_decode_scope_key(&spec, &ambient).expect("scope key");
        assert_eq!(key, "global");
    }

    #[test]
    fn build_schema_overlay_object_map() {
        let json: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/notion_schema_overlay/sample_database_search.json"
        )))
        .expect("fixture JSON");
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/notion_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        let overlay = build_schema_overlay(spec, &base, &json).expect("build overlay");
        assert!(overlay
            .entities
            .contains_key(&EntityName::from("Tasks__db_abc123")));
        let ent = overlay
            .entities
            .get(&EntityName::from("Tasks__db_abc123"))
            .unwrap();
        assert!(ent.fields.contains_key(&EntityFieldName::from("Status")));
    }

    #[test]
    fn augment_base_merges_fields_from_fixture() {
        let json = serde_json::json!({
            "fields": [
                { "name": "Priority", "type": "text" },
                { "name": "Estimate", "type": "number" }
            ]
        });
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/augment_base_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        let overlay = build_schema_overlay(spec, &base, &json).expect("build overlay");
        assert_eq!(overlay.entities.len(), 1);
        let task = overlay.entities.get(&EntityName::from("Task")).unwrap();
        assert!(task.fields.contains_key(&EntityFieldName::from("Priority")));
        assert!(task.fields.contains_key(&EntityFieldName::from("Estimate")));
    }

    #[test]
    fn augment_base_with_overlay_merges_into_existing_entity() {
        let json = serde_json::json!({
            "fields": [
                { "name": "Priority", "type": "text" },
                { "name": "Estimate", "type": "number" }
            ]
        });
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/augment_base_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        let overlay = build_schema_overlay(spec, &base, &json).expect("build overlay");
        let merged = base.with_overlay(overlay).expect("merge augment_base");
        assert_eq!(merged.entities.len(), 1);
        let task = merged.get_entity("Task").expect("Task");
        assert!(task.fields.contains_key(&EntityFieldName::from("id")));
        assert!(task.fields.contains_key(&EntityFieldName::from("Priority")));
        assert!(task.fields.contains_key(&EntityFieldName::from("Estimate")));
        assert_ne!(
            merged.catalog_cgs_hash_hex(),
            merged.effective_catalog_cgs_hash_hex()
        );
    }

    #[test]
    fn clickup_multi_step_pipeline_merges_fields_from_fixture_rows() {
        let teams: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/clickup_schema_overlay/sample_team_query.json"
        )))
        .expect("teams JSON");
        let fields_a: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/clickup_schema_overlay/sample_custom_field_query.json"
        )))
        .expect("fields JSON");
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/clickup_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        base.validate().expect("clickup overlay fixture validates");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        assert!(spec.is_multi_step_source());

        let team_rows = overlay_collect_rows(&teams, &[String::from("teams")]).expect("teams");
        let mut merged = serde_json::json!({});
        let merge = OverlayStepMergeSpec::AppendArray {
            path: vec!["fields".into()],
        };
        for row in &team_rows {
            let bind = resolve_overlay_row_bind(&spec.source.steps[1].bind, row, None, None)
                .expect("bind");
            assert_eq!(
                bind.get("team_id").map(String::as_str),
                Some(row["id"].as_str().unwrap())
            );
            overlay_merge_step_response(&mut merged, &merge, &fields_a).expect("merge");
        }
        let overlay = build_schema_overlay(spec, &base, &merged).expect("build overlay");
        let task = overlay.entities.get(&EntityName::from("Task")).unwrap();
        assert!(task
            .fields
            .contains_key(&EntityFieldName::from("Priority_Level")));
    }

    #[test]
    fn clickup_augment_base_fixture_sanitizes_field_names() {
        let json: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/clickup_schema_overlay/sample_custom_field_query.json"
        )))
        .expect("fixture JSON");
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/clickup_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        let overlay = build_schema_overlay(spec, &base, &json).expect("build overlay");
        let task = overlay.entities.get(&EntityName::from("Task")).unwrap();
        assert!(task
            .fields
            .contains_key(&EntityFieldName::from("Priority_Level")));
        assert!(task
            .fields
            .contains_key(&EntityFieldName::from("Story_Points")));
        assert!(task.fields.contains_key(&EntityFieldName::from("Blocked")));
    }

    #[test]
    fn cgs_with_overlay_merges_and_validates() {
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/fibery_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let json: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
        )))
        .expect("fixture JSON");
        let spec = base.schema_overlay.as_ref().unwrap();
        let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
        let merged = base.with_overlay(overlay).expect("merge");
        assert!(merged.get_entity("Cricket__Player").is_some());
        assert!(merged.schema_overlay_hash.is_some());
        assert_ne!(
            merged.catalog_cgs_hash_hex(),
            merged.effective_catalog_cgs_hash_hex()
        );
    }

    #[test]
    fn resolve_overlay_row_bind_from_api_row() {
        let mut bind = IndexMap::new();
        bind.insert("team_id".into(), "{{ row.id }}".into());
        let row = serde_json::json!({ "id": "777666555", "name": "Acme" });
        let resolved = resolve_overlay_row_bind(&bind, &row, None, None).expect("bind");
        assert_eq!(
            resolved.get("team_id").map(String::as_str),
            Some("777666555")
        );
    }

    #[test]
    fn overlay_merge_append_array_accumulates_fields() {
        let mut acc = serde_json::json!({});
        let merge = OverlayStepMergeSpec::AppendArray {
            path: vec!["fields".into()],
        };
        overlay_merge_step_response(
            &mut acc,
            &merge,
            &serde_json::json!({ "fields": [{ "name": "A", "type": "text" }] }),
        )
        .expect("merge");
        overlay_merge_step_response(
            &mut acc,
            &merge,
            &serde_json::json!({ "fields": [{ "name": "B", "type": "number" }] }),
        )
        .expect("merge");
        let fields = acc
            .get("fields")
            .and_then(|v| v.as_array())
            .expect("fields");
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn overlay_pipeline_cache_suffix_is_stable() {
        let a = overlay_pipeline_cache_suffix(&[serde_json::json!({"teams": []})]);
        let b = overlay_pipeline_cache_suffix(&[serde_json::json!({"teams": []})]);
        assert_eq!(a, b);
        assert!(a.starts_with(":pipeline:"));
    }

    #[test]
    fn build_schema_overlay_jira_nested_createmeta() {
        let json: JsonValue = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/schemas/jira_schema_overlay/sample_createmeta.json"
        )))
        .expect("fixture JSON");
        let base_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/jira_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("load bootstrap fixture");
        let spec = base.schema_overlay.as_ref().expect("schema_overlay spec");
        let overlay = build_schema_overlay(spec, &base, &json).expect("build overlay");
        assert!(overlay
            .entities
            .contains_key(&EntityName::from("Issue__MYPROJ__Story")));
        assert_eq!(
            overlay.scope_index.get("MYPROJ:Story").map(|n| n.as_str()),
            Some("Issue__MYPROJ__Story")
        );
        let ent = overlay
            .entities
            .get(&EntityName::from("Issue__MYPROJ__Story"))
            .unwrap();
        assert!(ent
            .fields
            .contains_key(&EntityFieldName::from("customfield_10042")));
    }

    #[test]
    fn jira_catalog_loads_with_schema_overlay() {
        let jira = Path::new("apis/jira");
        if !jira.join("domain.yaml").exists() {
            return;
        }
        let cgs = load_schema_dir(jira).expect("jira catalog loads");
        assert!(cgs.schema_overlay.is_some());
        cgs.validate().expect("jira catalog validates");
    }

    #[test]
    fn fibery_catalog_loads_with_schema_overlay() {
        let fibery = Path::new("apis/fibery");
        if !fibery.join("domain.yaml").exists() {
            return;
        }
        let cgs = load_schema_dir(fibery).expect("fibery catalog loads");
        assert!(
            cgs.schema_overlay.is_some(),
            "fibery declares schema_overlay"
        );
        cgs.validate().expect("fibery catalog validates");
    }

    #[test]
    fn notion_catalog_loads_with_schema_overlay() {
        let notion = Path::new("apis/notion");
        if !notion.join("domain.yaml").exists() {
            return;
        }
        let cgs = load_schema_dir(notion).expect("notion catalog loads");
        assert!(
            cgs.schema_overlay.is_some(),
            "notion declares schema_overlay"
        );
        cgs.validate().expect("notion catalog validates");
    }
}
