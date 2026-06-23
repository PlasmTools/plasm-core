use crate::cgs_federation::FederationDispatch;
use crate::expr::{
    ChainExpr, CreateExpr, DeleteExpr, EntityKey, Expr, GetExpr, InvokeExpr, QueryExpr,
};
use crate::schema::{
    capability_method_label_kebab, path_var_names_from_mapping_json, CapabilityKind, CGS,
};
use crate::typed_invoke::InvokeInputPayload;
use crate::value::Value;

use super::predicates::{render_predicate_wire, split_search_predicate};
use super::values::{render_id_slot, render_surface_value};

pub(crate) struct RenderCtx<'a> {
    pub cgs: &'a CGS,
    pub fed: Option<&'a FederationDispatch>,
    pub fallback_cgs: &'a CGS,
}

impl<'a> RenderCtx<'a> {
    pub fn cgs_for_entity(&self, entry_id: Option<&str>, entity: &str) -> &'a CGS {
        if let (Some(fed), Some(eid)) = (self.fed, entry_id) {
            if let Ok(c) = fed.resolve_entity(
                entity,
                crate::row_composition::ResolutionHint::default(),
                self.fallback_cgs,
            ) {
                if c.entry_id.as_deref() == Some(eid) {
                    return c;
                }
            }
            if let Some(c) = fed.cgs_for_catalog_entry_id(eid, entity) {
                return c;
            }
        }
        if let Some(eid) = entry_id {
            if self.cgs.entry_id.as_deref() == Some(eid) {
                return self.cgs;
            }
        }
        self.cgs
    }

    pub fn render_expr(&self, expr: &Expr) -> String {
        match expr {
            Expr::Query(q) => self.render_query(q),
            Expr::Get(g) => self.render_get(g),
            Expr::Create(c) => self.render_create(c),
            Expr::Delete(d) => self.render_delete(d),
            Expr::Invoke(i) => self.render_invoke(i),
            Expr::Chain(c) => self.render_chain(c),
            Expr::Page(p) => {
                if let Some(l) = p.limit {
                    format!("page({}, limit={l})", p.handle)
                } else {
                    format!("page({})", p.handle)
                }
            }
            Expr::Wait(w) => format!("wait({})", w.handle),
            Expr::Cancel(c) => format!("cancel({})", c.handle),
            Expr::TeachingValue { value } => render_surface_value(value),
        }
    }

    fn render_query(&self, q: &QueryExpr) -> String {
        let entry_id = q.catalog_entry_id.as_deref();
        let head = q.entity.as_str();
        let cgs = self.cgs_for_entity(entry_id, head);
        if self.is_search_query(q, cgs) {
            return self.render_search_query(head, q, entry_id, cgs);
        }
        if let Some(pred) = &q.predicate {
            return format!(
                "{head}{{{}}}",
                render_predicate_wire(pred, head, entry_id, None, None)
            );
        }
        head.to_string()
    }

    fn is_search_query(&self, q: &QueryExpr, cgs: &CGS) -> bool {
        q.capability_name
            .as_ref()
            .and_then(|name| cgs.get_capability(name))
            .map(|c| c.kind == CapabilityKind::Search)
            .unwrap_or(false)
    }

    fn render_search_query(
        &self,
        head: &str,
        q: &QueryExpr,
        entry_id: Option<&str>,
        cgs: &CGS,
    ) -> String {
        let cap_name = q.capability_name.as_deref().unwrap_or("");
        let cap = cgs.get_capability(cap_name);
        let q_field = cap
            .and_then(|c| c.object_params())
            .and_then(|fields| {
                fields
                    .iter()
                    .find(|f| matches!(f.role, Some(crate::ParameterRole::Search)) || f.required)
                    .map(|f| f.name.as_str())
            })
            .unwrap_or("q");
        let mut text = String::new();
        let mut filters = Vec::new();
        if let Some(pred) = &q.predicate {
            split_search_predicate(pred, q_field, &mut text, &mut filters);
        }
        let mut out = format!("{head}~{text}");
        if !filters.is_empty() {
            let inner = filters
                .iter()
                .map(|p| {
                    render_predicate_wire(p, q.entity.as_str(), entry_id, Some(cap_name), None)
                })
                .collect::<Vec<_>>()
                .join(",");
            out.push('{');
            out.push_str(&inner);
            out.push('}');
        }
        out
    }

    fn render_get(&self, g: &GetExpr) -> String {
        let entry_id = g.catalog_entry_id.as_deref();
        let head = g.reference.entity_type.as_str();
        if let Some(path_vars) = &g.path_vars {
            if !path_vars.is_empty() {
                let cgs = self.cgs_for_entity(entry_id, head);
                if let Some(ent) = cgs.get_entity(head) {
                    let parts: Vec<String> = ent
                        .key_vars
                        .iter()
                        .map(|k| {
                            let val = path_vars
                                .get(k.as_str())
                                .map(render_surface_value)
                                .unwrap_or_else(|| "$".to_string());
                            format!("{k}={val}")
                        })
                        .collect();
                    if !parts.is_empty() {
                        return format!("{head}({})", parts.join(", "));
                    }
                }
                let parts: Vec<String> = path_vars
                    .iter()
                    .map(|(k, v)| format!("{k}={}", render_surface_value(v)))
                    .collect();
                return format!("{head}({})", parts.join(", "));
            }
        }
        match &g.reference.key {
            EntityKey::Simple(id) => {
                if id.is_empty() {
                    format!("{head}()")
                } else {
                    format!("{head}({})", render_id_slot(id.as_str()))
                }
            }
            EntityKey::Compound(parts) => {
                let cgs = self.cgs_for_entity(entry_id, head);
                let ent = cgs.get_entity(head);
                let kv: Vec<String> = if let Some(ent) = ent {
                    ent.key_vars
                        .iter()
                        .filter_map(|k| {
                            parts
                                .get(k.as_str())
                                .map(|v| format!("{k}={}", render_id_slot(v)))
                        })
                        .collect()
                } else {
                    parts
                        .iter()
                        .map(|(k, v)| format!("{k}={}", render_id_slot(v)))
                        .collect()
                };
                format!("{head}({})", kv.join(", "))
            }
        }
    }

    fn render_create(&self, c: &CreateExpr) -> String {
        let entry_id = c.catalog_entry_id.as_deref();
        let cgs = self.cgs_for_entity(entry_id, c.entity.as_str());
        let cap = cgs.get_capability(c.capability.as_str());
        if let Some(cap) = cap {
            let kebab = capability_method_label_kebab(cap);
            let method = kebab;
            let args = self.render_invoke_args(
                &c.input,
                cap,
                entry_id,
                cap.domain.as_str(),
                c.capability.as_str(),
            );
            if let Some(recv) = &c.dotted_receiver {
                let base = self.render_expr(recv);
                return format!("{base}.{method}{args}");
            }
            let head = cap.domain.as_str();
            return format!("{head}.{method}{args}");
        }
        format!("{}.{cap}", c.entity.as_str(), cap = c.capability)
    }

    fn render_delete(&self, d: &DeleteExpr) -> String {
        let entry_id = d.catalog_entry_id.as_deref();
        let cgs = self.cgs_for_entity(entry_id, d.target.entity_type.as_str());
        let cap = cgs.get_capability(d.capability.as_str());
        let base = self.render_get(&GetExpr::from_ref_with_path_vars(
            d.target.clone(),
            d.path_vars.clone(),
        ));
        if let Some(cap) = cap {
            let method = capability_method_label_kebab(cap);
            let args = self.render_invoke_args(
                &InvokeInputPayload::raw(Value::Null),
                cap,
                entry_id,
                d.target.entity_type.as_str(),
                d.capability.as_str(),
            );
            if args == "()" || args.is_empty() {
                return format!("{base}.{method}");
            }
            return format!("{base}.{method}{args}");
        }
        format!("{base}.{}", d.capability)
    }

    fn render_invoke(&self, i: &InvokeExpr) -> String {
        let entry_id = i.catalog_entry_id.as_deref();
        let cgs = self.cgs_for_entity(entry_id, i.target.entity_type.as_str());
        let cap = cgs.get_capability(i.capability.as_str());
        let base = self.render_get(&GetExpr::from_ref_with_path_vars(
            i.target.clone(),
            i.path_vars.clone(),
        ));
        if let Some(cap) = cap {
            let method = capability_method_label_kebab(cap);
            let input = i
                .input
                .as_ref()
                .cloned()
                .unwrap_or_else(|| InvokeInputPayload::raw(Value::Null));
            let args = self.render_invoke_args(
                &input,
                cap,
                entry_id,
                i.target.entity_type.as_str(),
                i.capability.as_str(),
            );
            if args.is_empty() {
                return format!("{base}.{method}");
            }
            return format!("{base}.{method}{args}");
        }
        format!("{base}.{}", i.capability)
    }

    fn render_chain(&self, c: &ChainExpr) -> String {
        let src = self.render_expr(&c.source);
        format!("{src}.{}", c.selector)
    }

    fn render_invoke_args(
        &self,
        input: &InvokeInputPayload,
        cap: &crate::CapabilitySchema,
        _entry_id: Option<&str>,
        _entity: &str,
        _capability: &str,
    ) -> String {
        let value = input.to_value();
        match value {
            Value::Null => String::new(),
            Value::Object(map) if map.is_empty() => "()".to_string(),
            Value::Object(map) => {
                let path_vars = path_var_names_from_mapping_json(&cap.mapping.template.0);
                let parts: Vec<String> = map
                    .iter()
                    .filter(|(k, _)| !path_vars.contains(k))
                    .map(|(k, v)| format!("{k}={}", render_surface_value(v)))
                    .collect();
                if parts.is_empty() {
                    "()".to_string()
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            other => format!("({})", render_surface_value(&other)),
        }
    }
}

pub(crate) fn render_expr_wire(
    expr: &Expr,
    cgs: &CGS,
    fed: Option<&FederationDispatch>,
    fallback: &CGS,
) -> String {
    let ctx = RenderCtx {
        cgs,
        fed,
        fallback_cgs: fallback,
    };
    ctx.render_expr(expr)
}
