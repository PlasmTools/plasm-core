//! Program-context unquoted phrase tokens ([`Value::PhraseIdent`]) — validate and lower to strings.

use crate::cgs_federation::FederationDispatch;
use crate::schema::{CapabilitySchema, EntityDef, FieldSchema, InputFieldSchema, CGS};
use crate::typed_invoke::{InvokeInputPayload, TypedInvokeInput};
use crate::typed_literal::TypedComparisonValue;
use crate::{Expr, FieldType, Predicate, Value};
use std::collections::BTreeSet;

/// Single-catalog or federated CGS resolution for phrase-ident validation.
#[derive(Clone, Copy)]
enum PhraseIdentCgsScope<'a> {
    Single(&'a CGS),
    Federated {
        fed: &'a FederationDispatch,
        fallback: &'a CGS,
    },
}

impl<'a> PhraseIdentCgsScope<'a> {
    fn resolve(&self, catalog_entry_id: Option<&str>, entity: &str) -> Result<&'a CGS, String> {
        match self {
            Self::Single(cgs) => {
                if cgs.entities.contains_key(entity) {
                    Ok(*cgs)
                } else {
                    Err(format!("unknown entity `{entity}`"))
                }
            }
            Self::Federated { fed, fallback } => {
                crate::catalog_ownership::resolve_cgs_for_stamped_catalog(
                    catalog_entry_id,
                    entity,
                    fed,
                    fallback,
                )
                .map_err(|e| e.to_string())
            }
        }
    }

    fn resolve_capability_cgs(
        &self,
        catalog_entry_id: Option<&str>,
        entity: &str,
    ) -> Result<&'a CGS, String> {
        self.resolve(catalog_entry_id, entity)
    }
}

/// True when `s` is a single ASCII identifier token (no spaces or punctuation).
#[must_use]
pub fn is_identifier_phrase(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Schema context for one invoke/predicate slot.
pub struct PhraseIdentFieldContext<'a> {
    pub field_type: &'a FieldType,
    pub allowed_values: Option<&'a [String]>,
}

/// Validate one identifier-shaped [`Value::PhraseIdent`].
pub fn validate_identifier_phrase(
    ident: &str,
    program_labels: &BTreeSet<String>,
    ctx: Option<&PhraseIdentFieldContext<'_>>,
) -> Result<(), String> {
    if !is_identifier_phrase(ident) {
        return Ok(());
    }
    if program_labels.contains(ident) {
        return Err(format!(
            "`{ident}` names a program binding in this plan — use `{ident}` or `{ident}.<field>` as a binding reference, not an unquoted literal"
        ));
    }
    let Some(ctx) = ctx else {
        return Ok(());
    };
    if phrase_ident_allowed_as_typed_literal(ident, ctx.field_type, ctx.allowed_values) {
        return Ok(());
    }
    if matches!(
        ctx.field_type,
        FieldType::String | FieldType::Blob | FieldType::Uuid | FieldType::Date | FieldType::Json
    ) {
        return Err(format!(
            "unknown program binding `{ident}` — quote the value if you meant a literal string"
        ));
    }
    Ok(())
}

fn phrase_ident_allowed_as_typed_literal(
    ident: &str,
    field_type: &FieldType,
    allowed_values: Option<&[String]>,
) -> bool {
    match field_type {
        FieldType::Select | FieldType::MultiSelect => {
            allowed_values.is_some_and(|vals| vals.iter().any(|v| v == ident))
        }
        FieldType::Boolean => matches!(ident, "true" | "false"),
        FieldType::Integer | FieldType::Number => ident.parse::<f64>().is_ok(),
        _ => false,
    }
}

fn field_context_from_input_field<'a>(
    field: &'a InputFieldSchema,
    cgs: &'a CGS,
) -> Option<PhraseIdentFieldContext<'a>> {
    let nv = field.named_value(cgs).ok()?;
    Some(PhraseIdentFieldContext {
        field_type: &nv.field_type,
        allowed_values: nv.allowed_values.as_deref(),
    })
}

