use super::*;

pub(super) fn validated_plan_expr_ir(
    ir: &PlanExprIr,
    node_index: usize,
    path: &str,
) -> Result<ValidatedPlanExprIr, String> {
    let _ = (node_index, path);
    let expr = ir.expr.clone();
    Ok(ValidatedPlanExprIr {
        expr,
        projection: ir.projection.clone(),
        display_expr: ir.display_expr.clone(),
    })
}

pub(super) fn validated_plan_expr_template(
    template: &PlanExprTemplate,
    node_index: usize,
    path: &str,
) -> Result<ValidatedPlanExprTemplate, String> {
    validate_plan_expr_template(template, node_index, path)?;
    Ok(ValidatedPlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        display_expr: template.display_expr.clone(),
        input_bindings: template.input_bindings.clone(),
    })
}

pub(super) fn validated_effect_template(
    template: &EffectTemplate,
    node_index: usize,
) -> Result<ValidatedEffectTemplate, String> {
    validate_effect_template(template, node_index)?;
    Ok(ValidatedEffectTemplate {
        kind: template.kind,
        qualified_entity: template.qualified_entity.clone(),
        expr_template: template.expr_template.clone(),
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
) -> Result<(), String> {
    use plasm_core::operand_binding::{BindOperands, OperandResolver, ResolvedValue};
    struct CheckOperands<'a, 'b> {
        ctx: &'a plasm_core::TemplateRefContext<'b>,
        index: usize,
    }
    impl CheckOperands<'_, '_> {
        fn reference(&self, reference: &plasm_core::PlasmInputRef) -> Result<(), String> {
            let path = match reference {
                plasm_core::PlasmInputRef::NodeInput { node, path } => {
                    if !self
                        .ctx
                        .input_aliases
                        .iter()
                        .any(|(alias, _)| *alias == node)
                    {
                        return Err(format!(
                            "plan.nodes[{}] operand references undeclared input alias {node:?}",
                            self.index
                        ));
                    }
                    path
                }
                plasm_core::PlasmInputRef::RowBinding { binding, path } => {
                    if self.ctx.row_binding != Some(binding.as_str()) {
                        return Err(format!("plan.nodes[{}] operand references row binding {binding:?} outside its scope", self.index));
                    }
                    path
                }
            };
            if path.iter().any(|segment| segment.trim().is_empty()) {
                return Err(format!(
                    "plan.nodes[{}] operand contains an empty field path segment",
                    self.index
                ));
            }
            Ok(())
        }
    }
    impl OperandResolver for CheckOperands<'_, '_> {
        type Error = String;
        fn resolve(
            &mut self,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<ResolvedValue, String> {
            self.reference(reference)?;
            Ok(ResolvedValue::null())
        }
        fn identity(
            &mut self,
            _target: plasm_core::operand_binding::IdentityTarget<'_>,
            reference: &plasm_core::PlasmInputRef,
        ) -> Result<plasm_core::EntityId, String> {
            self.reference(reference)?;
            Ok(plasm_core::EntityId::from("operand-inspection"))
        }
        fn string(
            &mut self,
            value: &plasm_core::program_string_template::CompiledProgramString,
        ) -> Result<String, String> {
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
                    return Err(format!(
                        "plan.nodes[{}] string operand references undeclared alias {root:?}",
                        self.index
                    ));
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
) -> Result<(), String> {
    if !t.kind.is_template_allowed() {
        return Err(format!(
            "plan.nodes[{node_index}].effect_template.kind {:?} is not executable",
            t.kind
        ));
    }
    validate_plan_expr_template(&t.ir_template, node_index, "effect_template.ir_template")?;
    for b in &t.input_bindings {
        if b.from.trim().is_empty() || b.to.trim().is_empty() {
            return Err(format!(
                "plan.nodes[{node_index}].effect_template.input_bindings must be non-empty"
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_plan_expr_template(
    template: &PlanExprTemplate,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    for binding in &template.input_bindings {
        if binding.from.trim().is_empty() {
            return Err(format!(
                "plan.nodes[{node_index}].{path}.input_bindings must have non-empty from"
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_plan_data_input(
    input: &PlanDataInput,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
    if input.node.trim().is_empty() || !by_id.contains_key(&input.node) {
        return Err(format!(
            "plan.nodes[{node_index}].derive_template.inputs references unknown id {:?}",
            input.node
        ));
    }
    if input.alias.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].derive_template.inputs alias must be non-empty"
        ));
    }
    Ok(())
}

pub(super) fn validate_derive_value_inputs(
    template: &DeriveTemplate,
    node_index: usize,
) -> Result<(), String> {
    let mut inputs_by_alias = HashMap::new();
    for input in &template.inputs {
        if inputs_by_alias
            .insert(input.alias.as_str(), input.node.as_str())
            .is_some()
        {
            return Err(format!(
                "plan.nodes[{node_index}].derive_template.inputs duplicate alias {:?}",
                input.alias
            ));
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
) -> Result<(), String> {
    match value {
        PlanValue::NodeSymbol { node, alias, .. } => match inputs_by_alias.get(alias.as_str()) {
            Some(input_node) if *input_node == node.as_str() => Ok(()),
            Some(input_node) => Err(format!(
                "plan.nodes[{node_index}].derive_template.value node symbol alias {:?} points at {:?}, not {:?}",
                alias, input_node, node
            )),
            None => Err(format!(
                "plan.nodes[{node_index}].derive_template.value node symbol alias {:?} is not declared in inputs",
                alias
            )),
        },
        PlanValue::Template {
            template,
            input_bindings,
        } => {
            for binding in input_bindings {
                let Some((alias, _)) = binding.from.split_once('.') else {
                    continue;
                };
                validate_template_alias(alias, node_index, inputs_by_alias, item_binding)?;
            }
            for alias in template.roots() {
                validate_template_alias(alias, node_index, inputs_by_alias, item_binding)?;
            }
            Ok(())
        }
        PlanValue::Array { items } => {
            for item in items {
                validate_plan_value_input_refs(item, node_index, inputs_by_alias, item_binding)?;
            }
            Ok(())
        }
        PlanValue::EntityRefKey { key, .. } => {
            validate_plan_value_input_refs(key, node_index, inputs_by_alias, item_binding)
        }
        PlanValue::Object { fields } => {
            for field in fields.values() {
                validate_plan_value_input_refs(field, node_index, inputs_by_alias, item_binding)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_template_alias(
    alias: &str,
    node_index: usize,
    inputs_by_alias: &HashMap<&str, &str>,
    item_binding: Option<&str>,
) -> Result<(), String> {
    if item_binding == Some(alias) || inputs_by_alias.contains_key(alias) {
        return Ok(());
    }
    // Under a for_each / derive row cursor, bare Minijinja wires are row fields.
    if item_binding.is_some() {
        return Ok(());
    }
    Err(format!(
        "plan.nodes[{node_index}].derive_template.value template references undeclared alias {alias:?}"
    ))
}

pub(super) fn validate_compute_template(
    t: &ComputeTemplate,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
    if t.source.trim().is_empty() || !by_id.contains_key(&t.source) {
        return Err(format!(
            "plan.nodes[{node_index}].compute.source references unknown id {:?}",
            t.source
        ));
    }
    if t.schema.fields.is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].compute.schema.fields must be non-empty"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for field in &t.schema.fields {
        if !seen.insert(field.name.as_str().to_string()) {
            return Err(format!(
                "plan.nodes[{node_index}].compute.schema.fields contains duplicate {:?}",
                field.name.as_str()
            ));
        }
    }
    if t.page_size == Some(0) {
        return Err(format!(
            "plan.nodes[{node_index}].compute.page_size must be greater than zero"
        ));
    }
    match &t.op {
        ComputeOp::Project { fields } if fields.is_empty() => {
            return Err(format!(
                "plan.nodes[{node_index}].compute.project.fields must be non-empty"
            ));
        }
        ComputeOp::Filter { predicates } => {
            for (j, p) in predicates.iter().enumerate() {
                validate_predicate(p, node_index, j)?;
            }
        }
        ComputeOp::With { columns } if columns.is_empty() => {
            return Err(format!(
                "plan.nodes[{node_index}].compute.with.columns must be non-empty"
            ));
        }
        ComputeOp::GroupBy { aggregates, .. } | ComputeOp::Aggregate { aggregates } => {
            if aggregates.is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].compute aggregates must be non-empty"
                ));
            }
            for agg in aggregates {
                if agg.function != AggregateFunction::Count && agg.field.is_none() {
                    return Err(format!(
                        "plan.nodes[{node_index}].compute aggregate {:?} requires a field",
                        agg.name.as_str()
                    ));
                }
            }
        }
        ComputeOp::Limit { count } if *count == 0 => {
            return Err(format!(
                "plan.nodes[{node_index}].compute.limit.count must be greater than zero"
            ));
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
) -> Result<(), String> {
    if columns.is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.columns must be non-empty"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for column in columns {
        OutputName::new(column.as_str().to_string())
            .map_err(|e| format!("plan.nodes[{node_index}].compute.render.columns: {e}"))?;
        if !seen.insert(column.as_str().to_string()) {
            return Err(format!(
                "plan.nodes[{node_index}].compute.render.columns has duplicate {:?}",
                column.as_str()
            ));
        }
    }
    for (alias, wire) in column_aliases {
        OutputName::new(alias.clone())
            .map_err(|e| format!("plan.nodes[{node_index}].compute.render.column_aliases: {e}"))?;
        if !columns.iter().any(|c| c.as_str() == wire.as_str()) {
            return Err(format!(
                "plan.nodes[{node_index}].compute.render.column_aliases[{alias:?}] must map to a wire column"
            ));
        }
    }
    for label in render_bindings {
        OutputName::new(label.as_str().to_string())
            .map_err(|e| format!("plan.nodes[{node_index}].compute.render.render_bindings: {e}"))?;
        if matches!(label.as_str(), "rows" | "source") {
            return Err(format!(
                "plan.nodes[{node_index}].compute.render.render_bindings must not use reserved name {:?}",
                label.as_str()
            ));
        }
    }
    if template.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.template must be non-empty"
        ));
    }
    if template.chars().count() > PLAN_RENDER_MAX_TEMPLATE_CHARS {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.template exceeds {PLAN_RENDER_MAX_TEMPLATE_CHARS} characters"
        ));
    }
    if let Some(span) = plasm_core::find_dollar_interpolation_in_minijinja_body(template) {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.template uses abolished `${{…}}` interpolation ({span}); use Minijinja `{{ … }}` over `rows` (also bound under the source label when applicable). Later string params use `{{{{ binding.content }}}}` or bare `param=binding.content`."
        ));
    }
    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    env.add_template("plan_render", template)
        .map_err(|e| format!("plan.nodes[{node_index}].compute.render.template: {e}"))?;
    if t.schema.entity.as_deref() != Some("PlanRender")
        || t.schema.fields.len() != 1
        || t.schema.fields[0].name.as_str() != "content"
        || t.schema.fields[0].value_kind != SyntheticValueKind::String
    {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.schema must be entity PlanRender with a single string field named 'content'"
        ));
    }
    Ok(())
}
