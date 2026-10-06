use super::super::*;
use super::super::{value_at_dotted, value_at_segments};
use super::input_rows::materialized_result_use_inputs;
use plasm_core::operand_binding::{BindOperands, OperandResolver};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeOperandError {
    #[error(transparent)]
    Identity(#[from] plasm_core::operand_binding::IdentityCodecError),
    #[error("materialized plan input is invalid")]
    MaterializedInput(#[source] MaterializedInputError),
    #[error("row binding `{binding}` was used outside a row scope")]
    BindingOutsideRowScope { binding: String },
    #[error("row binding `{binding}` does not match active binding `{active}`")]
    BindingMismatch { binding: String, active: String },
    #[error("input alias `{alias}` is invalid")]
    InvalidAlias { alias: String },
    #[error("node input alias `{alias}` is unavailable")]
    UnavailableAlias { alias: String },
    #[error("node input field `{alias}.{path}` is unavailable in {entry_id}:{entity}")]
    MissingInputField {
        alias: String,
        path: String,
        entry_id: String,
        entity: String,
    },
    #[error("node input field `{alias}.{path}` produced no values in {entry_id}:{entity}")]
    EmptyInputValues {
        alias: String,
        path: String,
        entry_id: String,
        entity: String,
    },
    #[error("program string interpolation failed: {0}")]
    StringInterpolation(#[source] plasm_core::program_string_template::ProgramStringError),
    #[error("resolved operand contains a value that is not runtime data")]
    OperandValue,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DataOperandError {
    #[error("unknown row binding `{binding}`")]
    UnknownRowBinding { binding: String },
    #[error("row binding `{binding}` field `{field_path}` is unobserved")]
    RowFieldUnobserved { binding: String, field_path: String },
    #[error(transparent)]
    InputAlias(#[from] crate::plasm_plan::PlanAtomError),
    #[error("input alias `{alias}` is unavailable")]
    InputAliasUnavailable { alias: String },
    #[error("input alias `{alias}` is bound to `{actual}`, not `{expected}`")]
    InputAliasNodeMismatch {
        alias: String,
        actual: String,
        expected: String,
    },
    #[error("input `{node}` has no field `{field_path}`")]
    InputFieldMissing { node: String, field_path: String },
    #[error("data operands do not encode catalog identities")]
    IdentityUnsupported,
    #[error("data operand contains an unresolved runtime value")]
    UnresolvedOperandValue,
    #[error("plan data value evaluation failed")]
    DataValueEvaluation(#[source] Box<plasm_core::PlasmDataValueEvaluationError<DataOperandError>>),
    #[error(transparent)]
    ValueProjection(#[from] super::input_rows::ValueProjectionError),
    #[error("template input `{binding}` is unavailable")]
    TemplateInputUnavailable { binding: String },
    #[error(transparent)]
    Template(#[from] plasm_core::program_string_template::ProgramStringError),
}

#[derive(Debug, thiserror::Error)]
#[error("materialized plan input could not be bound")]
pub(crate) struct MaterializedInputError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WireCoercionContextError {
    #[error("input alias `{alias}` references unloaded catalog `{entry_id}` for `{entity}`")]
    CatalogUnavailable {
        alias: String,
        entry_id: String,
        entity: String,
    },
}

impl From<RuntimeOperandError> for plasm_runtime::ExecutionFailure {
    fn from(error: RuntimeOperandError) -> Self {
        Self::new(
            plasm_runtime::FailureCause::Runtime,
            "operand_binding",
            error.to_string(),
        )
    }
}

pub(crate) fn instantiate_parsed_expr_plan_inputs_with_rows(
    parsed: ParsedExpr,
    target_cgs: &CGS,
    input_rows: &BTreeMap<InputAlias, MaterializedInputRow>,
    wire_coercion_by_alias: &BTreeMap<InputAlias, WireCoercionCtx<'_>>,
) -> Result<ParsedExpr, RuntimeOperandError> {
    let scope = EvalScope::Root {
        row: &plasm_core::Value::Null,
    };
    let inputs = InputEnv { rows: input_rows };
    let env = PlanEvalEnv {
        scope,
        inputs,
        wire_coercion_by_alias,
    };
    let expr = parsed
        .expr
        .bind_operands(&mut RuntimeOperands(&env, target_cgs))?;
    Ok(ParsedExpr {
        expr,
        projection: parsed.projection,
        field_dot_extract: None,
    })
}

/// Bind typed references to materialized inputs without serializing expression structure.
pub(crate) fn instantiate_parsed_expr_plan_inputs(
    parsed: ParsedExpr,
    target_cgs: &CGS,
    uses_result: &[PlanResultUse],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<ParsedExpr, RuntimeOperandError> {
    if uses_result.is_empty() {
        return Ok(parsed);
    }
    let input_rows = materialized_result_use_inputs(materialized, uses_result, None)
        .map_err(|_| RuntimeOperandError::MaterializedInput(MaterializedInputError))?;
    let empty = BTreeMap::new();
    instantiate_parsed_expr_plan_inputs_with_rows(parsed, target_cgs, &input_rows, &empty)
}

pub(crate) fn wire_coercion_ctx_for_source_entity<'a>(
    cgs: &'a CGS,
    source_entity_name: &str,
) -> Option<WireCoercionCtx<'a>> {
    let ent = cgs.get_entity(source_entity_name)?;
    Some(WireCoercionCtx {
        cgs,
        source_entity: ent,
    })
}

/// Build alias-specific coercion contexts from each input's [`QualifiedEntityKey`] (no first-input heuristic).
/// Stamps each input's `id_field` from the source entity when the catalog is loaded.
pub(crate) fn wire_coercion_by_alias_from_inputs<'a>(
    es: &'a ExecuteSession,
    input_rows: &mut BTreeMap<InputAlias, MaterializedInputRow>,
) -> Result<BTreeMap<InputAlias, WireCoercionCtx<'a>>, WireCoercionContextError> {
    let mut out = BTreeMap::new();
    for (alias, row) in input_rows.iter_mut() {
        let entry_id = row.qualified_entity.entry_id.as_str();
        let entity = row.qualified_entity.entity.as_str();
        if entry_id.is_empty() || entity.is_empty() {
            // Synthetic / data-literal rows have no catalog coercion.
            continue;
        }
        let cgs = es
            .contexts_by_entry
            .get(entry_id)
            .map(|c| c.cgs.as_ref())
            .ok_or_else(|| WireCoercionContextError::CatalogUnavailable {
                alias: alias.as_str().to_owned(),
                entry_id: entry_id.to_owned(),
                entity: entity.to_owned(),
            })?;
        // Plan-computed / render synthetics have no EntityDef; retain their native fields.
        if let Some(ctx) = wire_coercion_ctx_for_source_entity(cgs, entity) {
            row.id_field = ctx.source_entity.id_field.to_string();
            out.insert(alias.clone(), ctx);
        }
    }
    Ok(out)
}
pub(crate) fn instantiate_expr_template(
    template: &ValidatedPlanExprTemplate,
    env: &PlanEvalEnv<'_>,
    target_cgs: &CGS,
) -> Result<ParsedExpr, RuntimeOperandError> {
    Ok(ParsedExpr {
        expr: template
            .expr
            .bind_operands(&mut RuntimeOperands(env, target_cgs))?,
        projection: template.projection.clone(),
        field_dot_extract: None,
    })
}

struct RuntimeOperands<'a, 'b>(&'a PlanEvalEnv<'b>, &'a CGS);

impl OperandResolver for RuntimeOperands<'_, '_> {
    type Error = RuntimeOperandError;
    fn resolve(
        &mut self,
        reference: &plasm_core::PlasmInputRef,
    ) -> Result<plasm_core::operand_binding::ResolvedValue, RuntimeOperandError> {
        let value = resolve_input_reference(reference, self.0)?;
        plasm_core::operand_binding::ResolvedValue::new(value)
            .map_err(|_| RuntimeOperandError::OperandValue)
    }
    fn identity(
        &mut self,
        target: plasm_core::operand_binding::IdentityTarget<'_>,
        reference: &plasm_core::PlasmInputRef,
    ) -> Result<plasm_core::EntityId, RuntimeOperandError> {
        let codec = plasm_core::operand_binding::IdentityCodec::compile(self.1, target)?;
        Ok(codec.encode(&resolve_input_reference(reference, self.0)?)?)
    }
    fn string(
        &mut self,
        value: &plasm_core::program_string_template::CompiledProgramString,
    ) -> Result<String, RuntimeOperandError> {
        value
            .render(&plan_binding_scope_owned(self.0))
            .map_err(RuntimeOperandError::StringInterpolation)
    }
}

pub(crate) fn plan_binding_scope_owned(
    env: &PlanEvalEnv<'_>,
) -> BTreeMap<String, plasm_core::Value> {
    insert_plan_eval_scope(env, Clone::clone)
}

fn insert_plan_eval_scope(
    env: &PlanEvalEnv<'_>,
    mut row_to_value: impl FnMut(&plasm_core::Value) -> plasm_core::Value,
) -> BTreeMap<String, plasm_core::Value> {
    let mut scope = BTreeMap::new();
    // Flatten bound row fields first so `{{ title }}` works; aliases overwrite on conflict.
    if let EvalScope::Bound { row, binding } = &env.scope {
        let row_value = row_to_value(row);
        if let plasm_core::Value::Object(map) = &row_value {
            for (k, v) in map {
                scope.insert(k.clone(), v.clone());
            }
        }
        scope.insert(binding.as_str().to_string(), row_value.clone());
        // Convention: `_` always names the row cursor when bound.
        if binding.as_str() != "_" {
            scope.insert("_".to_string(), row_value);
        }
    }
    for (alias, input) in env.inputs.rows {
        let row_value = row_to_value(&input.row);
        scope.insert(alias.as_str().to_string(), row_value.clone());
        scope.insert(input.node.as_str().to_string(), row_value);
    }
    scope
}

pub(crate) fn coerce_node_input_value(
    ctx: Option<&WireCoercionCtx<'_>>,
    path: &[String],
    value: plasm_core::Value,
) -> plasm_core::Value {
    let Some(ctx) = ctx else {
        return value;
    };
    let Some(field) = path.last().map(String::as_str) else {
        return value;
    };
    match plasm_core::parent_entity_field_type(ctx.cgs, ctx.source_entity, field) {
        Ok(ft) => {
            let nv = ctx
                .source_entity
                .fields
                .get(field)
                .and_then(|f| f.named_value(ctx.cgs).ok());
            plasm_core::coerce_value_for_field_type(
                &ft,
                nv.and_then(|n| n.value_format),
                nv.and_then(|n| n.array_items.as_ref()),
                value.clone(),
            )
            .unwrap_or(value)
        }
        Err(_) => value,
    }
}

pub(crate) fn node_input_hole_from_identity(
    ctx: Option<&WireCoercionCtx<'_>>,
    id_field: &str,
    identity: &Option<plasm_core::RowIdentity>,
    path: &[String],
    row: &plasm_core::Value,
) -> Option<plasm_core::Value> {
    let identity = identity.as_ref()?;
    if path.is_empty() {
        let slot = identity.reference.primary_slot_str();
        return Some(coerce_node_input_value(
            ctx,
            path,
            plasm_core::Value::String(slot),
        ));
    }
    if path.len() == 1 {
        let key = path[0].as_str();
        if let Some(v) = identity.ambient.get(key) {
            return Some(coerce_node_input_value(
                ctx,
                path,
                plasm_core::Value::String(v.clone()),
            ));
        }
        if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
            if let Some(v) = parts.get(key).and_then(|s| s.as_lit_str()) {
                let raw = ctx
                    .map(|c| plasm_core::identity_slot_to_value(c.cgs, c.source_entity, key, v))
                    .unwrap_or_else(|| plasm_core::Value::String(v.to_string()));
                return Some(coerce_node_input_value(ctx, path, raw));
            }
        }
        // Primary identity: CGS `id_field` (e.g. AuthSession.access_token) or legacy `"id"`.
        if key == "id" || key == id_field {
            let slot = identity.reference.primary_slot_str();
            return Some(coerce_node_input_value(
                ctx,
                path,
                plasm_core::Value::String(slot),
            ));
        }
    }
    value_at_segments(row, path)
        .cloned()
        .map(|v| coerce_node_input_value(ctx, path, v))
}

pub(crate) fn resolve_input_reference(
    reference: &plasm_core::PlasmInputRef,
    env: &PlanEvalEnv<'_>,
) -> Result<plasm_core::Value, RuntimeOperandError> {
    match reference {
        plasm_core::PlasmInputRef::RowBinding { binding, path } => {
            let EvalScope::Bound {
                binding: scope_binding,
                ..
            } = &env.scope
            else {
                return Err(RuntimeOperandError::BindingOutsideRowScope {
                    binding: binding.clone(),
                });
            };
            if binding != scope_binding.as_str() {
                return Err(RuntimeOperandError::BindingMismatch {
                    binding: binding.clone(),
                    active: scope_binding.to_string(),
                });
            }
            Ok(value_at_segments(env.scope.row(), path)
                .cloned()
                .unwrap_or(plasm_core::Value::Null))
        }
        plasm_core::PlasmInputRef::NodeInput { node, path } => {
            let alias = node;
            let alias = InputAlias::new(alias.to_string()).map_err(|_| {
                RuntimeOperandError::InvalidAlias {
                    alias: alias.to_string(),
                }
            })?;
            let input = env.inputs.rows.get(&alias).ok_or_else(|| {
                RuntimeOperandError::UnavailableAlias {
                    alias: alias.to_string(),
                }
            })?;
            let wire_ctx = env.wire_coercion_for_alias(&alias);
            let id_field = wire_ctx
                .map(|c| c.source_entity.id_field.as_str())
                .unwrap_or(input.id_field.as_str());
            if input.rows.len() > 1 && !path.is_empty() {
                let mut values = Vec::with_capacity(input.rows.len());
                for (row, ident) in input.rows.iter().zip(input.row_identities.iter()) {
                    let cell = value_at_segments(row, path)
                        .cloned()
                        .or_else(|| {
                            node_input_hole_from_identity(wire_ctx, id_field, ident, path, row)
                        })
                        .ok_or_else(|| RuntimeOperandError::MissingInputField {
                            alias: alias.to_string(),
                            path: path.join("."),
                            entry_id: input.qualified_entity.entry_id.clone(),
                            entity: input.qualified_entity.entity.clone(),
                        })?;
                    if !cell.is_null() {
                        values.push(coerce_node_input_value(wire_ctx, path, cell));
                    }
                }
                if values.is_empty() {
                    return Err(RuntimeOperandError::EmptyInputValues {
                        alias: alias.to_string(),
                        path: path.join("."),
                        entry_id: input.qualified_entity.entry_id.clone(),
                        entity: input.qualified_entity.entity.clone(),
                    });
                }
                return Ok(plasm_core::Value::Array(values));
            }
            if path.is_empty() {
                if let Some(value) = node_input_hole_from_identity(
                    wire_ctx,
                    id_field,
                    &input.row_identity,
                    path,
                    &input.row,
                ) {
                    if value.as_str().is_none_or(|s| !s.is_empty()) {
                        return Ok(value);
                    }
                }
                return Ok(input.row.clone());
            }
            let from_row = input
                .rows
                .first()
                .and_then(|row| value_at_segments(row, path))
                .cloned();
            let from_row_usable = from_row.as_ref().is_some_and(|v| !v.is_null());
            if from_row_usable {
                return Ok(coerce_node_input_value(wire_ctx, path, from_row.unwrap()));
            }
            if let Some(value) = node_input_hole_from_identity(
                wire_ctx,
                id_field,
                &input.row_identity,
                path,
                &input.row,
            ) {
                if value.as_str().is_none_or(|s| !s.is_empty()) {
                    return Ok(value);
                }
            }
            Err(RuntimeOperandError::MissingInputField {
                alias: alias.to_string(),
                path: path.join("."),
                entry_id: input.qualified_entity.entry_id.clone(),
                entity: input.qualified_entity.entity.clone(),
            })
        }
    }
}
/// Keep the scalar/record distinction explicit while storing every row as a native record.
fn native_output_rows(
    values: Vec<plasm_core::Value>,
) -> (Vec<plasm_core::ValueRow>, Vec<MaterializedValueShape>) {
    let shapes = values
        .iter()
        .map(|value| {
            if value.is_object() {
                MaterializedValueShape::Record
            } else {
                MaterializedValueShape::ScalarColumn
            }
        })
        .collect();
    (
        values
            .into_iter()
            .map(plasm_core::ValueRow::from_output)
            .collect(),
        shapes,
    )
}

pub(crate) fn plan_value_to_rows(
    value: &PlanValue,
) -> Result<(Vec<plasm_core::ValueRow>, Vec<MaterializedValueShape>), DataOperandError> {
    let inputs = BTreeMap::new();
    let scope = EvalScope::Root {
        row: &plasm_core::Value::Null,
    };
    let input_env = InputEnv { rows: &inputs };
    let empty_coercion = BTreeMap::new();
    let env = PlanEvalEnv {
        scope,
        inputs: input_env,
        wire_coercion_by_alias: &empty_coercion,
    };
    let value = eval_plan_value(value, &env)?;
    match value {
        plasm_core::Value::Array(items) => Ok(native_output_rows(items)),
        value => Ok(native_output_rows(vec![value])),
    }
}

/// Pure `derive` (map) row production: evaluate `value` once per source row under the item binding
/// scope plus singleton `inputs`. **PEC:** this is the *single* derive kernel — both live execute
/// ([`materialize_executable_plan_step`]) and dry preflight materialize derive rows through here, so
/// the only planned/live difference is the source of `source_rows` (I/O), never the derivation.
pub(crate) fn derive_node_rows(
    kind: crate::plasm_plan::DeriveKind,
    item_binding: &BindingName,
    value: &PlanValue,
    source_rows: &[plasm_core::ValueRow],
    input_rows: &BTreeMap<InputAlias, MaterializedInputRow>,
) -> Result<(Vec<plasm_core::ValueRow>, Vec<MaterializedValueShape>), DataOperandError> {
    let empty_coercion = BTreeMap::new();
    let mut rows = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        let scope = EvalScope::Bound {
            row,
            binding: item_binding,
        };
        let inputs = InputEnv { rows: input_rows };
        let env = PlanEvalEnv {
            scope,
            inputs,
            wire_coercion_by_alias: &empty_coercion,
        };
        rows.push(eval_plan_value(value, &env)?);
    }
    if kind == crate::plasm_plan::DeriveKind::Cell {
        let shapes = vec![MaterializedValueShape::ScalarColumn; rows.len()];
        let rows = rows
            .into_iter()
            .map(|value| plasm_core::ValueRow::from_iter([("value".into(), value)]))
            .collect();
        Ok((rows, shapes))
    } else {
        Ok(native_output_rows(rows))
    }
}

pub(crate) enum EvalScope<'a> {
    Root {
        row: &'a plasm_core::Value,
    },
    Bound {
        row: &'a plasm_core::Value,
        binding: &'a BindingName,
    },
}

impl<'a> EvalScope<'a> {
    fn row(&self) -> &'a plasm_core::Value {
        match self {
            Self::Root { row } | Self::Bound { row, .. } => row,
        }
    }
}

pub(crate) struct InputEnv<'a> {
    pub(crate) rows: &'a BTreeMap<InputAlias, MaterializedInputRow>,
}

pub(crate) struct WireCoercionCtx<'a> {
    cgs: &'a CGS,
    source_entity: &'a plasm_core::EntityDef,
}

