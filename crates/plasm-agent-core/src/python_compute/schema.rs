//! Derive and validate materialized scalar fields at the Python boundary.
use super::*;

#[derive(Debug, thiserror::Error)]
pub enum PythonSchemaError {
    #[error("Python source schema dependency depth exceeded")]
    DependencyDepthExceeded,
    #[error("Python source schema node is absent")]
    SourceNodeMissing,
    #[error("Python source schema owner is absent")]
    SourceOwnerMissing,
    #[error("Python source field is absent")]
    SourceFieldMissing,
    #[error("Python source field is absent from its port schema")]
    PortFieldMissing,
    #[error("Python compute input row is missing required field {field}")]
    InputFieldMissing { field: String },
    #[error("Python source field is excluded by its projection")]
    FieldExcludedByProjection,
    #[error("Python source has no schema owner")]
    SchemaOwnerMissing,
    #[error("Python source field is unknown to its catalog entity")]
    CatalogFieldMissing,
    #[error("Python source schema cannot represent the requested operation")]
    UnsupportedSource,
    #[error("Python union field contracts are incompatible")]
    UnionFieldMismatch,
    #[error("Python input catalog ownership does not match its source")]
    CatalogOwnershipMismatch,
    #[error("Python schema owner does not match its input contract")]
    InputSchemaOwnershipMismatch,
    #[error("Python source has no recursive record schema")]
    RecursiveSchemaMissing,
    #[error(transparent)]
    ValueContract(#[from] plasm_core::value_contract::ValueContractError),
    #[error(transparent)]
    RowContract(#[from] plasm_core::row_plan::contracts::RowContractError),
    #[error(transparent)]
    Arithmetic(#[from] plasm_core::value_arithmetic::ArithmeticContractError),
    #[error(transparent)]
    CatalogOwnership(#[from] crate::catalog_ownership::CatalogOwnershipError),
    #[error(transparent)]
    SyntheticSchema(#[from] plasm_core::plasm_monad::SyntheticResultSchemaError),
    #[error(transparent)]
    InputBudget(#[from] ComputeInputBudgetError),
    #[error("Python input value could not be observed: {0}")]
    ObservedValue(#[source] plasm_core::value_contract::ValueContractError),
    #[error("Python input value violates its declared contract: {0}")]
    InvalidValue(#[source] plasm_core::value_contract::ValueContractError),
    #[error("correlated map-body schema failed")]
    MapBody(#[source] Box<crate::map_body_schema::MapBodySchemaError>),
}

impl From<crate::map_body_schema::MapBodySchemaError> for PythonSchemaError {
    fn from(error: crate::map_body_schema::MapBodySchemaError) -> Self {
        Self::MapBody(Box::new(error))
    }
}

#[cfg(test)]
fn test_membership(
    complete: bool,
) -> plasm_core::collection_codec::RecordedCollection<plasm_core::Ref> {
    use plasm_core::collection_codec::{
        CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
    };
    RecordingCodec::new()
        .record(
            CollectionIdentity::for_untyped_observation(&"compute_fixture").unwrap(),
            vec![plasm_core::Ref::new("Row", "1")],
            if complete {
                Observation::ExactOutput { decoded: 1 }
            } else {
                Observation::UnprovenPage
            },
        )
        .unwrap()
}

#[cfg(test)]
pub(super) fn materialize_rows(
    fields: &BTreeMap<String, plasm_core::value_contract::ValueContract>,
    optional_fields: &std::collections::BTreeSet<String>,
    rows: &[ValueRow],
    cgs: &CGS,
    entry: &str,
) -> Result<crate::python_pool::TypedRecords, PythonSchemaError> {
    materialize_rows_in(fields, optional_fields, rows, cgs, entry, &BTreeMap::new())
}

pub(super) fn materialize_rows_in(
    fields: &BTreeMap<String, plasm_core::value_contract::ValueContract>,
    optional_fields: &std::collections::BTreeSet<String>,
    rows: &[ValueRow],
    cgs: &CGS,
    entry: &str,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
) -> Result<crate::python_pool::TypedRecords, PythonSchemaError> {
    validate_input_budget(rows)?;

    rows.iter()
        .map(|row| {
            fields
                .iter()
                .filter(|(name, _)| row.get(name).is_some() || !optional_fields.contains(*name))
                .map(|(name, kind)| {
                    let value =
                        row.get(name)
                            .ok_or_else(|| PythonSchemaError::InputFieldMissing {
                                field: name.clone(),
                            })?;
                    let lookup = |entry: &str| catalogs.get(entry).map(AsRef::as_ref);
                    let value = kind
                        .observed_value_in(value, cgs, entry, &lookup)
                        .map_err(PythonSchemaError::ObservedValue)?;
                    kind.validate_in(&value, cgs, entry, name, &lookup)
                        .map_err(PythonSchemaError::InvalidValue)?;
                    Ok((name.clone(), value))
                })
                .collect()
        })
        .collect()
}

pub(crate) fn source_field_kind(
    es: &crate::execute_session::ExecuteSession,
    nodes: &[crate::plasm_plan::ValidatedPlanNode],
    id: &str,
    field: &str,
    depth: usize,
) -> Result<plasm_core::value_contract::ValueContract, PythonSchemaError> {
    use crate::plasm_plan::ValidatedPlanNode as Node;
    use plasm_core::plasm_monad::ComputeOp;
    if depth > 256 {
        return Err(PythonSchemaError::DependencyDepthExceeded);
    }
    if let Some((head, tail)) = field.split_once('.') {
        let mut value = source_field_kind(es, nodes, id, head, depth + 1)?;
        for name in tail.split('.') {
            value = value.field(name)?;
        }
        return Ok(value);
    }
    let node = nodes
        .iter()
        .find(|n| n.id().as_str() == id)
        .ok_or(PythonSchemaError::SourceNodeMissing)?;
    let owner = match node {
        Node::Capture(c) => {
            if let Some(value) = &c.value_contract {
                return value.field(field).map_err(Into::into);
            }
            if let Some(schema) = &c.schema {
                return schema
                    .fields
                    .iter()
                    .find(|f| f.name.as_str() == field)
                    .and_then(|f| f.value_type.clone())
                    .ok_or(PythonSchemaError::PortFieldMissing);
            }
            &c.entity
        }
        Node::Surface(s) => {
            if !s.projection.is_empty() && !s.projection.iter().any(|f| f == field) {
                return Err(PythonSchemaError::FieldExcludedByProjection);
            }
            s.qualified_entity
                .as_ref()
                .ok_or(PythonSchemaError::SchemaOwnerMissing)?
        }
        Node::RelationTraversal(s) => {
            if s.relation
                .ir
                .projection
                .as_ref()
                .is_some_and(|p| !p.iter().any(|f| f == field))
            {
                return Err(PythonSchemaError::FieldExcludedByProjection);
            }
            &s.relation.target
        }
        Node::ForEach(source) => {
            for projection in [&source.projection, &source.effect_template.projection] {
                if !projection.is_empty() && !projection.iter().any(|f| f == field) {
                    return Err(PythonSchemaError::FieldExcludedByProjection);
                }
            }
            &source.effect_template.qualified_entity
        }
        Node::IterateUntil(iteration) => {
            return source_field_kind(es, nodes, iteration.source.as_str(), field, depth + 1)
        }
        Node::MapBody(map) => {
            return crate::map_body_schema::output_schema(es, &map.body)
                .map_err(|error| PythonSchemaError::MapBody(Box::new(error)))?
                .fields
                .into_iter()
                .find(|f| f.name.as_str() == field)
                .and_then(|f| f.value_type)
                .ok_or(PythonSchemaError::SourceFieldMissing);
        }
        Node::Data(_) | Node::Derive(_) => {
            let t =
                crate::map_body_schema::row_contract_at(es, nodes, node.id().as_str(), depth + 1)?;
            return plasm_core::plasm_monad::SyntheticResultSchema::for_value(t)?
                .fields
                .into_iter()
                .find(|f| f.name.as_str() == field)
                .and_then(|f| f.value_type)
                .ok_or(PythonSchemaError::SourceFieldMissing);
        }
        Node::Compute(c) => {
            let next =
                |field: &str| source_field_kind(es, nodes, &c.compute.source, field, depth + 1);
            return match &c.compute.op {
                ComputeOp::Project { fields } => next(
                    &fields
                        .iter()
                        .find(|(k, _)| k.as_str() == field)
                        .ok_or(PythonSchemaError::SourceFieldMissing)?
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
                        plasm_core::value_contract::ValueContract::aggregate(
                            a.function,
                            input.as_ref(),
                        )
                        .map_err(Into::into)
                    } else if matches!(&c.compute.op, ComputeOp::GroupBy { keys, .. } if keys.iter().any(|k| k.dotted() == field))
                    {
                        next(field)
                    } else {
                        Err(PythonSchemaError::SourceFieldMissing)
                    }
                }
                ComputeOp::Filter { .. }
                | ComputeOp::Sort { .. }
                | ComputeOp::Limit { .. }
                | ComputeOp::DedupeBy { .. } => next(field),
                ComputeOp::MergeBranches { other } => {
                    Ok(plasm_core::value_contract::ValueContract::join(
                        next(field)?,
                        source_field_kind(es, nodes, other.as_str(), field, depth + 1)?,
                    ))
                }
                ComputeOp::Union { other } => {
                    let left = next(field)?;
                    let right = source_field_kind(es, nodes, other.as_str(), field, depth + 1)?;
                    if left.shape != right.shape {
                        return Err(PythonSchemaError::UnionFieldMismatch);
                    }
                    Ok(plasm_core::value_contract::ValueContract::join(left, right))
                }
                ComputeOp::Python { .. } => c
                    .compute
                    .schema
                    .fields
                    .iter()
                    .find(|f| f.name.as_str() == field)
                    .and_then(|f| f.value_type.clone())
                    .ok_or(PythonSchemaError::SourceFieldMissing),
                ComputeOp::Render { .. }
                    if c.compute
                        .schema
                        .fields
                        .iter()
                        .any(|f| f.name.as_str() == field) =>
                {
                    Ok(plasm_core::value_contract::ValueContract::scalar(
                        FieldType::String,
                    ))
                }
                _ => Err(PythonSchemaError::UnsupportedSource),
            };
        }
    };
    let cgs =
        crate::catalog_ownership::resolve_cgs_for_entry_entity(es, &owner.entry_id, &owner.entity)?;
    let entity = cgs
        .get_entity(&owner.entity)
        .ok_or(PythonSchemaError::SourceOwnerMissing)?;
    if let Some(relation) = entity
        .relations
        .get(field)
        .filter(|_| !entity.fields.contains_key(field))
    {
        return Ok(super::observed_relation_type(relation, &owner.entry_id));
    }
    let f = entity
        .fields
        .get(field)
        .ok_or(PythonSchemaError::CatalogFieldMissing)?;
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
    id: &'a str,
    expected: &EntityBinding,
) -> Result<(), PythonSchemaError> {
    validate_source_owner_at(nodes, id, expected, 0)
}

fn validate_source_owner_at<'a>(
    nodes: &'a [crate::plasm_plan::ValidatedPlanNode],
    mut id: &'a str,
    expected: &EntityBinding,
    depth: usize,
) -> Result<(), PythonSchemaError> {
    use crate::plasm_plan::ValidatedPlanNode as Node;
    for depth in depth..256 {
        let node = nodes
            .iter()
            .find(|n| n.id().as_str() == id)
            .ok_or(PythonSchemaError::SourceNodeMissing)?;
        let owner = match node {
            Node::MapBody(map) => {
                if let plasm_core::plasm_monad::ScopedOutput::Rows { entity, .. } = &map.body.output
                {
                    return if entity.entry_id == expected.entry_id.as_str()
                        && entity.entity == expected.entity.as_str()
                    {
                        Ok(())
                    } else {
                        Err(PythonSchemaError::CatalogOwnershipMismatch)
                    };
                }
                id = map.body.parent.source.as_str();
                continue;
            }
            Node::Derive(d) => {
                // Value records carry dependencies, not receiver ownership. A
                // catalog dependency supplies the checking context; field
                // contracts are independently re-derived below.
                if d.inputs.iter().any(|input| {
                    validate_source_owner_at(nodes, input.node.as_str(), expected, depth + 1)
                        .is_ok()
                }) {
                    return Ok(());
                }
                id = d.source.as_str();
                continue;
            }
            Node::Compute(c) => {
                id = &c.compute.source;
                continue;
            }
            Node::Capture(c) => &c.entity,
            Node::IterateUntil(iteration) => {
                id = iteration.source.as_str();
                continue;
            }
            Node::Surface(s) => s
                .qualified_entity
                .as_ref()
                .ok_or(PythonSchemaError::SourceOwnerMissing)?,
            Node::RelationTraversal(s) => &s.relation.target,
            Node::ForEach(s) => &s.effect_template.qualified_entity,
            _ => return Err(PythonSchemaError::UnsupportedSource),
        };
        if owner.entry_id != expected.entry_id.as_str() || owner.entity != expected.entity.as_str()
        {
            return Err(PythonSchemaError::InputSchemaOwnershipMismatch);
        }
        return Ok(());
    }
    Err(PythonSchemaError::DependencyDepthExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_row as json;

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
        let nominal = ValueContract::from_cgs(&cgs, "matrix", symbols.as_ref(), &token).unwrap();
        let value_type = nominal.fields["lines"].value_type.clone();
        let schema = SyntheticResultSchema {
            optional_fields: Default::default(),
            entity: None,
            fields: vec![plasm_core::plasm_monad::SyntheticFieldSchema {
                name: plasm_core::plasm_monad::OutputName::new("lines").unwrap(),
                value_kind: value_type.summary(),
                value_type: Some(value_type),
                source: None,
            }],
        };
        let checked = PreparedCompute::prepare_input(
            &source,
            &cgs,
            "matrix",
            symbols.as_ref(),
            Some((&schema, &token)),
            ComputeInputMode::Singleton,
        )
        .unwrap();
        let relation = &checked.contract.as_ref().unwrap().fields["lines"].value_type;
        assert!(
            matches!(&relation.shape, ValueShape::Array { element } if matches!(&element.shape, ValueShape::Scalar { field_type: FieldType::EntityRef {entry_id, target} } if entry_id.as_str() == "matrix" && target.as_str() == "LangLine"))
        );
        let declaration = checked
            .contract
            .as_ref()
            .unwrap()
            .declaration(symbols.as_ref())
            .unwrap();
        assert!(declaration.contains("lines: list[EntityRef]"));
        let pool = crate::python_pool::PythonPool::default();
        for (rows, expected) in [
            (
                json!({"lines":[{"_ref":{"kind":"simple","entity":"LangLine","id":"l1"},"optional":null,"children":[]}]}),
                "relation_count=1",
            ),
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
                        &checked.contract.as_ref().unwrap().owner,
                        &test_membership(true),
                        &[rows]
                    )
                    .await
                    .unwrap(),
                Value::String(expected.into())
            );
        }
        for rows in [json!({}), json!({"lines": null}), json!({"lines": [null]})] {
            assert!(checked
                .run(
                    &pool,
                    &checked.contract.as_ref().unwrap().owner,
                    &test_membership(true),
                    &[rows]
                )
                .await
                .is_err());
        }
        assert!(checked
            .run(
                &pool,
                &checked.contract.as_ref().unwrap().owner,
                &test_membership(false),
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
            let rejected = match PreparedCompute::prepare_input(
                &source,
                &cgs,
                "matrix",
                symbols.as_ref(),
                Some((&schema, &token)),
                ComputeInputMode::Singleton,
            ) {
                Ok(prepared) => prepared.admit().is_err(),
                Err(_) => true,
            };
            assert!(rejected);
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
            optional_fields: Default::default(),
            entity: None,
            fields: vec![plasm_core::SyntheticFieldSchema {
                name: plasm_core::OutputName::new("records").unwrap(),
                value_kind: value_type.summary(),
                value_type: Some(value_type),
                source: None,
            }],
        };
        let checked = PreparedCompute::prepare_input("@compute\ndef render(row: Row) -> str:\n    return '|'.join(str(item.n) for item in row.records)\n", &cgs, "types", symbols.as_ref(), Some((&schema, &token)), ComputeInputMode::Singleton).unwrap();
        let indexed = PreparedCompute::prepare_input(
            "@compute\ndef render(row: Row) -> str:\n    return str(row.records[0]['n'])\n",
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, &token)),
            ComputeInputMode::Singleton,
        );
        let pool = crate::python_pool::PythonPool::default();
        let indexed = indexed.unwrap();
        indexed.admit().unwrap();
        assert_eq!(
            indexed
                .run(
                    &pool,
                    &indexed.contract.as_ref().unwrap().owner,
                    &test_membership(true),
                    &[json!({"records": [{"n": 9007199254740993_i64}]})]
                )
                .await
                .unwrap(),
            Value::String("9007199254740993".into())
        );
        let json_schema = SyntheticResultSchema {
            optional_fields: Default::default(),
            entity: None,
            fields: vec![plasm_core::SyntheticFieldSchema {
                name: plasm_core::OutputName::new("document").unwrap(),
                value_kind: T::scalar(FieldType::Json).summary(),
                value_type: Some(T::scalar(FieldType::Json)),
                source: None,
            }],
        };
        let json_access = PreparedCompute::prepare_input(
            &format!("@compute\ndef render(row: Value[{token}]) -> str:\n    return str(row.document['n']) if isinstance(row.document, dict) else ''\n"),
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&json_schema, &token)),

            ComputeInputMode::Singleton).unwrap();
        assert_eq!(
            json_access
                .run(
                    &pool,
                    &json_access.contract.as_ref().unwrap().owner,
                    &test_membership(true),
                    &[json!({"document": {"n": 7}})]
                )
                .await
                .unwrap(),
            Value::String("7".into())
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
                        &checked.contract.as_ref().unwrap().owner,
                        &test_membership(true),
                        &[rows]
                    )
                    .await
                    .unwrap(),
                Value::String(expected.into())
            );
        }
        let invalid_row = json!({"records": [{"n": true}]});
        let fields = BTreeMap::from([(
            "records".into(),
            schema.fields[0].value_type.clone().unwrap(),
        )]);
        let schema_error = materialize_rows(
            &fields,
            &schema.optional_fields,
            std::slice::from_ref(&invalid_row),
            &cgs,
            "types",
        )
        .unwrap_err();
        assert!(
            matches!(
                &schema_error,
                PythonSchemaError::InvalidValue(
                    plasm_core::value_contract::ValueContractError::MaterializedValueMismatch { path }
                ) if path == "records[0].n"
            ),
            "{schema_error:?}"
        );
        assert!(schema_error.to_string().contains("records[0].n"));
        assert!(std::error::Error::source(&schema_error)
            .unwrap()
            .is::<plasm_core::value_contract::ValueContractError>());
        let bad = checked
            .run(
                &pool,
                &checked.contract.as_ref().unwrap().owner,
                &test_membership(true),
                &[invalid_row],
            )
            .await
            .unwrap_err();
        assert!(bad.diagnostic().contains("records[0].n"), "{bad}");
    }

