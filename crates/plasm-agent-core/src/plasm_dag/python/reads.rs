use super::catalog_operations::{CatalogOperation, CatalogReadKind};
use super::*;
use ruff_python_ast::ExprCall;
impl Lower<'_> {
    pub(super) fn read(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        method: &str,
        owner: plasm_core::symbol_tuning::EntityBinding,
        id: &str,
    ) -> Result<String, String> {
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            self.es,
            owner.entry_id.as_str(),
            owner.entity.as_str(),
        )?;
        let specific = if matches!(method, "query" | "get" | "search") {
            None
        } else {
            let symbols = self.state.sym_map_for(self.es);
            let binding = symbols
                .resolve_session_method(method)
                .map_err(|e| at(site, &e.to_string()))?;
            if binding.entry_id != owner.entry_id || binding.domain != owner.entity {
                return Err(at(site, "read method and entity ownership differ"));
            }
            Some(binding.capability.clone())
        };
        let kind = if let Some(capability) = &specific {
            let cap = cgs
                .get_capability(capability.as_str())
                .ok_or("missing method capability")?;
            let CatalogOperation::Read(kind) = CatalogOperation::from_kind(cap.kind) else {
                return Err(at(site, "method is not a catalog read"));
            };
            kind
        } else {
            CatalogReadKind::primary(method).ok_or("unknown primary read")?
        };
        let mut expr = match kind {
            CatalogReadKind::Query | CatalogReadKind::Search => {
                if !call.arguments.args.is_empty() {
                    return Err(at(site, "query/search require named selection arguments"));
                }
                let mut q = plasm_core::QueryExpr::all(owner.entity.as_str());
                if let Some(capability) = &specific {
                    q.capability_name = Some(capability.clone());
                } else if kind == CatalogReadKind::Search {
                    q.capability_name = Some(
                        cgs.primary_search_capability(owner.entity.as_str())
                            .ok_or("entity has no primary search capability")?
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
                        .ok_or_else(|| at(site, "keyword unpacking is not admitted"))?;
                    if !keys.insert(key.as_str()) {
                        return Err(at(site, "duplicate selection argument"));
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
                                .ok_or("missing entity")?,
                            cgs,
                            key.as_str(),
                        )
                        .map_err(|e| e.to_string())?;
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
                let entity = cgs
                    .get_entity(owner.entity.as_str())
                    .ok_or("missing Get entity")?;
                let reference = if specific
                    .as_ref()
                    .and_then(|name| cgs.get_capability(name.as_str()))
                    .or_else(|| cgs.primary_get_capability(owner.entity.as_str()))
                    .is_some_and(|cap| !cap.get_requires_identity_anchor(cgs))
                {
                    if !call.arguments.args.is_empty() || !call.arguments.keywords.is_empty() {
                        return Err(at(site, "this Get takes no identity arguments"));
                    }
                    plasm_core::GetExpr::pathless_nullary(owner.entity.as_str()).reference
                } else if entity.key_vars.len() > 1 {
                    if !call.arguments.args.is_empty() {
                        return Err(at(
                            site,
                            "compound Get requires exactly its named identity keys",
                        ));
                    }
                    let mut slots = BTreeMap::new();
                    for kw in &call.arguments.keywords {
                        let key = kw.arg.as_ref().ok_or_else(|| {
                            at(site, "identity keyword unpacking is not admitted")
                        })?;
                        if !entity.key_vars.iter().any(|k| k.as_str() == key.as_str()) {
                            return Err(at(site, "unknown compound identity key"));
                        }
                        if slots
                            .insert(key.to_string(), self.identity_slot(&kw.value)?)
                            .is_some()
                        {
                            return Err(at(site, "duplicate compound identity key"));
                        }
                    }
                    if slots.len() != entity.key_vars.len() {
                        return Err(at(site, "compound Get requires every identity key"));
                    }
                    plasm_core::Ref::compound_slots(owner.entity.as_str(), slots)
                } else {
                    if call.arguments.args.len() > 1 {
                        return Err(at(site, "Get takes at most one positional identity"));
                    }
                    let mut identity = call.arguments.args.first();
                    for kw in &call.arguments.keywords {
                        let key = kw.arg.as_ref().ok_or_else(|| {
                            at(site, "identity keyword unpacking is not admitted")
                        })?;
                        if key.as_str() != "identity" {
                            return Err(at(site, "unexpected Get argument; expected identity"));
                        }
                        if identity.replace(&kw.value).is_some() {
                            return Err(at(site, "Get received multiple values for identity"));
                        }
                    }
                    let identity = identity
                        .ok_or_else(|| at(site, "Get requires identity (positional or keyword)"))?;
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
                g.capability_name = specific.clone();
                g.catalog_entry_id = plasm_core::CatalogEntryStamp::some(owner.entry_id.clone());
                plasm_core::Expr::Get(g)
            }
        };
        plasm_core::apply_required_selection_defaults_in_expr(&mut expr, cgs, "")
            .map_err(|e| e.to_string())?;
        self.emit_catalog(id, expr)
    }
    fn identity_slot(&mut self, e: &PyExpr) -> Result<plasm_core::IdentitySlot, String> {
        let value = if let PyExpr::UnaryOp(unary) = e {
            if unary.op != ruff_python_ast::UnaryOp::USub {
                return Err(at(e, "unsupported identity expression"));
            }
            // Parse the signed spelling together so i64::MIN remains exact.
            let PyExpr::NumberLiteral(number) = &*unary.operand else {
                return Err(at(e, "identity negation requires an integer literal"));
            };
            let ruff_python_ast::Number::Int(number) = &number.value else {
                return Err(at(e, "IEEE float is not an identity literal"));
            };
            plasm_core::Value::Integer(
                format!("-{number}")
                    .parse()
                    .map_err(|_| at(e, "integer identity out of range"))?,
            )
        } else {
            self.write_value(e)?
        };
        match value {
            plasm_core::Value::PlasmInputRef(input) => Ok(plasm_core::IdentitySlot::binding(input)),
            plasm_core::Value::String(id) => Ok(plasm_core::IdentitySlot::lit(id)),
            plasm_core::Value::Bool(id) => Ok(plasm_core::IdentitySlot::lit(id.to_string())),
            plasm_core::Value::Integer(id) => Ok(plasm_core::IdentitySlot::lit(id.to_string())),
            _ => Err(at(
                e,
                "identity requires a string, exact integer, boolean or typed binding",
            )),
        }
    }
    pub(super) fn filter(
        &mut self,
        site: &PyExpr,
        source: &str,
        id: &str,
        expression: &PyExpr,
    ) -> Result<String, String> {
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
