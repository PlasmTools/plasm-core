use super::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OperandValidationError {
    #[error("plan node {node_index} operand references undeclared input alias `{alias}`")]
    UndeclaredInputAlias { node_index: usize, alias: String },
    #[error("plan node {node_index} operand references row binding `{binding}` outside its scope")]
    RowBindingOutOfScope { node_index: usize, binding: String },
    #[error("plan node {node_index} operand contains an empty field path segment")]
    EmptyFieldPathSegment { node_index: usize },
    #[error("plan node {node_index} string operand references undeclared alias `{alias}`")]
    UndeclaredStringAlias { node_index: usize, alias: String },
}

#[derive(Debug, Error)]
pub enum PlanDataInputError {
    #[error("plan node {node_index} derive input references unknown source `{node}`")]
    UnknownSource { node_index: usize, node: String },
    #[error("plan node {node_index} derive input alias must be non-empty")]
    EmptyAlias { node_index: usize },
}

#[derive(Debug, Error)]
pub enum PlanExpressionError {
    #[error("plan node {node_index} effect template kind {kind:?} is not executable")]
    NonExecutableEffect {
        node_index: usize,
        kind: PlanNodeKind,
    },
    #[error("plan node {node_index} effect template input binding {binding_index} requires non-empty from and to aliases")]
    EmptyEffectBinding {
        node_index: usize,
        binding_index: usize,
    },
    #[error("plan node {node_index} {path} input binding {binding_index} requires a non-empty from alias")]
    EmptyExpressionBinding {
        node_index: usize,
        path: String,
        binding_index: usize,
    },
}

#[derive(Debug, Error)]
pub enum PlanValueInputError {
    #[error("plan node {node_index} uses row binding `{binding}` outside its scope")]
    RowBindingOutOfScope { node_index: usize, binding: String },
    #[error("plan node {node_index} node alias `{alias}` refers to `{actual}`, not `{expected}`")]
    NodeAliasMismatch {
        node_index: usize,
        alias: String,
        actual: String,
        expected: String,
    },
    #[error("plan node {node_index} references undeclared input alias `{alias}`")]
    UndeclaredInputAlias { node_index: usize, alias: String },
    #[error("plan node {node_index} references undeclared template alias `{alias}`")]
    UndeclaredTemplateAlias { node_index: usize, alias: String },
    #[error("plan node {node_index} derive inputs repeat alias `{alias}`")]
    DuplicateInputAlias { node_index: usize, alias: String },
}

