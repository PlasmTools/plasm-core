//! Derive and validate materialized scalar fields at the Python boundary.
use super::*;

pub(super) fn kind_from_schema(kind: SyntheticValueKind) -> Option<Kind> {
    use plasm_core::value_contract::{ValueContract, ValueShape};
    let shape = match kind {
        SyntheticValueKind::String => ValueShape::Scalar {
            field_type: FieldType::String,
        },
        SyntheticValueKind::Integer => ValueShape::Scalar {
            field_type: FieldType::Integer,
        },
        SyntheticValueKind::Number => ValueShape::Scalar {
            field_type: FieldType::Number,
        },
        SyntheticValueKind::Boolean => ValueShape::Scalar {
            field_type: FieldType::Boolean,
        },
        SyntheticValueKind::Null => ValueShape::Null,
        _ => return None,
    };
    Some(Kind::Typed(ValueContract {
        shape,
        domain: None,
        nullable: false,
    }))
}

pub(super) fn materialize_rows(
    fields: &BTreeMap<String, Kind>,
    rows: &[Value],
    cgs: &CGS,
    entry: &str,
) -> Result<crate::python_pool::TypedRecords, String> {
    if rows.len() > MAX_INPUT_ROWS
        || serde_json::to_vec(rows).map_err(|e| e.to_string())?.len() > 1_048_576
    {
        return Err("compute input budget exceeded".into());
    }
    rows.iter()
        .map(|row| {
            fields
                .iter()
                .map(|(name, kind)| {
                    let value = row
                        .get(name)
                        .ok_or_else(|| format!("compute input missing {name}"))?;
                    if let Kind::Typed(t) = kind {
                        t.validate(value, cgs, entry, name)?;
                        return Ok((name.clone(), value.clone()));
                    }
                    let valid = match kind {
                        Kind::String => value.is_string(),
                        Kind::Integer => value.as_i64().is_some(),
                        Kind::Number => value.is_number(),
                        Kind::Boolean => value.is_boolean(),
                        Kind::Null => value.is_null(),
                        _ => false,
                    };
                    if !valid {
                        return Err(format!("compute input {name}: value violates row schema"));
                    }
                    Ok((name.clone(), value.clone()))
                })
                .collect()
        })
        .collect()
}

