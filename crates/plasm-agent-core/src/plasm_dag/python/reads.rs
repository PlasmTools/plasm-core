use super::catalog_operations::{CatalogReadKind, ReadSelection};
use super::*;
use ruff_python_ast::ExprCall;
impl Lower<'_> {
    pub(super) fn read(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        selection: ReadSelection<'_>,
        owner: plasm_core::symbol_tuning::EntityBinding,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let kind = selection.kind();
        let cgs = match &selection {
            ReadSelection::Taught(resolved) => resolved.cgs,
            ReadSelection::Primary(_) => crate::catalog_ownership::resolve_cgs_for_entry_entity(
                self.es,
                owner.entry_id.as_str(),
                owner.entity.as_str(),
            )?,
        };
        let specific = match &selection {
            ReadSelection::Taught(resolved) => Some(resolved.capability.clone()),
            ReadSelection::Primary(_) => None,
        };
        let mut expr = match kind {
            CatalogReadKind::Query | CatalogReadKind::Search => {
                if !call.arguments.args.is_empty() {
                    return Err(at(
                        site,
                        PythonSourceError::ReadPositionalArguments {
                            actual: call.arguments.args.len(),
                        },
                    ));
                }
                let mut q = plasm_core::QueryExpr::all(owner.entity.as_str());
                if let Some(capability) = &specific {
                    q.capability_name = Some(capability.clone());
                } else if kind == CatalogReadKind::Search {
                    q.capability_name = Some(
                        cgs.primary_search_capability(owner.entity.as_str())
                            .ok_or(crate::program_rejection::PythonLoweringInvariantError::PrimarySearchCapabilityMissing)?
                            .name
                            .clone(),
                    );
                }
                let mut predicates = Vec::new();
                let mut keys = BTreeSet::new();
                for kw in &call.arguments.keywords {
                    let key = kw
                        .arg
                        .as_ref()
                        .ok_or_else(|| at(site, PythonSourceError::SelectionUnpacking))?;
                    if !keys.insert(key.as_str()) {
                        return Err(at(
                            site,
                            PythonSourceError::DuplicateSelectionArgument {
                                argument: key.to_string(),
                            },
                        ));
                    }
                    let field = self
                        .state
                        .sym_map_for(self.es)
                        .resolve_query_filter_field(
                            plasm_core::symbol_tuning::CatalogScope::qualified(
                                owner.entry_id.as_str(),
                            ),
                            owner.entity.as_str(),
                            cgs.get_entity(owner.entity.as_str())
                                .ok_or(crate::program_rejection::PythonLoweringInvariantError::CatalogEntityMissing)?,
                            cgs,
                            key.as_str(),
                        )
                        .map_err(PythonLoweringError::from)?;
                    predicates.push(plasm_core::Predicate::eq(
                        field,
                        self.write_value(&kw.value)?,
                    ));
                }
                if !predicates.is_empty() {
                    q.predicate = Some(if predicates.len() == 1 {
                        predicates.remove(0)
                    } else {
                        plasm_core::Predicate::And { args: predicates }
                    });
                }
                q.catalog_entry_id = plasm_core::CatalogEntryStamp::some(owner.entry_id.clone());
                plasm_core::rowset::normalize_query_expr_to_rowset(
                    &q,
                    cgs,
                    owner.entry_id.as_str(),
                )?;
                plasm_core::Expr::Query(q)
            }
            CatalogReadKind::Get => {
                let mut g = plasm_core::GetExpr::pathless_nullary(owner.entity.as_str());
                g.capability_name = specific.clone();
                let cap = g.capability(cgs).map_err(PythonLoweringError::from)?;
                let entity = cgs.get_entity(owner.entity.as_str()).ok_or(
                    crate::program_rejection::PythonLoweringInvariantError::GetEntityMissing,
                )?;
                let (identity_keywords, input_keywords): (Vec<_>, Vec<_>) =
                    call.arguments.keywords.iter().partition(|kw| {
                        cap.get_requires_identity_anchor(cgs)
                            && kw.arg.as_ref().is_some_and(|key| {
                                if entity.key_vars.len() > 1 {
                                    entity
                                        .key_vars
                                        .iter()
                                        .any(|slot| slot.as_str() == key.as_str())
                                } else {
                                    key.as_str() == "identity"
                                }
                            })
                    });
                let input =
                    self.invocation_arguments(site, input_keywords.into_iter(), &owner, cap)?;
                let input = inputs::normalize(cap, input, cgs).map_err(|error| match error {
                    PythonLoweringError::Input(error) => input_at(site, (*error).clone()),
                    other => at(site, other),
                })?;
                let reference = if !cap.get_requires_identity_anchor(cgs) {
                    if !call.arguments.args.is_empty() || !identity_keywords.is_empty() {
                        return Err(at(
                            site,
                            PythonSourceError::NullaryGetArguments {
                                positional: call.arguments.args.len(),
                                keywords: identity_keywords.len(),
                            },
                        ));
                    }
                    plasm_core::GetExpr::pathless_nullary(owner.entity.as_str()).reference
                } else if entity.key_vars.len() > 1 {
                    if !call.arguments.args.is_empty() {
                        return Err(at(
                            site,
                            PythonSourceError::CompoundGetArgumentShape {
                                entity: owner.entity.to_string(),
                                expected: entity.key_vars.iter().map(ToString::to_string).collect(),
                                positional: call.arguments.args.len(),
                            },
                        ));
                    }
                    let mut slots = BTreeMap::new();
                    for kw in &identity_keywords {
                        let key = kw
                            .arg
                            .as_ref()
                            .ok_or_else(|| at(site, PythonSourceError::IdentityUnpacking))?;
                        if slots
                            .insert(key.to_string(), self.identity_slot(&kw.value)?)
                            .is_some()
                        {
                            return Err(at(
                                site,
                                PythonSourceError::DuplicateCompoundIdentityKey {
                                    entity: owner.entity.to_string(),
                                    key: key.to_string(),
                                },
                            ));
                        }
                    }
                    if slots.len() != entity.key_vars.len() {
                        return Err(at(
                            site,
                            PythonSourceError::MissingCompoundIdentityKeys {
                                entity: owner.entity.to_string(),
                                missing: entity
                                    .key_vars
                                    .iter()
                                    .filter(|key| !slots.contains_key(key.as_str()))
                                    .map(ToString::to_string)
                                    .collect(),
                            },
                        ));
                    }
                    plasm_core::Ref::compound_slots(owner.entity.as_str(), slots)
                } else {
                    if call.arguments.args.len() > 1 {
                        return Err(at(
                            site,
                            PythonSourceError::GetPositionalArgumentCount {
                                actual: call.arguments.args.len(),
                            },
                        ));
                    }
                    let mut identity = call.arguments.args.first();
                    for kw in &identity_keywords {
                        if identity.replace(&kw.value).is_some() {
                            return Err(at(site, PythonSourceError::DuplicateGetIdentity));
                        }
                    }
                    let identity =
                        identity.ok_or_else(|| at(site, PythonSourceError::MissingGetIdentity))?;
                    match self.identity_slot(identity)? {
                        plasm_core::IdentitySlot::Binding(input) => {
                            plasm_core::Ref::simple_binding(owner.entity.as_str(), input)
                        }
                        plasm_core::IdentitySlot::Lit(id) => {
                            plasm_core::Ref::new(owner.entity.as_str(), id.as_str())
                        }
                    }
                };
                let mut g = plasm_core::GetExpr::from_ref(reference);
                g.capability_name = Some(cap.name.clone());
                g.input = Some(input.into());
                g.catalog_entry_id = plasm_core::CatalogEntryStamp::some(owner.entry_id.clone());
                plasm_core::Expr::Get(g)
            }
        };
        plasm_core::apply_required_selection_defaults_in_expr(&mut expr, cgs, "")
            .map_err(PythonLoweringError::from)?;
        self.emit_catalog(id, expr)
    }
    fn identity_slot(
        &mut self,
        e: &PyExpr,
    ) -> Result<plasm_core::IdentitySlot, PythonLoweringError> {
        let value = if let PyExpr::UnaryOp(unary) = e {
            if unary.op != ruff_python_ast::UnaryOp::USub {
                return Err(at(e, PythonSourceError::UnsupportedIdentityExpression));
            }
            // Parse the signed spelling together so i64::MIN remains exact.
            let PyExpr::NumberLiteral(number) = &*unary.operand else {
                return Err(at(e, PythonSourceError::IdentityNegationRequiresInteger));
            };
            let ruff_python_ast::Number::Int(number) = &number.value else {
                return Err(at(e, PythonSourceError::FloatIdentity));
            };
            plasm_core::Value::Integer(format!("-{number}").parse().map_err(|source| {
                at(
                    e,
                    PythonSourceError::IntegerIdentityOutOfRange {
                        literal: format!("-{number}"),
                        source,
                    },
                )
            })?)
        } else {
            self.write_value(e)?
        };
        match value {
            plasm_core::Value::PlasmInputRef(input) => Ok(plasm_core::IdentitySlot::binding(input)),
            plasm_core::Value::String(id) => Ok(plasm_core::IdentitySlot::lit(id)),
            plasm_core::Value::Bool(id) => Ok(plasm_core::IdentitySlot::lit(id.to_string())),
            plasm_core::Value::Integer(id) => Ok(plasm_core::IdentitySlot::lit(id.to_string())),
            _ => Err(at(e, PythonSourceError::ExpectedIdentityValue)),
        }
    }
    pub(super) fn filter(
        &mut self,
        site: &PyExpr,
        source: &str,
        id: &str,
        expression: &PyExpr,
    ) -> Result<String, PythonLoweringError> {
        let callback = self.callback(expression)?;
        let body = self.scoped_callback_body(
            site,
            source,
            &callback,
            std::num::NonZeroU32::new(65_536).expect("positive bound"),
            super::body::ScopeMode::Filter,
        )?;
        let schema = crate::map_body_schema::output_schema(self.es, &body)?;
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: false,
            page_size: None,
            source: super::super::types::DagNodeSource::MapBody {
                body: Box::new(body),
                schema,
            },
        })
    }
}