fn field_context_from_entity_field<'a>(
    field: &'a FieldSchema,
    cgs: &'a CGS,
) -> Option<PhraseIdentFieldContext<'a>> {
    let nv = field.named_value(cgs).ok()?;
    Some(PhraseIdentFieldContext {
        field_type: &nv.field_type,
        allowed_values: nv.allowed_values.as_deref(),
    })
}

fn validate_value_phrase_idents(
    value: &Value,
    program_labels: &BTreeSet<String>,
    ctx: Option<&PhraseIdentFieldContext<'_>>,
) -> Result<(), String> {
    match value {
        Value::PhraseIdent(ident) => validate_identifier_phrase(ident, program_labels, ctx),
        Value::Array(items) => {
            for item in items {
                validate_value_phrase_idents(item, program_labels, None)?;
            }
            Ok(())
        }
        Value::Object(map)
        | Value::UnionCtor {
            ctor_fields: map, ..
        } => {
            for v in map.values() {
                validate_value_phrase_idents(v, program_labels, None)?;
            }
            Ok(())
        }
        Value::PlasmInputRef(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Integer(_)
        | Value::Float(_)
        | Value::String(_) => Ok(()),
    }
}

fn validate_invoke_input_object(
    input: &Value,
    program_labels: &BTreeSet<String>,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<(), String> {
    let Some(obj) = input.as_object() else {
        return validate_value_phrase_idents(input, program_labels, None);
    };
    for (key, val) in obj {
        let ctx = cap_params
            .iter()
            .find(|p| p.name == *key)
            .and_then(|f| field_context_from_input_field(f, cgs));
        match val {
            Value::PhraseIdent(ident) => {
                validate_identifier_phrase(ident, program_labels, ctx.as_ref())?;
            }
            Value::Array(items) => {
                let item_ctx = cap_params
                    .iter()
                    .find(|p| p.name == *key)
                    .and_then(|f| f.resolved_array_items(cgs))
                    .map(|items| PhraseIdentFieldContext {
                        field_type: &items.field_type,
                        allowed_values: items.allowed_values.as_deref(),
                    });
                for item in items {
                    if let Value::PhraseIdent(ident) = item {
                        validate_identifier_phrase(ident, program_labels, item_ctx.as_ref())?;
                    } else {
                        validate_value_phrase_idents(item, program_labels, None)?;
                    }
                }
            }
            _ => validate_value_phrase_idents(val, program_labels, ctx.as_ref())?,
        }
    }
    Ok(())
}

fn validate_path_vars(
    path_vars: &indexmap::IndexMap<String, Value>,
    program_labels: &BTreeSet<String>,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<(), String> {
    for (key, val) in path_vars {
        let ctx = cap_params
            .iter()
            .find(|p| p.name == *key)
            .and_then(|f| field_context_from_input_field(f, cgs));
        match val {
            Value::PhraseIdent(ident) => {
                validate_identifier_phrase(ident, program_labels, ctx.as_ref())?;
            }
            _ => validate_value_phrase_idents(val, program_labels, ctx.as_ref())?,
        }
    }
    Ok(())
}

fn validate_predicate_phrase_idents(
    predicate: &Predicate,
    program_labels: &BTreeSet<String>,
    entity: &EntityDef,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<(), String> {
    match predicate {
        Predicate::True | Predicate::False => Ok(()),
        Predicate::Comparison { field, value, .. } => {
            let ctx = cap_params
                .iter()
                .find(|p| p.name == *field)
                .and_then(|f| field_context_from_input_field(f, cgs))
                .or_else(|| {
                    entity
                        .fields
                        .get(field.as_str())
                        .and_then(|f| field_context_from_entity_field(f, cgs))
                });
            validate_value_phrase_idents(&value.to_value(), program_labels, ctx.as_ref())
        }
        Predicate::And { args } | Predicate::Or { args } => {
            for arg in args {
                validate_predicate_phrase_idents(arg, program_labels, entity, cap_params, cgs)?;
            }
            Ok(())
        }
        Predicate::Not { predicate } => {
            validate_predicate_phrase_idents(predicate, program_labels, entity, cap_params, cgs)
        }
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                validate_predicate_phrase_idents(inner, program_labels, entity, cap_params, cgs)?;
            }
            Ok(())
        }
    }
}

fn cap_params_for_capability(cap: &CapabilitySchema) -> Vec<InputFieldSchema> {
    let Some(is) = &cap.input_schema else {
        return Vec::new();
    };
    match &is.input_type {
        crate::InputType::Object { fields, .. } => fields.clone(),
        _ => Vec::new(),
    }
}

fn cap_params_for_query(cgs: &CGS, capability_name: Option<&str>) -> Vec<InputFieldSchema> {
    let Some(name) = capability_name else {
        return Vec::new();
    };
    cgs.get_capability(name)
        .map(cap_params_for_capability)
        .unwrap_or_default()
}

fn normalize_value_phrase_idents(value: &mut Value) {
    value.normalize_phrase_idents_in_tree();
}

fn normalize_comparison_value(value: &mut TypedComparisonValue) {
    let mut v = value.to_value();
    normalize_value_phrase_idents(&mut v);
    *value = TypedComparisonValue::from_value(v);
}

fn normalize_predicate(pred: &mut Predicate) {
    match pred {
        Predicate::Comparison { value, .. } => normalize_comparison_value(value),
        Predicate::And { args } | Predicate::Or { args } => {
            for arg in args {
                normalize_predicate(arg);
            }
        }
        Predicate::Not { predicate } => normalize_predicate(predicate),
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                normalize_predicate(inner);
            }
        }
        Predicate::True | Predicate::False => {}
    }
}

