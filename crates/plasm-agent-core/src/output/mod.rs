use indexmap::IndexMap;
use plasm_core::{
    AgentPresentation, EntityName, FieldType, TypedFieldValue, Value, ValueTableCellBudget, CGS,
    PLASM_ATTACHMENT_KEY,
};
use plasm_runtime::{CachedEntity, ExecutionResult};
use std::collections::BTreeSet;

mod in_band_fidelity;
mod presentation_fields;
mod summary;

pub use in_band_fidelity::{InBandSummaryReport, SummaryFidelityLoss};
pub(crate) use presentation_fields::{lossy_summary_field_names, LossySummaryFieldNames};
pub(crate) use summary::format_result_tsv_with_cgs;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Json,
    Table,
    Compact,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Self {
        match s {
            "table" => Self::Table,
            "compact" => Self::Compact,
            _ => Self::Json,
        }
    }
}

/// Human-readable / agent summary formatting. When `cgs` is set, string fields with
/// [`AgentPresentation::ReferenceOnly`] are replaced with `(in artifact)` in table/compact (with
/// optional `mime_type_hint` from CGS appended as `(mime)`); [`AgentPresentation::Lossy`] strings
/// are capped in cells but stay full-fidelity in JSON snapshots. JSON-shaped output keeps full values.
///
/// **Attachment-shaped values:** any field whose decoded `Value` contains reserved
/// `__plasm_attachment: { uri, mime_type | media_type }` (and/or `bytes_base64`) is rendered in
/// table/TSV without inlining raw bytes. Non-blob columns use a single cell `uri (mime)` or
/// `(in artifact) (mime)` for bytes-only attachments. **CGS `field_type: blob`** fields use two
/// adjacent columns, `{field}_ref` and `{field}_mime`, so the URI (or `(in artifact)`) and MIME
/// stay visually split without duplicating the hint on the reference placeholder.
///
/// **`(in artifact)` (MCP):** this placeholder means the full string was withheld from the Markdown
/// table to save tokens—the value is still present in the **run snapshot JSON**. **`Lossy`**
/// columns may show an abbreviated cell without `(in artifact)`; the full string is likewise only
/// authoritative in the snapshot. Agents **MUST** call MCP **`resources/read`** on the `plasm://…`
/// URI from the Markdown body, `_meta.plasm.steps`, or any `resource_link` block when the tool
/// surfaces a snapshot URI for that run.
pub fn format_result_with_cgs(
    result: &ExecutionResult,
    format: OutputFormat,
    cgs: Option<&CGS>,
) -> (String, Vec<String>, InBandSummaryReport) {
    match format {
        OutputFormat::Json => (
            format_json(result),
            Vec::new(),
            InBandSummaryReport::default(),
        ),
        OutputFormat::Table => format_table_with_cgs(result, cgs),
        OutputFormat::Compact => format_compact_with_cgs(result, cgs),
    }
}

pub fn format_result(result: &ExecutionResult, format: OutputFormat) -> String {
    format_result_with_cgs(result, format, None).0
}

/// Sorted unique field names shown as `(in artifact)` in table/compact when `cgs` is set.
pub fn reference_only_omitted_field_names(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
) -> Vec<String> {
    let mut omitted = BTreeSet::new();
    let mut report = InBandSummaryReport::default();
    let _ = format_table_inner(result, cgs, &mut omitted, &mut report);
    omitted.into_iter().collect()
}

/// Strip fields not in the projection from each entity in the result.
///
/// Top-level wire keys match directly; **dotted paths** walk nested JSON/object shapes (e.g.
/// `author.login` pulls `login` from the `author` object field).
pub fn apply_projection(result: &mut ExecutionResult, fields: &[String]) {
    for entity in &mut result.entities {
        let mut next: IndexMap<String, TypedFieldValue> = IndexMap::new();
        for f in fields {
            if let Some(v) = entity.fields.get(f.as_str()) {
                next.insert(f.clone(), v.clone());
            } else if f.contains('.') {
                if let Some(v) = typed_field_value_at_dotted_path(&entity.fields, f.as_str()) {
                    next.insert(f.clone(), v);
                }
            }
        }
        entity.fields = next;
    }
}