    #[tokio::test]
    async fn observed_record_presence_survives_monty_codec_without_null_filling() {
        use plasm_core::symbol_tuning::SymbolRender;
        use plasm_core::value_contract::ValueContract as T;
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let symbols =
            plasm_core::TeachingExposureSession::new(&cgs, "types", &["Sample"]).to_symbol_map();
        let token = symbols.entity_sym_for("types", "Sample");
        let mut optional = T::scalar(FieldType::String);
        optional.nullable = true;
        let value_type = T::record(
            BTreeMap::from([
                ("n".into(), T::scalar(FieldType::Integer)),
                ("maybe".into(), optional),
            ]),
            std::collections::BTreeSet::from(["maybe".into()]),
        );
        let schema = SyntheticResultSchema {
            optional_fields: Default::default(),
            entity: None,
            fields: vec![plasm_core::SyntheticFieldSchema {
                name: plasm_core::OutputName::new("record").unwrap(),
                value_kind: value_type.summary(),
                value_type: Some(value_type),
                source: None,
            }],
        };
        let source = "@compute\ndef render(row: Row) -> str:\n    try:\n        return str(row.record.maybe)\n    except AttributeError:\n        return 'absent'\n";
        let checked = PreparedCompute::prepare_input(
            source,
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, &token)),
            ComputeInputMode::Singleton,
        )
        .unwrap();
        let pool = crate::python_pool::PythonPool::default();
        for (record, expected) in [
            (json!({"n":1}), "absent"),
            (json!({"n":1,"maybe":null}), "None"),
            (json!({"n":1,"maybe":"value"}), "value"),
        ] {
            assert_eq!(
                checked
                    .run(
                        &pool,
                        &checked.contract.as_ref().unwrap().owner,
                        &test_membership(true),
                        &[json!({"record":record})]
                    )
                    .await
                    .unwrap(),
                Value::String(expected.into())
            );
        }
        for record in [json!({}), json!({"n":true}), json!({"n":1,"maybe":42})] {
            assert!(checked
                .run(
                    &pool,
                    &checked.contract.as_ref().unwrap().owner,
                    &test_membership(true),
                    &[json!({"record":record})]
                )
                .await
                .is_err());
        }
        let unguarded = PreparedCompute::prepare_input(
            "@compute\ndef render(row: Row) -> str:\n    return str(row.record.maybe)\n",
            &cgs,
            "types",
            symbols.as_ref(),
            Some((&schema, &token)),
            ComputeInputMode::Singleton,
        )
        .unwrap();
        let error = unguarded
            .run(
                &pool,
                &unguarded.contract.as_ref().unwrap().owner,
                &test_membership(true),
                &[json!({"record":{"n":1}})],
            )
            .await
            .unwrap_err();
        assert!(error.diagnostic().contains("AttributeError"), "{error}");
        pool.close().await;
    }

    #[test]
    fn scalar_rows_preserve_types_order_duplicates_and_empty() {
        let fields = BTreeMap::from([
            (
                "n".into(),
                plasm_core::value_contract::ValueContract::scalar(FieldType::Integer),
            ),
            (
                "s".into(),
                plasm_core::value_contract::ValueContract::scalar(FieldType::String),
            ),
            (
                "b".into(),
                plasm_core::value_contract::ValueContract::scalar(FieldType::Boolean),
            ),
            (
                "f".into(),
                plasm_core::value_contract::ValueContract::scalar(FieldType::Number),
            ),
        ]);
        let row = json!({"n": 3, "s": "three", "b": true, "f": 1.5, "unconsumed": {"secret": 1}});
        let rows = materialize_rows(
            &fields,
            &Default::default(),
            &[row.clone(), row],
            &CGS::default(),
            "test",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], rows[1]);
        assert_eq!(rows[0]["n"], crate::fixture_value!(3));
        assert_eq!(rows[0]["f"], crate::fixture_value!(1.5));
        assert_eq!(rows[0]["b"], crate::fixture_value!(true));
        assert!(!rows[0].contains_key("unconsumed"));
        assert!(
            materialize_rows(&fields, &Default::default(), &[], &CGS::default(), "test")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn row_boundary_rejects_wrong_types_missing_fields_and_excess() {
        let fields = BTreeMap::from([(
            "n".into(),
            plasm_core::value_contract::ValueContract::scalar(FieldType::Integer),
        )]);
        for bad in [
            json!({}),
            json!({"n": "3"}),
            json!({"n": true}),
            json!({"n": 3.5}),
            json!({"n": null}),
            json!({"n": u64::MAX}),
        ] {
            assert!(materialize_rows(
                &fields,
                &Default::default(),
                &[bad],
                &CGS::default(),
                "test"
            )
            .is_err());
        }
        assert!(materialize_rows(
            &fields,
            &Default::default(),
            &vec![json!({"n": 1}); MAX_INPUT_ROWS + 1],
            &CGS::default(),
            "test"
        )
        .is_err());
        assert!(matches!(
            validate_input_budget(&[json!({"s": "x".repeat(1_048_576)})]),
            Err(ComputeInputBudgetError::Value(
                plasm_core::ValueBudgetError::BytesExceeded
            ))
        ));
        assert!(matches!(
            validate_input_budget(&vec![json!({}); MAX_INPUT_ROWS + 1]),
            Err(ComputeInputBudgetError::RowCountExceeded)
        ));
    }
}