fn normalize_typed_invoke_input(input: &mut TypedInvokeInput) {
    match input {
        TypedInvokeInput::Leaf(_) => {}
        TypedInvokeInput::Json(v) => normalize_value_phrase_idents(v),
        TypedInvokeInput::Array(items) => {
            for item in items {
                normalize_typed_invoke_input(item);
            }
        }
        TypedInvokeInput::Object { fields, extra } => {
            for item in fields.values_mut() {
                normalize_typed_invoke_input(item);
            }
            if let Some(ex) = extra {
                for v in ex.values_mut() {
                    normalize_value_phrase_idents(v);
                }
            }
        }
        TypedInvokeInput::Union { value, .. } => normalize_typed_invoke_input(value),
        TypedInvokeInput::PlasmInputRef(_) => {}
    }
}

fn normalize_invoke_payload(payload: &mut InvokeInputPayload) {
    match payload {
        InvokeInputPayload::Raw(v) => normalize_value_phrase_idents(v),
        InvokeInputPayload::Typed(t) => normalize_typed_invoke_input(t),
    }
}

fn lower_expr_phrase_idents(
    expr: &mut Expr,
    program_labels: &BTreeSet<String>,
    scope: PhraseIdentCgsScope<'_>,
    validate: bool,
) -> Result<(), String> {
    match expr {
        Expr::Invoke(inv) => {
            let cgs = scope.resolve_capability_cgs(
                inv.catalog_entry_id.as_deref(),
                inv.target.entity_type.as_str(),
            )?;
            if validate {
                let cap = cgs
                    .get_capability(inv.capability.as_str())
                    .ok_or_else(|| format!("unknown capability `{}`", inv.capability))?;
                let cap_params = cap_params_for_capability(cap);
                if let Some(input) = &inv.input {
                    validate_invoke_input_object(
                        &input.to_value(),
                        program_labels,
                        &cap_params,
                        cgs,
                    )?;
                }
                if let Some(pv) = &inv.path_vars {
                    validate_path_vars(pv, program_labels, &cap_params, cgs)?;
                }
            }
            if let Some(input) = &mut inv.input {
                normalize_invoke_payload(input);
            }
            if let Some(pv) = &mut inv.path_vars {
                for v in pv.values_mut() {
                    normalize_value_phrase_idents(v);
                }
            }
            Ok(())
        }
        Expr::Create(create) => {
            let cgs = scope.resolve_capability_cgs(
                create.catalog_entry_id.as_deref(),
                create.entity.as_str(),
            )?;
            if validate {
                let cap = cgs
                    .get_capability(create.capability.as_str())
                    .ok_or_else(|| format!("unknown capability `{}`", create.capability))?;
                let cap_params = cap_params_for_capability(cap);
                validate_invoke_input_object(
                    &create.input.to_value(),
                    program_labels,
                    &cap_params,
                    cgs,
                )?;
            }
            normalize_invoke_payload(&mut create.input);
            Ok(())
        }
        Expr::Delete(del) => {
            let cgs = scope.resolve_capability_cgs(
                del.catalog_entry_id.as_deref(),
                del.target.entity_type.as_str(),
            )?;
            if validate {
                let cap = cgs
                    .get_capability(del.capability.as_str())
                    .ok_or_else(|| format!("unknown capability `{}`", del.capability))?;
                let cap_params = cap_params_for_capability(cap);
                if let Some(pv) = &del.path_vars {
                    validate_path_vars(pv, program_labels, &cap_params, cgs)?;
                }
            }
            if let Some(pv) = &mut del.path_vars {
                for v in pv.values_mut() {
                    normalize_value_phrase_idents(v);
                }
            }
            Ok(())
        }
        Expr::Get(get) => {
            let cgs = scope.resolve(
                get.catalog_entry_id.as_deref(),
                get.reference.entity_type.as_str(),
            )?;
            if validate {
                if let Some(pv) = &get.path_vars {
                    validate_path_vars(pv, program_labels, &[], cgs)?;
                }
            }
            if let Some(pv) = &mut get.path_vars {
                for v in pv.values_mut() {
                    normalize_value_phrase_idents(v);
                }
            }
            Ok(())
        }
        Expr::Query(q) => {
            let cgs = scope.resolve(q.catalog_entry_id.as_deref(), q.entity.as_str())?;
            if validate {
                let ent = cgs
                    .get_entity(q.entity.as_str())
                    .ok_or_else(|| format!("unknown entity `{}`", q.entity))?;
                let cap_params = cap_params_for_query(cgs, q.capability_name.as_deref());
                if let Some(pred) = &q.predicate {
                    validate_predicate_phrase_idents(pred, program_labels, ent, &cap_params, cgs)?;
                }
            }
            if let Some(pred) = &mut q.predicate {
                normalize_predicate(pred);
            }
            Ok(())
        }
        Expr::Chain(c) => {
            lower_expr_phrase_idents(&mut c.source, program_labels, scope, validate)?;
            if let crate::ChainStep::Explicit { expr } = &mut c.step {
                lower_expr_phrase_idents(expr, program_labels, scope, validate)?;
            }
            Ok(())
        }
        Expr::TeachingValue { .. } | Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) => Ok(()),
    }
}

