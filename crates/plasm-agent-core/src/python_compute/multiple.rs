//! Independent compute arguments are fields of one reviewed dependency packet.
//! Packing preserves each collection; it never joins or zips their rows.
use super::*;
use plasm_core::value_contract::ValueShape;

impl PreparedCompute {
    pub(super) fn prepare_multiple(
        source: &str,
        def: &ruff_python_ast::StmtFunctionDef,
        imports: &crate::python_datetime::Imports,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        rows: Option<(&SyntheticResultSchema, &str)>,
        domains: &ReturnDomains,
    ) -> Result<Self, String> {
        let p = &def.parameters;
        if !p.posonlyargs.is_empty()
            || !p.kwonlyargs.is_empty()
            || p.vararg.is_some()
            || p.kwarg.is_some()
            || p.args.iter().any(|a| a.default.is_some())
        {
            return Err("compute requires annotated positional inputs without defaults".into());
        }
        let (schema, token) =
            rows.ok_or("multi-input compute requires a typed dependency packet")?;
        if !token.is_empty() {
            return Err("compute dependency packet cannot carry entity authority".into());
        }
        let input = schema.row_contract()?;
        let ValueShape::Record { fields } = &input.shape else {
            return Err("compute inputs require a record packet".into());
        };
        if fields.len() != p.args.len() {
            return Err("compute dependency count mismatch".into());
        }
        let mut stubs = upstream::stubs_in(fields, cgs, &domains.catalogs)?;
        upstream::domain_aliases(&domains.types, cgs, &domains.catalogs, &mut stubs)?;
        let mut typed_inputs = fields.clone();
        let mut parameters = Vec::new();
        let mut arguments = Vec::new();
        for argument in &p.args {
            let parameter = &argument.parameter;
            let annotation = parameter
                .annotation
                .as_deref()
                .ok_or("every compute input requires an annotation")?;
            let actual = fields
                .get(parameter.name.as_str())
                .ok_or("missing named compute dependency")?;
            let (row_annotation, row_type) = match annotation {
                Expr::Subscript(s)
                    if name(&s.value) == Some("list") && arguments::is_row(&s.slice) =>
                {
                    let ValueShape::Array { element } = &actual.shape else {
                        return Err("list[Row] input requires a collection".into());
                    };
                    (&*s.slice, &**element)
                }
                _ => (annotation, actual),
            };
            let mut expression = format!("__input[0].{}", parameter.name);
            if arguments::is_row(row_annotation) {
                if !matches!(
                    row_type.shape,
                    ValueShape::Record { .. } | ValueShape::ObservedRecord { .. }
                ) {
                    return Err("Row input requires a record".into());
                }
                if name(row_annotation) != Some("Row") {
                    let expected = returns::resolve(
                        row_annotation,
                        row_type,
                        &domains.types,
                        &domains.catalogs,
                        cgs,
                        entry,
                        symbols,
                        &imports.source,
                    )?;
                    if !inference::assignable(row_type, &expected)? {
                        return Err(
                            "compute row annotation differs from its dependency contract".into(),
                        );
                    }
                }
            } else {
                let expected = returns::prepare(
                    annotation,
                    actual,
                    &domains.types,
                    &domains.catalogs,
                    cgs,
                    entry,
                    symbols,
                    &imports.source,
                )?;
                if let Err(original) = expected.check(actual) {
                    let (record, collection) = match &actual.shape {
                        ValueShape::Array { element } => (&**element, true),
                        _ => (actual, false),
                    };
                    let record_fields = match &record.shape {
                        ValueShape::Record { fields }
                        | ValueShape::ObservedRecord { fields, .. } => fields,
                        _ => return Err(original),
                    };
                    let adapted = arguments::resolve(
                        annotation,
                        record,
                        record_fields,
                        domains,
                        cgs,
                        entry,
                        symbols,
                        &imports.source,
                        if collection {
                            ComputeInputMode::Collection
                        } else {
                            ComputeInputMode::Singleton
                        },
                    )?;
                    if adapted.mapping.is_none() || adapted.per_row == collection {
                        return Err(original);
                    }
                    let input = if collection {
                        expression
                    } else {
                        format!("[{expression}]")
                    };
                    expression = adapted.expression().replace("__input", &input);
                    typed_inputs.insert(parameter.name.to_string(), adapted.value_type);
                }
            }
            parameters.push(format!(
                "{}: {}",
                parameter.name,
                upstream::input_type(
                    &typed_inputs[parameter.name.as_str()],
                    cgs,
                    &domains.catalogs,
                    &mut stubs
                )?
            ));
            arguments.push(expression);
        }
        let body_imports = diagnostic_imports(source, def, &imports.source);
        let inferred_return_row = if def
            .returns
            .as_deref()
            .is_some_and(returns::uses_row_placeholder)
        {
            let inputs = p
                .args
                .iter()
                .map(|arg| {
                    let name = arg.parameter.name.as_str();
                    (name, &typed_inputs[name])
                })
                .collect::<Vec<_>>();
            let output =
                inference::infer_body(&definition_body(source, def)?, &inputs, &body_imports)?;
            Some(returns::inferred_row_binding(&output)?)
        } else {
            None
        };
        let annotation = def
            .returns
            .as_deref()
            .map(|a| {
                returns::prepare(
                    a,
                    inferred_return_row.as_ref().unwrap_or(&input),
                    &domains.types,
                    &domains.catalogs,
                    cgs,
                    entry,
                    symbols,
                    &body_imports,
                )
            })
            .transpose()?;
        if let Some(annotation) = &annotation {
            let inputs = p
                .args
                .iter()
                .map(|arg| {
                    let name = arg.parameter.name.as_str();
                    (name, &typed_inputs[name])
                })
                .collect::<Vec<_>>();
            annotation.check_body(
                &definition_body(source, def)?,
                &inputs,
                cgs,
                &domains.catalogs,
            )?;
        }
        let declared_output = annotation
            .as_ref()
            .map(|a| a.contract())
            .transpose()?
            .flatten();
        let output = if let Some(output) = declared_output {
            output
        } else {
            let inputs = p
                .args
                .iter()
                .map(|arg| {
                    let name = arg.parameter.name.as_str();
                    (name, &typed_inputs[name])
                })
                .collect::<Vec<_>>();
            let output =
                inference::infer_body(&definition_body(source, def)?, &inputs, &body_imports)?;
            if let Some(annotation) = &annotation {
                annotation.check(&output)?;
            }
            output
        };
        let output_type = upstream::output_type(&output, cgs, &domains.catalogs, &mut stubs)?;
        let body = definition_body(source, def)?;
        let definition = format!(
            "import datetime as PlasmDatetime\n{}\ndef {}({}) -> {}:{}\n",
            imports.source,
            def.name,
            parameters.join(", "),
            output_type,
            body
        );
        let executable = format!("{}\n{}({})", definition, def.name, arguments.join(", "));
        Ok(Self {
            independent_inputs: true,
            optional_fields: schema.optional_fields.clone(),
            contract: None,
            context: std::sync::Arc::new(cgs.clone()),
            entry: entry.into(),
            argument: arguments::Argument {
                mapping: None,
                field: None,
                per_row: true,
                value_type: input.clone(),
            },
            executable,
            definition,
            stubs,
            row_fields: Some(fields.clone()),
            per_row: true,
            output,
            catalogs: domains.catalogs.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::value_contract::ValueContract as Type;

    #[test]
    fn multi_input_return_row_is_inferred_from_returned_rows() {
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let symbols = plasm_core::symbol_tuning::TeachingExposureSession::new(&cgs, "types", &[])
            .to_symbol_map();
        let collection = |element| Type {
            shape: ValueShape::Array {
                element: Box::new(element),
            },
            domain: None,
            nullable: false,
        };
        let contact = Type::record(
            BTreeMap::from([("email".into(), Type::scalar(FieldType::String))]),
            Default::default(),
        );
        let mut timestamp = plasm_core::temporal_value::TemporalKind::Datetime.contract();
        timestamp.nullable = true;
        let item = Type::record(
            BTreeMap::from([
                ("recipient".into(), Type::scalar(FieldType::String)),
                ("created_at".into(), timestamp),
                (
                    "state".into(),
                    Type::from_domain(
                        &cgs,
                        "types",
                        &plasm_core::ValueDomainKey::new("state").unwrap(),
                    )
                    .unwrap(),
                ),
            ]),
            Default::default(),
        );
        let schema = SyntheticResultSchema::for_value(Type::record(
            BTreeMap::from([
                ("contacts".into(), collection(contact)),
                ("items".into(), collection(item.clone())),
            ]),
            Default::default(),
        ))
        .unwrap();
        let source = "from datetime import date\n@compute\ndef select(contacts: list[Row], items: list[Row]) -> list[Row]:\n    emails = set(c.email for c in contacts)\n    selected = []\n    for item in items:\n        if item.recipient in emails and item.created_at is not None:\n            if item.created_at.date() <= date(2025, 1, 1):\n                selected.append(item)\n    return selected\n";
        let prepared = PreparedCompute::prepare_input(
            source,
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, "")),
            ComputeInputMode::Singleton,
        )
        .unwrap();
        assert_eq!(prepared.output, collection(item.clone()));
        prepared.admit().unwrap();
        // Dependency order cannot select the meaning of the return placeholder.
        let reversed = source.replace(
            "contacts: list[Row], items: list[Row]",
            "items: list[Row], contacts: list[Row]",
        );
        let prepared = PreparedCompute::prepare_input(
            &reversed,
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, "")),
            ComputeInputMode::Singleton,
        )
        .unwrap();
        assert_eq!(prepared.output, collection(item.clone()));
        prepared.admit().unwrap();
        let single = source
            .replace("-> list[Row]", "-> Row")
            .replace("return selected", "return items[0]");
        let prepared = PreparedCompute::prepare_input(
            &single,
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, "")),
            ComputeInputMode::Singleton,
        )
        .unwrap();
        assert_eq!(prepared.output, item);
        prepared.admit().unwrap();
        for invalid in [
            source.replace("return selected", "return [1]"),
            source.replace("-> list[Row]", "-> Row"),
            source.replace("item.created_at.date()", "item.missing.date()"),
            source.replace("-> list[Row]", "-> list[int]"),
        ] {
            assert!(
                PreparedCompute::prepare_input(
                    &invalid,
                    &cgs,
                    "types",
                    symbols.as_ref(),
                    Some((&schema, "")),
                    ComputeInputMode::Singleton,
                )
                .is_err(),
                "accepted {invalid}"
            );
        }
    }
}
