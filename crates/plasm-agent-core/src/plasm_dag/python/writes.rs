//! Session-taught effects lowered into the same typed admission and ordering as Plasm.
use super::catalog_operations::{CatalogWriteKind, ResolvedCatalogCall};
use super::*;
use plasm_core::symbol_tuning::{CatalogScope, EntityBinding};
use plasm_core::{CatalogEntryStamp, IdentitySlot, PlasmInputRef, Value};
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn write(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        owner: EntityBinding,
        resolved: ResolvedCatalogCall<'_, CatalogWriteKind>,
        receiver: Option<&str>,
        id: &str,
    ) -> Result<String, String> {
        let symbols = self.state.sym_map_for(self.es);
        let kind = resolved.kind;
        let cgs = resolved.cgs;
        let cap = resolved.schema;
        if !call.arguments.args.is_empty() {
            return Err(at(
                site,
                "writes require named arguments; positional payloads are not admitted",
            ));
        }
        if cap.requires_receiver() && receiver.is_none() {
            return Err(at(site, "this method requires an entity receiver"));
        }
        let mut input = indexmap::IndexMap::new();
        for kw in &call.arguments.keywords {
            let key = kw
                .arg
                .as_ref()
                .ok_or_else(|| at(site, "write argument unpacking is not admitted"))?;
            let wire = if inputs::is_union_tag(cap, key.as_str()) {
                key.to_string()
            } else {
                symbols
                    .resolve_cap_param(
                        CatalogScope::qualified(owner.entry_id.as_str()),
                        owner.entity.as_str(),
                        resolved.capability.as_str(),
                        key.as_str(),
                        cap,
                    )
                    .map_err(|e| at(site, &e.to_string()))?
            };
            let value = self.write_value(&kw.value)?;
            if input.insert(wire, value).is_some() {
                return Err(at(site, "duplicate write argument"));
            }
        }
        let stamp = CatalogEntryStamp::some(owner.entry_id.clone());
        let target = if let Some(receiver) = receiver {
            let entity = cgs
                .get_entity(owner.entity.as_str())
                .ok_or("missing receiver entity")?;
            if entity.key_vars.is_empty() {
                plasm_core::Ref::simple_binding(
                    owner.entity.as_str(),
                    self.input_ref(receiver, vec![entity.id_field.to_string()]),
                )
            } else {
                plasm_core::Ref::compound_slots(
                    owner.entity.as_str(),
                    entity
                        .key_vars
                        .iter()
                        .map(|key| {
                            (
                                key.to_string(),
                                IdentitySlot::binding(
                                    self.input_ref(receiver, vec![key.to_string()]),
                                ),
                            )
                        })
                        .collect(),
                )
            }
        } else {
            // Same canonical pathless anchor as the existing parser, never a remote Get.
            plasm_core::GetExpr::pathless_nullary(owner.entity.as_str()).reference
        };
        let value = inputs::normalize(cap, input, cgs).map_err(|error| at(site, &error))?;
        let expr = match kind {
            CatalogWriteKind::Create => {
                let mut create =
                    plasm_core::CreateExpr::new(resolved.capability, owner.entity, value);
                create.catalog_entry_id = stamp;
                if receiver.is_some() {
                    let mut get = plasm_core::GetExpr::from_ref(target);
                    get.catalog_entry_id = create.catalog_entry_id.clone();
                    create.dotted_receiver = Some(Box::new(plasm_core::Expr::Get(get)));
                }
                plasm_core::Expr::Create(create)
            }
            CatalogWriteKind::Delete => {
                let mut delete = plasm_core::DeleteExpr::with_target(resolved.capability, target);
                delete.catalog_entry_id = stamp;
                delete.input = Some(value.into());
                plasm_core::Expr::Delete(delete)
            }
            CatalogWriteKind::Update | CatalogWriteKind::Action => {
                let mut invoke =
                    plasm_core::InvokeExpr::with_target(resolved.capability, target, Some(value));
                invoke.catalog_entry_id = stamp;
                plasm_core::Expr::Invoke(invoke)
            }
        };
        self.emit_catalog(id, expr)
    }

    pub(super) fn write_value(&mut self, e: &PyExpr) -> Result<Value, String> {
        let mut inputs = BTreeMap::new();
        let value = self.scoped_value(e, &mut inputs)?;
        self.value_operand(value, &inputs.into_values().collect::<Vec<_>>())
    }

    fn value_operand(
        &mut self,
        value: PlasmDataValue,
        inputs: &[crate::plasm_plan::PlanDataInput],
    ) -> Result<Value, String> {
        Ok(match value {
            PlasmDataValue::Literal { value } => value.into_value(),
            PlasmDataValue::NodeSymbol { node, path, .. } => {
                Value::PlasmInputRef(PlasmInputRef::node_output(node, path))
            }
            PlasmDataValue::BindingSymbol { binding, path } => {
                Value::PlasmInputRef(PlasmInputRef::row_binding(binding, path))
            }
            PlasmDataValue::Array { items } => Value::Array(
                items
                    .into_iter()
                    .map(|v| self.value_operand(v, inputs))
                    .collect::<Result<_, _>>()?,
            ),
            PlasmDataValue::Object { fields } => Value::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| Ok((k, self.value_operand(v, inputs)?)))
                    .collect::<Result<_, String>>()?,
            ),
            value => {
                let id = self.fresh();
                let node = self.emit_value(value, inputs.to_vec(), &id)?;
                Value::PlasmInputRef(PlasmInputRef::node_output(node, vec![]))
            }
        })
    }

    pub(super) fn field_input(&mut self, e: &PyExpr) -> Result<PlasmInputRef, String> {
        let PyExpr::Attribute(a) = e else {
            return Err(at(e, "expected a field dependency"));
        };
        let source = self.expr(&a.value, None)?;
        let binding = source.as_str();
        let contract =
            super::super::binding_contract(&self.state, binding).ok_or("unknown input binding")?;
        if !contract.row_cardinality.permits_scalar_field_extract() {
            return Err(at(e, "field input requires a proven singleton"));
        }
        let row_schema = super::super::schema_validate::resolve_immediate_compute_schema(
            &self.state,
            &[],
            binding,
        );
        let path = super::super::schema_validate::resolve_schema_field_path(
            self.es,
            None,
            Some(&contract.row_entity),
            row_schema.as_ref(),
            &FieldPath::from_dotted(a.attr.as_str())?,
        )?;
        super::super::schema_validate::validate_compute_paths_for_dag_source(
            self.es,
            &self.state,
            &[],
            binding,
            std::slice::from_ref(&path),
            "field input",
        )?;
        Ok(self.input_ref(binding, path.segments().to_vec()))
    }
}
