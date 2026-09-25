//! Session-taught effects lowered into the same typed admission and ordering as Plasm.
use super::*;
use plasm_core::symbol_tuning::{CatalogScope, EntityBinding};
use plasm_core::{CapabilityKind, CatalogEntryStamp, IdentitySlot, PlasmInputRef, Value};
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn write(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        token: &str,
        owner: EntityBinding,
        receiver: Option<&str>,
        id: &str,
    ) -> Result<String, String> {
        let symbols = self.state.sym_map_for(self.es);
        let method = symbols
            .resolve_session_method(token)
            .map_err(|e| at(site, &e.to_string()))?;
        if method.entry_id != owner.entry_id || method.domain != owner.entity {
            return Err(at(
                site,
                "method and receiver catalog/entity ownership differ",
            ));
        }
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            self.es,
            owner.entry_id.as_str(),
            owner.entity.as_str(),
        )?;
        let cap = cgs
            .get_capability(method.capability.as_str())
            .ok_or("missing method capability")?;
        if !matches!(
            cap.kind,
            CapabilityKind::Create
                | CapabilityKind::Update
                | CapabilityKind::Delete
                | CapabilityKind::Action
        ) {
            return Err(at(site, "method is not a mutation or action"));
        }
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
                        method.capability.as_str(),
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
        let expr = match cap.kind {
            CapabilityKind::Create => {
                let mut create =
                    plasm_core::CreateExpr::new(method.capability, owner.entity, value);
                create.catalog_entry_id = stamp;
                if receiver.is_some() {
                    let mut get = plasm_core::GetExpr::from_ref(target);
                    get.catalog_entry_id = create.catalog_entry_id.clone();
                    create.dotted_receiver = Some(Box::new(plasm_core::Expr::Get(get)));
                }
                plasm_core::Expr::Create(create)
            }
            CapabilityKind::Delete => {
                let mut delete = plasm_core::DeleteExpr::with_target(method.capability, target);
                delete.catalog_entry_id = stamp;
                delete.input = Some(value.into());
                plasm_core::Expr::Delete(delete)
            }
            CapabilityKind::Update | CapabilityKind::Action => {
                let mut invoke =
                    plasm_core::InvokeExpr::with_target(method.capability, target, Some(value));
                invoke.catalog_entry_id = stamp;
                plasm_core::Expr::Invoke(invoke)
            }
            _ => return Err(at(site, "unsupported write kind")),
        };
        self.emit_catalog(id, expr)
    }

    pub(super) fn write_value(&self, e: &PyExpr) -> Result<Value, String> {
        match e {
            PyExpr::Name(n) => {
                if self
                    .row_scope
                    .as_ref()
                    .is_some_and(|scope| scope.parameter == n.id.as_str())
                {
                    return Err(at(e, "pass a row field, not the whole lambda row"));
                }
                if !self.state.contains(n.id.as_str()) {
                    return Err(at(e, "unknown write input binding"));
                }
                Ok(Value::PlasmInputRef(PlasmInputRef::node_output(
                    n.id.as_str(),
                    vec![],
                )))
            }
            PyExpr::Attribute(a) => {
                let binding = name(&a.value)
                    .ok_or_else(|| at(e, "write field inputs require a named binding"))?;
                let binding = self.scoped_binding(binding);
                let contract = super::super::binding_contract(&self.state, binding)
                    .ok_or("unknown input binding")?;
                if !contract.row_cardinality.permits_scalar_field_extract() {
                    return Err(at(e, "write field input requires a proven singleton"));
                }
                let row_schema = super::super::schema_validate::resolve_immediate_compute_schema(
                    &self.state,
                    &[],
                    binding,
                );
                let path = super::super::schema_validate::resolve_sort_field_path(
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
                    "write input",
                )?;
                Ok(Value::PlasmInputRef(
                    self.input_ref(binding, path.segments().to_vec()),
                ))
            }
            PyExpr::List(list) => Ok(Value::Array(
                list.elts
                    .iter()
                    .map(|e| self.write_value(e))
                    .collect::<Result<_, _>>()?,
            )),
            PyExpr::Dict(dict) => {
                let mut fields = indexmap::IndexMap::new();
                for item in &dict.items {
                    let key = string(
                        item.key
                            .as_ref()
                            .ok_or("write input dictionary unpacking is not admitted")?,
                    )?;
                    if fields.insert(key, self.write_value(&item.value)?).is_some() {
                        return Err(at(e, "duplicate input field"));
                    }
                }
                Ok(Value::Object(fields))
            }
            PyExpr::NoneLiteral(_) => Ok(Value::Null),
            _ => literal(e),
        }
    }
}
