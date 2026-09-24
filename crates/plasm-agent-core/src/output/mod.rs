use indexmap::IndexMap;
use plasm_core::{
    AgentPresentation, EntityName, FieldType, TypedFieldValue, Value, ValueTableCellBudget, CGS,
    PLASM_ATTACHMENT_KEY,
};
use plasm_runtime::{CachedEntity, ExecutionResult, OperationAck};
use std::collections::BTreeSet;

mod in_band_fidelity;
mod presentation_fields;
mod summary;

pub use in_band_fidelity::{InBandSummaryReport, SummaryFidelityLoss};
pub(crate) use presentation_fields::{lossy_summary_field_names, LossySummaryFieldNames};
pub(crate) use summary::{format_result_tsv_with_cgs, format_result_tsv_with_full_fidelity_cgs};

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
/// **Unavailable detail fields:** after a hydrate soft-fail, fields listed on
/// [`CachedEntity::unavailable_fields`] render as `(unavailable)` — not a blank cell — so retained
/// summary rows are not mistaken for present-empty content.
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
    let _ = format_table_inner(result, cgs, None, &mut omitted, &mut report);
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

/// Empty row body. Operations are a sibling field — never substituted here.
pub(crate) fn format_empty_result_body(_result: &ExecutionResult) -> String {
    "(no results)".into()
}

/// HTTP-2 wire object: `rows` plus `operations`. Never a bare entity array.
pub fn http_execute_results_value(result: &ExecutionResult) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = result.entities.iter().map(entity_to_json).collect();
    let operations: Vec<serde_json::Value> = result
        .operations
        .entries()
        .iter()
        .map(operation_ack_to_json)
        .collect();
    serde_json::json!({
        "rows": rows,
        "operations": operations,
        "coverage": result.coverage.as_str(),
    })
}

fn operation_ack_to_json(ack: &OperationAck) -> serde_json::Value {
    serde_json::json!({
        "entry_id": ack.entry_id,
        "capability": ack.capability,
        "entity": ack.entity,
        "logical_invocations": ack.logical_invocations,
        "completed": ack.completed,
        "failed": ack.failed,
        "source": ack.source.as_wire_str(),
        "description": ack.description,
        "outcomes": ack.outcomes,
    })
}

/// Derived presentation for nonempty ledgers. Does not say "applied".
pub(crate) fn format_operations_block(result: &ExecutionResult) -> String {
    if result.operations.is_empty() {
        return String::new();
    }
    let mut out = String::from("\noperations:\n");
    let mut any_failed = false;
    for ack in result.operations.entries() {
        any_failed |= ack.failed > 0;
        out.push_str(&format!(
            "- capability=`{}` entity=`{}` completed={} failed={} invocations={} source={} — {}\n",
            ack.capability,
            ack.entity,
            ack.completed,
            ack.failed,
            ack.logical_invocations,
            ack.source.as_wire_str(),
            ack.description,
        ));
        for outcome in &ack.outcomes {
            let error = outcome
                .error
                .as_deref()
                .map(|message| format!(" — {message}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  - row={} identity=`{}` status={}{}\n",
                outcome.source_index,
                outcome.source_identity.as_deref().unwrap_or("unknown"),
                outcome.status.as_wire_str(),
                error
            ));
        }
    }
    if any_failed {
        out.push_str("Completed operations are recorded; this result does not imply rollback.\n");
    }
    out
}