pub(super) fn source_field_kind(
    es: &crate::execute_session::ExecuteSession,
    nodes: &[crate::plasm_plan::ValidatedPlanNode],
    id: &str,
    field: &str,
    depth: usize,
) -> Result<plasm_core::value_contract::ValueContract, String> {
    use crate::plasm_plan::ValidatedPlanNode as Node;
    use plasm_core::plasm_monad::ComputeOp;
    if depth > 256 {
        return Err("Python source schema depth exceeded".into());
    }
    let node = nodes
        .iter()
        .find(|n| n.id().as_str() == id)
        .ok_or("missing Python schema source")?;
    let owner = match node {
        Node::Surface(s) => {
            if !s.projection.is_empty() && !s.projection.iter().any(|f| f == field) {
                return Err("Python field omitted by source projection".into());
            }
            s.qualified_entity.as_ref().ok_or("missing schema owner")?
        }
        Node::RelationTraversal(s) => {
            if s.relation
                .ir
                .projection
                .as_ref()
                .is_some_and(|p| !p.iter().any(|f| f == field))
            {
                return Err("Python field omitted by relation projection".into());
            }
            &s.relation.target
        }
        Node::ForEach(source) => {
            for projection in [&source.projection, &source.effect_template.projection] {
                if !projection.is_empty() && !projection.iter().any(|f| f == field) {
                    return Err("Python field omitted by fanout projection".into());
                }
            }
            &source.effect_template.qualified_entity
        }
        Node::Derive(d) => {
            let t = plasm_core::value_contract::ValueContract::data_value(
                &d.value,
                &mut |binding, path| {
                    let source = if binding == d.item_binding.as_str() {
                        d.source.as_str()
                    } else {
                        binding
                    };
                    source_field_kind(es, nodes, source, &path.join("."), depth + 1)
                },
            )?;
            let plasm_core::value_contract::ValueShape::Record { fields } = t.shape else {
                return Err("derived Python input must be a record".into());
            };
            return fields
                .get(field)
                .cloned()
                .ok_or("missing derived field".into());
        }
        Node::Compute(c) => {
            let next =
                |field: &str| source_field_kind(es, nodes, &c.compute.source, field, depth + 1);
            return match &c.compute.op {
                ComputeOp::Project { fields } => next(
                    &fields
                        .iter()
                        .find(|(k, _)| k.as_str() == field)
                        .ok_or("unknown projected Python field")?
                        .1
                        .dotted(),
                ),
                ComputeOp::With { columns } => {
                    match columns.iter().find(|c| c.name.as_str() == field) {
                        Some(c) => match &c.expr {
                            plasm_core::WithExpr::Field(path) => next(&path.dotted()),
                            _ => plasm_core::value_contract::ValueContract::with_expr(
                                &c.expr,
                                &mut |p| next(&p.dotted()),
                            ),
                        },
                        None => next(field),
                    }
                }
                ComputeOp::Aggregate { aggregates } | ComputeOp::GroupBy { aggregates, .. } => {
                    if let Some(a) = aggregates.iter().find(|a| a.name.as_str() == field) {
                        let input = a.field.as_ref().map(|f| next(&f.dotted())).transpose()?;
                        Ok(plasm_core::value_contract::ValueContract::aggregate(
                            a.function,
                            input.as_ref(),
                        ))
                    } else if matches!(&c.compute.op, ComputeOp::GroupBy { keys, .. } if keys.iter().any(|k| k.dotted() == field))
                    {
                        next(field)
                    } else {
                        Err("unknown aggregate Python field".into())
                    }
                }
                ComputeOp::Filter { .. }
                | ComputeOp::Sort { .. }
                | ComputeOp::Limit { .. }
                | ComputeOp::DedupeBy { .. } => next(field),
                ComputeOp::Union { other } => {
                    let left = next(field)?;
                    let right = source_field_kind(es, nodes, other.as_str(), field, depth + 1)?;
                    if left != right {
                        return Err("Python union field type mismatch".into());
                    }
                    Ok(left)
                }
                ComputeOp::Python { .. } | ComputeOp::Render { .. } if field == "content" => Ok(
                    plasm_core::value_contract::ValueContract::scalar(FieldType::String),
                ),
                _ => Err("unsupported Python row schema source".into()),
            };
        }
        _ => return Err("unsupported Python row schema source".into()),
    };
    let cgs =
        crate::catalog_ownership::resolve_cgs_for_entry_entity(es, &owner.entry_id, &owner.entity)?;
    let entity = cgs
        .get_entity(&owner.entity)
        .ok_or("unknown Python row entity")?;
    if let Some(relation) = entity
        .relations
        .get(field)
        .filter(|_| !entity.fields.contains_key(field))
    {
        return Ok(super::observed_relation_type(relation, &owner.entry_id));
    }
    let f = entity.fields.get(field).ok_or("unknown Python row field")?;
    let mut t = plasm_core::value_contract::ValueContract::from_domain(
        cgs,
        &owner.entry_id,
        f.kind.registry_key(),
    )?;
    t.nullable = !f.required;
    Ok(t)
}