pub(crate) struct PlanEvalEnv<'a> {
    pub(crate) scope: EvalScope<'a>,
    pub(crate) inputs: InputEnv<'a>,
    /// Per-alias wire coercion from each input's catalog-qualified source entity.
    pub(crate) wire_coercion_by_alias: &'a BTreeMap<InputAlias, WireCoercionCtx<'a>>,
}

impl<'a> PlanEvalEnv<'a> {
    fn wire_coercion_for_alias(&self, alias: &InputAlias) -> Option<&WireCoercionCtx<'a>> {
        self.wire_coercion_by_alias.get(alias)
    }
}

pub(crate) fn eval_plan_value(
    value: &PlanValue,
    env: &PlanEvalEnv<'_>,
) -> Result<plasm_core::Value, DataOperandError> {
    value
        .evaluate(&mut DataOperands(env))
        .map(plasm_core::operand_binding::ResolvedValue::into_value)
        .map_err(|error| DataOperandError::DataValueEvaluation(Box::new(error)))
}

struct DataOperands<'a, 'b>(&'a PlanEvalEnv<'b>);
impl plasm_core::operand_binding::OperandResolver for DataOperands<'_, '_> {
    type Error = DataOperandError;
    fn resolve(
        &mut self,
        reference: &plasm_core::PlasmInputRef,
    ) -> Result<plasm_core::operand_binding::ResolvedValue, DataOperandError> {
        use plasm_core::PlasmInputRef;
        match reference {
            PlasmInputRef::NodeInput { node, path } => self.node(node, node, path),
            PlasmInputRef::RowBinding { binding, path } => {
                let dotted = path.join(".");
                let path = match &self.0.scope {
                    EvalScope::Root { .. } if binding == "_" => dotted.as_str(),
                    EvalScope::Bound {
                        binding: actual, ..
                    } if binding == "_" || binding == actual.as_str() => {
                        strip_binding(&dotted, actual)
                    }
                    _ => {
                        return Err(DataOperandError::UnknownRowBinding {
                            binding: binding.clone(),
                        });
                    }
                };
                let value = value_at_dotted(self.0.scope.row(), path).ok_or_else(|| {
                    DataOperandError::RowFieldUnobserved {
                        binding: binding.clone(),
                        field_path: path.to_owned(),
                    }
                })?;
                plasm_core::operand_binding::ResolvedValue::new(value.clone())
                    .map_err(|_| DataOperandError::UnresolvedOperandValue)
            }
        }
    }
    fn node(
        &mut self,
        node: &str,
        alias: &str,
        path: &[String],
    ) -> Result<plasm_core::operand_binding::ResolvedValue, DataOperandError> {
        let input_alias = InputAlias::new(alias.to_owned())?;
        let input = self.0.inputs.rows.get(&input_alias).ok_or_else(|| {
            DataOperandError::InputAliasUnavailable {
                alias: alias.to_owned(),
            }
        })?;
        if input.node.as_str() != node {
            return Err(DataOperandError::InputAliasNodeMismatch {
                alias: alias.to_owned(),
                actual: input.node.to_string(),
                expected: node.to_owned(),
            });
        }
        match input.proof {
            crate::plasm_plan::InputCardinalityProof::Acknowledgement
            | crate::plasm_plan::InputCardinalityProof::Collection
            | crate::plasm_plan::InputCardinalityProof::StaticSingleton
            | crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton => {}
        }
        let value = value_at_segments(&input.row, path).ok_or_else(|| {
            DataOperandError::InputFieldMissing {
                node: node.to_owned(),
                field_path: path.join("."),
            }
        })?;
        let value = if path.is_empty() {
            match &input.value_projection {
                Some(fields) => {
                    super::input_rows::project_value_rows(value, fields, &input.optional_fields)?
                }
                None => value.clone(),
            }
        } else {
            value.clone()
        };
        plasm_core::operand_binding::ResolvedValue::new(value)
            .map_err(|_| DataOperandError::UnresolvedOperandValue)
    }
    fn identity(
        &mut self,
        _: plasm_core::operand_binding::IdentityTarget<'_>,
        _: &plasm_core::PlasmInputRef,
    ) -> Result<plasm_core::EntityId, DataOperandError> {
        Err(DataOperandError::IdentityUnsupported)
    }
    fn string(
        &mut self,
        template: &plasm_core::program_string_template::CompiledProgramString,
    ) -> Result<String, DataOperandError> {
        render_template(template, self.0)
    }
    fn template(
        &mut self,
        template: &plasm_core::program_string_template::CompiledProgramString,
        bindings: &[plasm_core::PlanInputBinding],
    ) -> Result<String, DataOperandError> {
        let mut scope = plan_binding_scope_owned(self.0);
        let original = scope.clone();
        for binding in bindings {
            let value = original.get(&binding.from).ok_or_else(|| {
                DataOperandError::TemplateInputUnavailable {
                    binding: binding.from.clone(),
                }
            })?;
            scope.insert(binding.to.clone(), value.clone());
        }
        template.render(&scope).map_err(DataOperandError::Template)
    }
}