/// Validate then lower program-context [`Value::PhraseIdent`] nodes in an expression tree.
pub fn lower_program_phrase_idents_in_expr(
    expr: &mut Expr,
    program_labels: &BTreeSet<String>,
    cgs: &CGS,
) -> Result<(), String> {
    lower_expr_phrase_idents(expr, program_labels, PhraseIdentCgsScope::Single(cgs), true)
}

/// Federated variant: resolve owning [`CGS`] per stamped `catalog_entry_id` (mirrors
/// [`crate::type_checker::type_check_expr_federated`]).
pub fn lower_program_phrase_idents_in_expr_federated(
    expr: &mut Expr,
    program_labels: &BTreeSet<String>,
    fed: &FederationDispatch,
    fallback: &CGS,
) -> Result<(), String> {
    lower_expr_phrase_idents(
        expr,
        program_labels,
        PhraseIdentCgsScope::Federated { fed, fallback },
        true,
    )
}

/// Lower validated [`Value::PhraseIdent`] nodes to [`Value::String`] across an expression tree.
pub fn normalize_program_phrase_idents_in_expr(expr: &mut Expr, cgs: &CGS) {
    let labels = BTreeSet::new();
    let _ = lower_expr_phrase_idents(expr, &labels, PhraseIdentCgsScope::Single(cgs), false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FieldType;

    #[test]
    fn rejects_phrase_ident_shadowing_program_label() {
        let mut labels = BTreeSet::new();
        labels.insert("body".into());
        let err = validate_identifier_phrase(
            "body",
            &labels,
            Some(&PhraseIdentFieldContext {
                field_type: &FieldType::String,
                allowed_values: None,
            }),
        )
        .expect_err("shadows binding");
        assert!(err.contains("program binding"), "{err}");
    }

    #[test]
    fn rejects_unknown_binding_on_string_param() {
        let labels = BTreeSet::new();
        let err = validate_identifier_phrase(
            "body",
            &labels,
            Some(&PhraseIdentFieldContext {
                field_type: &FieldType::String,
                allowed_values: None,
            }),
        )
        .expect_err("unknown binding");
        assert!(err.contains("unknown program binding"), "{err}");
    }

    #[test]
    fn allows_hyphenated_phrase_on_string_param() {
        let labels = BTreeSet::new();
        validate_identifier_phrase(
            "matrix-dev",
            &labels,
            Some(&PhraseIdentFieldContext {
                field_type: &FieldType::String,
                allowed_values: None,
            }),
        )
        .expect("hyphenated literal");
    }

    /// Stamped `catalog_entry_id` must route phrase-ident validation to the owning catalog graph.
    #[test]
    fn federated_query_phrase_ident_resolves_stamped_catalog() {
        use crate::cgs_federation::FederationDispatch;
        use crate::CatalogEntryStamp;
        use crate::CgsContext;
        use crate::QueryExpr;
        use indexmap::IndexMap;
        use std::path::Path;
        use std::sync::Arc;

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let pokeapi_dir = root.join("../../apis/pokeapi");
        let matrix_dir = root.join("../../fixtures/schemas/plasm_language_matrix");
        if !pokeapi_dir.is_dir() {
            return;
        }
        let pokeapi = Arc::new(crate::loader::load_schema_dir(&pokeapi_dir).expect("pokeapi"));
        let matrix = Arc::new(crate::loader::load_schema_dir(&matrix_dir).expect("matrix"));
        let mut by_entry = IndexMap::new();
        by_entry.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", matrix.clone())),
        );
        by_entry.insert(
            "pokeapi".into(),
            Arc::new(CgsContext::entry("pokeapi", pokeapi.clone())),
        );
        let layers: Vec<&CGS> = vec![matrix.as_ref(), pokeapi.as_ref()];
        let mut exp = crate::symbol_tuning::TeachingExposureSession::new(
            matrix.as_ref(),
            "github",
            &["LangItem"],
        );
        exp.expose_entities(&layers, pokeapi.clone(), "pokeapi", &["Pokemon"]);
        let fed = FederationDispatch::from_contexts_and_exposure(by_entry, &exp);
        let mut q = QueryExpr::all("Pokemon");
        q.catalog_entry_id = CatalogEntryStamp::some("pokeapi".into());
        let mut expr = Expr::Query(q);
        let labels = BTreeSet::new();
        lower_program_phrase_idents_in_expr_federated(&mut expr, &labels, &fed, matrix.as_ref())
            .expect("pokeapi-stamped query must validate against pokeapi graph");
        let mut primary_only = expr.clone();
        let err = lower_program_phrase_idents_in_expr(&mut primary_only, &labels, matrix.as_ref())
            .expect_err("primary github graph lacks Pokemon entity");
        assert!(err.contains("unknown entity"), "{err}");
    }
}