pub(super) fn validate_source_owner<'a>(
    nodes: &'a [crate::plasm_plan::ValidatedPlanNode],
    mut id: &'a str,
    expected: &EntityBinding,
) -> Result<(), String> {
    use crate::plasm_plan::ValidatedPlanNode as Node;
    for _ in 0..256 {
        let node = nodes
            .iter()
            .find(|n| n.id().as_str() == id)
            .ok_or("missing Python schema source")?;
        let owner = match node {
            Node::Derive(d) => {
                id = d.source.as_str();
                continue;
            }
            Node::Compute(c) => {
                id = &c.compute.source;
                continue;
            }
            Node::Surface(s) => s
                .qualified_entity
                .as_ref()
                .ok_or("missing Python source owner")?,
            Node::RelationTraversal(s) => &s.relation.target,
            Node::ForEach(s) => &s.effect_template.qualified_entity,
            _ => return Err("unsupported Python schema owner".into()),
        };
        if owner.entry_id != expected.entry_id.as_str() || owner.entity != expected.entity.as_str()
        {
            return Err("Python input schema catalog ownership mismatch".into());
        }
        return Ok(());
    }
    Err("Python schema ownership depth exceeded".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn observed_relation_values_preserve_types_without_traversal_or_coverage_promotion() {
        use plasm_core::symbol_tuning::SymbolRender;
        use plasm_core::value_contract::ValueShape;
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let symbols =
            plasm_core::TeachingExposureSession::new(&cgs, "matrix", &["LangItem"]).to_symbol_map();
        let token = symbols.entity_sym_for("matrix", "LangItem");
        let source = format!("@compute\ndef render(row: Value[{token}]) -> str:\n    return f'relation_count={{len(row.lines)}}'\n");
        let checked =
            CheckedCompute::compile_input(&source, &cgs, "matrix", symbols.as_ref(), None, true)
                .unwrap();
        let relation = &checked.contract.fields["lines"].value_type;
        assert!(
            matches!(&relation.shape, ValueShape::Array { element } if matches!(&element.shape, ValueShape::Scalar { field_type: FieldType::EntityRef {entry_id, target} } if entry_id.as_str() == "matrix" && target.as_str() == "LangLine"))
        );
        let declaration = checked.contract.declaration(symbols.as_ref()).unwrap();
        assert!(declaration.contains("lines: list[EntityRef]"));
        let pool = crate::python_pool::PythonPool::default();
        for (rows, expected) in [
            (
                json!({"lines": ["LangLine:l1", "LangLine:l2"]}),
                "relation_count=2",
            ),
            (json!({"lines": []}), "relation_count=0"),
        ] {
            assert_eq!(
                checked
                    .run(
                        &pool,
                        &checked.contract.owner,
                        ResultCoverage::Complete,
                        &[rows]
                    )
                    .await
                    .unwrap(),
                expected
            );
        }
        for rows in [json!({}), json!({"lines": null}), json!({"lines": [null]})] {
            assert!(checked
                .run(
                    &pool,
                    &checked.contract.owner,
                    ResultCoverage::Complete,
                    &[rows]
                )
                .await
                .is_err());
        }
        assert!(checked
            .run(
                &pool,
                &checked.contract.owner,
                ResultCoverage::Unknown,
                &[json!({"lines": []})]
            )
            .await
            .is_err());
        for expression in [
            "row.lines.list()",
            "row.lines[0].DELETE()",
            "row.lines[0].title",
        ] {
            let source = format!(
                "@compute\ndef render(row: Value[{token}]) -> str:\n    return str({expression})\n"
            );
            assert!(CheckedCompute::compile_input(
                &source,
                &cgs,
                "matrix",
                symbols.as_ref(),
                None,
                true
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn nested_record_arrays_use_the_same_checked_contract_and_codec() {
        use plasm_core::symbol_tuning::SymbolRender;
        use plasm_core::value_contract::{ValueContract as T, ValueShape};
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let symbols =
            plasm_core::TeachingExposureSession::new(&cgs, "types", &["Sample"]).to_symbol_map();
        let token = symbols.entity_sym_for("types", "Sample");
        let count = T::from_domain(
            &cgs,
            "types",
            &plasm_core::ValueDomainKey::new("count").unwrap(),
        )
        .unwrap();
        let element = T {
            shape: ValueShape::Record {
                fields: BTreeMap::from([("n".into(), count)]),
            },
            domain: None,
            nullable: false,
        };
        let value_type = T {
            shape: ValueShape::Array {
                element: Box::new(element),
            },
            domain: None,
            nullable: false,
        };
        let schema = SyntheticResultSchema {
            entity: None,
            fields: vec![plasm_core::SyntheticFieldSchema {
                name: plasm_core::OutputName::new("records").unwrap(),
                value_kind: value_type.summary(),
                value_type: Some(value_type),
                source: None,
            }],
        };
        let checked = CheckedCompute::compile_input("@compute\ndef render(row: Row) -> str:\n    return '|'.join(str(item.n) for item in row.records)\n", &cgs, "types", symbols.as_ref(), Some((&schema, &token)), true).unwrap();
        let indexed = CheckedCompute::compile_input(
            "@compute\ndef render(row: Row) -> str:\n    return str(row.records[0]['n'])\n",
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, &token)),
            true,
        );
        assert!(matches!(indexed, Err(error) if error.contains("record.field")));
        let pool = crate::python_pool::PythonPool::default();
        let json_access = CheckedCompute::compile_input(
            &format!("@compute\ndef render(row: Value[{token}]) -> str:\n    return str(row.document['n'])\n"),
            &cgs,
            "types",
            symbols.as_ref(),
            None,
            true,
        ).unwrap();
        assert_eq!(
            json_access
                .run(
                    &pool,
                    &json_access.contract.owner,
                    ResultCoverage::Complete,
                    &[json!({"document": {"n": 7}})]
                )
                .await
                .unwrap(),
            "7"
        );
        for (rows, expected) in [
            (
                json!({"records": [{"n": 9007199254740993_i64}, {"n": 2}]}),
                "9007199254740993|2",
            ),
            (json!({"records": []}), ""),
        ] {
            assert_eq!(
                checked
                    .run(
                        &pool,
                        &checked.contract.owner,
                        ResultCoverage::Complete,
                        &[rows]
                    )
                    .await
                    .unwrap(),
                expected
            );
        }
        let bad = checked
            .run(
                &pool,
                &checked.contract.owner,
                ResultCoverage::Complete,
                &[json!({"records": [{"n": true}]})],
            )
            .await
            .unwrap_err();
        assert!(bad.contains("records[0].n"), "{bad}");
    }

    #[test]
    fn scalar_rows_preserve_types_order_duplicates_and_empty() {
        let fields = BTreeMap::from([
            ("n".into(), Kind::Integer),
            ("s".into(), Kind::String),
            ("b".into(), Kind::Boolean),
            ("f".into(), Kind::Number),
        ]);
        let row = json!({"n": 3, "s": "three", "b": true, "f": 1.5, "unconsumed": {"secret": 1}});
        let rows = materialize_rows(&fields, &[row.clone(), row], &CGS::default(), "test").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], rows[1]);
        assert_eq!(rows[0]["n"], json!(3));
        assert_eq!(rows[0]["f"], json!(1.5));
        assert_eq!(rows[0]["b"], json!(true));
        assert!(!rows[0].contains_key("unconsumed"));
        assert!(materialize_rows(&fields, &[], &CGS::default(), "test")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn row_boundary_rejects_wrong_types_missing_fields_and_excess() {
        let fields = BTreeMap::from([("n".into(), Kind::Integer)]);
        for bad in [
            json!({}),
            json!({"n": "3"}),
            json!({"n": true}),
            json!({"n": 3.5}),
            json!({"n": null}),
            json!({"n": u64::MAX}),
        ] {
            assert!(materialize_rows(&fields, &[bad], &CGS::default(), "test").is_err());
        }
        assert!(materialize_rows(
            &fields,
            &vec![json!({"n": 1}); MAX_INPUT_ROWS + 1],
            &CGS::default(),
            "test"
        )
        .is_err());
        assert!(validate_input_budget(&[json!({"s": "x".repeat(1_048_576)})]).is_err());
    }
}