pub(crate) fn strip_binding<'a>(path: &'a str, binding: &BindingName) -> &'a str {
    let binding = binding.as_str();
    if path == binding {
        return "";
    }
    if let Some(rest) = path.strip_prefix(&format!("{binding}.")) {
        return rest;
    }
    path
}

pub(crate) fn render_template(
    template: &plasm_core::program_string_template::CompiledProgramString,
    env: &PlanEvalEnv<'_>,
) -> Result<String, DataOperandError> {
    let current_row = match env.scope {
        EvalScope::Bound { row, .. } => Some(row),
        EvalScope::Root { .. } => None,
    };
    let mut bindings = BTreeMap::new();
    for (alias, input) in env.inputs.rows {
        bindings.insert(alias.as_str().to_string(), input.rows.clone());
        bindings.insert(input.node.as_str().to_string(), input.rows.clone());
    }
    let mut ctx = plasm_core::unified_template_context(current_row, &bindings);
    if let Some(row) = current_row {
        ctx.insert("_".to_string(), minijinja::Value::from_serialize(row));
        if let EvalScope::Bound { binding, .. } = env.scope {
            if binding.as_str() != "_" {
                ctx.insert(
                    binding.as_str().to_string(),
                    minijinja::Value::from_serialize(row),
                );
            }
        }
    }
    template
        .render_minijinja_context(&ctx)
        .map_err(DataOperandError::Template)
}