pub(crate) const REFERENCE_ONLY_PLACEHOLDER: &str = "(in artifact)";
/// Soft-fail hydrate: requested detail field was not obtained (not present-empty).
pub(crate) const UNAVAILABLE_FIELD_PLACEHOLDER: &str = "(unavailable)";

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
    max_entity_rows: Option<usize>,
) -> Vec<String> {
    let entities: &[plasm_runtime::CachedEntity] = match max_entity_rows {
        Some(max) => {
            let end = result.entities.len().min(max);
            &result.entities[..end]
        }
        None => &result.entities,
    };
    let mut columns: Vec<String> = Vec::new();
    let mut emitted: BTreeSet<String> = BTreeSet::new();

    for entity in entities {
        let ent_def = cgs.and_then(|g| g.get_entity(entity.reference.entity_type.as_str()));
        for key in entity.fields.keys().chain(entity.unavailable_fields.iter()) {
            if emitted.contains(key.as_str()) {
                continue;
            }
            if let Some(ent) = ent_def {
                if !ent.fields.contains_key(key.as_str()) {
                    continue;
                }
            }
            let any_blob = entities
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
            if let Some(ent) = ent_def {
                if !ent.relations.contains_key(rel.as_str()) {
                    continue;
                }
            }
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
    if entity.unavailable_fields.contains(col) {
        return UNAVAILABLE_FIELD_PLACEHOLDER.to_string();
    }
    if let Some(base) = col.strip_suffix("_ref").filter(|b| !b.is_empty()) {
        if entity.unavailable_fields.contains(base) {
            return UNAVAILABLE_FIELD_PLACEHOLDER.to_string();
        }
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
        if entity.unavailable_fields.contains(base) {
            return UNAVAILABLE_FIELD_PLACEHOLDER.to_string();
        }
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
            &wire, pres, mime_hint, omitted, col, report, false,
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
                if summary_string_needs_full_fidelity_restore(presentation, s) {
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

/// Default-presentation strings longer than this become `(in artifact)` in summary cells.
pub(crate) const SUMMARY_DEFAULT_STRING_OMIT_CHARS: usize = 256;

/// Whether a summary cell would drop/truncate this string — and thus needs restore under full fidelity.
pub(crate) fn summary_string_needs_full_fidelity_restore(
    presentation: Option<AgentPresentation>,
    s: &str,
) -> bool {
    match presentation {
        Some(AgentPresentation::ReferenceOnly) | Some(AgentPresentation::Lossy) => true,
        Some(AgentPresentation::Default) | None => {
            s.chars().count() > SUMMARY_DEFAULT_STRING_OMIT_CHARS
        }
    }
}

/// Encoded UTF-8 bytes of all string cells admitted by full-fidelity MCP rendering.
/// Budget the actual escaped representation, including short strings and layout.
pub(crate) fn summary_sensitive_string_bytes(
    result: &ExecutionResult,
    _cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
) -> usize {
    result
        .entities
        .iter()
        .take(max_entity_rows.unwrap_or(usize::MAX))
        .flat_map(|entity| {
            entity.fields.iter().filter_map(|(_field, value)| {
                let Value::String(s) = value.to_value() else {
                    return None;
                };
                Some(
                    serde_json::to_string(&s)
                        .expect("string serialization")
                        .len(),
                )
            })
        })
        .sum()
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
    format_result_table_with_cgs(result, cgs, None)
}

/// ASCII table with optional in-band entity row cap (same omission rules as [`format_result_tsv_with_cgs`]).
pub(crate) fn format_result_table_with_cgs(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
) -> (String, Vec<String>, InBandSummaryReport) {
    let mut omitted = BTreeSet::new();
    let mut report = InBandSummaryReport::default();
    let text = format_table_inner(result, cgs, max_entity_rows, &mut omitted, &mut report);
    (text, omitted.into_iter().collect(), report)
}

pub(crate) fn format_table_inner(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
    omitted: &mut BTreeSet<String>,
    report: &mut InBandSummaryReport,
) -> String {
    if result.entities.is_empty() {
        let mut body = format_empty_result_body(result);
        body.push_str(&format_operations_block(result));
        return body;
    }

    let columns = union_entity_table_columns(result, cgs, max_entity_rows);

    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    let row_limit = max_entity_rows.unwrap_or(usize::MAX);
    let rows: Vec<Vec<String>> = result
        .entities
        .iter()
        .take(row_limit)
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

    out.push_str(&format_operations_block(result));
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
    use plasm_core::value_domain::{ProfileId, ValueDomain};
    use plasm_core::{
        AgentPresentation, CapabilityKind, CapabilityMapping, CapabilitySchema, FieldSchema,
        FieldType, FieldValueKind, NamedValueSchema, Ref, ResourceSchema, ValueDomainKey,
        PLASM_ATTACHMENT_KEY,
    };
    use plasm_runtime::{ExecutionSource, ExecutionStats, ResultCoverage};
    use std::collections::BTreeMap;

    use super::in_band_fidelity::SummaryFidelityLoss;

    fn tiny_cgs_markdown_reference_only() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "out_note_id".into(),
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::String,
                    None,
                    Some(ProfileId::Markdown),
                    None,
                    None,
                ),
                None,
            ),
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
                data_class: None,
                attachment_media: None,
                wire_path: None,
                derive: None,
                currency_field: None,
            }],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .expect("resource");
        cgs
            .add_capability(CapabilitySchema {
                name: "note_query".into(),
                description: String::new(),
                kind: CapabilityKind::Query,
                domain: "Note".into(),
                identity_key: None,
            invalidates_entities: vec![],
                mapping: Some(CapabilityMapping {
                    template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "notes"}]}).into(),
                }),
                derived: None,
                inputs: Default::default(),
                output_schema: None,
                provides: vec![],
                sanitizes: vec![],
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: None,
                deterministic: None,
            })
            .expect("capability");
        cgs.validate().expect("validate");
        cgs
    }

    fn tiny_cgs_lossy_desc() -> CGS {
        let mut cgs = CGS::new();
        for (k, profile) in [
            ("out_spell_id", None),
            ("out_spell_name", None),
            ("out_spell_desc", Some(ProfileId::Document)),
        ] {
            cgs.values.insert(
                k.into(),
                NamedValueSchema::from_domain(
                    String::new(),
                    ValueDomain::from_legacy(&FieldType::String, None, profile, None, None),
                    None,
                ),
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
                    data_class: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    currency_field: None,
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
                    data_class: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    currency_field: None,
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
                    data_class: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    currency_field: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .expect("resource");
        cgs.add_capability(CapabilitySchema {
            name: "spell_get".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Spell".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "spells"}]}).into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            sanitizes: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            deterministic: None,
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let lossy = lossy_summary_field_names(&result, Some(&cgs));
        assert_eq!(lossy.as_slice(), &["desc".to_string()]);
    }

    #[test]
    fn result_tsv_and_column_schema_cannot_name_untaught_field() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/return_projection_teaching");
        let cgs = plasm_core::loader::load_schema_dir(&dir).expect("return_projection_teaching");
        let taught =
            CGS::default_ordered_entity_field_names(cgs.get_entity("Notice").expect("Notice"));
        let r = Ref {
            entity_type: "Notice".into(),
            key: plasm_core::EntityKey::Simple("n1".into()),
        };
        let mut fields = IndexMap::new();
        for name in &taught {
            fields.insert(name.clone(), Value::String(format!("v-{name}")));
        }
        fields.insert("shadow_col".into(), Value::String("rogue".into()));
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let cols = union_entity_table_columns(&result, Some(&cgs), None);
        assert!(
            !cols.iter().any(|c| c == "shadow_col"),
            "dump must not name a field the card omitted: {cols:?}"
        );
        for name in &taught {
            assert!(
                cols.iter().any(|c| c == name),
                "dump must include taught field {name}: {cols:?}"
            );
        }
        let (tsv, _, _) = format_result_tsv_with_cgs(&result, Some(&cgs), None);
        assert!(
            !tsv.split('\n').next().unwrap_or("").contains("shadow_col"),
            "TSV header must not name shadow_col:\n{tsv}"
        );
        let schema = crate::run_ui_column_schema::build_run_step_column_schema(
            &result,
            Some(&cgs),
            Some("default"),
            Some("Notice"),
        )
        .expect("column schema");
        assert!(
            schema.columns.iter().all(|c| c["name"] != "shadow_col"),
            "binding column_schema must not name shadow_col: {:?}",
            schema.columns
        );
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let (s, omitted, _) = format_result_with_cgs(&result, OutputFormat::Table, Some(&cgs));
        assert!(s.contains(REFERENCE_ONLY_PLACEHOLDER), "{}", s);
        assert_eq!(omitted, vec!["id".to_string()]);

        let (tsv, omitted_tsv, _) = format_result_tsv_with_cgs(&result, Some(&cgs), None);
        assert!(tsv.contains(REFERENCE_ONLY_PLACEHOLDER), "{}", tsv);
        assert_eq!(omitted_tsv, omitted);
    }

    #[test]
    fn reference_only_with_mime_hint_includes_mime_in_table_cell() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "out_file_id".into(),
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
                None,
            ),
        );
        cgs.values.insert(
            "out_file_content".into(),
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::Blob, None, None, None, None),
                None,
            ),
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
                    data_class: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    currency_field: None,
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
                    data_class: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                    currency_field: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .expect("resource");
        cgs
            .add_capability(CapabilitySchema {
                name: "file_get".into(),
                description: String::new(),
                kind: CapabilityKind::Get,
                domain: "File".into(),
                identity_key: None,
            invalidates_entities: vec![],
                mapping: Some(CapabilityMapping {
                    template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "f"}]}).into(),
                }),
                derived: None,
                inputs: Default::default(),
                output_schema: None,
                provides: vec![],
                sanitizes: vec![],
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: None,
                deterministic: None,
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
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
        // Synthetic MIME columns obey the same lossless cell codec as stored values.
        let mime = "application/pdf\tprofile=example\nrevision=1";
        cgs.entities
            .get_mut("File")
            .unwrap()
            .fields
            .get_mut("content")
            .unwrap()
            .mime_type_hint = Some(mime.into());
        let (tsv, _, _) = format_result_tsv_with_cgs(&result, Some(&cgs), None);
        let lines = tsv.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let cells = lines[0]
            .split('\t')
            .zip(lines[1].split('\t'))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            serde_json::from_str::<String>(cells["content_mime"]).unwrap(),
            mime
        );
        assert_eq!(cells["content_ref"], REFERENCE_ONLY_PLACEHOLDER);
        let mut unavailable = result;
        unavailable.entities[0]
            .unavailable_fields
            .insert("content".into());
        let (tsv, _, _) = format_result_tsv_with_cgs(&unavailable, Some(&cgs), None);
        let lines = tsv.lines().collect::<Vec<_>>();
        let cells = lines[0]
            .split('\t')
            .zip(lines[1].split('\t'))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(cells["content_mime"], UNAVAILABLE_FIELD_PLACEHOLDER);
        assert_eq!(cells["content_ref"], UNAVAILABLE_FIELD_PLACEHOLDER);
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let (tsv, omitted, report) = format_result_tsv_with_cgs(&result, None, None);
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
            row1.split('\t')
                .any(|cell| serde_json::from_str::<String>(cell).ok().as_deref() == Some("a\tb")),
            "inner tab must round trip: {row1}"
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let (tsv, omitted, report) = format_result_tsv_with_cgs(&result, None, None);
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
            coverage: ResultCoverage::Unknown,
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
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let (table, omitted, _) = format_result_with_cgs(&result, OutputFormat::Table, None);
        assert!(omitted.iter().any(|c| c == "body"), "{omitted:?}");
        assert!(table.contains("(in artifact)"), "{table}");
    }

    fn ack(
        capability: &str,
        completed: usize,
        failed: usize,
        source: ExecutionSource,
    ) -> plasm_runtime::OperationAck {
        plasm_runtime::OperationAck {
            entry_id: "langmatrix".into(),
            capability: capability.into(),
            entity: "LangItem".into(),
            logical_invocations: completed.saturating_add(failed),
            completed,
            failed,
            source,
            description: "Records a ping against the item (matrix conformance).".into(),
            outcomes: Vec::new(),
        }
    }

    fn result_with_operations(
        operations: plasm_runtime::OperationLedger,
        source: ExecutionSource,
    ) -> ExecutionResult {
        ExecutionResult {
            entities: vec![],
            count: 0,
            has_more: false,
            coverage: ResultCoverage::Unknown,
            pagination_resume: None,
            paging_handle: None,
            source,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
            operations,
        }
    }

    #[test]
    fn http_wire_is_rows_and_operations_object() {
        let wire = http_execute_results_value(&result_with_operations(
            plasm_runtime::OperationLedger::empty(),
            ExecutionSource::Cache,
        ));
        assert!(wire.is_object(), "HTTP-2 forbids a bare row array: {wire}");
        assert_eq!(wire["rows"], serde_json::json!([]));
        assert_eq!(wire["operations"], serde_json::json!([]));
        assert_eq!(wire["coverage"], serde_json::json!("unknown"));
    }

    #[test]
    fn empty_query_has_no_operations_block() {
        let result = result_with_operations(
            plasm_runtime::OperationLedger::empty(),
            ExecutionSource::Live,
        );
        let body = format_table_inner(
            &result,
            None,
            None,
            &mut BTreeSet::new(),
            &mut InBandSummaryReport::default(),
        );
        assert_eq!(body, "(no results)");
        assert!(!body.contains("operations:"));
        let wire = http_execute_results_value(&result);
        assert_eq!(wire["rows"], serde_json::json!([]));
        assert_eq!(wire["operations"], serde_json::json!([]));
    }

    #[test]
    fn empty_iteration_and_no_row_success_are_distinct() {
        let empty_loop = result_with_operations(
            plasm_runtime::OperationLedger::from_ack(plasm_runtime::OperationAck::empty_iteration(
                "langmatrix",
                "LangItem",
                "langitem_ping",
                "Records a ping against the item (matrix conformance).",
                ExecutionSource::Cache,
            )),
            ExecutionSource::Cache,
        );
        let completed = result_with_operations(
            plasm_runtime::OperationLedger::from_ack(ack(
                "langitem_ping",
                1,
                0,
                ExecutionSource::Live,
            )),
            ExecutionSource::Live,
        );
        let empty_md = format_operations_block(&empty_loop);
        let done_md = format_operations_block(&completed);
        assert!(empty_md.contains("invocations=0"), "{empty_md}");
        assert!(empty_md.contains("completed=0"), "{empty_md}");
        assert!(done_md.contains("completed=1"), "{done_md}");
        assert!(!done_md.contains("applied"), "{done_md}");
        assert_eq!(
            http_execute_results_value(&empty_loop)["operations"][0]["logical_invocations"],
            0
        );
        assert_eq!(
            http_execute_results_value(&completed)["operations"][0]["completed"],
            1
        );
    }

    #[test]
    fn partial_failure_wire_keeps_completed_and_forbids_rollback_claim() {
        let mut ledger = plasm_runtime::OperationLedger::from_ack(ack(
            "langitem_delete",
            2,
            0,
            ExecutionSource::Live,
        ));
        ledger.merge_ack(ack("langitem_delete", 0, 1, ExecutionSource::Live));
        ledger.merge_ack(ack("langitem_ping", 1, 0, ExecutionSource::Live));
        let result = result_with_operations(ledger, ExecutionSource::Live);
        let wire = http_execute_results_value(&result);
        assert_eq!(wire["rows"], serde_json::json!([]));
        assert_eq!(wire["operations"].as_array().map(|a| a.len()), Some(2));
        let delete = wire["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["capability"] == "langitem_delete")
            .expect("delete");
        assert_eq!(delete["completed"], 2);
        assert_eq!(delete["failed"], 1);
        let md = format_operations_block(&result);
        assert!(md.contains("does not imply rollback"), "{md}");
        assert!(!md.contains("applied"), "{md}");
    }

    #[test]
    fn replay_source_is_recorded_not_live() {
        let result = result_with_operations(
            plasm_runtime::OperationLedger::from_ack(ack(
                "langitem_ping",
                1,
                0,
                ExecutionSource::Replay,
            )),
            ExecutionSource::Replay,
        );
        let wire = http_execute_results_value(&result);
        assert_eq!(wire["operations"][0]["source"], "replay");
        assert!(format_operations_block(&result).contains("source=replay"));
    }

    /// Soft-fail retained summary: unavailable detail must not render as a blank / empty cell.
    #[test]
    fn soft_fail_unavailable_detail_not_blank_or_empty_in_tsv() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = plasm_core::loader::load_schema_dir(&dir).expect("language matrix");
        let r = Ref {
            entity_type: "LangSecuredNote".into(),
            key: plasm_core::EntityKey::Simple("1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("note_id".into(), Value::String("1".into()));
        fields.insert("title".into(), Value::String("kept summary".into()));
        // Present empty body (backend returned "") — distinct from unavailable.
        let empty_present = CachedEntity::from_decoded(
            r.clone(),
            {
                let mut f = fields.clone();
                f.insert("body".into(), Value::String(String::new()));
                f
            },
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Complete,
        );
        // Soft-fail summary: title kept, body never obtained.
        let mut soft = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            plasm_runtime::EntityCompleteness::Summary,
        );
        soft.mark_detail_fields_unavailable(["body"]);

        let soft_result = ExecutionResult {
            entities: vec![soft],
            count: 1,
            has_more: false,
            coverage: ResultCoverage::Partial,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let empty_result = ExecutionResult {
            entities: vec![empty_present],
            count: 1,
            has_more: false,
            coverage: ResultCoverage::Complete,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
            operations: plasm_runtime::OperationLedger::empty(),
        };

        let (soft_tsv, _, _) = format_result_tsv_with_cgs(&soft_result, Some(&cgs), None);
        let (empty_tsv, _, _) = format_result_tsv_with_cgs(&empty_result, Some(&cgs), None);
        assert!(
            soft_tsv.contains(UNAVAILABLE_FIELD_PLACEHOLDER),
            "soft-fail body must show (unavailable), got:\n{soft_tsv}"
        );
        assert!(
            soft_tsv.contains("kept summary"),
            "summary title must be retained:\n{soft_tsv}"
        );
        assert!(
            !empty_tsv.contains(UNAVAILABLE_FIELD_PLACEHOLDER),
            "present-empty must not be marked unavailable:\n{empty_tsv}"
        );
        let soft_row =
            plasm_runtime::entity_to_agent_row_json(&soft_result.entities[0], Some(&cgs));
        assert_eq!(
            soft_row.get("_unavailable_fields"),
            Some(&serde_json::json!(["body"]))
        );
        assert!(soft_row.get("body").is_none());
        let empty_row =
            plasm_runtime::entity_to_agent_row_json(&empty_result.entities[0], Some(&cgs));
        assert!(empty_row.get("_unavailable_fields").is_none());
        assert_eq!(empty_row.get("body"), Some(&serde_json::json!("")));
    }
}