#[derive(Debug, Error)]
pub enum ComputeTemplateValidationError {
    #[error("plan node {node_index} compute source `{source_id}` is unknown")]
    UnknownSource {
        node_index: usize,
        source_id: String,
    },
    #[error("compute presence contract names an undeclared field")]
    PresenceContractUndeclaredField,
    #[error("plan node {node_index} compute schema must declare fields")]
    EmptySchemaFields { node_index: usize },
    #[error("compute schema declares duplicate field `{field}`")]
    DuplicateSchemaField { field: String },
    #[error("plan node {node_index} page size must be positive")]
    ZeroPageSize { node_index: usize },
    #[error(transparent)]
    SyntheticSchema(#[from] plasm_core::plasm_monad::SyntheticResultSchemaError),
    #[error("Python compute requires its exact typed value schema and no paging")]
    InvalidPythonSchema,
    #[error("plan node {node_index} projection must declare fields")]
    EmptyProjection { node_index: usize },
    #[error("filter contains an empty boolean branch")]
    EmptyFilterBranch,
    #[error(transparent)]
    ValueInput(#[from] PlanValueInputError),
    #[error("plan node {node_index} predicate references unknown binding `{binding}`")]
    UnknownPredicateBinding { node_index: usize, binding: String },
    #[error("plan node {node_index} `with` must declare columns")]
    EmptyWithColumns { node_index: usize },
    #[error("plan node {node_index} aggregate operation must declare aggregates")]
    EmptyAggregates { node_index: usize },
    #[error("aggregate `{name}` requires a field")]
    AggregateFieldRequired { name: String },
    #[error("plan node {node_index} union target `{target}` is unknown")]
    UnknownUnionTarget { node_index: usize, target: String },
    #[error(transparent)]
    Render(#[from] RenderTemplateValidationError),
}

#[derive(Debug, Error)]
pub enum RenderTemplateValidationError {
    #[error("plan node {node_index} render columns must be non-empty")]
    EmptyColumns { node_index: usize },
    #[error("render column name is invalid")]
    InvalidColumnName(#[source] plasm_core::plasm_monad::PlanAtomError),
    #[error("render columns contain duplicate `{column}`")]
    DuplicateColumn { column: String },
    #[error("render alias name is invalid")]
    InvalidAliasName(#[source] plasm_core::plasm_monad::PlanAtomError),
    #[error("render alias `{alias}` does not name a projected wire column")]
    AliasDoesNotNameColumn { alias: String },
    #[error("render binding name is invalid")]
    InvalidBindingName(#[source] plasm_core::plasm_monad::PlanAtomError),
    #[error("render binding name `source` is reserved")]
    ReservedBindingName,
    #[error("plan node {node_index} render template must be non-empty")]
    EmptyTemplate { node_index: usize },
    #[error("plan node {node_index} render template exceeds the supported length")]
    TemplateTooLong { node_index: usize },
    #[error("render template uses abolished dollar interpolation at {span}; use Minijinja {{{{ expression }}}} syntax instead")]
    AbolishedInterpolation { span: String },
    #[error("render template is invalid")]
    InvalidTemplate(#[source] minijinja::Error),
    #[error("render schema must be PlanRender with exactly one string field")]
    InvalidRenderSchema,
}

pub(super) fn validated_plan_expr_ir(
    ir: &PlanExprIr,
    node_index: usize,
    path: &str,
) -> Result<ValidatedPlanExprIr, PlanExpressionError> {
    let _ = (node_index, path);
    let expr = ir.expr.clone();
    Ok(ValidatedPlanExprIr {
        expr,
        projection: ir.projection.clone(),
    })
}

pub(super) fn validated_plan_expr_template(
    template: &PlanExprTemplate,
    node_index: usize,
    path: &str,
) -> Result<ValidatedPlanExprTemplate, PlanExpressionError> {
    validate_plan_expr_template(template, node_index, path)?;
    Ok(ValidatedPlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        input_bindings: template.input_bindings.clone(),
    })
}

pub(super) fn validated_effect_template(
    template: &EffectTemplate,
    node_index: usize,
) -> Result<ValidatedEffectTemplate, PlanExpressionError> {
    validate_effect_template(template, node_index)?;
    Ok(ValidatedEffectTemplate {
        kind: template.kind,
        qualified_entity: template.qualified_entity.clone(),
        ir_template: validated_plan_expr_template(
            &template.ir_template,
            node_index,
            "effect_template.ir_template",
        )?,
        effect_class: template.effect_class,
        result_shape: template.result_shape,
        projection: template.projection.clone(),
        input_bindings: template.input_bindings.clone(),
    })
}

pub(super) fn validate_expression_operands(
    expr: &plasm_core::Expr,
    node_index: usize,
    ctx: &plasm_core::TemplateRefContext<'_>,
) -> Result<(), OperandValidationError> {
    use plasm_core::operand_binding::{BindOperands, OperandResolver, ResolvedValue};
    struct CheckOperands<'a, 'b> {
        ctx: &'a plasm_core::TemplateRefContext<'b>,
        index: usize,
    }
    impl CheckOperands<'_, '_> {
        fn reference(
            &self,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<(), OperandValidationError> {
            let path = match reference {
                plasm_core::PlasmInputRef::NodeInput { node, path } => {
                    if !self
                        .ctx
                        .input_aliases
                        .iter()
                        .any(|(alias, _)| *alias == node)
                    {
                        return Err(OperandValidationError::UndeclaredInputAlias {
                            node_index: self.index,
                            alias: node.clone(),
                        });
                    }
                    path
                }
                plasm_core::PlasmInputRef::RowBinding { binding, path } => {
                    if self.ctx.row_binding != Some(binding.as_str()) {
                        return Err(OperandValidationError::RowBindingOutOfScope {
                            node_index: self.index,
                            binding: binding.clone(),
                        });
                    }
                    path
                }
            };
            if path.iter().any(|segment| segment.trim().is_empty()) {
                return Err(OperandValidationError::EmptyFieldPathSegment {
                    node_index: self.index,
                });
            }
            Ok(())
        }
    }
    impl OperandResolver for CheckOperands<'_, '_> {
        type Error = OperandValidationError;
        fn resolve(
            &mut self,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<ResolvedValue, OperandValidationError> {
            self.reference(reference)?;
            Ok(ResolvedValue::null())
        }
        fn identity(
            &mut self,
            _target: plasm_core::operand_binding::IdentityTarget<'_>,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<plasm_core::EntityId, OperandValidationError> {
            self.reference(reference)?;
            Ok(plasm_core::EntityId::from("operand-inspection"))
        }
        fn string(
            &mut self,
            value: &plasm_core::program_string_template::CompiledProgramString,
        ) -> Result<String, OperandValidationError> {
            for root in value.roots() {
                if self.ctx.row_binding == Some(root.as_str())
                    || self
                        .ctx
                        .input_aliases
                        .iter()
                        .any(|(alias, _)| *alias == root)
                {
                    continue;
                }
                let dotted = value
                    .paths()
                    .iter()
                    .any(|path| path.len() > 1 && path.first() == Some(root));
                if self.ctx.row_binding.is_none() || dotted {
                    return Err(OperandValidationError::UndeclaredStringAlias {
                        node_index: self.index,
                        alias: root.clone(),
                    });
                }
            }
            Ok(value.source().to_owned())
        }
    }
    expr.bind_operands(&mut CheckOperands {
        ctx,
        index: node_index,
    })?;
    Ok(())
}

pub(super) fn validate_effect_template(
    t: &EffectTemplate,
    node_index: usize,
) -> Result<(), PlanExpressionError> {
    if !t.kind.is_template_allowed() {
        return Err(PlanExpressionError::NonExecutableEffect {
            node_index,
            kind: t.kind,
        });
    }
    validate_plan_expr_template(&t.ir_template, node_index, "effect_template.ir_template")?;
    for (binding_index, b) in t.input_bindings.iter().enumerate() {
        if b.from.trim().is_empty() || b.to.trim().is_empty() {
            return Err(PlanExpressionError::EmptyEffectBinding {
                node_index,
                binding_index,
            });
        }
    }
    Ok(())
}

pub(super) fn validate_plan_expr_template(
    template: &PlanExprTemplate,
    node_index: usize,
    path: &str,
) -> Result<(), PlanExpressionError> {
    for (binding_index, binding) in template.input_bindings.iter().enumerate() {
        if binding.from.trim().is_empty() {
            return Err(PlanExpressionError::EmptyExpressionBinding {
                node_index,
                path: path.to_owned(),
                binding_index,
            });
        }
    }
    Ok(())
}

pub(super) fn validate_plan_data_input(
    input: &PlanDataInput,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), PlanDataInputError> {
    if input.node.trim().is_empty() || !by_id.contains_key(&input.node) {
        return Err(PlanDataInputError::UnknownSource {
            node_index,
            node: input.node.clone(),
        });
    }
    if input.alias.trim().is_empty() {
        return Err(PlanDataInputError::EmptyAlias { node_index });
    }
    Ok(())
}

pub(super) fn validate_derive_value_inputs(
    template: &DeriveTemplate,
    node_index: usize,
) -> Result<(), PlanValueInputError> {
    let mut inputs_by_alias = HashMap::new();
    for input in &template.inputs {
        if inputs_by_alias
            .insert(input.alias.as_str(), input.node.as_str())
            .is_some()
        {
            return Err(PlanValueInputError::DuplicateInputAlias {
                node_index,
                alias: input.alias.to_string(),
            });
        }
    }
    validate_plan_value_input_refs(
        &template.value,
        node_index,
        &inputs_by_alias,
        template.item_binding.as_deref(),
    )
}

fn validate_plan_value_input_refs(
    value: &PlanValue,
    node_index: usize,
    inputs_by_alias: &HashMap<&str, &str>,
    item_binding: Option<&str>,
) -> Result<(), PlanValueInputError> {
    use plasm_core::operand_binding::{
        BindOperands, IdentityTarget, OperandResolver, ResolvedValue,
    };
    struct Check<'a> {
        inputs: &'a HashMap<&'a str, &'a str>,
        item: Option<&'a str>,
        index: usize,
    }
    impl OperandResolver for Check<'_> {
        type Error = PlanValueInputError;
        fn resolve(
            &mut self,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<ResolvedValue, PlanValueInputError> {
            match reference {
                plasm_core::PlasmInputRef::NodeInput { node, path } => self.node(node, node, path),
                plasm_core::PlasmInputRef::RowBinding { binding, .. } => {
                    if self.item != Some(binding.as_str())
                        && !(binding == "_" && self.item.is_some())
                    {
                        return Err(PlanValueInputError::RowBindingOutOfScope {
                            node_index: self.index,
                            binding: binding.clone(),
                        });
                    }
                    Ok(ResolvedValue::null())
                }
            }
        }
        fn node(
            &mut self,
            node: &str,
            alias: &str,
            _: &[String],
        ) -> Result<ResolvedValue, PlanValueInputError> {
            match self.inputs.get(alias) {
                Some(input) if *input == node => Ok(ResolvedValue::null()),
                Some(input) => Err(PlanValueInputError::NodeAliasMismatch {
                    node_index: self.index,
                    alias: alias.to_owned(),
                    actual: (*input).to_owned(),
                    expected: node.to_owned(),
                }),
                None => Err(PlanValueInputError::UndeclaredInputAlias {
                    node_index: self.index,
                    alias: alias.to_owned(),
                }),
            }
        }
        fn identity(
            &mut self,
            _: IdentityTarget<'_>,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<plasm_core::EntityId, PlanValueInputError> {
            self.resolve(reference)?;
            Ok(plasm_core::EntityId::from("validation"))
        }
        fn string(
            &mut self,
            template: &plasm_core::program_string_template::CompiledProgramString,
        ) -> Result<String, PlanValueInputError> {
            self.template(template, &[])
        }
        fn template(
            &mut self,
            template: &plasm_core::program_string_template::CompiledProgramString,
            bindings: &[plasm_core::PlanInputBinding],
        ) -> Result<String, PlanValueInputError> {
            for root in template.roots() {
                let alias = bindings
                    .iter()
                    .find(|binding| &binding.to == root)
                    .map(|binding| binding.from.as_str())
                    .unwrap_or(root);
                validate_template_alias(alias, self.index, self.inputs, self.item)?;
            }
            Ok(String::new())
        }
    }
    value.bind_operands(&mut Check {
        inputs: inputs_by_alias,
        item: item_binding,
        index: node_index,
    })?;
    Ok(())
}

fn validate_template_alias(
    alias: &str,
    node_index: usize,
    inputs_by_alias: &HashMap<&str, &str>,
    item_binding: Option<&str>,
) -> Result<(), PlanValueInputError> {
    if item_binding == Some(alias) || inputs_by_alias.contains_key(alias) {
        return Ok(());
    }
    // Under a for_each / derive row cursor, bare Minijinja wires are row fields.
    if item_binding.is_some() {
        return Ok(());
    }
    Err(PlanValueInputError::UndeclaredTemplateAlias {
        node_index,
        alias: alias.to_owned(),
    })
}

pub(super) fn validate_compute_template(
    t: &ComputeTemplate,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), ComputeTemplateValidationError> {
    if t.source.trim().is_empty() || !by_id.contains_key(&t.source) {
        return Err(ComputeTemplateValidationError::UnknownSource {
            node_index,
            source_id: t.source.clone(),
        });
    }
    if !t.schema.optional_fields.iter().all(|name| {
        t.schema
            .fields
            .iter()
            .any(|field| field.name.as_str() == name)
    }) {
        return Err(ComputeTemplateValidationError::PresenceContractUndeclaredField);
    }
    if t.schema.fields.is_empty() && !matches!(t.op, ComputeOp::Python { .. }) {
        return Err(ComputeTemplateValidationError::EmptySchemaFields { node_index });
    }
    let mut seen = std::collections::BTreeSet::new();
    for field in &t.schema.fields {
        if !seen.insert(field.name.as_str().to_string()) {
            return Err(ComputeTemplateValidationError::DuplicateSchemaField {
                field: field.name.as_str().to_owned(),
            });
        }
    }
    if t.page_size == Some(0) {
        return Err(ComputeTemplateValidationError::ZeroPageSize { node_index });
    }
    match &t.op {
        ComputeOp::Python { output_type, .. } => {
            if t.schema
                != plasm_core::plasm_monad::SyntheticResultSchema::for_value(output_type.clone())?
                || t.page_size.is_some()
                || t.collection_alias.is_some()
            {
                return Err(ComputeTemplateValidationError::InvalidPythonSchema);
            }
        }
        ComputeOp::Project { fields } if fields.is_empty() => {
            return Err(ComputeTemplateValidationError::EmptyProjection { node_index });
        }
        ComputeOp::Filter { predicates } => {
            if predicates.has_empty_branch() {
                return Err(ComputeTemplateValidationError::EmptyFilterBranch);
            }
            for p in predicates {
                let collection_operand =
                    matches!(p.op, PlanPredicateOp::In | PlanPredicateOp::NotIn)
                        && matches!(p.value, PlanValue::BindingSymbol { .. });
                if !collection_operand {
                    let inputs = by_id.keys().map(|id| (id.as_str(), id.as_str())).collect();
                    validate_plan_value_input_refs(&p.value, node_index, &inputs, None)?;
                }
                for dependency in p.value.dependencies() {
                    if !by_id.contains_key(&dependency) {
                        return Err(ComputeTemplateValidationError::UnknownPredicateBinding {
                            node_index,
                            binding: dependency,
                        });
                    }
                }
            }
        }
        ComputeOp::With { columns } if columns.is_empty() => {
            return Err(ComputeTemplateValidationError::EmptyWithColumns { node_index });
        }
        ComputeOp::GroupBy { aggregates, .. } | ComputeOp::Aggregate { aggregates } => {
            if aggregates.is_empty() {
                return Err(ComputeTemplateValidationError::EmptyAggregates { node_index });
            }
            for agg in aggregates {
                if agg.function != AggregateFunction::Count && agg.field.is_none() {
                    return Err(ComputeTemplateValidationError::AggregateFieldRequired {
                        name: agg.name.as_str().to_owned(),
                    });
                }
            }
        }
        ComputeOp::Union { other } | ComputeOp::MergeBranches { other } => {
            if !by_id.contains_key(other.as_str()) {
                return Err(ComputeTemplateValidationError::UnknownUnionTarget {
                    node_index,
                    target: other.as_str().to_owned(),
                });
            }
        }
        ComputeOp::Render {
            columns,
            template,
            column_aliases,
            render_bindings,
        } => {
            validate_render_compute_template(
                t,
                columns,
                template,
                column_aliases,
                render_bindings,
                node_index,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_render_compute_template(
    t: &ComputeTemplate,
    columns: &[OutputName],
    template: &str,
    column_aliases: &BTreeMap<String, OutputName>,
    render_bindings: &[OutputName],
    node_index: usize,
) -> Result<(), RenderTemplateValidationError> {
    if columns.is_empty() {
        return Err(RenderTemplateValidationError::EmptyColumns { node_index });
    }
    let mut seen = std::collections::BTreeSet::new();
    for column in columns {
        OutputName::new(column.as_str().to_string())
            .map_err(RenderTemplateValidationError::InvalidColumnName)?;
        if !seen.insert(column.as_str().to_string()) {
            return Err(RenderTemplateValidationError::DuplicateColumn {
                column: column.as_str().to_owned(),
            });
        }
    }
    for (alias, wire) in column_aliases {
        OutputName::new(alias.clone()).map_err(RenderTemplateValidationError::InvalidAliasName)?;
        if !columns.iter().any(|c| c.as_str() == wire.as_str()) {
            return Err(RenderTemplateValidationError::AliasDoesNotNameColumn {
                alias: alias.clone(),
            });
        }
    }
    for label in render_bindings {
        OutputName::new(label.as_str().to_string())
            .map_err(RenderTemplateValidationError::InvalidBindingName)?;
        if label.as_str() == "source" {
            return Err(RenderTemplateValidationError::ReservedBindingName);
        }
    }
    if template.trim().is_empty() {
        return Err(RenderTemplateValidationError::EmptyTemplate { node_index });
    }
    if template.chars().count() > PLAN_RENDER_MAX_TEMPLATE_CHARS {
        return Err(RenderTemplateValidationError::TemplateTooLong { node_index });
    }
    if let Some(span) = plasm_core::find_dollar_interpolation_in_minijinja_body(template) {
        return Err(RenderTemplateValidationError::AbolishedInterpolation {
            span: span.to_owned(),
        });
    }
    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    plasm_core::register_shared_minijinja_filters(&mut env);
    env.add_template("plan_render", template)
        .map_err(RenderTemplateValidationError::InvalidTemplate)?;
    if t.schema.entity.as_deref() != Some("PlanRender")
        || t.schema.fields.len() != 1
        || t.schema.fields[0].value_kind != SyntheticValueKind::String
    {
        return Err(RenderTemplateValidationError::InvalidRenderSchema);
    }
    Ok(())
}