pub(crate) fn synthetic_projection(node: &ValidatedPlanNode) -> Option<Vec<String>> {
    match node {
        ValidatedPlanNode::Compute(compute)
            if matches!(&compute.compute.op, ComputeOp::Python { output_type, .. }
                if !output_type.is_non_null_record()) =>
        {
            None
        }
        ValidatedPlanNode::Compute(compute) => Some(
            compute
                .compute
                .schema
                .fields
                .iter()
                .map(|f| f.name.as_str().to_string())
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod native_output_shape_tests {
    use super::*;
    #[test]
    fn row_scoped_derivation_binds_records_and_requires_explicit_scalar_extraction() {
        let (source, _) = native_output_rows(vec![plasm_core::Value::Integer(7)]);
        let binding = BindingName::new("item").unwrap();
        let reference = |path| PlanValue::BindingSymbol {
            binding: "item".into(),
            path,
        };
        let (records, shapes) = derive_node_rows(
            crate::plasm_plan::DeriveKind::Map,
            &binding,
            &reference(vec![]),
            &source,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(records, source);
        assert_eq!(shapes, [MaterializedValueShape::Record]);
        let (scalars, shapes) = derive_node_rows(
            crate::plasm_plan::DeriveKind::Cell,
            &binding,
            &reference(vec!["value".into()]),
            &source,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(scalars, source);
        assert_eq!(shapes, [MaterializedValueShape::ScalarColumn]);
    }

    #[test]
    fn scalar_extraction_keeps_object_and_array_payloads_in_one_cell() {
        use plasm_core::{Value, ValueRow};
        let binding = BindingName::new("item").unwrap();
        for payload in [
            Value::Object(indexmap::IndexMap::from([(
                "url".into(),
                Value::String("fixture".into()),
            )])),
            Value::Object(indexmap::IndexMap::from([(
                "value".into(),
                Value::Integer(7),
            )])),
            Value::Object(indexmap::IndexMap::new()),
            Value::Array(vec![Value::Integer(1), Value::Null]),
            Value::Null,
        ] {
            let source = vec![ValueRow::from_iter([("payload".into(), payload.clone())])];
            let (rows, shapes) = derive_node_rows(
                crate::plasm_plan::DeriveKind::Cell,
                &binding,
                &PlanValue::BindingSymbol {
                    binding: "item".into(),
                    path: vec!["payload".into()],
                },
                &source,
                &BTreeMap::new(),
            )
            .unwrap();
            assert_eq!(rows, vec![ValueRow::from_iter([("value".into(), payload)])]);
            assert_eq!(shapes, [MaterializedValueShape::ScalarColumn]);
        }
    }

    #[test]
    fn normalization_retains_each_value_shape_without_inspecting_encoded_rows() {
        use plasm_core::Value;
        let record = Value::Object(indexmap::IndexMap::from([(
            "value".into(),
            Value::Integer(7),
        )]));
        let (rows, shapes) = native_output_rows(vec![Value::Integer(7), record]);
        assert_eq!(rows[0], rows[1]);
        assert_eq!(
            shapes,
            [
                MaterializedValueShape::ScalarColumn,
                MaterializedValueShape::Record
            ]
        );
    }
}
