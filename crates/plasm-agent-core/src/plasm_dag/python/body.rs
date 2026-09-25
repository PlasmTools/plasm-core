use super::*;
use std::num::NonZeroU32;

pub(super) fn is_map(e: &PyExpr) -> bool {
    matches!(e,PyExpr::Call(c) if matches!(&*c.func,PyExpr::Attribute(a) if a.attr.as_str()=="map"))
}
impl Lower<'_> {
    pub(super) fn map(
        &mut self,
        e: &PyExpr,
        methods: &BTreeMap<String, String>,
    ) -> Result<CorrelatedBody, String> {
        let PyExpr::Call(call) = e else {
            return Err(at(e, "expected map"));
        };
        let PyExpr::Attribute(attr) = &*call.func else {
            return Err(at(e, "expected map receiver"));
        };
        if call.arguments.args.len() != 1
            || call.arguments.keywords.len() != 1
            || call.arguments.keywords[0].arg.as_ref().map(|s| s.as_str()) != Some("max_parents")
        {
            return Err(at(e, "map requires one lambda and explicit max_parents"));
        }
        let bound = u32::try_from(integer(&call.arguments.keywords[0].value)?)
            .ok()
            .and_then(NonZeroU32::new)
            .filter(|n| n.get() <= 256)
            .ok_or("max_parents must be between 1 and 256")?;
        let source = self.expr(&attr.value, None)?;
        let node = self.state.get(&source).ok_or("map source missing")?;
        let super::super::types::DagNodeSource::Surface {
            qualified_entity: owner,
            effect_class: EffectClass::Read,
            ..
        } = &node.source
        else {
            return Err(at(e, "this map slice requires a direct catalog read"));
        };
        let owner = owner.clone();
        let PyExpr::Lambda(lambda) = &call.arguments.args[0] else {
            return Err(at(e, "map requires a lambda"));
        };
        let p = lambda
            .parameters
            .as_ref()
            .ok_or("map requires one row parameter")?;
        if p.args.len() != 1
            || !p.posonlyargs.is_empty()
            || !p.kwonlyargs.is_empty()
            || p.vararg.is_some()
            || p.kwarg.is_some()
            || p.args[0].default.is_some()
        {
            return Err(at(e, "map requires one row parameter"));
        }
        let row = p.args[0].parameter.name.as_str();
        if row == "self" || row.starts_with("__") {
            return Err(at(e, "reserved map parameter"));
        }
        let PyExpr::Dict(dict) = &*lambda.body else {
            return Err(at(e, "map must return a record"));
        };
        if dict.items.is_empty() {
            return Err(at(e, "map output record must be nonempty"));
        }
        let mut body = empty_comp(None);
        let mut spans = BTreeMap::new();
        let mut fields = BTreeMap::new();
        let mut inputs = Vec::new();
        let mut deps = BTreeSet::from([StepId::new("parent")?]);
        let symbols = self.state.sym_map_for(self.es);
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            self.es,
            &owner.entry_id,
            &owner.entity,
        )?;
        for item in &dict.items {
            let key = string(
                item.key
                    .as_ref()
                    .ok_or("dictionary unpacking is not admitted")?,
            )?;
            let value = match &item.value {
                PyExpr::Attribute(field) if name(&field.value) == Some(row) => {
                    let wire = crate::plasm_plan_run::resolve_wire_field_token(
                        self.es,
                        None,
                        Some(&owner),
                        field.attr.as_str(),
                    )?;
                    PlasmDataValue::BindingSymbol {
                        binding: row.into(),
                        path: vec![wire],
                    }
                }
                PyExpr::Call(compute) => {
                    let PyExpr::Attribute(method) = &*compute.func else {
                        return Err(at(e, "expected self.compute_method"));
                    };
                    if name(&method.value) != Some("self")
                        || compute.arguments.args.len() != 1
                        || !compute.arguments.keywords.is_empty()
                    {
                        return Err(at(e, "compute requires self.method(relation)"));
                    }
                    let code = methods
                        .get(method.attr.as_str())
                        .ok_or("unknown compute method")?;
                    let PyExpr::Attribute(hop) = &compute.arguments.args[0] else {
                        return Err(at(e, "compute requires a captured relation"));
                    };
                    if name(&hop.value) != Some(row) {
                        return Err(at(e, "relation receiver must be the captured row"));
                    }
                    let relation = symbols
                        .resolve_session_relation(hop.attr.as_str())
                        .map_err(|e| e.to_string())?;
                    if relation.entry_id.as_str() != owner.entry_id
                        || relation.source_entity.as_str() != owner.entity
                    {
                        return Err(at(e, "relation ownership does not match captured row"));
                    }
                    let declared = cgs
                        .get_entity(&owner.entity)
                        .ok_or("missing parent entity")?
                        .relations
                        .get(relation.relation_wire.as_str())
                        .ok_or("relation missing from catalog")?;
                    if declared.cardinality != plasm_core::schema::Cardinality::Many {
                        return Err(at(e, "compute collection input requires a many relation"));
                    }
                    let target = PlanQualifiedEntityKey {
                        entry_id: owner.entry_id.clone(),
                        entity: relation.target_entity.to_string(),
                    };
                    let target_cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
                        self.es,
                        &target.entry_id,
                        &target.entity,
                    )?;
                    let checked = crate::python_compute::CheckedCompute::compile(
                        code,
                        target_cgs,
                        &target.entry_id,
                        symbols.as_ref(),
                    )?;
                    if checked.contract.owner.entity.as_str() != target.entity {
                        return Err(at(e, "compute annotation does not match relation result"));
                    }
                    let children = format!("children{}", inputs.len());
                    let reduced = format!("compute{}", inputs.len());
                    spans.insert(children.clone(), span(hop));
                    spans.insert(reduced.clone(), span(compute));
                    let mut expr =
                        super::super::relation::relation_continuation_expr_from_source_row_hole(
                            self.es,
                            &owner,
                            relation.relation_wire.as_str(),
                        )?;
                    // The shared relation helper uses a source port; this scope calls it parent.
                    if let plasm_core::Expr::Chain(chain) = &mut expr {
                        if let plasm_core::Expr::Get(get) = &mut *chain.source {
                            let entity = cgs.get_entity(&owner.entity).ok_or("missing parent")?;
                            if !entity.key_vars.is_empty() {
                                return Err(at(
                                    e,
                                    "compound capture identities are not admitted yet",
                                ));
                            }
                            get.reference = plasm_core::Ref::simple_binding(
                                owner.entity.as_str(),
                                plasm_core::PlasmInputRef::node_output(
                                    "parent",
                                    vec![entity.id_field.to_string()],
                                ),
                            );
                        }
                    }
                    body.steps.insert(
                        children.clone(),
                        PlasmStepPayload::FlatMapRelation(FlatMapRelationPayload {
                            relation: plasm_core::plasm_monad::PlanRelationTraversal {
                                source: "parent".into(),
                                relation: relation.relation_wire.to_string(),
                                target: target.clone(),
                                cardinality: RelationCardinality::Many,
                                source_cardinality: RelationSourceCardinality::Single,
                                expr: String::new(),
                                ir: plasm_core::plasm_monad::PlanExprIr {
                                    expr,
                                    projection: None,
                                    display_expr: None,
                                },
                                binding_proofs: vec![],
                                materialize: declared.materialize.clone(),
                                view_embed_proof: None,
                            },
                            effect_class: EffectClass::Read,
                            result_shape: ResultShape::List,
                        }),
                    );
                    body.bind.topo.push(StepId::new(&children)?);
                    body.bind.holes.insert(
                        StepId::new(&children)?,
                        vec![PlasmHoleUse {
                            step: StepId::new("parent")?,
                            alias: "parent".into(),
                        }],
                    );
                    body.bind.deps.insert(
                        StepId::new(&children)?,
                        BTreeSet::from([StepId::new("parent")?]),
                    );
                    body.steps.insert(
                        reduced.clone(),
                        PlasmStepPayload::Map(MapPayload {
                            compute: ComputeTemplate {
                                source: children.clone(),
                                op: ComputeOp::Python {
                                    source: code.clone(),
                                    entry_id: target.entry_id,
                                    entity: target.entity,
                                    catalog_hash: target_cgs.catalog_cgs_hash_hex(),
                                    contract_version: 3,
                                    input_schema: None,
                                    per_row: false,
                                },
                                schema: SyntheticResultSchema {
                                    entity: None,
                                    fields: vec![SyntheticFieldSchema {
                                        value_type: None,
                                        name: OutputName::new("content")?,
                                        value_kind: SyntheticValueKind::String,
                                        source: None,
                                    }],
                                },
                                page_size: None,
                                collection_alias: None,
                            },
                            effect_class: EffectClass::ArtifactRead,
                            result_shape: ResultShape::Single,
                        }),
                    );
                    body.bind.topo.push(StepId::new(&reduced)?);
                    body.bind.deps.insert(
                        StepId::new(&reduced)?,
                        BTreeSet::from([StepId::new(&children)?]),
                    );
                    inputs.push(PlanDataInput {
                        node: reduced.clone(),
                        alias: reduced.clone(),
                        cardinality: InputCardinality::Singleton,
                    });
                    deps.insert(StepId::new(&reduced)?);
                    PlasmDataValue::NodeSymbol {
                        node: reduced.clone(),
                        alias: reduced,
                        path: vec!["content".into()],
                    }
                }
                _ => record_value(&item.value, row, &mut |field| {
                    crate::plasm_plan_run::resolve_wire_field_token(
                        self.es,
                        None,
                        Some(&owner),
                        field,
                    )
                })?,
            };
            if fields.insert(key, value).is_some() {
                return Err(at(e, "duplicate output field"));
            }
        }
        body.steps.insert(
            "output".into(),
            PlasmStepPayload::Derive(DerivePayload {
                derive: DeriveTemplate {
                    kind: DeriveKind::Map,
                    source: Some("parent".into()),
                    item_binding: Some(BindingName::new(row)?),
                    inputs,
                    value: PlasmDataValue::Object { fields },
                },
                effect_class: EffectClass::ArtifactRead,
                result_shape: ResultShape::Single,
            }),
        );
        body.bind.topo.push(StepId::new("output")?);
        body.bind.deps.insert(StepId::new("output")?, deps);
        spans.insert("output".into(), span(&*lambda.body));
        body.metadata
            .insert("python_source_spans".into(), serde_json::json!(spans));
        body.return_ = PlasmReturn::Step {
            step: StepId::new("output")?,
        };
        Ok(CorrelatedBody {
            parent: ParentCapture {
                source: StepId::new(source)?,
                local: StepId::new("parent")?,
                entity: PlanQualifiedEntityKey {
                    entry_id: owner.entry_id,
                    entity: owner.entity,
                },
            },
            max_parents: bound,
            body,
        })
    }
}

fn record_value(
    expr: &PyExpr,
    row: &str,
    resolve: &mut impl FnMut(&str) -> Result<String, String>,
) -> Result<PlasmDataValue, String> {
    Ok(match expr {
        PyExpr::Attribute(field) if name(&field.value) == Some(row) => {
            PlasmDataValue::BindingSymbol {
                binding: row.into(),
                path: vec![resolve(field.attr.as_str())?],
            }
        }
        PyExpr::Dict(dict) => {
            let mut fields = BTreeMap::new();
            for item in &dict.items {
                let key = string(
                    item.key
                        .as_ref()
                        .ok_or("dictionary unpacking is not admitted")?,
                )?;
                if fields
                    .insert(key, record_value(&item.value, row, resolve)?)
                    .is_some()
                {
                    return Err("duplicate record field".into());
                }
            }
            PlasmDataValue::Object { fields }
        }
        PyExpr::List(list) => PlasmDataValue::Array {
            items: list
                .elts
                .iter()
                .map(|e| record_value(e, row, resolve))
                .collect::<Result<_, _>>()?,
        },
        _ => PlasmDataValue::Literal {
            value: plasm_core::operand_binding::ResolvedValue::new(literal(expr)?)?,
        },
    })
}