fn typed_field_value_at_dotted_path(
    fields: &IndexMap<String, TypedFieldValue>,
    path: &str,
) -> Option<TypedFieldValue> {
    let mut segments = path.split('.');
    let first = segments.next()?;
    let mut cur = fields.get(first)?.to_value();
    for seg in segments {
        cur = match cur {
            Value::Object(m) => m.get(seg)?.clone(),
            _ => return None,
        };
    }
    Some(TypedFieldValue::from(cur))
}

/// JSON value for HTTP `POST /execute/...` bodies: entity rows only (no duration, count, or cache stats).
pub fn http_execute_results_value(result: &ExecutionResult) -> serde_json::Value {
    let entities: Vec<serde_json::Value> = result.entities.iter().map(entity_to_json).collect();
    serde_json::Value::Array(entities)
}

const REFERENCE_ONLY_PLACEHOLDER: &str = "(in artifact)";

fn try_plasm_attachment_inner(v: &Value) -> Option<&indexmap::IndexMap<String, Value>> {
    let obj = v.as_object()?;
    obj.get(PLASM_ATTACHMENT_KEY)?.as_object()
}

fn try_plasm_attachment_cell(v: &Value) -> Option<String> {
    let inner = try_plasm_attachment_inner(v)?;
    if let Some(Value::String(uri)) = inner.get("uri") {
        if !uri.is_empty() {
            let mime = inner
                .get("mime_type")
                .or_else(|| inner.get("media_type"))
                .and_then(|m| match m {
                    Value::String(s) if !s.is_empty() => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("application/octet-stream");
            return Some(format!("{uri} ({mime})"));
        }
    }
    if let Some(Value::String(b64)) = inner.get("bytes_base64") {
        if !b64.is_empty() {
            let mime = inner
                .get("mime_type")
                .or_else(|| inner.get("media_type"))
                .and_then(|m| match m {
                    Value::String(s) if !s.is_empty() => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("application/octet-stream");
            return Some(format!("{REFERENCE_ONLY_PLACEHOLDER} ({mime})"));
        }
    }
    None
}

fn field_type_is_blob(cgs: Option<&CGS>, entity_type: &EntityName, field: &str) -> bool {
    let Some(cgs) = cgs else {
        return false;
    };
    cgs.entities
        .get(entity_type.as_str())
        .and_then(|e| e.fields.get(field))
        .and_then(|fs| cgs.named_value_for_slot(fs).ok())
        .is_some_and(|nv| matches!(nv.field_type, FieldType::Blob))
}

/// Column order for agent table/TSV: fields (blob → ref+mime) + relations — no cache metadata columns.
pub(crate) fn union_entity_table_columns(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
) -> Vec<String> {
    let mut columns: Vec<String> = Vec::new();
    let mut emitted: BTreeSet<String> = BTreeSet::new();

    for entity in &result.entities {
        for key in entity.fields.keys() {
            if emitted.contains(key.as_str()) {
                continue;
            }
            let any_blob = result
                .entities
                .iter()
                .any(|e| field_type_is_blob(cgs, &e.reference.entity_type, key.as_str()));
            if any_blob {
                let kref = format!("{key}_ref");
                let kmime = format!("{key}_mime");
                columns.push(kref.clone());
                columns.push(kmime.clone());
                emitted.insert(kref);
                emitted.insert(kmime);
                emitted.insert(key.to_string());
            } else {
                columns.push(key.clone());
                emitted.insert(key.clone());
            }
        }
        for rel in entity.relations.keys() {
            if emitted.insert(rel.clone()) {
                columns.push(rel.clone());
            }
        }
    }

    columns
}

fn format_blob_ref_column_cell(
    v: Option<&Value>,
    cgs: Option<&CGS>,
    entity_type: &EntityName,
    base_field: &str,
    omitted: &mut BTreeSet<String>,
    report: Option<&mut InBandSummaryReport>,
) -> String {
    let pres = field_presentation(cgs, entity_type, base_field);
    let mime_hint = field_mime_hint(cgs, entity_type, base_field);
    let Some(v) = v else {
        return String::new();
    };

    if let Some(inner) = try_plasm_attachment_inner(v) {
        if let Some(Value::String(uri)) = inner.get("uri") {
            if !uri.is_empty() {
                if let Some(rep) = report {
                    in_band_fidelity::record_attachment_ref_summary(base_field, rep);
                }
                return uri.clone();
            }
        }
        if inner
            .get("bytes_base64")
            .is_some_and(|b| matches!(b, Value::String(s) if !s.is_empty()))
        {
            omitted.insert(base_field.to_string());
            if let Some(rep) = report {
                in_band_fidelity::record_attachment_ref_summary(base_field, rep);
            }
            return REFERENCE_ONLY_PLACEHOLDER.into();
        }
    }

    format_value_for_summary_cell_impl(v, pres, mime_hint, omitted, base_field, report, true)
}

fn format_blob_mime_column_cell(
    v: Option<&Value>,
    cgs: Option<&CGS>,
    entity_type: &EntityName,
    base_field: &str,
) -> String {
    let mime_hint = field_mime_hint(cgs, entity_type, base_field);
    let Some(v) = v else {
        return mime_hint
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or_default()
            .to_string();
    };

    if let Some(inner) = try_plasm_attachment_inner(v) {
        let from_obj = inner
            .get("mime_type")
            .or_else(|| inner.get("media_type"))
            .and_then(|m| match m {
                Value::String(s) if !s.trim().is_empty() => Some(s.as_str()),
                _ => None,
            });
        if let Some(m) = from_obj {
            return m.to_string();
        }
        if inner
            .get("bytes_base64")
            .is_some_and(|b| matches!(b, Value::String(s) if !s.is_empty()))
        {
            return mime_hint
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .unwrap_or("application/octet-stream")
                .to_string();
        }
    }

    mime_hint
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or_default()
        .to_string()
}

pub(super) fn format_summary_column_cell(
    col: &str,
    entity: &CachedEntity,
    cgs: Option<&CGS>,
    omitted: &mut BTreeSet<String>,
    report: Option<&mut InBandSummaryReport>,
) -> String {
    if col == "_ref" {
        return entity.reference.to_string();
    }
    if let Some(base) = col.strip_suffix("_ref").filter(|b| !b.is_empty()) {
        if field_type_is_blob(cgs, &entity.reference.entity_type, base) {
            let blob_val = entity.fields.get(base).map(|tf| tf.to_value());
            return format_blob_ref_column_cell(
                blob_val.as_ref(),
                cgs,
                &entity.reference.entity_type,
                base,
                omitted,
                report,
            );
        }
    }
    if let Some(base) = col.strip_suffix("_mime").filter(|b| !b.is_empty()) {
        if field_type_is_blob(cgs, &entity.reference.entity_type, base) {
            let blob_val = entity.fields.get(base).map(|tf| tf.to_value());
            return format_blob_mime_column_cell(
                blob_val.as_ref(),
                cgs,
                &entity.reference.entity_type,
                base,
            );
        }
    }

    let pres = field_presentation(cgs, &entity.reference.entity_type, col);
    let mime_hint = field_mime_hint(cgs, &entity.reference.entity_type, col);
    if let Some(v) = entity.fields.get(col) {
        let wire = v.to_value();
        return format_value_for_summary_cell_impl(
            &wire,
            pres,
            mime_hint,
            omitted,
            col,
            report,
            false,
        );
    }
    if let Some(refs) = entity.relations.get(col) {
        let parts: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
        return parts.join(", ");
    }
    String::new()
}

pub(super) fn field_mime_hint<'a>(
    cgs: Option<&'a CGS>,
    entity_type: &EntityName,
    field_name: &str,
) -> Option<&'a str> {
    let cgs = cgs?;
    let fs = cgs
        .entities
        .get(entity_type.as_str())?
        .fields
        .get(field_name)?;
    fs.mime_type_hint.as_deref()
}

pub(super) fn field_presentation(
    cgs: Option<&CGS>,
    entity_type: &EntityName,
    field_name: &str,
) -> Option<AgentPresentation> {
    let cgs = cgs?;
    let ent = cgs.entities.get(entity_type.as_str())?;
    let fs = ent.fields.get(field_name)?;
    let nv = cgs.named_value_for_slot(fs).ok()?;
    if matches!(nv.field_type, FieldType::String | FieldType::Blob) {
        Some(fs.effective_agent_presentation(cgs))
    } else {
        None
    }
}

fn format_value_for_summary_cell_impl(
    v: &Value,
    presentation: Option<AgentPresentation>,
    mime_hint: Option<&str>,
    omitted: &mut BTreeSet<String>,
    field_name: &str,
    report: Option<&mut InBandSummaryReport>,
    omit_mime_suffix_on_reference_placeholder: bool,
) -> String {
    if let Some(cell) = try_plasm_attachment_cell(v) {
        if let Some(rep) = report {
            in_band_fidelity::record_attachment_ref_summary(field_name, rep);
        }
        return cell;
    }

    let out = match presentation {
        Some(AgentPresentation::ReferenceOnly) => {
            omitted.insert(field_name.to_string());
            if omit_mime_suffix_on_reference_placeholder {
                REFERENCE_ONLY_PLACEHOLDER.into()
            } else {
                match mime_hint.map(str::trim).filter(|m| !m.is_empty()) {
                    Some(m) => format!("{REFERENCE_ONLY_PLACEHOLDER} ({m})"),
                    None => REFERENCE_ONLY_PLACEHOLDER.into(),
                }
            }
        }
        Some(AgentPresentation::Lossy) => v.format_for_table_cell(&ValueTableCellBudget {
            max_total_len: 72,
            ..Default::default()
        }),
        Some(AgentPresentation::Default) | None => {
            if let Value::String(s) = v {
                if s.chars().count() > 256 {
                    omitted.insert(field_name.to_string());
                    if let Some(rep) = report {
                        in_band_fidelity::record_value_cell_fidelity(
                            v,
                            presentation,
                            field_name,
                            REFERENCE_ONLY_PLACEHOLDER,
                            rep,
                        );
                    }
                    return REFERENCE_ONLY_PLACEHOLDER.into();
                }
            }
            v.format_for_table_cell(&ValueTableCellBudget::default())
        }
    };
    if let Some(rep) = report {
        in_band_fidelity::record_value_cell_fidelity(v, presentation, field_name, &out, rep);
    }
    out
}

fn format_json(result: &ExecutionResult) -> String {
    let entities: Vec<serde_json::Value> = result.entities.iter().map(entity_to_json).collect();

    serde_json::to_string_pretty(&serde_json::json!({
        "count": result.count,
        "source": format!("{:?}", result.source),
        "results": entities,
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn format_compact_with_cgs(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
) -> (String, Vec<String>, InBandSummaryReport) {
    let mut omitted = BTreeSet::new();
    let lines: Vec<String> = result
        .entities
        .iter()
        .map(|e| {
            let v = entity_to_json_with_cgs(e, cgs, &mut omitted);
            serde_json::to_string(&v).unwrap_or_default()
        })
        .collect();
    (
        lines.join("\n"),
        omitted.into_iter().collect(),
        InBandSummaryReport::default(),
    )
}

fn entity_to_json_with_cgs(
    entity: &CachedEntity,
    cgs: Option<&CGS>,
    omitted: &mut BTreeSet<String>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in &entity.fields {
        let pres = field_presentation(cgs, &entity.reference.entity_type, k);
        let mime_hint = field_mime_hint(cgs, &entity.reference.entity_type, k);
        let wire = v.to_value();
        let out_val = match pres {
            Some(AgentPresentation::ReferenceOnly) => {
                if let Some(cell) = try_plasm_attachment_cell(&wire) {
                    serde_json::Value::String(cell)
                } else {
                    omitted.insert(k.clone());
                    let text = match mime_hint.map(str::trim).filter(|m| !m.is_empty()) {
                        Some(m) => format!("{REFERENCE_ONLY_PLACEHOLDER} ({m})"),
                        None => REFERENCE_ONLY_PLACEHOLDER.into(),
                    };
                    serde_json::Value::String(text)
                }
            }
            Some(AgentPresentation::Lossy) => {
                serde_json::Value::String(wire.format_for_table_cell(&ValueTableCellBudget {
                    max_total_len: 72,
                    ..Default::default()
                }))
            }
            _ => serde_json::to_value(&wire).unwrap_or(serde_json::Value::Null),
        };
        map.insert(k.clone(), out_val);
    }
    for (k, refs) in &entity.relations {
        map.insert(
            k.clone(),
            serde_json::Value::Array(
                refs.iter()
                    .map(|r| serde_json::Value::String(r.to_string()))
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(map)
}

fn format_table_with_cgs(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
) -> (String, Vec<String>, InBandSummaryReport) {
    let mut omitted = BTreeSet::new();
    let mut report = InBandSummaryReport::default();
    let text = format_table_inner(result, cgs, &mut omitted, &mut report);
    (text, omitted.into_iter().collect(), report)
}

fn format_table_inner(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    omitted: &mut BTreeSet<String>,
    report: &mut InBandSummaryReport,
) -> String {
    if result.entities.is_empty() {
        return "(no results)".into();
    }

    let columns = union_entity_table_columns(result, cgs);

    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    let rows: Vec<Vec<String>> = result
        .entities
        .iter()
        .map(|entity| {
            columns
                .iter()
                .enumerate()
                .map(|(i, col)| {
                    let val = format_summary_column_cell(
                        col.as_str(),
                        entity,
                        cgs,
                        omitted,
                        Some(report),
                    );
                    if val.len() > widths[i] {
                        widths[i] = val.len();
                    }
                    val
                })
                .collect()
        })
        .collect();

    let mut out = String::new();

    let header: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
        .collect();
    out.push_str(&header.join("  "));
    out.push('\n');

    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    out.push_str(&sep.join("  "));
    out.push('\n');

    for row in &rows {
        let formatted: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, val)| format!("{:<width$}", val, width = widths[i]))
            .collect();
        out.push_str(&formatted.join("  "));
        out.push('\n');
    }

    out
}

fn entity_to_json(entity: &CachedEntity) -> serde_json::Value {
    entity.payload_to_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use plasm_compile::DecodedRelation;
    use plasm_core::{
        AgentPresentation, CapabilityKind, CapabilityMapping, CapabilitySchema, FieldSchema,
        FieldType, FieldValueKind, NamedValueSchema, Ref, ResourceSchema, StringSemantics,
        ValueDomainKey, PLASM_ATTACHMENT_KEY,
    };
    use plasm_runtime::{ExecutionSource, ExecutionStats};

    use super::in_band_fidelity::SummaryFidelityLoss;

    fn tiny_cgs_markdown_reference_only() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "out_note_id".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Markdown),
                array_items: None,
            },
        );
        cgs.add_resource(ResourceSchema {
            name: "Note".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![FieldSchema {
                name: "id".into(),
                kind: FieldValueKind::Registry(ValueDomainKey::new("out_note_id").expect("key")),
                description: String::new(),
                required: true,
                agent_presentation: Some(AgentPresentation::ReferenceOnly),
                mime_type_hint: None,
                attachment_media: None,
                wire_path: None,
                derive: None,
            }],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .expect("resource");
        cgs
            .add_capability(CapabilitySchema {
                name: "note_query".into(),
                description: String::new(),
                kind: CapabilityKind::Query,
                domain: "Note".into(),
                mapping: CapabilityMapping {
                    template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "notes"}]}).into(),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: None,
            })
            .expect("capability");
        cgs.validate().expect("validate");
        cgs
    }

    fn tiny_cgs_lossy_desc() -> CGS {
        let mut cgs = CGS::new();
        for (k, sem) in [
            ("out_spell_id", StringSemantics::Short),
            ("out_spell_name", StringSemantics::Short),
            ("out_spell_desc", StringSemantics::Document),
        ] {
            cgs.values.insert(
                k.into(),
                NamedValueSchema {
                    description: String::new(),
                    field_type: FieldType::String,
                    value_format: None,
                    allowed_values: None,
                    string_semantics: Some(sem),
                    array_items: None,
                },
            );
        }
        cgs.add_resource(ResourceSchema {
            name: "Spell".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("out_spell_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("out_spell_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "desc".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("out_spell_desc").expect("key"),
                    ),
                    description: String::new(),
                    required: false,
                    agent_presentation: Some(AgentPresentation::Lossy),
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .expect("resource");
        cgs.add_capability(CapabilitySchema {
            name: "spell_get".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Spell".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "spells"}]}).into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
        })
        .expect("capability");
        cgs.validate().expect("validate");
        cgs
    }

    #[test]
    fn lossy_summary_field_names_lists_lossy_columns() {
        let cgs = tiny_cgs_lossy_desc();
        let r = Ref {
            entity_type: "Spell".into(),
            key: plasm_core::EntityKey::Simple("wind-wall".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("id".into(), Value::String("wind-wall".into()));
        fields.insert("name".into(), Value::String("Wind Wall".into()));
        fields.insert("desc".into(), Value::String("long text ".repeat(20)));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let lossy = lossy_summary_field_names(&result, Some(&cgs));
        assert_eq!(lossy.as_slice(), &["desc".to_string()]);
    }

    #[test]
    fn reference_only_table_shows_placeholder_and_tracks_field() {
        let cgs = tiny_cgs_markdown_reference_only();
        let r = Ref {
            entity_type: "Note".into(),
            key: plasm_core::EntityKey::Simple("1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("id".into(), Value::String("very long markdown body".into()));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let (s, omitted, _) = format_result_with_cgs(&result, OutputFormat::Table, Some(&cgs));
        assert!(s.contains(REFERENCE_ONLY_PLACEHOLDER), "{}", s);
        assert_eq!(omitted, vec!["id".to_string()]);

        let (tsv, omitted_tsv, _) = format_result_tsv_with_cgs(&result, Some(&cgs));
        assert!(tsv.contains(REFERENCE_ONLY_PLACEHOLDER), "{}", tsv);
        assert_eq!(omitted_tsv, omitted);
    }

    #[test]
    fn reference_only_with_mime_hint_includes_mime_in_table_cell() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "out_file_id".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "out_file_content".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Blob,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        cgs.add_resource(ResourceSchema {
            name: "File".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("out_file_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "content".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("out_file_content").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: Some("application/pdf".into()),
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .expect("resource");
        cgs
            .add_capability(CapabilitySchema {
                name: "file_get".into(),
                description: String::new(),
                kind: CapabilityKind::Get,
                domain: "File".into(),
                mapping: CapabilityMapping {
                    template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "f"}]}).into(),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: None,
            })
            .expect("capability");
        cgs.validate().expect("validate");

        let r = Ref {
            entity_type: "File".into(),
            key: plasm_core::EntityKey::Simple("1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("id".into(), Value::String("1".into()));
        fields.insert(
            "content".into(),
            Value::String("%PDF-1.4 binary ".repeat(40)),
        );
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let (s, omitted, _) = format_result_with_cgs(&result, OutputFormat::Table, Some(&cgs));
        let lines: Vec<&str> = s.lines().collect();
        let header = lines.first().copied().unwrap_or_default();
        assert!(
            header.contains("content_ref") && header.contains("content_mime"),
            "expected split blob headers, got: {header}"
        );
        assert!(
            s.contains("(in artifact)") && s.contains("application/pdf"),
            "expected ref placeholder and CGS mime column: {s}"
        );
        assert!(
            !s.contains("(in artifact) (application/pdf)"),
            "mime must not duplicate on the ref column: {s}"
        );
        assert!(omitted.contains(&"content".to_string()));
    }

    #[test]
    fn plasm_attachment_object_renders_uri_mime_and_skips_omitted_list() {
        let r = Ref {
            entity_type: "Doc".into(),
            key: plasm_core::EntityKey::Simple("a1".into()),
        };
        let mut inner = IndexMap::new();
        inner.insert(
            "uri".into(),
            Value::String("plasm://execute/ph/s1/run/r1".into()),
        );
        inner.insert("mime_type".into(), Value::String("image/png".into()));
        let mut att = IndexMap::new();
        att.insert(PLASM_ATTACHMENT_KEY.to_string(), Value::Object(inner));
        let mut fields = IndexMap::new();
        fields.insert("preview".into(), Value::Object(att));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let (s, omitted, report) = format_result_with_cgs(&result, OutputFormat::Table, None);
        assert!(s.contains("plasm://execute/ph/s1/run/r1"), "{}", s);
        assert!(s.contains("image/png"), "{}", s);
        assert!(!omitted.iter().any(|c| c == "preview"));
        assert_eq!(
            report.loss_for("preview"),
            Some(SummaryFidelityLoss::AttachmentRefSummary)
        );
    }

    #[test]
    fn format_result_tsv_tabs_without_reference_only() {
        let r = Ref {
            entity_type: "Note".into(),
            key: plasm_core::EntityKey::Simple("1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("id".into(), Value::String("n1".into()));
        fields.insert("title".into(), Value::String("a\tb".into()));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let (tsv, omitted, report) = format_result_tsv_with_cgs(&result, None);
        assert!(omitted.is_empty(), "{omitted:?}");
        assert!(
            !report.any_loss(),
            "short cells should not record fidelity loss: {report:?}"
        );
        let header = tsv.lines().next().expect("header");
        assert!(header.contains('\t'), "two columns: {header}");
        let row1 = tsv.lines().nth(1).expect("row");
        assert!(row1.contains('\t'), "two cells: {row1}");
        assert!(
            row1.ends_with("a b") || row1.contains("\ta b") || row1.contains("a b\t"),
            "inner tab collapsed to space: {row1}"
        );
    }

    #[test]
    fn tsv_long_string_without_cgs_records_default_budget_clamp() {
        let r = Ref {
            entity_type: "Spell".into(),
            key: plasm_core::EntityKey::Simple("s1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("desc".into(), Value::String("word ".repeat(80)));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let (tsv, omitted, report) = format_result_tsv_with_cgs(&result, None);
        assert!(omitted.iter().any(|c| c == "desc"), "{omitted:?}");
        assert!(tsv.contains("(in artifact)"), "{tsv}");
        assert!(
            report.loss_for("desc").is_some(),
            "expected fidelity loss for long desc: {report:?}"
        );
    }

    #[test]
    fn table_long_string_without_cgs_uses_in_artifact_placeholder() {
        let r = Ref {
            entity_type: "Note".into(),
            key: plasm_core::EntityKey::Simple("1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("body".into(), Value::String("x".repeat(300)));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
        };
        let (table, omitted, _) = format_result_with_cgs(&result, OutputFormat::Table, None);
        assert!(omitted.iter().any(|c| c == "body"), "{omitted:?}");
        assert!(table.contains("(in artifact)"), "{table}");
    }
}
